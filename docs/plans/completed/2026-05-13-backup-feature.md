# Backup feature — implementation plan

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Add a `:backup` verb (bound to `alt-shift-b`) that copies the selection with a configurable suffix (default `.bak`), with rename-style prefilled-name editing for single targets and a ConfirmOverlay diff for bulk (stage ≥ 2) targets. Never overwrites — numbered fallback (`.bak`, `.bak.1`, …).

**Architecture:** Three new internals (`backup`, `backup_one`, `backup_apply`) mirroring the `bulk_rename` / external `rename` / `bulk_rename_apply` triplet, but executed in-process (`fs::copy` / `copy_dir_recursively`) instead of shelling out. App-level intercept routes the trigger to single-vs-bulk based on stage size. New `{:backup-name}` placeholder type and new `Conf::backup_suffix` config field.

**Tech Stack:** Rust, existing broot infrastructure (`crokey`, `serde`, `tempfile` for tests).

## Overview

broot today has a `:rename` flow that prefills the input bar with the current filename and lets the user edit before applying. This plan adds a parallel `:backup` flow that **copies** the selection (original stays) to a name with a configurable suffix. For 2+ staged paths, an auto-suffix bulk plan is built and surfaced in a ConfirmOverlay diff so the user can review and approve in one keystroke. Collisions on the default suffix are resolved by appending `.1`, `.2`, … up to `.999` — broot never silently overwrites.

Source of truth for design decisions: `docs/plans/2026-05-13-backup-feature-design.md`.

## Context (from discovery)

- **Trigger / receiver split**: `bulk_rename` internal (key-bound, App-intercepted) plus external `rename` (`auto_exec:false`, prefill receiver) — `src/verb/verb_store.rs:245-274`, `src/app/app.rs:737-755` (single-file branch), `src/app/app.rs:843-849` (`find_external_rename_verb_id`).
- **Placeholder substitution**: `get_sel_name_standard_replacement` at `src/verb/execution_builder.rs:126-141`; handles `file-name`, `file-stem`, etc. Takes `con: &AppContext`.
- **In-process copy primitive**: `copy_dir_recursively` at `src/app/panel_state.rs:1662` (currently private — needs to be made `pub(crate)`). Used today by stage `copy_from_staging` and `move_from_staging`.
- **ConfirmOverlay caller pattern**: `request_confirm` at `src/app/app.rs:142`, `OverlayOutcome::CloseAndRun` handling at `src/app/app.rs:867-898`. Body soft-wrap on `" → "` lines lives at `src/app/overlay/confirm.rs:417` and is reused as-is.
- **Bulk-stage deny-list**: `is_stage_consuming_internal` at `src/app/app.rs:1310`. `Internal::backup` is **not** added — the App-level intercept catches it before the deny-list check (mirrors `bulk_rename`).
- **Config plumbing**: `Conf::backup_suffix` follows the `date_time_format` / `icon_theme` pattern. Field at `src/conf/conf.rs:~75`, merge line at `~245`, default applied in `src/app/app_context.rs:~193`. CLAUDE.md "Bookmarks config plumbing" gotcha applies — forgetting the `overwrite!` line silently drops user values.
- **App state field**: `pending_backup: Option<BackupRun>` next to `pending_bulk_rename` at `src/app/app.rs:75-90`. Cleared in the `OverlayOutcome::Close` arm.
- **Internal enum**: `src/verb/internal.rs` (declarative `internals!` macro).

## Development Approach

- **Testing approach**: Regular (code + tests in the same task)
- complete each task fully before moving to the next
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task** (`cargo test`)
- **CRITICAL: update this plan file when scope changes during implementation**
- maintain backward compatibility — `alt-shift-b` is not currently bound, so this is purely additive

## Testing Strategy

- **unit tests**:
  - `src/backup/mod.rs` — `next_free_backup_name`, `plan_bulk_backup`, `apply` (using `tempfile`)
  - `src/verb/verb_store.rs` — pin tests for the three internals' registration and the `alt-shift-b` key binding
  - `src/conf/conf.rs` — round-trip test for the `backup_suffix` deserialization
  - `src/app/app_context.rs` (or wherever the validation lives) — `is_valid_backup_suffix` rejection cases
- **integration tests**: a new test module (e.g. inline in `src/app/app.rs` if a `#[cfg(test)]` block already exists; otherwise alongside the new module) that drives `Internal::backup` against synthetic stages and asserts overlay state.
- **e2e tests**: broot has no automated UI/e2e suite. Manual smoke testing covered in Post-Completion.

## Progress Tracking
- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

| Internal | Key | invocation pattern | auto_exec | role |
|---|---|---|---|---|
| `Internal::backup`       | `alt-shift-b` | (none)                          | `true`  | Trigger; App-intercept routes to single or bulk |
| `Internal::backup_one`   | (none)        | `backup_one {new_filename:backup-name}` | `false` | Single-file receiver; prefill bar, then apply on Enter |
| `Internal::backup_apply` | (none)        | (none)                          | `true`  | Bulk receiver; consumes `App::pending_backup` |

**Key flow:**
- `alt-shift-b` with stage < 2 → `Internal::backup` → intercept synthesizes `Command::VerbTrigger { backup_one }` → `panel_input` prefills `backup_one {next_free_name}` → user edits → Enter → `Internal::backup_one` with args → `fs::copy` or `copy_dir_recursively`.
- `alt-shift-b` with stage ≥ 2 → `Internal::backup` → intercept builds `BackupRun` via `plan_bulk_backup`, stashes on `App::pending_backup`, opens `ConfirmOverlay` listing `src → dst` rows with `pending = ":backup_apply"`. On Enter → `Internal::backup_apply` → `mem::take(pending_backup)` → sequential copy.

**Numbered-fallback policy:** Default prefill probes `{name}{suffix}`, then `{name}{suffix}.1`, `.2`, … up to `.999`. If the user manually types a colliding name, the apply rejects with status error (no silent override). Same logic for every row of a bulk plan.

## Technical Details

### `BackupRun` data shape

```rust
pub struct BackupCopy { pub src: PathBuf, pub dst: PathBuf }
pub struct BackupRun  { pub copies: Vec<BackupCopy> }
const MAX_BACKUP_BUMP: u32 = 999;
```

### Suffix validation (rejects → fallback to `.bak` with `cli_log::warn!`)
- empty string
- contains `/`, `\\`, or `\0`

### Partial-failure semantics for bulk
Mirrors `bulk_rename::apply`: sequential, stop on first `io::Error`, surviving copies stay, stage NOT cleared, status row reports the failing path.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): all code in this repo, including tests and CLAUDE.md updates.
- **Post-Completion** (no checkboxes): manual smoke testing in a live terminal.

## Implementation Steps

### Task 1: Add `backup_suffix` config plumbing

**Files:**
- Modify: `src/conf/conf.rs`
- Modify: `src/app/app_context.rs`
- Modify: `resources/default-conf/conf.hjson`

- [ ] in `src/conf/conf.rs`, add `pub backup_suffix: Option<String>` field with `#[serde(alias = "backup-suffix")]`, positioned alphabetically near other scalar string fields
- [ ] in the merge block in `src/conf/conf.rs` (~line 245), add `overwrite!(self, backup_suffix, conf);` — verify this is reached during `read_file`
- [ ] in `src/app/app_context.rs`, add `pub backup_suffix: String` field on `AppContext` and resolve it in `AppContext::from` with validation: reject empty/contains `/`/`\\`/`\0`, fall back to `".bak"` with `cli_log::warn!` on rejection
- [ ] extract the validation predicate as a small free function `fn is_valid_backup_suffix(s: &str) -> bool` near `AppContext::from` so it's testable
- [ ] add commented sample line to `resources/default-conf/conf.hjson` near other commented options: `# backup_suffix: ".bak"` plus a one-line comment explaining the `.N` collision behavior
- [ ] write test in `src/conf/conf.rs` (or `src/app/app_context.rs`) for `is_valid_backup_suffix` covering: empty, contains `/`, contains `\\`, contains `\0`, valid `.bak`, valid `~`, valid `.backup`
- [ ] write test that constructs a `Conf` with `backup_suffix: Some("...")` and verifies `AppContext::from` stores the value
- [ ] write test that an invalid suffix falls back to `.bak`
- [ ] run `cargo test` — must pass before Task 2

### Task 2: Add `backup` module with name computation and bulk planning

**Files:**
- Create: `src/backup/mod.rs`
- Modify: `src/lib.rs` (add `pub mod backup;`)

- [ ] create `src/backup/mod.rs` with `BackupCopy`, `BackupRun`, `MAX_BACKUP_BUMP` const, `next_free_backup_name(src: &Path, suffix: &str) -> Option<PathBuf>`, and `plan_bulk_backup(paths: &[PathBuf], suffix: &str) -> BackupRun`
- [ ] `next_free_backup_name`: probe `parent / {file_name}{suffix}` first, then `.1` through `.MAX_BACKUP_BUMP`; return `None` only when every candidate exists
- [ ] `plan_bulk_backup`: skip paths with no parent or no file_name; for each remaining path call `next_free_backup_name`; for paths that exhaust the cap, include them with `dst = src` (sentinel that the apply step will catch) OR drop them — pick the cleaner option during implementation and document in the function doc-comment
- [ ] expose module from `src/lib.rs`
- [ ] write tests using `tempfile`: zero collisions → `.bak`; collision on `.bak` only → `.bak.1`; collisions on `.bak`, `.bak.1`, `.bak.2` → `.bak.3`; all `.bak` through `.bak.MAX_BACKUP_BUMP` exist → `None`
- [ ] write test for `plan_bulk_backup` with a heterogeneous list (regular file, dir, nonexistent path); spot-check each row's `dst`
- [ ] write test for directory: `foo/` produces `foo.bak/` (still a directory)
- [ ] write test for unusual suffix like `~` to confirm format-agnosticism
- [ ] run `cargo test` — must pass before Task 3

### Task 3: Add `apply` function for executing a BackupRun

**Files:**
- Modify: `src/backup/mod.rs`
- Modify: `src/app/panel_state.rs` (expose `copy_dir_recursively` as `pub(crate)`)

- [ ] in `src/app/panel_state.rs:1662`, change `fn copy_dir_recursively` to `pub(crate) fn copy_dir_recursively` so the backup module can call it
- [ ] add `pub fn apply(run: &BackupRun) -> Result<(), (PathBuf, std::io::Error)>` to `src/backup/mod.rs`
- [ ] for each `BackupCopy`: if `src.is_dir()`, call `crate::app::panel_state::copy_dir_recursively(&src, &dst)`; else `std::fs::copy(&src, &dst).map(drop)`
- [ ] stop on first `Err`, returning the failing path + error; copies before the failure stay
- [ ] handle the cap-exhaust sentinel from Task 2 (if `dst == src`, return error `"too many backups for {src}"` for that row before doing any copy)
- [ ] write test using `tempfile`: 3-file run, all succeed
- [ ] write test where one of the destinations is unwritable (set parent dir read-only on Unix; skip the test on Windows with `#[cfg(unix)]` or similar gate): asserts surviving copies stay and the error path + message are returned
- [ ] write test for directory copy: source dir with nested files copies into dst dir with same nested structure
- [ ] run `cargo test` — must pass before Task 4

### Task 4: Add three `Internal` variants and register them in the verb store

**Files:**
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`

- [ ] add `backup`, `backup_one`, `backup_apply` variants to the `Internal` enum / `internals!` macro in `src/verb/internal.rs`; copy the metadata shape of nearby zero-arg entries like `bulk_rename`
- [ ] in `src/verb/verb_store.rs::add_builtin_verbs`, register `Internal::backup` with `.with_key(key!(alt-shift-b))` and a short description; positioned NEAR the `bulk_rename` block to keep related verbs together
- [ ] register `Internal::backup_one` with the invocation pattern `backup_one {new_filename:backup-name}`, `auto_exec(false)`, no key, description that it's the receiver
- [ ] register `Internal::backup_apply` with no invocation pattern, `auto_exec(true)`, no key, hidden from completion (set the hidden flag if the macro supports it; otherwise omit description), description that it's bulk-apply plumbing
- [ ] verify by reading the call site that the chain `.with_key(...)` / `.auto_exec(false)` is available — if `auto_exec` isn't directly available on internals, follow the same trick the external `rename` verb uses (it's `auto_exec: false`); inspect `verb_store.rs:259-274` for the exact builder calls
- [ ] add pin test `backup_internals_registered` in `verb_store.rs` (mirror `sort_by_type_dirs_internals_registered`): asserts all three internals are present by name
- [ ] add pin test `backup_keybind_resolves_to_trigger`: simulate `find_key_verb` with `alt-shift-b` and assert the returned verb's internal is `Internal::backup`, not the receiver/apply variants
- [ ] run `cargo test` — must pass before Task 5

### Task 5: Add `backup-name` placeholder to execution_builder

**Files:**
- Modify: `src/verb/execution_builder.rs`

- [ ] in `get_sel_name_standard_replacement` (`src/verb/execution_builder.rs:126-141`), add a `"backup-name"` match arm
- [ ] the arm calls `crate::backup::next_free_backup_name(path, &con.backup_suffix)`, takes `file_name()` from the result, converts to `String`, returns `Some(...)`
- [ ] if `next_free_backup_name` returns `None` (cap exhausted), fall back to the bare `format!("{}{}", path.file_name().to_string_lossy(), &con.backup_suffix)` so the prefill is still useful
- [ ] write unit test using `tempfile`: create a file, call the helper or invoke the parser path that resolves `{new_filename:backup-name}` against that file, assert the returned string equals `<name>.bak`
- [ ] write unit test: pre-create `<name>.bak`, assert the helper returns `<name>.bak.1`
- [ ] write unit test for cap-exhaust → returns the bare name fallback
- [ ] run `cargo test` — must pass before Task 6

### Task 6: Wire `Internal::backup` intercept and `run_backup` handler

**Files:**
- Modify: `src/app/app.rs`

- [ ] add `pub pending_backup: Option<crate::backup::BackupRun>` to `App` struct, next to `pending_bulk_rename` (~`src/app/app.rs:75-90`)
- [ ] initialize to `None` in `App::new` (or wherever the existing `pending_bulk_rename: None` is set)
- [ ] add helper `fn find_internal_verb_id(&self, target: Internal, con: &AppContext) -> Option<VerbId>` near `find_external_rename_verb_id` (`app.rs:843`); iterates `con.verb_store.verbs()` and matches `verb.get_internal() == Some(target)`
- [ ] in `App::apply_command`, after the `skip_confirm` / overlay-open early-out and **before** `maybe_bulk_stage_confirm`, add intercept arms for `Internal::backup`, `Internal::backup_one`, `Internal::backup_apply` (matching via `resolved_internal(cmd, con)`); route to three new private methods `run_backup`, `run_backup_one`, `run_backup_apply`
- [ ] implement `run_backup`: read `app_state.stage.paths()`. If `len() >= 2`, build `plan_bulk_backup`, stash on `self.pending_backup`, open `ConfirmOverlay` (use existing `request_confirm`-style helper) with body = `src → dst` lines, title `"Backup N files?"`, confirm label `"Backup"`, danger `false`, pending command `Command::from_raw(":backup_apply", true)`, return `Ok(CmdResult::Keep)`. If `len() < 2`, find the verb id for `Internal::backup_one`, synthesize `Command::VerbTrigger`, recurse into `apply_command`.
- [ ] handle the edge case where the planner returns at least one cap-exhausted row in the bulk path: open a status error and DO NOT open the overlay (or surface the error in the overlay footer — pick what fits cleanest with the existing `ConfirmOverlay` API)
- [ ] extend the `OverlayOutcome::Close` arm at `src/app/app.rs:867-898` to also `self.pending_backup = None` (regression-prevention: cancelled overlay leaving stale plan)
- [ ] write integration test (inline `#[cfg(test)]` if a test module exists; otherwise a new one): construct an `App` with a stage of 2+ paths, dispatch `Internal::backup`, assert `self.overlay` is `Some(Overlay::Confirm(_))` and `self.pending_backup.is_some()`
- [ ] write test: same setup, simulate the Close path (Esc / cancel), assert `pending_backup` becomes `None`
- [ ] write test: stage size 0 dispatches the recurse-into-VerbTrigger path (assert by checking that `pending_backup` stays `None` and overlay stays `None`)
- [ ] run `cargo test` — must pass before Task 7

### Task 7: Implement `run_backup_one` (single-file receiver)

**Files:**
- Modify: `src/app/app.rs`

- [ ] implement `run_backup_one`: extract `args` from the incoming command (the user-edited filename); if `args` is `None` or empty, return a status error
- [ ] derive target: `parent = selection.parent()?`; `target = parent.join(args)`
- [ ] if `target.exists()`, return status error `"backup destination already exists"` (no silent overwrite of an explicit user choice)
- [ ] dispatch on `selection.is_dir()`: call `crate::app::panel_state::copy_dir_recursively(&selection, &target)` for dirs, `std::fs::copy(&selection, &target).map(drop)` for files
- [ ] on success, refresh panels and synthesize a focus on the new target (look at how `AddOverlay::try_commit` produces `CloseAndFocus(full)` — same pattern: `CmdResult` that triggers a `:focus <target>` follow-up so the new backup is selected in the tree)
- [ ] on I/O error, set status error with the path + io::Error string
- [ ] write integration test using `tempfile`: dispatch `Internal::backup_one` with `args=Some("foo.bak")` on a single file, assert file is created and original still exists
- [ ] write test for directory: `args=Some("dir.bak")` on a directory, assert recursive copy occurred
- [ ] write test for explicit collision: pre-create `foo.bak`, dispatch with `args=Some("foo.bak")`, assert status error and no fs change
- [ ] write test for filesystem-root selection: assert status error `"cannot backup the filesystem root"`
- [ ] run `cargo test` — must pass before Task 8

### Task 8: Implement `run_backup_apply` (bulk receiver)

**Files:**
- Modify: `src/app/app.rs`

- [ ] implement `run_backup_apply`: `let run = self.pending_backup.take()` — if `None`, status error `"no pending backup"` (stale-guard mirroring `pending_bulk_rename`)
- [ ] call `crate::backup::apply(&run)`; on `Ok(())` set status to `"backed up N files"` and refresh panels (do NOT clear stage)
- [ ] on `Err((path, e))` set status with the failing path and error, refresh panels (do NOT clear stage so partial-state is visible and re-runnable)
- [ ] write integration test using `tempfile`: prepare a `BackupRun` with 3 copies, stash to `pending_backup`, dispatch `Internal::backup_apply`, assert all 3 dsts created, all 3 srcs still exist, `pending_backup` is now `None`
- [ ] write test for partial-failure: 3-copy run where the second dst is in an unwritable parent (`#[cfg(unix)]` gated), assert first copy succeeded, second failed, third NOT attempted; status row carries the failing path; `pending_backup` is `None`
- [ ] write test that dispatching `:backup_apply` with no pending plan yields the stale-guard status error
- [ ] run `cargo test` — must pass before Task 9

### Task 9: Verify end-to-end + acceptance criteria

**Files:**
- (no code changes — verification only)

- [ ] run full test suite: `cargo test --all-features`
- [ ] run `cargo clippy --all-features -- -D warnings`
- [ ] verify that `alt-shift-b` with no selection / no stage produces a clean status error (not a panic)
- [ ] verify that the existing `:cp` / `:mv` overwrite-confirm flows still work unchanged (the backup feature must not have leaked into `resolve_overwrite_target`)
- [ ] verify that the existing `bulk_rename` flow still works (stage 2+ → F2 → editor → diff overlay)
- [ ] verify the bulk-stage deny-list still does NOT include `Internal::backup` — adding it would double-confirm

### Task 10: Documentation update

**Files:**
- Modify: `CLAUDE.md`
- Modify: `docs/plans/2026-05-13-backup-feature.md` (move to completed)

- [ ] in `CLAUDE.md`, add a new `## Backup feature` section after the "Bulk rename" sub-section, covering:
  - The three-internal split and the auto_exec asymmetry (why a single verb can't serve both trigger and receiver roles)
  - The numbered-fallback policy and the `MAX_BACKUP_BUMP` cap
  - The `{new_filename:backup-name}` placeholder and where it's evaluated
  - The `Conf::backup_suffix` plumbing checklist (field + merge line + AppContext default + validation)
  - The `pending_backup` Close-arm cleanup invariant
  - The "manual collision rejects, prefill collision bumps" overwrite asymmetry
- [ ] do NOT update README.md unless the user already documents user-facing verbs there (broot's README is sparse on verb docs — verify before adding)
- [ ] `mkdir -p docs/plans/completed` (the dir already exists, but be safe)
- [ ] move both the design and the plan into `docs/plans/completed/`:
  - `mv docs/plans/2026-05-13-backup-feature-design.md docs/plans/completed/`
  - `mv docs/plans/2026-05-13-backup-feature.md docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only.*

**Manual smoke testing** (broot has no automated UI suite; verify in a live terminal):
1. Single file with no existing backup: select a file, press `alt-shift-b`, observe prefilled `<name>.bak`, press Enter, verify file is copied (both original and `.bak` present in the tree).
2. Single file with existing `.bak`: pre-create `foo.bak`, select `foo`, press `alt-shift-b`, observe prefilled `foo.bak.1`, accept, verify `foo`, `foo.bak`, and `foo.bak.1` all exist.
3. Single file, edit name to a name that exists: pre-create `custom.txt`, select `foo`, press `alt-shift-b`, edit prefill to `custom.txt`, press Enter, observe status error and no fs change.
4. Directory backup: select a directory, press `alt-shift-b`, accept the prefilled name, verify recursive copy.
5. Bulk via stage: stage 3 files, press `alt-shift-b`, observe ConfirmOverlay listing all three `src → dst` rows, press Enter, verify all three `.bak`s created and originals still exist.
6. Bulk cancel: stage 3 files, press `alt-shift-b`, press Esc, verify no copies made and `pending_backup` cleared (a follow-up `:backup_apply` typed directly should report "no pending backup").
7. Custom suffix: set `backup_suffix: "~"` in `conf.hjson`, restart, verify the prefill is now `<name>~` instead of `<name>.bak`.
8. Invalid suffix: set `backup_suffix: ""` in `conf.hjson`, restart, verify `cli_log::warn!` fires and the actual suffix used is `.bak`.
9. Filesystem root: navigate to `/`, attempt backup, verify status error (no crash).

**External system updates:** none. broot has no consumer libraries or external integrations affected by this feature.
