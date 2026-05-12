# Vim-like keybindings — design

Date: 2026-05-12
Status: design approved, ready for planning

## Goal

Extend broot's existing Command mode (the `modal: true` config flag) with
a richer vim-like binding set so users who opt in get single-key access
to rename, delete, sort, navigation, panel toggles, and clipboard
operations. Default behavior for users with `modal: false` (the current
default) is preserved for every bare-letter binding.

## Scope decisions (locked in during brainstorm)

1. **Trigger surface.** All new bare-letter bindings fire only in
   Command mode. The default `modal: false` stays default. Alt-modifier
   bindings work in both modes (modifiers already bypass
   `is_key_only_modal` in `src/command/panel_input.rs:414`).

2. **No chord support.** `gg` is substituted by `g` (single key) for
   `select_first`. `G` is `select_last`. No new pending-key
   infrastructure.

3. **`d` policy.** `d` → `:trash` (recoverable, fires confirm),
   `D` → `:rm` (permanent, fires confirm via existing
   `requires_confirm`). Both stay in the destructive-confirm intercept
   path at `src/app/app.rs:185-205`.

4. **Sort.** `o` opens a new `SortOverlay` (single-key picks). Does
   not cycle; does not require arrow keys.

5. **New verbs.** Three new internals are required: `copy_name`,
   `copy_file_content`, `open_sort_overlay`. Everything else maps to
   existing internals.

6. **Conflict policy.** `find_key_verb` is already first-match-wins and
   user-defined verbs register before builtins
   (`src/verb/verb_store.rs:46`, before `add_builtin_verbs` at `:64`).
   No new conflict-resolution code needed; user overrides win
   automatically.

7. **alt-h replacement.** `alt-h` (current `toggle_hidden`) is replaced
   by `alt-.`. One binding for one action; release notes mention the
   change.

## Binding map

### Bare-letter bindings (Command mode only)

| Key | Internal / verb | Notes |
|---|---|---|
| `r` | `bulk_rename` | Inherits F2 behavior: stage ≥ 2 → bulk editor, else single-file rename prompt |
| `d` | `:trash` external | Name-based confirm intercept |
| `D` | `:rm` external | `requires_confirm` flag |
| `y` | `copy_from_staging` | Existing; prompts for destination |
| `Y` | `copy_file_content` | **new internal** |
| `x` | `move_from_staging` | Existing |
| `o` | `open_sort_overlay` | **new internal** opens SortOverlay |
| `b` | `bookmarks` | Adds `b` alongside existing `alt-b` |
| `c` | `copy_name` | **new internal** |
| `C` | `copy_path` | Existing |
| `=` | `toggle_stage` | Adds `=` alongside existing `+` |
| `g` | `select_first` | Substitutes `gg` |
| `G` | `select_last` | |
| `q` | `quit` | Adds `q` alongside existing `ctrl-q` |
| `R` | `refresh` | Adds `R` alongside existing `F5` |
| `n` | `next_match` | Adds `n` alongside existing `tab` |
| `N` | `previous_match` | Adds `N` alongside existing `backtab` |

### Alt bindings (work in both modes)

| Key | Internal | Notes |
|---|---|---|
| `alt-.` | `toggle_hidden` | **replaces** `alt-h` |
| `alt-g` | `toggle_git_status` | new |
| `alt-i` | `toggle_ignore` | new |
| `alt-s` | `toggle_staging_area` | new |
| `alt-p` | `toggle_preview` | new |
| `alt-t` | `toggle_tree` | new |

## New internals

All three follow existing patterns; no new architectural concepts.

### `copy_name`

- Handler in `src/app/panel_state.rs::on_internal_generic`, clone of
  the `copy_path` arm at `:138-155`.
- Reads `selection.path.file_name()`. Errors if no selection or no
  filename component.
- `#[cfg(feature = "clipboard")]` gating identical to `copy_path`.
- Writes via `terminal_clipboard::set_string`.

### `copy_file_content`

- Handler same module as above.
- Refuses if selection is a directory or symlink (error: "selection is
  not a regular file").
- Refuses if `fs::metadata(path)?.len() > 10 * 1024 * 1024` (error:
  "file too large to copy as text"). 10 MB const; not config-tunable
  in v1.
- Reads with `fs::read`, validates UTF-8 (error: "binary content,
  cannot copy as text"), writes via `terminal_clipboard::set_string`.
- `#[cfg(feature = "clipboard")]` gating.

### `open_sort_overlay`

- App-level internal. Intercepted in `App::apply_command` before panel
  dispatch (same shape as the bulk-rename intercept at
  `src/app/app.rs:185-205`).
- Returns `CmdResult::OpenOverlay(Overlay::Sort(SortOverlay::new()))`.

## SortOverlay

New file: `src/app/overlay/sort.rs`. New enum variant in the `Overlay`
enum (`src/app/overlay/mod.rs:127-137`): `Sort(SortOverlay)`. Three
dispatch shims updated in `app.rs` exactly as `Add` was — see the
CLAUDE.md "Overlay routing" section for the invariant.

### Key dispatch

| Key | Action |
|---|---|
| `n` | `CloseAndRun(":no_sort")` |
| `s` | `CloseAndRun(":sort_by_size")` |
| `d` | `CloseAndRun(":sort_by_date")` |
| `c` | `CloseAndRun(":sort_by_count")` |
| `t` | `CloseAndRun(":sort_by_type")` |
| `f` | `CloseAndRun(":sort_by_type_dirs_first")` |
| `l` | `CloseAndRun(":sort_by_type_dirs_last")` |
| `esc` / `q` | `Close` |
| any other key | `Stay` |

### Render

- Bordered box (use existing `draw_frame` + `draw_frame_title`).
- Title: `"Sort by"`.
- 7 body lines, format `[<letter>] <label>`:
  - `[s] size`
  - `[d] date`
  - `[c] count`
  - `[t] type`
  - `[f] type, dirs first`
  - `[l] type, dirs last`
  - `[n] none`
- Sizing: floor 40, cap 80% of screen width, height = `lines + 5` capped
  at 15. Same policy as `ConfirmOverlay`'s short branch
  (`src/app/overlay/confirm.rs:155-205`).
- Bail out as no-op render when `area.width < 8 || area.height < 5`,
  mirroring `AddOverlay::render` (`src/app/overlay/add.rs:226`).
- `draw_frame_title` called with `selected: false` (overlays have no
  selectable root — match `Goto`, `Confirm`, `Add` at the three render
  sites in the existing dispatch shims).

## Files modified

1. `src/verb/internal.rs` — declare 3 new internals in `internals!`.
2. `src/verb/verb_store.rs` — `.add_internal(...)` for 3 new internals;
   `.with_key(...)` lines for every binding in the table above;
   **replace** the `with_key(key!(alt-h))` on `toggle_hidden` with
   `key!(alt-.)`.
3. `src/app/panel_state.rs` — handlers for `Internal::copy_name` and
   `Internal::copy_file_content`.
4. `src/app/app.rs` — App-level intercept arm for
   `Internal::open_sort_overlay`; opens `Overlay::Sort(SortOverlay::new())`.
5. `src/app/overlay/mod.rs` — add `Sort(SortOverlay)` variant; update
   the three `Overlay` dispatch shims (`render`, `handle_key`,
   `handle_mouse`) to forward to the new variant.
6. `src/app/overlay/sort.rs` — new file (~120 LOC).

## Tests

1. `src/verb/verb_store.rs` unit tests — extend the existing
   key-resolution tests with cases for `r`, `d`, `D`, `g`, `G`, `q`,
   `R`, `n`, `N`, asserting each resolves to the expected verb only
   when the panel/selection matches.
2. `src/verb/verb_store.rs` unit test — pin that bare letter keys are
   gated by Command mode (Mode::Input → no resolution; Mode::Command →
   resolves). Mirror the structure of existing destructive-verb pin
   tests at `:756-781`.
3. `src/app/overlay/sort.rs` unit tests — handle_key returns the right
   `CloseAndRun(...)` for each of the 7 letters; esc and `q` return
   `Close`; arbitrary keys return `Stay`.
4. `src/verb/verb_store.rs` unit tests — `copy_name` and
   `copy_file_content` register; resolution returns them for `c`/`Y`.
5. `src/app/panel_state.rs` test (or integration) — `copy_file_content`
   errors on directory selection; errors on file > 10 MB; errors on
   non-UTF-8 content.

## Compatibility

- Users with custom `key:` overrides in `conf.hjson` are unaffected:
  user verbs register first, `find_key_verb` is first-match-wins.
- Default (`modal: false`) users see no change for bare letters
  (they're still gated to filter-by-typing).
- Default users **do** see new alt-* bindings activate (alt-g, alt-i,
  alt-s, alt-p, alt-t, alt-.). Pre-existing alt-h users need release
  notes pointing to alt-.

## Out of scope (deferred)

- Chord support (`gg`, `dd`, `yy` sequences).
- Counts (`5j` to move 5 lines down).
- Visual mode (`v` selection).
- Marks (`mX` / `'X`).
- Macros (`q<key>` / `@<key>`).
- Flipping `modal` default to `true`.
- Making the 10 MB `copy_file_content` cap configurable.
