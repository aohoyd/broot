# Bulk unification

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Collapse single-file rename/backup flows into the existing bulk flows, and turn the `=` / `+` / `ctrl-g` stage keys into add-and-advance fast-stagers.

**Architecture:** Three independent changes. (1) `run_bulk_rename` and `run_backup` no longer branch on stage size — both unconditionally use the bulk path with `paths = stage || [selection]`. (2) The `Internal::backup_one` receiver and its placeholder plumbing (`VerbArgFlag::BackupName`, `prefill_backup_one`, `plan_single_backup`, etc.) are deleted. (3) `BrowserState::on_internal` arms for `Internal::stage` and `Internal::toggle_stage` advance the selection after staging; `=` and `ctrl-g` rebind from `toggle_stage` to `stage`.

**Tech Stack:** Rust, broot's existing `App` / `BrowserState` / `Overlay::Confirm` patterns, ConfirmOverlay with `CloseAndRun` re-dispatch, `Tree::move_selection`.

## Overview

- F2 always opens `$EDITOR` + `ConfirmOverlay` for rename (same UX whether stage is 0, 1, or many).
- alt-shift-b always shows `ConfirmOverlay` with auto-computed `.bak` / `.bak.N` destinations (no editor).
- `=`, `+`, `ctrl-g` all become "add + advance" — pure fast-stage keys. `-` keeps remove-only-no-advance.

Problem solved: the current single-file flows depend on a fragile input-bar-prefill mechanism (`Command::VerbTrigger` synthesis + `auto_exec:false` + `panel_input.rs:538` prefill), which the user reports is broken for rename and which required a custom workaround for backup (`prefill_backup_one`). Unifying single-file with bulk eliminates two parallel code paths.

## Context (from discovery)

- Design doc: `docs/plans/2026-05-14-bulk-unification-design.md`
- Key files:
  - `src/app/app.rs` — `run_bulk_rename`, `run_backup`, intercept arms, pending payloads
  - `src/browser/browser_state.rs` — `on_internal` stage/unstage/toggle_stage arms
  - `src/verb/verb_store.rs` — key bindings and verb registration
  - `src/verb/internal.rs` — `Internal` enum, invocation patterns
  - `src/verb/verb_arg_def.rs` — `VerbArgFlag` enum
  - `src/verb/execution_builder.rs` — `get_sel_name_standard_replacement`
  - `CLAUDE.md` — Backup subsection rewrite
- Related patterns:
  - `OverlayOutcome::CloseAndRun` re-dispatch (overlay routing in `src/app/app.rs`)
  - `pending_bulk_rename` and `pending_backup` payload pattern
  - `Tree::move_selection(dy, page_height, cycle)` from `src/tree/tree.rs`

## Development Approach

- **Testing approach**: Regular (code first, then tests)
- Complete each task fully before moving to the next
- Make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- Run `cargo test --all-features` after each task
- Maintain backward compatibility for typed verbs (`:rename newname` still works from command bar)

## Testing Strategy

- **Unit tests**: required for every task
- No e2e suite in this project — visual smoke is documented in Post-Completion for manual verification
- Tests live alongside the code in `#[cfg(test)] mod tests { ... }` blocks (existing convention)

## Progress Tracking

- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix
- Update plan if implementation deviates from original scope

## Solution Overview

Three localized refactors. None touch the bulk machinery itself (`bulk_rename::serialize/parse/plan/apply`, `backup::plan_bulk_backup/apply`, `ConfirmOverlay`); they only change how those entry points are reached. Code volume is net-negative — Section 2 deletes substantially more than Sections 1 and 3 add.

Key design decisions:
- Path collection uses `stage || [selection]` — empty stage falls back to current selection, treating single-file as N=1 bulk.
- The external `:rename` verb stays callable via typed invocation but is no longer reached via F2.
- `Internal::toggle_stage` stays in the enum and trait for users with custom conf bindings; only the default key bindings move.
- Stage advance is non-cycling at the last entry (predictable "spam `+` until done" behaviour).
- `Internal::unstage` does NOT advance (you may want to inspect what you just removed).

## Technical Details

**Data flow — F2 (post-change)**:
```
F2 key → Internal::bulk_rename (auto_exec:true, App intercept)
       → run_bulk_rename:
           paths = stage.is_empty() ? [selection] : stage.paths()
           edit_in_external(serialize(paths))
           run = plan(parse(edited))
           pending_bulk_rename = Some(run)
           request_confirm(":bulk_rename_apply")
       → user confirms → CloseAndRun → run_bulk_rename_apply
           take pending → bulk_rename::apply
```

**Data flow — alt-shift-b (post-change)**:
```
alt-shift-b → Internal::backup (auto_exec:true, App intercept)
            → run_backup:
                paths = stage.is_empty() ? [selection] : stage.paths()
                run = plan_bulk_backup(paths, suffix)
                if run.has_cap_exhaust(): return cap_exhaust_message(run)
                pending_backup = Some(run)
                request_confirm(":backup_apply")
            → user confirms → CloseAndRun → run_backup_apply
                take pending → backup::apply
```

**Data flow — `+` / `=` / `ctrl-g` (post-change)**:
```
key → Internal::stage (in BrowserState::on_internal)
    → self.stage(app_state, cc, con)
    → self.displayed_tree_mut().move_selection(1, page_height, false)
    → CmdResult::Keep
```

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): code changes, test additions, doc updates achievable in this repo
- **Post-Completion** (no checkboxes): manual visual smoke testing

## Implementation Steps

### Task 1: Unify rename flow to always-bulk

**Files:**
- Modify: `src/app/app.rs`

- [ ] In `run_bulk_rename` (around `src/app/app.rs:792-849`), delete the `stage.len() < 2` branch that synthesizes `Command::VerbTrigger { external_rename, None }` and re-dispatches into `apply_command`.
- [ ] Replace with unified path collection:
  ```rust
  let paths: Vec<PathBuf> = if app_state.stage.is_empty() {
      vec![self.active_panel_state().selection().path.to_path_buf()]
  } else {
      app_state.stage.paths().to_vec()
  };
  ```
  (Confirm the exact accessor for the active selection during implementation — `self.active_panel_state().selection()` vs an explicit `panel.state().selection()` call. Walk the existing call sites in `app.rs` to match conventions.)
- [ ] Delete the helper `find_external_rename_verb_id` (no remaining callers after the branch is gone). Verify with `rg find_external_rename_verb_id`.
- [ ] Write tests in `src/app/app.rs`'s existing test module (or a sibling `rename_routing_tests` module):
  - `bulk_rename_empty_stage_uses_selection` — construct a minimal scenario, assert that calling `run_bulk_rename` with `stage.is_empty()` invokes `bulk_rename::serialize` with `[selection.path]`. If `run_bulk_rename` is too coupled to mock, extract the path-collection step into a free function `collect_rename_paths(app_state, selection) -> Vec<PathBuf>` and unit-test that.
  - `bulk_rename_with_stage_uses_stage_paths` — same scenario but stage has 2 paths; assert those are used and selection is ignored.
- [ ] Run `cargo test --all-features` — all passing before next task. If `find_external_rename_verb_id` had a dedicated unit test, delete it.

### Task 2: Unify backup flow + remove backup_one plumbing

**Files:**
- Modify: `src/app/app.rs`
- Modify: `src/verb/internal.rs`
- Modify: `src/verb/verb_store.rs`
- Modify: `src/verb/verb_arg_def.rs`
- Modify: `src/verb/execution_builder.rs`
- Modify: `CLAUDE.md`

- [ ] In `run_backup` (around `src/app/app.rs:934-986`), delete the `stage.len() < 2` branch that calls `prefill_backup_one`. Replace with unified path collection (same shape as Task 1):
  ```rust
  let paths: Vec<PathBuf> = if app_state.stage.is_empty() {
      vec![self.active_panel_state().selection().path.to_path_buf()]
  } else {
      app_state.stage.paths().to_vec()
  };
  let run = plan_bulk_backup(&paths, &con.backup_suffix);
  if run.has_cap_exhaust() {
      return cap_exhaust_message(&run);
  }
  self.pending_backup = Some(run);
  self.request_confirm(..., Command::from_raw(":backup_apply", true));
  ```
  (Confirm the exact `has_cap_exhaust` predicate during implementation — it may be a method or an inspection of `run.copies` for `src == dst` sentinels. See `src/backup/mod.rs:115-176` and existing usage in `app.rs:960-973`.)
- [ ] Delete `Internal::backup_one` variant from `src/verb/internal.rs:57` (and any associated invocation pattern at `:203`).
- [ ] Delete the `add_internal_no_doc` registration of `backup_one` in `src/verb/verb_store.rs:288-293`.
- [ ] Delete the App-level intercept arm for `Internal::backup_one` in `src/app/app.rs` (the arm that routes to `run_backup_one`).
- [ ] Delete the methods/functions: `prefill_backup_one`, `run_backup_one`, `plan_single_backup`, `resolve_backup_one_invocation_parser`. Use `rg "fn prefill_backup_one|fn run_backup_one|fn plan_single_backup|fn resolve_backup_one_invocation_parser"` to confirm scope before deleting.
- [ ] Delete `VerbArgFlag::BackupName` variant and its doc comment in `src/verb/verb_arg_def.rs:32-44`. Remove the corresponding arms in `FromStr` (`:140`) and `Display` (`:159`).
- [ ] Delete the `"backup-name"` match arm in `get_sel_name_standard_replacement` (`src/verb/execution_builder.rs:230-260`).
- [ ] Update the pin test `backup_internals_registered` (`src/verb/verb_store.rs:1319-1383`) to assert that `Internal::backup` and `Internal::backup_apply` are registered AND that `Internal::backup_one` is NOT (negative assertion). Keep `backup_keybind_resolves_to_trigger` (`:1391-1403`) as-is.
- [ ] Rewrite the "### Backup" subsection in `CLAUDE.md` (currently under "## Overlay routing") to describe the two-internal architecture: `Internal::backup` (trigger, `auto_exec:true`, alt-shift-b) and `Internal::backup_apply` (re-dispatched from confirm). Remove all references to `backup_one`, `prefill_backup_one`, `plan_single_backup`, `VerbArgFlag::BackupName`, `resolve_backup_one_invocation_parser`, and the `{new_filename:backup-name}` placeholder. The "Suffix config plumbing checklist" and the `is_stage_consuming_internal` exclusion paragraph stay (still apply to the remaining two internals). Update line references inline.
- [ ] Write new tests in `src/app/app.rs`'s `backup_routing_tests` module:
  - `bulk_backup_empty_stage_uses_selection` — same shape as the rename test in Task 1.
  - `bulk_backup_with_stage_uses_stage_paths`
  - `cap_exhaust_short_circuits_overlay` — pin existing behaviour: if the planned run includes a cap-sentinel row, no overlay is opened and a status message is returned.
- [ ] Run `cargo test --all-features` — all passing. Existing tests for `plan_single_backup`, `resolve_backup_one_invocation_parser`, `prefill_backup_one`, and the `{new_filename:backup-name}` placeholder must be deleted alongside the symbols they exercised (search with `rg backup_one|plan_single_backup|prefill_backup_one|BackupName` to find them).

### Task 3: Stage + advance + rebind keys

**Files:**
- Modify: `src/browser/browser_state.rs`
- Modify: `src/verb/verb_store.rs`

- [ ] In `src/browser/browser_state.rs::on_internal`, locate the arms for `Internal::stage` and `Internal::toggle_stage` (around the staging-related dispatch — search for `Internal::stage` and `Internal::toggle_stage` in this file). Modify the `Internal::stage` arm: after the existing `self.stage(app_state, cc, con)` call, append `self.displayed_tree_mut().move_selection(1, page_height, false);` and return the existing result.
- [ ] Modify the `Internal::toggle_stage` arm in the same file: replace the `self.toggle_stage(...)` call with `self.stage(...)`, then append the same `move_selection` call. This makes `=` semantically identical to `+` at the BrowserState layer (no toggle behaviour for the default key binding).
- [ ] Leave `Internal::unstage` arm unchanged (no advance).
- [ ] In `src/verb/verb_store.rs:357-363`, change the `=` binding from `toggle_stage` to `stage` (the existing `.with_key(key!('='))` line currently sits on a `toggle_stage` registration; move it onto the `stage` registration). Same for `ctrl-g`: move `.with_key(key!(ctrl-g))` from `toggle_stage` to `stage`. The `+` binding on `stage` stays.
- [ ] `Internal::toggle_stage` itself remains registered (still callable as `:toggle_stage` via typed verb) — it just no longer has a default key binding.
- [ ] Write tests in `src/browser/browser_state.rs`'s existing test module (or a sibling module if `on_internal` isn't directly testable):
  - `stage_advances_selection` — set up a small tree with N>1 lines, call the `Internal::stage` arm (or simulate via a helper), assert `tree.selection` incremented by 1.
  - `stage_at_last_entry_no_panic` — set selection to last entry, stage, assert selection stays at last entry and no panic.
  - `toggle_stage_now_acts_like_stage` — set selection on an already-staged entry, call the `Internal::toggle_stage` arm, assert the entry is STILL staged (not removed) AND selection advanced.
  - Look at existing tests in `browser_state.rs` for the construction pattern; if they construct a full `BrowserState` for testing, follow that. If they don't and `on_internal` requires too much scaffolding, extract the advance step into a small helper (`stage_with_advance(&mut tree, page_height)`) and test that directly.
- [ ] Add a pin test in `src/verb/verb_store.rs`'s test module:
  - `stage_keys_bound_to_stage_not_toggle` — assert that the verbs reachable from `=`, `+`, and `ctrl-g` all have `get_internal() == Some(Internal::stage)`.
- [ ] Run `cargo test --all-features` — all passing.

### Task 4: Verify acceptance criteria

- [ ] Verify Section 1 (rename) requirements: F2 with empty stage goes through editor + ConfirmOverlay (manually trace via test or read code post-change).
- [ ] Verify Section 2 (backup) requirements: alt-shift-b with empty stage goes through ConfirmOverlay only (no editor).
- [ ] Verify Section 3 (staging) requirements: `+`, `=`, `ctrl-g` all stage AND advance; `-` only removes (no advance).
- [ ] Verify all `backup_one`-related symbols are gone: `rg "backup_one|prefill_backup_one|plan_single_backup|resolve_backup_one_invocation_parser|BackupName"` should return zero matches in `src/` (CHANGELOG mentions are fine, test names of negative assertions are fine).
- [ ] Verify `find_external_rename_verb_id` is gone: `rg find_external_rename_verb_id` should return zero matches.
- [ ] Run full test suite: `cargo test --all-features`
- [ ] Run release build: `cargo build --release --all-features` — no warnings
- [ ] Run `cargo clippy --all-features --all-targets` — no new warnings

### Task 5: Update documentation

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `CLAUDE.md` (if not already done in Task 2)
- (Move plan: `docs/plans/2026-05-14-bulk-unification.md` → `docs/plans/completed/`)

- [ ] Add CHANGELOG entry under `### next` summarizing the three changes user-visibly: "rename and backup now always use the confirm-overlay flow; `=` / `+` / `ctrl-g` now stage + advance instead of toggle."
- [ ] Confirm CLAUDE.md "### Backup" subsection was rewritten in Task 2; if not, do it now. Also update the line references in CLAUDE.md sections affected by the deletions (the deleted symbols will have shifted line counts in `app.rs` and `verb_arg_def.rs`).
- [ ] Check if README.md needs an update — the existing "alt-shift-b" row likely doesn't change (key still maps to backup, just different flow), but verify with a grep for `rename` / `backup` / `toggle_stage` / `=` and update if any reference describes the OLD behaviour.
- [ ] Move plan: `mkdir -p docs/plans/completed && mv docs/plans/2026-05-14-bulk-unification.md docs/plans/completed/` (do this LAST after everything else verifies).

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual visual smoke testing**:
- Browse to a populated directory, press F2 on a single file → `$EDITOR` should open with one line containing the filename. Edit it, save and quit. ConfirmOverlay should show `src → dst`, Enter should commit, status should show success.
- Stage 3 files (with `+`), press F2 → same flow with 3 lines in `$EDITOR`, 3 rows in ConfirmOverlay.
- Press alt-shift-b on a single file → no editor; ConfirmOverlay should show `src → src.bak` (or `src.bak.1` if `src.bak` exists), Enter commits.
- Stage 2 files, press alt-shift-b → ConfirmOverlay with 2 rows.
- Browse, press `+` on an entry → entry should stage and cursor should move down one row.
- Press `=` on an already-staged entry → entry should remain staged (not toggle off), cursor should advance.
- Press `ctrl-g` → same as `+` (stage + advance).
- Press `-` on a staged entry → entry unstages, cursor does NOT move.
- Press `+` on the last entry of the tree → entry stages, cursor stays at last entry (no cycle, no panic).
- Trigger cap exhaust: create `name.bak`, `name.bak.1`, ..., `name.bak.999` for a single source path, press alt-shift-b → status row shows cap-exhaust message, no overlay opens.

**No external system updates needed** — pure local refactor with no API surface change.
