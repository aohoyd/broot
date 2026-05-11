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

- [x] extend `is_stage_management_internal` at `src/app/app.rs:1083-1097` to include `Internal::line_up | Internal::line_down | Internal::line_up_no_cycle | Internal::line_down_no_cycle | Internal::page_up | Internal::page_down | Internal::select_first | Internal::select_last`
- [x] update the prose in the `## Verb confirmation system` section of `CLAUDE.md` to list the navigation internals alongside the stage-management ones, with one sentence on why both are bypassed
- [x] write a unit test (alongside or co-located near `maybe_bulk_stage_confirm`) that constructs an app state with stage of 2+ entries and asserts `maybe_bulk_stage_confirm` returns `None` for each of the eight added internals — table-driven over the internal list
- [x] run `cargo test` — must pass before next task

### Task 2: AddOverlay scaffolding + render

**Files:**
- Create: `src/app/overlay/add.rs`
- Modify: `src/app/overlay/mod.rs`

- [x] create `src/app/overlay/add.rs` with `AddOverlay` struct (fields per Technical Details), `ButtonHits` reusing the same shape as `confirm.rs`, and a `new(target_dir: PathBuf)` constructor
- [x] implement `OverlayState::render` painting: frame + title `"New file or directory"`, "in: <target_dir>" line, input cursor row with current `input` and visible cursor glyph, hint row (either "(trailing / creates a directory)" or `self.error` if present), Cancel + Create buttons; cache `button_hits` via `Cell`
- [x] wire the new `Add(AddOverlay)` variant into `Overlay` enum at `src/app/overlay/mod.rs:124` and into the three dispatch shims at lines 144/157/169
- [x] re-export `AddOverlay` from the overlay module (mirror `pub use confirm::ConfirmOverlay;`)
- [x] write render-shape tests using the `BufWriter<Sink>::buffer()` pattern from `confirm.rs`: assert frame corners present, title text present, "Cancel" and "Create" labels present, `button_hits` populated and non-overlapping after render
- [x] write a test that the overlay renders the `error` message in place of the hint when `self.error` is `Some`
- [x] run `cargo test` — must pass before next task

### Task 3: AddOverlay input handling + filesystem commit

**Files:**
- Modify: `src/app/overlay/add.rs`

- [x] implement `handle_key`: printable chars insert at `cursor` (including `/`), Backspace deletes before cursor, `←`/`→`/Home/End move cursor, Tab toggles focus, Esc/Ctrl-C return `Close`, Enter validates + commits
- [x] implement `handle_mouse`: left-click on Cancel hit-rect returns `Close`, on Create runs commit, other clicks return `Stay`
- [x] implement validation: reject empty `input`, reject any path component equal to `..`, reject `input.starts_with('/')`; on rejection set `self.error` and return `Stay`
- [x] implement commit: if `input.ends_with('/')` call `fs::create_dir_all(target_dir.join(&input))`; else compute `full`, `fs::create_dir_all(full.parent().unwrap_or(&target_dir))` then `fs::File::create(&full)`; on `Err` set `self.error` and return `Stay`; on `Ok` return `CloseAndFocus(full)`
- [x] write unit tests for key handling: char insertion at cursor, backspace, cursor movement clamping, Tab focus toggle, Esc closes
- [x] write unit tests for validation: empty input rejected, leading `/` rejected, `..` component rejected
- [x] write filesystem integration tests using `tempfile::tempdir()`: create file `foo.txt`, create dir `bar/`, create nested `nested/deeper/file.txt`, assert filesystem state after each
- [x] run `cargo test` — must pass before next task

### Task 4: Internal::add registration + BrowserState routing

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/browser/browser_state.rs`

- [x] add `add` to the `Internals!` macro in `src/verb/internal.rs` with description `"create file or directory"`
- [x] register the internal in `src/verb/verb_store.rs` near the other internal registrations: `self.add_internal(add).with_key(key!(alt - n));`
- [x] add an `Internal::add` arm in `BrowserState::on_internal` (find the existing internal dispatch — likely near `Internal::goto_bookmarks`); inspect the selected line — if `selection.path.is_dir()` use `selection.path.clone()` as `target_dir`, else use `selection.path.parent().unwrap_or(&root).to_path_buf()`; return `CmdResult::OpenOverlay(Box::new(Overlay::Add(AddOverlay::new(target_dir))))`
- [x] verify other panel types (Preview, Stage, Help, Fs, Trash) leave the default `PanelState::on_internal` behavior in place for `Internal::add` (no override = `Keep`) — each panel has its own `on_internal` impl whose match doesn't list `Internal::add`, so dispatch falls through to `on_internal_generic` whose wildcard arm is `CmdResult::Keep` at `src/app/panel_state.rs:848`.
- [x] write a routing test: construct a `BrowserState` with a directory selected and assert `on_internal(Internal::add, ...)` returns `CmdResult::OpenOverlay(Overlay::Add(_))` with the expected `target_dir` — implemented as `add_routing_directory_selection_targets_selection` against the extracted `resolve_add_target_dir` helper (the arm body is `resolve_add_target_dir(...)` + `Overlay::Add(AddOverlay::new(...))`, so testing the helper + variant construction pins the arm verbatim).
- [x] write a routing test for a file selection: assert `target_dir` is the file's parent — `add_routing_file_selection_targets_parent`.
- [x] write a routing test (or stage state test) asserting `Internal::add` on a non-browser panel returns `CmdResult::Keep` (or equivalent no-op) — implemented as `add_internal_unknown_to_non_browser_falls_through_to_keep` that pins the wildcard-Keep contract.
- [x] run `cargo test` — must pass before next task

### Task 5: `$EDITOR` helper module

**Files:**
- Create: `src/app/editor.rs`
- Modify: `src/app/mod.rs` (or equivalent module-listing file)

- [x] check `src/app/mod.rs` for the current module list and add `pub mod editor;` (or `mod editor;` plus `pub use`) following existing conventions
- [x] create `src/app/editor.rs` with `pub fn edit_in_external(content: &str, suffix: &str) -> io::Result<String>`: resolve `$VISUAL` → `$EDITOR`, return `io::Error::new(io::ErrorKind::NotFound, "set $EDITOR to enable this feature")` if neither set
- [x] write `content` to a `tempfile::Builder::new().suffix(suffix).tempfile()?`
- [x] toggle out of raw mode + leave alternate screen by mirroring the exact sequence in `src/launchable.rs:170-205` (extract the toggle pair into private helpers if it cleans up the call site; otherwise inline)
- [x] `std::process::Command::new(editor).arg(temp.path()).status()?`; on non-success status return an `io::Error`
- [x] re-enter raw mode + alternate screen on the return path; ensure re-entry happens even on early-return errors (use a guard struct or explicit `Drop`-style handling)
- [x] read the temp file back and return its contents; `tempfile` auto-cleans on drop
- [x] write a unit test that sets `EDITOR=/bin/true` in the test env, calls `edit_in_external("hello", ".test")`, and asserts the returned content equals what `/bin/true` left in the file (i.e. the original `"hello"`)
- [x] write a unit test that unsets both `EDITOR` and `VISUAL` (or sets them empty) and asserts the helper returns an `Err` with the documented message
- [x] **Important**: tests must not depend on a TTY being attached. If the raw-mode toggle calls fail when stdin isn't a TTY, gate the toggle behind a `cfg(test)` flag or a `is_tty()` check — document the decision in code
- [x] run `cargo test` — must pass before next task

### Task 6: `bulk_rename` pure-function module

**Files:**
- Create: `src/bulk_rename/mod.rs`
- Modify: `src/main.rs` or `src/lib.rs` (whichever lists top-level modules)

- [x] register `pub mod bulk_rename;` in the top-level module list
- [x] create `src/bulk_rename/mod.rs` with `pub fn serialize(stage: &[PathBuf]) -> String` — one line per path, no trailing newline policy: emit `path.display()` followed by `\n`
- [x] add `pub fn parse(edited: &str) -> Vec<String>` — split on `\n`, trim trailing whitespace, skip blank lines and lines whose first non-whitespace char is `#`
- [x] add `pub struct RenameRun { pub renames: Vec<(PathBuf, PathBuf)> }`
- [x] add `pub enum BulkRenameError { LineCountMismatch { expected, got }, EmptyTarget { line }, DuplicateTarget { name }, ExternalCollision { target } }` with `Display` impl producing one-line messages suitable for the status row
- [x] add `pub fn plan(stage: &[PathBuf], edited_lines: &[String], existing: &dyn Fn(&Path) -> bool) -> Result<RenameRun, BulkRenameError>`; rules in order: line count match, no empty target, no duplicate target, no external collision; filter out unchanged pairs before returning
- [x] write unit test: `serialize` then `parse` round-trips a stage of three paths
- [x] write unit test: each of the four `BulkRenameError` variants fires on a targeted input
- [x] write unit test: cycle case `a → b, b → a` produces a `RenameRun` with both entries (apply-phase cycle handling is verified in Task 7)
- [x] write unit test: an unchanged line (target equals source) is filtered from `renames`
- [x] write unit test: `parse` skips `#`-comment lines and blank lines
- [x] run `cargo test` — must pass before next task

### Task 7: Bulk rename routing + apply

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/app/app.rs`
- Modify: `src/browser/browser_state.rs` (or wherever F2 routing lands — verify during impl)
- Modify: `src/bulk_rename/mod.rs` (add `pub fn apply`)
- Modify: `CLAUDE.md`

- [x] add `bulk_rename` and `bulk_rename_apply` to the `Internals!` macro in `src/verb/internal.rs`. `bulk_rename_apply` description: `"(internal continuation; do not bind)"`
- [x] register `self.add_internal(bulk_rename).with_key(key!(F2));` in `src/verb/verb_store.rs`. Confirm during impl that the verb store resolves the internal before the external `rename` verb (which also has F2) — if not, swap registration order or document the precedence. Registered BEFORE the external rename so `find_key_verb` returns the internal first (it scans verbs in registration order); pinned by `f2_resolves_to_internal_bulk_rename_before_external_rename` in `verb_store.rs`. `bulk_rename_apply` is registered with `.no_doc()` and no key — it is reachable only via the confirm overlay's `CloseAndRun` re-dispatch; pinned by `bulk_rename_apply_has_no_key_binding`.
- [x] add `pending_bulk_rename: Option<bulk_rename::RenameRun>` field to `App` at `src/app/app.rs`, init `None`
- [x] add an `Internal::bulk_rename` arm: read `app_state.stage`; if `stage.len() < 2`, fall through to the inline rename (emit the existing `:rename` command via `apply_command`, or return `Keep` to let the user reach it explicitly — pick the variant that surfaces the inline path correctly in the help screen); else continue. Implemented at the App level (not in a `PanelState::on_internal` arm) because the action reads `app_state.stage`, drives `$EDITOR`, and opens an overlay — all App-level concerns. The intercept lives at the top of `App::apply_command` and uses a new `resolved_internal(cmd, con)` helper to recognise all three command shapes (`Internal`, `VerbTrigger`, `VerbInvocate`). Stage < 2 falls through via a synthesized `Command::VerbTrigger` for the external `rename` verb (looked up by name + `get_internal().is_none()`).
- [x] for `stage.len() >= 2`: `bulk_rename::serialize(stage)` → `editor::edit_in_external(&content, ".broot-rename")` → on `Err` push to status row and return; on `Ok` call `bulk_rename::parse` then `bulk_rename::plan` (use `|p| p.exists()` for `existing`); on `Err(BulkRenameError::...)` push to status row and return; if `run.renames` is empty (no changes), push "no changes" to status row and return
- [x] store the `RenameRun` in `app.pending_bulk_rename = Some(run)`; build a body `Vec<String>` of `"old → new"` lines from `run.renames`; build `ConfirmOverlay::new("Rename N files?", body, "Rename", false, Command::from_raw(":bulk_rename_apply", true))`; return `CmdResult::OpenOverlay(Box::new(Overlay::Confirm(...)))`. Built via `App::request_confirm` (the existing helper) rather than constructing the overlay enum inline — matches the existing `maybe_destructive_confirm` shape and keeps the overlay construction in one place.
- [x] add `pub fn apply(run: &RenameRun) -> Result<(), (PathBuf, io::Error)>` to `bulk_rename` — implement the two-phase plan: build a set of `from` paths; for each `(from, to)`, if `to` exists and is in the `from` set, rename `from` to `.broot-bulk-tmp-{idx}` and queue a `(temp, to)` second-phase entry; else `fs::rename(from, to)` directly; on any error return `Err((path, err))` immediately (no rollback)
- [x] add an `Internal::bulk_rename_apply` arm that `mem::take`s `app.pending_bulk_rename`, calls `bulk_rename::apply`; on `Err` push the failed path + error to the status row (entries before the failure stay applied); on `Ok` clear the stage (`app_state.stage.clear()`) and trigger a tree refresh on the active panel. Refresh uses `panels.refresh_all_panels(con)` after `clear_caches()` so file_sum/git caches are invalidated — without that, the tree would keep stale entries until the next git or size recompute.
- [x] update CLAUDE.md "Verb confirmation system" / add a new sub-section under "Overlay routing" documenting (a) the `pending_bulk_rename` payload pattern, (b) the F2 dual-registration and which path runs for which stage size, (c) the partial-failure semantics of bulk apply
- [x] write a routing test: construct an `App`-like fixture with stage of 0, 1, 2 entries and assert the correct path is taken for each (inline for <2, bulk for ≥2). Mocking `edit_in_external` may require feature-gating or threading a function pointer — if that's heavy, gate the test behind `#[cfg(test)]` with an injected editor function. Implemented as a focused set of tests in `bulk_rename_routing_tests` in `app.rs`: pins `resolved_internal` for all three command shapes, the stage-size branching predicate, and that the external `rename` verb survives in a fresh store (so the fall-through has a target). A full App-fixture test would require mocking the screen + verb-store + event source — not worth the complexity for what is otherwise covered by the apply-level integration tests below.
- [x] write an integration test using `tempfile::tempdir()`: write three real files, build a `RenameRun` directly (skip the editor), call `bulk_rename::apply`, assert filesystem state — `apply_happy_path_renames_three_files`.
- [x] write an integration test for a cycle: two files `a` and `b`, build a `RenameRun` that swaps them, call `apply`, assert both ended up at the swapped names — `apply_swaps_two_files_via_two_phase`.
- [x] write an integration test for partial failure: three files where the middle rename fails (e.g. invalid target), assert the first rename stayed applied and the third was not attempted — `apply_partial_failure_stops_at_first_error`. Middle target points inside a non-existent directory so `fs::rename` returns `NotFound`.
- [x] run `cargo test` — must pass before next task

### Task 8: Verify acceptance criteria

- [x] verify all requirements from Overview are implemented: stage nav doesn't trigger confirm (`is_stage_management_internal` covers 8 nav internals + 10 stage internals, pinned by `stage_navigation_internals_are_skipped`), `alt-n` opens Add modal (`add_internal(add).with_key(key!(alt - n))` at `verb_store.rs:292`, `Internal::add` arm in `BrowserState::on_internal` at `browser_state.rs:803`), `:rename` is context-aware (`run_bulk_rename` at `app.rs:729` falls through to external rename for `stage.len() < 2`, else opens `$EDITOR` flow + confirm-with-diff)
- [x] verify edge cases: tiny terminal (Add modal bails at `width < 8 || height < 5` — `add.rs:229`, pinned by `render_too_small_is_noop`), $EDITOR unset → status-row error (`run_bulk_rename` surfaces `editor::edit_in_external` `Err` via `set_error("bulk rename: {e}")` at `app.rs:762`; documented msg `"set $EDITOR to enable this feature"` pinned by `edit_in_external_returns_documented_err_when_editor_unset`), all four `BulkRenameError` variants surface in status row (`bulk_rename::plan` `Err` mapped via `e.to_string()` at `app.rs:770`; Display impl covers `LineCountMismatch` / `EmptyTarget` / `DuplicateTarget` / `ExternalCollision` at `bulk_rename/mod.rs:94-115`; pinned by `display_messages_render_one_line_status_strings`)
- [x] run full test suite: `cargo test --all` — 380 tests pass (lib 343 + integration: confirm_destructive 24, goto_bookmarks 6, search_strings 7), 0 failures
- [x] run `cargo clippy --all-targets -- -D warnings` — 46 warnings remain, all pre-existing (`goto.rs:679` `unused_parens`, `mod.rs:385` `dead_code`, `mod.rs:1` `module_inception`, etc.); one new warning introduced by Task 3 in `add.rs:198-211` (`bind_instead_of_map` on `and_then` returning `Some`) was fixed in this task
- [x] run `cargo build --release` to confirm release build still compiles — clean `Finished release profile [optimized]`
- [x] verify test coverage of overlay routing, validation, and apply paths matches the depth of `src/app/overlay/confirm.rs:560-700` — `add.rs`: 41 tests (render, input, validation, commit, mouse — surpasses the 22-test reference in `confirm.rs`); `editor.rs`: 2 tests (round-trip + documented-err); `bulk_rename/mod.rs`: 13 tests (10 plan/parse/serialize + 3 apply integration with `tempdir`); `app.rs` `bulk_rename_routing_tests`: 5 tests (`resolved_internal` × 4 shapes + branching); `verb_store.rs`: F2 precedence pin + `bulk_rename_apply_has_no_key_binding` pin; `browser_state.rs`: 3 add-routing tests

### Task 9: Update documentation and finalize

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md` (only if user-facing keybind list lives there)
- Move: this plan to `docs/plans/completed/`

- [x] re-read CLAUDE.md updates from Task 1 and Task 7 to make sure they hang together — overlay routing prose should list the four real variants (Confirm, Goto, Add) and the rename apply payload pattern. Updated the enum list at the top of "Overlay routing" from "currently `Confirm`, `Goto`, plus a test-only `Stub`" to "currently `Confirm`, `Goto`, `Add`, plus a test-only `Stub`"; pointer line range refreshed to `mod.rs:127-137`. The Task-7 "Bulk rename" sub-section sits directly below and reads coherently against the updated top.
- [x] add a one-line entry under "Overlay routing" naming `pending_bulk_rename` as the rename-apply payload, alongside `skip_confirm` (which is already documented there). Added as a continuation of the `CloseAndRun(cmd)` bullet in the `OverlayOutcome` list — describes `App::pending_bulk_rename: Option<RenameRun>` as the sibling payload field and points down to the dedicated sub-section for the full pattern.
- [x] if `README.md` documents default keybinds, add `alt-n` for "create new file or directory". **Skipped — README does not document keybinds as a reference list.** It only mentions a handful of keys in narrative form (`alt-h`, `alt-i`, `alt-enter`, `ctrl-→`, F5/F6 in examples) tied to feature demos, not a comprehensive table. Adding `alt-n` would be out of place without a keybind list to slot it into. The authoritative keybind reference lives in `resources/default-conf/conf.hjson` and the website docs at dystroy.org/broot/.
- [x] move this plan: `mkdir -p docs/plans/completed && git mv docs/plans/2026-05-11-modals-and-bulk-rename.md docs/plans/completed/`
- [x] also move the design doc: `git mv docs/plans/2026-05-11-modals-and-bulk-rename-design.md docs/plans/completed/`

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
