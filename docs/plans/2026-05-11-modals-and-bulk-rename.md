# Modal refinements + Add modal + Bulk rename

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Fix the stage-panel bulk-confirm false-positives on navigation keys; add a new `Internal::add` overlay for creating files/dirs (trailing slash semantics, `alt-n`); and make `:rename` context-aware so a stage of two or more files opens an `$EDITOR`-backed bulk-rename flow with a diff confirm before apply.

**Architecture:** Three independent features sharing the existing overlay system (`Overlay` enum at `src/app/overlay/mod.rs:124`, `CmdResult::OpenOverlay`, `OverlayOutcome::CloseAndRun`). The bulk-rename plan payload is stored on `App` as a new `pending_bulk_rename` field, mirroring the existing `App::skip_confirm` pattern — no `Command` enum changes. The `$EDITOR` helper is extracted into a reusable module so future editor integrations can share it.

**Tech Stack:** Rust, termimad (frames, input field), crokey (key parsing), tempfile (already a dep), crossterm. No new dependencies.

## Overview

Today, broot's stage panel raises the bulk-staging confirm modal whenever the user is on stage with 2+ entries and presses any verb-shaped key — including pure navigation (`j`/`k`/↓/↑/PgUp/PgDn). Source: `is_stage_management_internal` lists only stage-management internals, not navigation. Fix is a single match arm extension.

The Add modal is new. It's a free-text overlay that asks for a filename relative to the current selection's directory (or the selection itself if it's a dir). Trailing slash creates a directory (mkdir -p style); otherwise a regular file. On success, broot navigates onto the new entry via `OverlayOutcome::CloseAndFocus`.

Bulk rename overloads `:rename` (F2). Stage empty or stage = 1 → existing inline single-rename behavior is unchanged. Stage ≥ 2 → broot writes the stage paths to a temp file, launches `$EDITOR`, parses the result, validates (line count, collisions, etc.), opens a `ConfirmOverlay` showing the diff (`old → new`), and on confirm performs the renames (two-phase for cycles).

Reference design: `docs/plans/2026-05-11-modals-and-bulk-rename-design.md`.

## Context (from discovery)

- **Overlay system**: `Overlay` enum has three variants today (Confirm, Goto, test-only Stub) at `src/app/overlay/mod.rs:124`. Adding a fourth means editing three dispatch shims (lines 144/157/169). Routing invariants documented in `CLAUDE.md` under "Overlay routing".
- **Confirm prompt intercept**: `App::apply_command` at `src/app/app.rs:157-190` runs three checks: bulk staging, overwrite, per-verb. The bypass list `is_stage_management_internal` lives at `:1083-1097`.
- **Stage**: `Stage` is `Vec<PathBuf>` + version counter at `src/stage/stage.rs`. Selection iteration via `FilteredStage`. Stage panel internal dispatch at `src/stage/stage_state.rs:428-451`.
- **Existing rename**: external verb at `src/verb/verb_store.rs:241-256`. F2 bound, `auto_exec: false`, single-file via `{file}` / `{parent}`.
- **Existing mkdir**: external verb at `:200-211`, shells out. No `Internal::add`.
- **Goto overlay**: closest precedent for an input-shaped overlay at `src/app/overlay/goto.rs` — but only matches single chars, no accumulator.
- **No `$EDITOR` integration anywhere in Rust source** (verified by grep). `tempfile` crate is already in `Cargo.toml:64` and used by `src/preview/preview_transformer.rs`.
- **`{staging}` substitution**: `src/verb/execution_builder.rs:214` supports `{staging:space-separated}` for external verbs. A user can write `vidir {staging:space-separated}` today; we're building a native flow with a diff confirm instead.

## Development Approach

- **Testing approach**: Regular (code first, tests alongside in the same task)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional — they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run `cargo test` after each task
- run `cargo build` after structural changes (enum variants, new modules)
- maintain backward compatibility — existing keybinds, verbs, conf.hjson schemas stay valid

## Testing Strategy

- **unit tests**: required for every task (see Development Approach)
- **integration tests with `tempfile::tempdir()`**: required for filesystem-touching tasks (Add modal commit path, bulk-rename apply)
- **render-output capture**: follow the pattern in `src/app/overlay/confirm.rs:560-700` — use `std::io::BufWriter::with_capacity(64 * 1024, std::io::sink())` and inspect `buffer()` pre-flush to assert overlay rendering without touching real stderr
- **no UI e2e tests**: broot is a TUI; manual smoke tests are the e2e equivalent. Add a "Manual verification" entry in Post-Completion for each user-facing piece.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

Three loosely-coupled deliverables landing in order:

1. **Stage nav bypass fix** — Section 1 of the design doc. Single-PR-sized.
2. **Add modal** — Section 2. New overlay variant + new internal + filesystem commit. Self-contained.
3. **Bulk rename** — Section 3. Pure-function module + `$EDITOR` helper + Confirm overlay reuse + apply logic. Largest piece.

Each builds on the previous only at the routing layer. The overlay enum gains one variant (Add); the bulk-rename plan rides through a new `App` field and reuses the existing `ConfirmOverlay`.

## Technical Details

**`AddOverlay` fields**:
```rust
pub struct AddOverlay {
    target_dir: PathBuf,
    input: String,
    cursor: usize,
    error: Option<String>,
    button_hits: Cell<Option<ButtonHits>>,
}
```

**`pending_bulk_rename` field on `App`**:
```rust
pending_bulk_rename: Option<RenameRun>,
```

**`bulk_rename::RenameRun`**:
```rust
pub struct RenameRun { pub renames: Vec<(PathBuf, PathBuf)> }
```

**`BulkRenameError` variants**: `LineCountMismatch`, `EmptyTarget`, `DuplicateTarget`, `ExternalCollision`.

**Two-phase rename for cycles**: when `to` exists as a remaining `from`, first phase renames `from → .broot-bulk-tmp-{n}` and queues a second phase that renames the temp to its final `to`.

**Processing flow (bulk rename)**:
```
Internal::bulk_rename (stage.len() >= 2)
  → bulk_rename::serialize(&stage) → temp file
  → editor::edit_in_external(content, ".broot-rename") → edited string
  → bulk_rename::parse(edited) → Vec<String>
  → bulk_rename::plan(stage, parsed, fs_exists) → RenameRun
  → store pending_bulk_rename
  → OpenOverlay(Confirm("Rename N files?", body=diff, cmd=":bulk_rename_apply"))
  → on confirm: Internal::bulk_rename_apply consumes pending, runs apply, clears stage, refreshes tree
```

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): all Rust source changes, tests, CLAUDE.md documentation updates
- **Post-Completion** (no checkboxes): manual TUI smoke tests, the `$EDITOR` integration manual test (since CI can't realistically launch a real editor), bumping the changelog entry if the project tracks one

## Implementation Steps

### Task 1: Stage navigation bypass fix

**Files:**
- Modify: `src/app/app.rs`
- Modify: `CLAUDE.md`

- [ ] extend `is_stage_management_internal` at `src/app/app.rs:1083-1097` to include `Internal::line_up | Internal::line_down | Internal::line_up_no_cycle | Internal::line_down_no_cycle | Internal::page_up | Internal::page_down | Internal::select_first | Internal::select_last`
- [ ] update the prose in the `## Verb confirmation system` section of `CLAUDE.md` to list the navigation internals alongside the stage-management ones, with one sentence on why both are bypassed
- [ ] write a unit test (alongside or co-located near `maybe_bulk_stage_confirm`) that constructs an app state with stage of 2+ entries and asserts `maybe_bulk_stage_confirm` returns `None` for each of the eight added internals — table-driven over the internal list
- [ ] run `cargo test` — must pass before next task

### Task 2: AddOverlay scaffolding + render

**Files:**
- Create: `src/app/overlay/add.rs`
- Modify: `src/app/overlay/mod.rs`

- [ ] create `src/app/overlay/add.rs` with `AddOverlay` struct (fields per Technical Details), `ButtonHits` reusing the same shape as `confirm.rs`, and a `new(target_dir: PathBuf)` constructor
- [ ] implement `OverlayState::render` painting: frame + title `"New file or directory"`, "in: <target_dir>" line, input cursor row with current `input` and visible cursor glyph, hint row (either "(trailing / creates a directory)" or `self.error` if present), Cancel + Create buttons; cache `button_hits` via `Cell`
- [ ] wire the new `Add(AddOverlay)` variant into `Overlay` enum at `src/app/overlay/mod.rs:124` and into the three dispatch shims at lines 144/157/169
- [ ] re-export `AddOverlay` from the overlay module (mirror `pub use confirm::ConfirmOverlay;`)
- [ ] write render-shape tests using the `BufWriter<Sink>::buffer()` pattern from `confirm.rs`: assert frame corners present, title text present, "Cancel" and "Create" labels present, `button_hits` populated and non-overlapping after render
- [ ] write a test that the overlay renders the `error` message in place of the hint when `self.error` is `Some`
- [ ] run `cargo test` — must pass before next task

### Task 3: AddOverlay input handling + filesystem commit

**Files:**
- Modify: `src/app/overlay/add.rs`

- [ ] implement `handle_key`: printable chars insert at `cursor` (including `/`), Backspace deletes before cursor, `←`/`→`/Home/End move cursor, Tab toggles focus, Esc/Ctrl-C return `Close`, Enter validates + commits
- [ ] implement `handle_mouse`: left-click on Cancel hit-rect returns `Close`, on Create runs commit, other clicks return `Stay`
- [ ] implement validation: reject empty `input`, reject any path component equal to `..`, reject `input.starts_with('/')`; on rejection set `self.error` and return `Stay`
- [ ] implement commit: if `input.ends_with('/')` call `fs::create_dir_all(target_dir.join(&input))`; else compute `full`, `fs::create_dir_all(full.parent().unwrap_or(&target_dir))` then `fs::File::create(&full)`; on `Err` set `self.error` and return `Stay`; on `Ok` return `CloseAndFocus(full)`
- [ ] write unit tests for key handling: char insertion at cursor, backspace, cursor movement clamping, Tab focus toggle, Esc closes
- [ ] write unit tests for validation: empty input rejected, leading `/` rejected, `..` component rejected
- [ ] write filesystem integration tests using `tempfile::tempdir()`: create file `foo.txt`, create dir `bar/`, create nested `nested/deeper/file.txt`, assert filesystem state after each
- [ ] run `cargo test` — must pass before next task

### Task 4: Internal::add registration + BrowserState routing

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/browser/browser_state.rs`

- [ ] add `add` to the `Internals!` macro in `src/verb/internal.rs` with description `"create file or directory"`
- [ ] register the internal in `src/verb/verb_store.rs` near the other internal registrations: `self.add_internal(add).with_key(key!(alt - n));`
- [ ] add an `Internal::add` arm in `BrowserState::on_internal` (find the existing internal dispatch — likely near `Internal::goto_bookmarks`); inspect the selected line — if `selection.path.is_dir()` use `selection.path.clone()` as `target_dir`, else use `selection.path.parent().unwrap_or(&root).to_path_buf()`; return `CmdResult::OpenOverlay(Box::new(Overlay::Add(AddOverlay::new(target_dir))))`
- [ ] verify other panel types (Preview, Stage, Help, Fs, Trash) leave the default `PanelState::on_internal` behavior in place for `Internal::add` (no override = `Keep`)
- [ ] write a routing test: construct a `BrowserState` with a directory selected and assert `on_internal(Internal::add, ...)` returns `CmdResult::OpenOverlay(Overlay::Add(_))` with the expected `target_dir`
- [ ] write a routing test for a file selection: assert `target_dir` is the file's parent
- [ ] write a routing test (or stage state test) asserting `Internal::add` on a non-browser panel returns `CmdResult::Keep` (or equivalent no-op)
- [ ] run `cargo test` — must pass before next task

### Task 5: `$EDITOR` helper module

**Files:**
- Create: `src/app/editor.rs`
- Modify: `src/app/mod.rs` (or equivalent module-listing file)

- [ ] check `src/app/mod.rs` for the current module list and add `pub mod editor;` (or `mod editor;` plus `pub use`) following existing conventions
- [ ] create `src/app/editor.rs` with `pub fn edit_in_external(content: &str, suffix: &str) -> io::Result<String>`: resolve `$VISUAL` → `$EDITOR`, return `io::Error::new(io::ErrorKind::NotFound, "set $EDITOR to enable this feature")` if neither set
- [ ] write `content` to a `tempfile::Builder::new().suffix(suffix).tempfile()?`
- [ ] toggle out of raw mode + leave alternate screen by mirroring the exact sequence in `src/launchable.rs:170-205` (extract the toggle pair into private helpers if it cleans up the call site; otherwise inline)
- [ ] `std::process::Command::new(editor).arg(temp.path()).status()?`; on non-success status return an `io::Error`
- [ ] re-enter raw mode + alternate screen on the return path; ensure re-entry happens even on early-return errors (use a guard struct or explicit `Drop`-style handling)
- [ ] read the temp file back and return its contents; `tempfile` auto-cleans on drop
- [ ] write a unit test that sets `EDITOR=/bin/true` in the test env, calls `edit_in_external("hello", ".test")`, and asserts the returned content equals what `/bin/true` left in the file (i.e. the original `"hello"`)
- [ ] write a unit test that unsets both `EDITOR` and `VISUAL` (or sets them empty) and asserts the helper returns an `Err` with the documented message
- [ ] **Important**: tests must not depend on a TTY being attached. If the raw-mode toggle calls fail when stdin isn't a TTY, gate the toggle behind a `cfg(test)` flag or a `is_tty()` check — document the decision in code
- [ ] run `cargo test` — must pass before next task

### Task 6: `bulk_rename` pure-function module

**Files:**
- Create: `src/bulk_rename/mod.rs`
- Modify: `src/main.rs` or `src/lib.rs` (whichever lists top-level modules)

- [ ] register `pub mod bulk_rename;` in the top-level module list
- [ ] create `src/bulk_rename/mod.rs` with `pub fn serialize(stage: &[PathBuf]) -> String` — one line per path, no trailing newline policy: emit `path.display()` followed by `\n`
- [ ] add `pub fn parse(edited: &str) -> Vec<String>` — split on `\n`, trim trailing whitespace, skip blank lines and lines whose first non-whitespace char is `#`
- [ ] add `pub struct RenameRun { pub renames: Vec<(PathBuf, PathBuf)> }`
- [ ] add `pub enum BulkRenameError { LineCountMismatch { expected, got }, EmptyTarget { line }, DuplicateTarget { name }, ExternalCollision { target } }` with `Display` impl producing one-line messages suitable for the status row
- [ ] add `pub fn plan(stage: &[PathBuf], edited_lines: &[String], existing: &dyn Fn(&Path) -> bool) -> Result<RenameRun, BulkRenameError>`; rules in order: line count match, no empty target, no duplicate target, no external collision; filter out unchanged pairs before returning
- [ ] write unit test: `serialize` then `parse` round-trips a stage of three paths
- [ ] write unit test: each of the four `BulkRenameError` variants fires on a targeted input
- [ ] write unit test: cycle case `a → b, b → a` produces a `RenameRun` with both entries (apply-phase cycle handling is verified in Task 7)
- [ ] write unit test: an unchanged line (target equals source) is filtered from `renames`
- [ ] write unit test: `parse` skips `#`-comment lines and blank lines
- [ ] run `cargo test` — must pass before next task

### Task 7: Bulk rename routing + apply

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/app/app.rs`
- Modify: `src/browser/browser_state.rs` (or wherever F2 routing lands — verify during impl)
- Modify: `src/bulk_rename/mod.rs` (add `pub fn apply`)
- Modify: `CLAUDE.md`

- [ ] add `bulk_rename` and `bulk_rename_apply` to the `Internals!` macro in `src/verb/internal.rs`. `bulk_rename_apply` description: `"(internal continuation; do not bind)"`
- [ ] register `self.add_internal(bulk_rename).with_key(key!(F2));` in `src/verb/verb_store.rs`. Confirm during impl that the verb store resolves the internal before the external `rename` verb (which also has F2) — if not, swap registration order or document the precedence
- [ ] add `pending_bulk_rename: Option<bulk_rename::RenameRun>` field to `App` at `src/app/app.rs`, init `None`
- [ ] add an `Internal::bulk_rename` arm: read `app_state.stage`; if `stage.len() < 2`, fall through to the inline rename (emit the existing `:rename` command via `apply_command`, or return `Keep` to let the user reach it explicitly — pick the variant that surfaces the inline path correctly in the help screen); else continue
- [ ] for `stage.len() >= 2`: `bulk_rename::serialize(stage)` → `editor::edit_in_external(&content, ".broot-rename")` → on `Err` push to status row and return; on `Ok` call `bulk_rename::parse` then `bulk_rename::plan` (use `|p| p.exists()` for `existing`); on `Err(BulkRenameError::...)` push to status row and return; if `run.renames` is empty (no changes), push "no changes" to status row and return
- [ ] store the `RenameRun` in `app.pending_bulk_rename = Some(run)`; build a body `Vec<String>` of `"old → new"` lines from `run.renames`; build `ConfirmOverlay::new("Rename N files?", body, "Rename", false, Command::from_raw(":bulk_rename_apply", true))`; return `CmdResult::OpenOverlay(Box::new(Overlay::Confirm(...)))`
- [ ] add `pub fn apply(run: &RenameRun) -> Result<(), (PathBuf, io::Error)>` to `bulk_rename` — implement the two-phase plan: build a set of `from` paths; for each `(from, to)`, if `to` exists and is in the `from` set, rename `from` to `.broot-bulk-tmp-{idx}` and queue a `(temp, to)` second-phase entry; else `fs::rename(from, to)` directly; on any error return `Err((path, err))` immediately (no rollback)
- [ ] add an `Internal::bulk_rename_apply` arm that `mem::take`s `app.pending_bulk_rename`, calls `bulk_rename::apply`; on `Err` push the failed path + error to the status row (entries before the failure stay applied); on `Ok` clear the stage (`app_state.stage.clear()`) and trigger a tree refresh on the active panel
- [ ] update CLAUDE.md "Verb confirmation system" / add a new sub-section under "Overlay routing" documenting (a) the `pending_bulk_rename` payload pattern, (b) the F2 dual-registration and which path runs for which stage size, (c) the partial-failure semantics of bulk apply
- [ ] write a routing test: construct an `App`-like fixture with stage of 0, 1, 2 entries and assert the correct path is taken for each (inline for <2, bulk for ≥2). Mocking `edit_in_external` may require feature-gating or threading a function pointer — if that's heavy, gate the test behind `#[cfg(test)]` with an injected editor function
- [ ] write an integration test using `tempfile::tempdir()`: write three real files, build a `RenameRun` directly (skip the editor), call `bulk_rename::apply`, assert filesystem state
- [ ] write an integration test for a cycle: two files `a` and `b`, build a `RenameRun` that swaps them, call `apply`, assert both ended up at the swapped names
- [ ] write an integration test for partial failure: three files where the middle rename fails (e.g. invalid target), assert the first rename stayed applied and the third was not attempted
- [ ] run `cargo test` — must pass before next task

### Task 8: Verify acceptance criteria

- [ ] verify all requirements from Overview are implemented: stage nav doesn't trigger confirm, `alt-n` opens Add modal, `:rename` is context-aware
- [ ] verify edge cases: tiny terminal (Add modal bails like Confirm does for `width < 8 || height < 5`), $EDITOR unset → status-row error, all four `BulkRenameError` variants surface in status row
- [ ] run full test suite: `cargo test --all`
- [ ] run `cargo clippy --all-targets -- -D warnings`
- [ ] run `cargo build --release` to confirm release build still compiles
- [ ] verify test coverage of overlay routing, validation, and apply paths matches the depth of `src/app/overlay/confirm.rs:560-700`

### Task 9: Update documentation and finalize

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md` (only if user-facing keybind list lives there)
- Move: this plan to `docs/plans/completed/`

- [ ] re-read CLAUDE.md updates from Task 1 and Task 7 to make sure they hang together — overlay routing prose should list the four real variants (Confirm, Goto, Add) and the rename apply payload pattern
- [ ] add a one-line entry under "Overlay routing" naming `pending_bulk_rename` as the rename-apply payload, alongside `skip_confirm` (which is already documented there)
- [ ] if `README.md` documents default keybinds, add `alt-n` for "create new file or directory"
- [ ] move this plan: `mkdir -p docs/plans/completed && git mv docs/plans/2026-05-11-modals-and-bulk-rename.md docs/plans/completed/`
- [ ] also move the design doc: `git mv docs/plans/2026-05-11-modals-and-bulk-rename-design.md docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention — no checkboxes, informational only*

**Manual TUI smoke tests** (cannot be automated in CI):

1. **Add modal**: launch broot, press `alt-n`, type `hello.txt`, press Enter. Verify the file is created next to the cursor selection (or inside it if a directory was selected) and broot navigates onto it.
2. **Add modal — directory**: `alt-n`, type `newdir/`, Enter. Verify `newdir/` is created.
3. **Add modal — nested**: `alt-n`, type `a/b/c.txt`, Enter. Verify intermediate dirs are created and the file appears.
4. **Add modal — error path**: `alt-n`, type `../escape.txt`, Enter. Verify the modal stays open with an error in the hint row.
5. **Stage nav**: stage two files (`+` twice), focus stage panel, press `j`/`k`. Verify no confirm modal appears.
6. **Bulk rename — happy path**: stage three files, press `F2`. Verify `$EDITOR` opens with the three paths. Edit one or more names. Save and quit. Verify a confirm modal appears showing the `old → new` diff. Press `y`. Verify renames applied and stage cleared.
7. **Bulk rename — cycle**: stage two files `a` and `b`. F2. In the editor swap their names. Save. Confirm. Verify both ended swapped (the two-phase rename worked).
8. **Bulk rename — no $EDITOR**: `env -u EDITOR -u VISUAL broot`, stage 2+ files, F2. Verify a status-row error appears and broot does not crash.

**External system updates**: none. broot has no consuming projects to coordinate with.
