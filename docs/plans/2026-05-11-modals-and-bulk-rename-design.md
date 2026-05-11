# Modal refinements + Add modal + Bulk rename — design

## Goal

Three independent features landing on top of the overlay routing system
(`Overlay` enum, `OverlayOutcome`, `CmdResult::OpenOverlay`) and the
existing bulk-staging confirm intercept:

1. Stop the bulk-staging confirm modal from firing on pure navigation
   internals (`line_up`/`line_down` etc.) in the stage panel.
2. Add a new `Internal::add` overlay that creates a file or directory
   (trailing slash → directory) relative to the current selection,
   bound to `alt-n`.
3. Make `:rename` context-aware: empty stage / one path keeps the
   existing inline behavior; stage with two or more paths opens an
   `$EDITOR`-backed bulk-rename flow with a diff confirm before apply.

The user-facing confirm overlay itself is intentionally **not** in
scope: y/n hotkeys and Esc-to-cancel already work today.

## Non-goals

- No change to `ConfirmOverlay` button labels, focus model, or hotkeys.
- No template names, no recently-used list, no permission setting on
  the Add modal.
- No regex/sed bulk-rename mode. The diff confirm modal IS the safety
  net — we are not building a multi-pass refactor tool.
- No new `Command` enum variants. Plan payload passes through an
  `App`-level field (mirrors `App::skip_confirm`).
- No undo. Apply is destructive once confirmed.
- No light-terminal palette work or new style keys.

## Section 1 — Stage-panel nav bypass

**Files**: `src/app/app.rs` only.

`is_stage_management_internal` (`src/app/app.rs:1083-1097`) names the
internals that should bypass `maybe_bulk_stage_confirm`. The bypass
is keyed off "operates on the stage itself, not on its contents"
(per the comment block). Six navigation internals are handled by
`StageState::on_internal` (`src/stage/stage_state.rs:428-451`) but
absent from the bypass list, so they currently trigger the bulk
confirm modal when stage has 2+ entries.

**Change.** Extend the `matches!` arm:

```rust
Internal::stage
| Internal::unstage
| Internal::toggle_stage
| Internal::clear_stage
| Internal::stage_all_directories
| Internal::stage_all_files
| Internal::open_staging_area
| Internal::close_staging_area
| Internal::toggle_staging_area
| Internal::focus_staging_area_no_open
// --- NEW: pure navigation in stage panel ---
| Internal::line_up
| Internal::line_down
| Internal::line_up_no_cycle
| Internal::line_down_no_cycle
| Internal::page_up
| Internal::page_down
| Internal::select_first
| Internal::select_last
```

The `select_first`/`select_last` pair is added even though
`stage_state.rs` doesn't currently dispatch them — they're
navigation-shaped and must never confirm if added later.

**Doc update.** Extend the `## Verb confirmation system` section of
`CLAUDE.md` (the existing `is_stage_management_internal` reference) to
list the full set, so future agents see the navigation internals
listed alongside the stage-management ones.

**Tests.** Add a parameterised unit test next to the existing pin at
`src/verb/verb_store.rs:756-781` (or co-located with the function in
`app.rs`), asserting that `maybe_bulk_stage_confirm` returns `None`
for each of the eight added internals when stage has 2+ entries.

## Section 2 — Add modal

**Files**: `src/app/overlay/add.rs` (new), `src/app/overlay/mod.rs`,
`src/verb/internal.rs`, `src/verb/verb_store.rs`,
`src/app/panel_state.rs`, `src/browser/browser_state.rs`.

### Overlay variant

Add a fourth variant to the `Overlay` enum
(`src/app/overlay/mod.rs:124`):

```rust
pub enum Overlay {
    Confirm(ConfirmOverlay),
    Goto(GotoOverlay),
    Add(AddOverlay),
    #[cfg(test)] Stub(StubOverlay),
}
```

Wire the three dispatch shims (`render`, `handle_key`, `handle_mouse`)
at lines 144 / 157 / 169. That's the entire routing change per the
single-field overlay invariant documented in CLAUDE.md.

### `AddOverlay` shape

```rust
pub struct AddOverlay {
    target_dir: PathBuf,
    input: String,
    cursor: usize,         // byte index into `input`
    error: Option<String>, // last validation/fs error, shown in hint row
    button_hits: Cell<Option<ButtonHits>>, // Create / Cancel
}
```

No embedded `termimad::InputField`: overlay `render` takes `&self` and
the field's mutable cursor state would force a `RefCell`. Hand-rolled
char accumulator is ~20 lines and matches `GotoOverlay`'s posture.

Constructor: `AddOverlay::new(target_dir: PathBuf)`. Caller resolves
the directory at open time (see "Target resolution" below).

### Keys

| Key | Action |
|---|---|
| Printable char | Insert at `cursor`. `/` allowed (trailing slash semantics). |
| Backspace | Delete char before `cursor`. |
| `←` / `→` / Home / End | Move `cursor`. |
| Tab | Toggle button focus Create ↔ Cancel (mouse-shy users). |
| Enter | Validate + commit (see below). |
| Esc / Ctrl-C | Cancel — `OverlayOutcome::Close`. |

Validation rejects: leading `/`, any `..` component, empty input.
Anything else goes to the filesystem.

### Layout

Centred, width ~60, height 7:

```
╭─ New file or directory ────────────────────────────╮
│ in: /home/me/projects/broot/src                    │
│ ▏ <input cursor>                                   │
│ (trailing / creates a directory)                   │
│                                                    │
│  [ Cancel ]                          [ Create ]    │
╰────────────────────────────────────────────────────╯
```

Hint row 3 normally shows "(trailing / creates a directory)". On
validation/filesystem error, replaced with the error message in
`palette.file_error`.

### Target resolution

New `Internal::add` arm in `BrowserState::on_internal`
(`src/browser/browser_state.rs`). Inspect the selected line:

- `selection.path.is_dir()` → `target_dir = selection.path.clone()`
- else → `target_dir = selection.path.parent().unwrap_or(root).to_path_buf()`

Other panel types (`PreviewState`, `StageState`, `FsState`, `HelpState`,
`TrashState`) leave the default `PanelState` impl in place, which
returns `CmdResult::Keep` — `:add` is a no-op outside the browser.

Build `CmdResult::OpenOverlay(Box::new(Overlay::Add(AddOverlay::new(target_dir))))`.
The existing `App::OpenOverlay` handler at `src/app/app.rs:467` already
sets `self.overlay = Some(*overlay)` — no app-level change needed.

### Commit path

On `Enter`:

1. Compute `full = target_dir.join(&input)`.
2. If `input.ends_with('/')` → `fs::create_dir_all(&full)`.
3. Else → `fs::create_dir_all(full.parent().unwrap_or(&target_dir))`
   then `fs::File::create(&full)`.
4. On `Err`, set `self.error = Some(...)`, return `OverlayOutcome::Stay`
   — don't close on filesystem failure, let the user retry.
5. On `Ok`, return `OverlayOutcome::CloseAndFocus(full)`. The existing
   `CloseAndFocus` plumbing synthesises a `:focus <path>` so broot
   navigates onto the new entry.

### Verb registration

- `src/verb/internal.rs`: add `add` entry in the `Internals!` macro
  block. Description: `"create file or directory"`.
- `src/verb/verb_store.rs`: `self.add_internal(add).with_key(key!(alt - n));`

No shortcut bound — `:add` from the command bar already works via the
internal name.

### Tests

- Unit on `AddOverlay::handle_key`: printable chars accumulate,
  backspace deletes at cursor, cursor movement, Enter on empty input
  stays open with error.
- Validation: rejects `..` in input, rejects leading `/`.
- Routing pin: `Internal::add` → `CmdResult::OpenOverlay` from
  `BrowserState`; returns `Keep` from `PreviewState`, `StageState` etc.
- Smoke test against `tempfile::tempdir()`: exercise both branches
  (file vs trailing-slash directory), assert filesystem state.

## Section 3 — Context-aware `:rename` with bulk flow

**Files**: `src/app/editor.rs` (new), `src/verb/internal.rs`,
`src/verb/verb_store.rs`, `src/app/app.rs`, `src/app/panel_state.rs`,
`src/bulk_rename/mod.rs` (new).

### Trigger rule

| Stage size | `:rename` / F2 behavior |
|---|---|
| `0` | Inline single rename of cursor selection. Unchanged from today's external `rename {new_filename}` verb. |
| `1` | Inline single rename of the single staged path. (Treat it like the cursor selection.) |
| `≥ 2` | Bulk flow: $EDITOR → diff confirm → apply. |

The existing external `rename` verb registration at
`src/verb/verb_store.rs:241-256` stays — it provides the inline path.
A new `Internal::bulk_rename` is added and bound to `F2` at higher
priority. When invoked with stage size < 2, it falls through to the
inline verb (re-emit the `:rename` command, or return `Keep` and let
the user reach it via the verb directly — pick during impl based on
which is less surprising in the help screen).

### `$EDITOR` helper

New `src/app/editor.rs`:

```rust
pub fn edit_in_external(content: &str, suffix: &str) -> io::Result<String>;
```

Behavior:

1. Resolve editor: `$VISUAL` → `$EDITOR` → `Err("set $EDITOR to enable
   bulk rename")`. The status row surfaces this verbatim.
2. Write `content` to a `tempfile::NamedTempFile::new()` with the
   given suffix (e.g. `.broot-rename`). `tempfile` is already a
   dependency (`Cargo.toml:64`).
3. Toggle out of raw mode + leave alternate screen, mirroring
   `src/launchable.rs:170-205`.
4. `std::process::Command::new(editor).arg(temp.path()).status()?`.
5. Re-enter raw mode + alternate screen.
6. Read back the temp file. `tempfile` auto-cleans on drop.

This helper is intentionally generic. Future `$EDITOR` integrations
(edit conf, edit a script verb) can reuse it.

### Bulk-rename module

New `src/bulk_rename/mod.rs` with three pure functions, all
unit-testable without touching the filesystem or $EDITOR:

```rust
pub fn serialize(stage: &[PathBuf]) -> String;
pub fn parse(edited: &str) -> Vec<String>;  // trimmed, blanks/comments skipped

pub struct RenameRun { pub renames: Vec<(PathBuf, PathBuf)> }

pub enum BulkRenameError {
    LineCountMismatch { expected: usize, got: usize },
    EmptyTarget { line: usize },
    DuplicateTarget { name: String },
    ExternalCollision { target: PathBuf },
}

pub fn plan(
    stage: &[PathBuf],
    edited_lines: &[String],
    existing_paths: &dyn Fn(&Path) -> bool,
) -> Result<RenameRun, BulkRenameError>;
```

`plan` runs validation rules in order:
1. Line count must equal stage length.
2. No empty target names.
3. No two targets collide with each other.
4. No target points to an existing file *outside the rename set*
   (`existing_paths(target) && !stage.iter().any(|p| p == target)`).

Cycles are accepted, resolved at apply time. Unchanged pairs (same
old/new) are filtered out of `renames` so the confirm modal only
shows real changes.

### Confirm-with-diff modal

Reuse `ConfirmOverlay`:
- Title: `"Rename N files?"` where N = number of changed pairs.
- Body: `"old → new"` one per changed pair. Long paths are
  ellipsis-truncated by the existing overflow handling.
- Confirm label: `"Rename"`. `danger: false`.

### Carrying the rename plan

`Command` payloads are string-based and we don't want to invent a new
variant for one feature. Plan storage:

```rust
// in src/app/app.rs alongside skip_confirm
pending_bulk_rename: Option<RenameRun>,
```

Set when opening the confirm overlay (the bulk_rename internal handler
populates it, then returns `OpenOverlay`). Consumed when the
`Internal::bulk_rename_apply` arm runs through `apply_command` —
`OverlayOutcome::CloseAndRun(Command::from_raw(":bulk_rename_apply"))`
re-enters the loop, the apply arm `mem::take`s the field, executes,
and `skip_confirm` already handles the re-entry to bypass the verb
checks (see `app.rs:171`).

Single bookkeeping field, no `Command` shape change. Mirrors the
`skip_confirm` pattern documented in `CLAUDE.md`.

### Apply

For each `(from, to)`:
- If `to` exists and `to` is among the `from`s of *remaining* renames
  (i.e. cycle detected), two-phase: `fs::rename(from, .broot-bulk-tmp-{n})`
  first, append a deferred entry to a `final_phase` Vec.
- Else `fs::rename(from, to)`.
- On error, stop. Report the failed pair in the status row. Entries
  before the failure stay applied; the user can re-stage and retry.
  Document this in the verb help text.

After successful apply:
- Clear the stage (renamed paths are stale anyway).
- Trigger a tree refresh on the active panel.

### Verb registration

- `src/verb/internal.rs`: add `bulk_rename` and `bulk_rename_apply`.
  `bulk_rename_apply` is `auto_exec: true` and has no public help —
  it's an internal continuation.
- `src/verb/verb_store.rs`: `self.add_internal(bulk_rename).with_key(key!(F2));`
  (higher priority than the existing external rename verb at the same
  key). The external rename verb's key binding stays as a fallback;
  the verb store resolves the internal first.

### Tests

- Unit on `serialize`: round-trip stable on a `Vec<PathBuf>`.
- Unit on `parse`: blanks and `#`-prefixed lines skipped, trailing
  whitespace trimmed.
- Unit on `plan`: every error variant fires on a targeted input.
- Unit on `plan` cycle detection: `a → b, b → a` yields a `RenameRun`
  whose apply order works with two-phase.
- Integration with `tempfile::tempdir()`: stage 3 real files, run
  apply against a synthesised `RenameRun`, assert filesystem state.
- Integration with a cycle (a/b swap), assert both files end up
  swapped.
- Skip the actual `$EDITOR` invocation in CI — the helper is
  exercised manually or, if time allows, with `EDITOR=/bin/true`
  (no-op editor) just to pin the raw-mode toggle path.

## Build order

1. **Section 1** — stage nav bypass. One-line fix + tests + CLAUDE.md
   doc update. Lands independently.
2. **Section 2** — Add modal. New file `src/app/overlay/add.rs`, new
   internal, new keybind. Lands independently of Section 3.
3. **Section 3** — Bulk rename, in two sub-steps:
   - 3a: `src/app/editor.rs` helper + `src/bulk_rename/mod.rs` pure
     functions with their unit tests. No app-level wiring yet.
   - 3b: `Internal::bulk_rename`/`bulk_rename_apply`, the F2 routing,
     `pending_bulk_rename` field, integration with `ConfirmOverlay`.

Sections 1 and 2 ship as small PRs. Section 3 lands as one PR with
both sub-steps; 3a alone has no user-facing surface.

## Out of scope (reaffirmed)

- ConfirmOverlay button-label rework. Y/N hotkeys already work; the
  current "Cancel / {verb}" labels stay.
- Light-terminal palette work.
- Undo for bulk rename. Apply is destructive once confirmed.
- Regex/sed/template bulk modes.
- `Internal::add` permission setting or file-template support.
