# Bulk unification — design

Three related changes that collapse special-cased single-file flows
into the existing bulk paths, and turn the stage keys into
"add + advance" fast-stagers.

## Goals

1. F2 (rename) always goes through the bulk-rename flow. Single file
   is N=1: same `$EDITOR` + `ConfirmOverlay` UX as bulk, just with one
   line in the editor and one row in the diff.
2. alt-shift-b (backup) always goes through the bulk-backup flow.
   Single file is N=1: same auto-computed-next-free-name +
   `ConfirmOverlay` UX as bulk, just with one row in the diff. No
   editor step (matches existing bulk-backup).
3. `=` / `+` / `ctrl-g` become **add + advance**. Press the key →
   path joins the stage (no toggle, no remove) → selection moves to
   the next entry. Non-cycling: at the last entry, advance is a
   no-op. `-` keeps remove-only-no-advance semantics.

## Non-goals

- The external `:rename newname` verb stays callable from the command
  bar for users who want inline typing. Only the F2 key path changes.
- No new config keys for rename or backup.
- No "advance" added to `unstage`.
- No cycle behaviour for the stage advance.

## Section 1 — Rename unification

`run_bulk_rename` (`src/app/app.rs` around `:792-849`) currently has a
`stage.len() < 2` branch that synthesizes a `Command::VerbTrigger`
targeting the external `:rename` verb. **Delete that branch.** Replace
with a unified path-collection step:

```rust
let paths: Vec<PathBuf> = if app_state.stage.is_empty() {
    let sel = self.active_panel_state().selection().path.to_path_buf();
    vec![sel]
} else {
    app_state.stage.paths().to_vec()
};
```

Then the existing bulk flow runs unconditionally:

1. `bulk_rename::serialize(&paths)` → editor buffer
2. `editor::edit_in_external(...)` → suspend TUI, run `$EDITOR`
3. `bulk_rename::parse + plan` → `RenameRun`
4. `self.pending_bulk_rename = Some(run)`
5. `self.request_confirm(...)` → `ConfirmOverlay` with
   `Command::from_raw(":bulk_rename_apply", true)` as the pending
   command
6. Re-dispatch via `OverlayOutcome::CloseAndRun` → `run_bulk_rename_apply`
   takes the pending run, calls `bulk_rename::apply`

**Deleted symbols**:
- `find_external_rename_verb_id` (no remaining callers after the
  stage-<2 branch is gone)
- The stage-<2 if-branch in `run_bulk_rename`

**Edge cases**:
- Stage empty AND root is selected (`tree.selection == 0`): the bulk
  `plan` step's existing root-handling fires (returns an error which
  surfaces as a status message). No new code.
- Non-UTF-8 paths: bulk-rename's existing `$EDITOR` round-trip
  handling stands. No new code.

## Section 2 — Backup unification

Same shape as rename, simpler because there's no editor step.

`run_backup` (`src/app/app.rs:934-986`) currently has a `stage.len() < 2`
branch that calls `prefill_backup_one` to write a prefilled invocation
into the input bar. **Delete that branch.** Replace with:

```rust
let paths: Vec<PathBuf> = if app_state.stage.is_empty() {
    let sel = self.active_panel_state().selection().path.to_path_buf();
    vec![sel]
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

The cap-exhaust early-return uses the existing `has_cap_exhaust` /
`cap_exhaust_message` helpers, so the user sees the same "too many
existing backups for X" status row whether they triggered on 1 path or
50.

**Deleted symbols** (substantial cleanup of recently-merged code):

| Symbol | Location |
|---|---|
| `Internal::backup_one` enum variant | `src/verb/internal.rs:57` |
| `backup_one` invocation pattern registration | `src/verb/internal.rs:203` |
| `backup_one` `add_internal_no_doc(...)` | `src/verb/verb_store.rs:288-293` (one of three) |
| Intercept arm for `Internal::backup_one` in `apply_command` | `src/app/app.rs:265-275` |
| `prefill_backup_one` method | `src/app/app.rs:1001-1043` |
| `run_backup_one` method | `src/app/app.rs` (the receiver) |
| `plan_single_backup` free fn | `src/app/app.rs:1715-1760` |
| `resolve_backup_one_invocation_parser` | `src/app/app.rs:1677-1690` |
| `take_pending_backup_or_error` is kept (used by `run_backup_apply`) | unchanged |
| `VerbArgFlag::BackupName` variant | `src/verb/verb_arg_def.rs:32-44` (incl. doc comment) |
| `BackupName` arm in `FromStr` and `Display` | `src/verb/verb_arg_def.rs:140, 159` |
| `"backup-name"` match arm in `get_sel_name_standard_replacement` | `src/verb/execution_builder.rs:230-260` |
| `backup_internals_registered` pin test — **update** to assert only `backup` and `backup_apply` exist; `backup_one` must NOT be present | `src/verb/verb_store.rs:1319-1383` |
| `backup_keybind_resolves_to_trigger` — keep as-is | `src/verb/verb_store.rs:1391-1403` |
| CLAUDE.md "### Backup" subsection — rewrite to describe the
  two-internal architecture (trigger + apply) | `CLAUDE.md` |

**Kept**:
- `Internal::backup` (trigger, `auto_exec:true`, bound to alt-shift-b)
- `Internal::backup_apply` (re-dispatched from confirm CloseAndRun)
- `BackupRun`, `BackupCopy`, `MAX_BACKUP_BUMP`, `next_free_backup_name`,
  `plan_bulk_backup`, `apply` in `src/backup/mod.rs`
- `clear_pending_runs_slots` and `App::pending_backup`
- `Conf::backup_suffix`, `AppContext::backup_suffix`,
  `is_valid_backup_suffix`
- `cap_exhaust_message`

**UX implication**: users can no longer type a custom backup name.
The auto-computed `.bak` / `.bak.N` is the only option. This is the
user-stated direction.

## Section 3 — Stage + advance

`BrowserState::on_internal` (`src/browser/browser_state.rs`) currently
has separate arms for `Internal::stage`, `Internal::unstage`,
`Internal::toggle_stage`. They each call into the corresponding
`PanelState::{stage,unstage,toggle_stage}` trait method
(`src/app/panel_state.rs:951-1010`) and return `CmdResult::Keep`.

**Changes**:

1. **`Internal::stage` arm** (in `BrowserState::on_internal`): after
   the existing `self.stage(app_state, cc, con)` call, append a
   `self.displayed_tree_mut().move_selection(1, page_height, false)`.
2. **`Internal::toggle_stage` arm**: re-point to add-only semantics
   AND advance. The simplest implementation is to call
   `self.stage(...)` instead of `self.toggle_stage(...)`, then
   advance. This makes `=` identical to `+` at the BrowserState
   layer.
3. **Key bindings** in `src/verb/verb_store.rs:357-363`:
    - `+` → `stage`. Unchanged.
    - `=` → `toggle_stage`. **Re-point to `stage`.**
    - `ctrl-g` → `toggle_stage`. **Re-point to `stage`.**
4. **`Internal::toggle_stage`** itself: keep the internal in the enum
   and trait method. It remains callable as `:toggle_stage` for
   users who scripted around it via conf. Only the default key
   bindings move.
5. **`Internal::unstage` arm**: unchanged. No advance.

**Why edit the `BrowserState::on_internal` arms rather than the
trait method?** `page_height` is computed in `BrowserState::on_internal`
from `cc.panel.area.height`; the trait `stage` method takes
`app_state, cc, con` and has no direct access to the tree.
Composing at the on_internal layer keeps the trait method's
single-responsibility ("touch the stage set") intact and avoids a
new `CmdResult` variant.

**Other panels**: `StageState::on_internal` (and any other panel that
overrides `stage`/`toggle_stage`) does NOT gain the advance — the
behaviour is BrowserState-specific because only the tree has a
selection cursor to advance.

**Edge cases**:
- Selection already on the last entry → `move_selection(1, _, false)`
  is a no-op (non-cycling). The stage operation still happens.
- Pruning lines / non-selectable rows → `move_selection` already
  skips them.
- Stage panel is the active panel → BrowserState's
  `on_internal` isn't reached; existing trait default handles it
  without advance.

## Test plan

Cargo:
- `cargo build --release --all-features` clean
- `cargo test --all-features` — current 574 passing should stay
  passing (any backup_one-specific tests will be deleted alongside
  the symbols they exercise). New tests target the new behaviour.

New tests:

- `bulk_rename_with_empty_stage_uses_selection` — assert that
  F2 with `app_state.stage.is_empty()` packs the current selection
  into the rename run.
- `bulk_backup_with_empty_stage_uses_selection` — same for
  alt-shift-b.
- `stage_advances_selection` — `BrowserState::on_internal`
  with `Internal::stage`: assert `tree.selection` advances by 1.
- `stage_at_last_entry_no_panic` — stage when at last entry: assert
  selection stays and no panic.
- `toggle_stage_key_now_adds_only` — pin test: pressing `=` on an
  already-staged path no longer removes it.

Updates to existing tests:
- `backup_internals_registered` — assert `backup_one` is NOT
  registered (negative pin); assert `backup` and `backup_apply`
  remain.
- Any tests touching `plan_single_backup`,
  `resolve_backup_one_invocation_parser`, or `prefill_backup_one`
  are deleted along with the symbols.

Visual smoke (manual):
- Browse, press F2 on a single file → `$EDITOR` opens with one line,
  edit, save+quit → ConfirmOverlay shows `src → dst`, Enter commits.
- Stage 3 files, press F2 → same flow with 3 lines.
- Press alt-shift-b on a single file → no editor; ConfirmOverlay
  shows `src → src.bak`, Enter commits.
- Browse, press `+` → entry stages, cursor moves down one row.
- Press `=` on already-staged entry → entry stays staged (no toggle),
  cursor advances.

## Out of scope / follow-up

- A "rename without `$EDITOR`" quick path (currently still possible
  by typing `:rename newname` in the command bar).
- A `:backup newname` typed-verb variant for custom backup names.
- Cycle-around-bottom for the stage advance (could be a config key).
