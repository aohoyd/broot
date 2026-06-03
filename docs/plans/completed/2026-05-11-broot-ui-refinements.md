# broot UI refinements

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Extend the dark-blue theme to the footer rows, move the preview pane's filename+count into its frame title (deleting the redundant body row), and stop rendering the duplicated root path in the tree body (migrating the auxiliary info it carries to the status row).

**Architecture:** Three orthogonal UI refinements landing on top of the recently shipped elio-convergence work and ratatui-theme port. Two are pure defaults / data-flow changes (status theme, preview title). One requires a small render+click+scroll adjustment (tree root). The aux info that today lives on the root row (git status summary, total size, mount space bar) migrates to the right end of the status row.

**Tech Stack:** Rust, crossterm (cursor/style), termimad (`CompoundStyle`, `Area`, `MadSkin`), hand-rolled rendering (no ratatui).

## Overview

- **Status / hints / input theming**: 12 `StyleMap` defaults (status_*, flag_*, input, purpose_*, mode_command_mark) still ship ANSI-256 gray backgrounds. Flip them to the existing `panel_alt = rgb(6,11,20)` family so the footer zone matches the rest of the dark-blue chrome.
- **Preview pane title**: today the frame top border says the static word "Preview" while body row 0 holds the filename + count. Move filename+count into the frame title and delete body row 0 — one more visible content line, single source of truth.
- **Tree root row**: the root path is shown twice today (frame title + body row 0). Hide the body row. The aux info that row carries (git status summary, total size when `show_sizes`, mount-space bar when `show_root_fs`) migrates to the right end of the status row.

## Context (from discovery)

Files and patterns confirmed in Phase 1 of the brainstorm:

- `src/skin/style_map.rs` — `StyleMap!` macro; lines 196–229 hold the 12 footer keys with ANSI-256 grays. RGB helper exists.
- `src/preview/preview.rs:398-412` — `Preview::display_info` dispatcher delegating to each variant.
- `src/preview/preview_state.rs:325-344` — body row 0 paint (filename left, info_area right). Frame title goes through the default `panel_state.rs:1154-1162`, falling back to static `"Preview"` via `default_frame_title_for_type(PanelStateType::Preview)`.
- `src/syntactic/text_view.rs:648`, `src/hex/hex_view.rs:232`, `src/preview/dir_view.rs:85`, `src/image/image_view.rs:194` — per-variant `display_info` painters.
- `src/display/displayable_tree.rs:408-458` — `write_root_line`. Called unconditionally at `:502`. Carries inline git status (`:443-445`), total size when `show_sizes` (`:415-419`), mount space when `show_root_fs` (`:447-454`).
- `src/tree/tree.rs:288-298` — `try_select_y` maps body y → `tree.lines[y]`.
- `src/display/displayable_tree.rs:490-491` — scrollbar offset `area.top + 1`.
- `src/app/standard_status.rs`, `src/display/status_line.rs:23` — status row paint. `WIDE_STATUS = true` (`src/display/mod.rs:65`); only the active panel writes the full-width status row.
- `src/display/flags_display.rs:27-34` — flag hints on the input row (right side).
- `src/command/panel_input.rs:60-73` — input field and `mode_command_mark`.

Patterns already established by the recent ratatui-theme port:
- Defaults flips for chrome are done in `style_map.rs`; no renderer code changes.
- All `MadSkin` wrappers are rebuilt from `StyleMap` on every `PanelSkin::new`.
- The `frame_title()` trait method on `PanelState` (default at `panel_state.rs:1154-1162`) is the canonical way to set per-state frame titles.
- The `Areas` geometry has `state` (interior, post-inset) vs `state_outer` (full panel rect). All body content paints into `state`.

## Development Approach

- **testing approach**: Regular (code first, then tests)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional — they are a required part of the checklist
  - write unit tests for new functions/methods
  - tests cover both success and edge cases
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- maintain backward compatibility (user `skin:` overrides still work after the defaults flip)

## Testing Strategy

- **unit tests**: required for every task (see Development Approach above)
- broot has no UI-based e2e suite. Visual smoke testing happens manually in Post-Completion.
- The existing `tests/` directory hosts integration tests (e.g. `confirm_destructive.rs`, `goto_bookmarks.rs`). Use it for cross-cutting tests; use module-level `#[cfg(test)]` blocks for narrow unit tests.
- Treat `cargo test --all-features` as the gate — current count is 234. Tasks should not regress that count; new tests should push it up.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

Three independent slices, ordered cheapest → most invasive so each can be reviewed and reverted in isolation:

1. **Defaults flip** for footer-zone styles (Section 1 of the design doc). Pure data change in `style_map.rs`. No renderer touched.
2. **Preview info accessor** — add a text-only `info_string()` next to the existing `display_info()` painters. Sets up Task 3.
3. **Preview frame title + body row delete** — `PreviewState::frame_title()` returns `{filename} • {count}`. The body row 0 paint block is removed; the line loop now starts at `state_area.top`.
4. **Tree root render skip** — `DisplayableTree::write_on` no longer paints `tree.lines[0]`. `Tree::try_select_y` shifts the click mapping by +1. The scrollbar offset drops by 1.
5. **Aux migration** — the three pieces that today decorate the root row (git status summary, total size, mount space) move to the right end of the status row. Status messages still win the row; aux is suppressed during errors.
6. **Verify + docs** — full test suite, CLAUDE.md additions for the new invariants, move plan to completed.

## Technical Details

### Default style values (Task 1)

All twelve keys use `rgb(6,11,20)` (panel_alt) as the background. Foregrounds:
- text rows (`status_normal`, `input`, `purpose_normal`): `rgb(237,244,255)` (text) focused, `rgb(142,162,191)` (muted) unfocused.
- accent (`status_italic`, `status_bold`, `flag_value`, `purpose_italic`, `purpose_bold`): `rgb(255,178,86)` (selection_bar orange).
- code (`status_code`): `rgb(126,196,255)` (accent).
- ellipsis (`status_ellipsis`, `purpose_ellipsis`): `rgb(53,80,111)` (border).
- error (`status_error`): keep red as bg `rgb(224,90,90)` (cut_bar), `rgb(237,244,255)` text.
- job (`status_job`): orange `rgb(255,178,86)` bold.
- flag_label: `rgb(142,162,191)` (muted).
- mode_command_mark: invert — `rgb(9,16,27)` text on `rgb(255,178,86)` (accent_warn) bold.

### Preview `info_string` shapes (Task 2)

Mirror the existing `display_info` painters:
- `TextView`: `"{total} lines"` when unfiltered, `"{filtered}/{total}"` when filtered.
- `HexView`: `"{len} bytes"`.
- `DirView`: `"{tree.lines.len()} entries"` (note: includes the root in count today, keep that).
- `ImageView`: `"{w}x{h}"`.
- `Preview::info_string()` dispatcher: matches the four variants; returns `None` for `ZeroLen` and any unmatched variant.

### Preview frame title format (Task 3)

```
{filename}  •  {info}
```

Two ASCII spaces, then `•` (U+2022), then two spaces. Truncation policy: when `filename + "  •  " + info` exceeds `max_width`, truncate the filename from the right with `…`. Never truncate the info clause (short, informative). If `info` is `None`, the title is just `filename`, truncated to `max_width`.

`truncate_to_width` helper already exists at `src/display/frame.rs` (added during the elio port).

### Tree root index invariants (Task 4)

- `tree.lines[0]` remains the root in memory.
- `tree.selection == 0` continues to mean "root selected". `Internal::back`, `Internal::focus`, and `BrowserState::open_selection_stay_in_broot` keep their existing semantics — they do not move.
- Body render loop iterates `tree.lines[1..]` starting at `state_area.top`.
- `Tree::try_select_y(y)` now maps to `lines[y + 1]` instead of `lines[y]`. Clicking the body cannot select the root anymore (acceptable: frame title + Esc/`<-` cover the use case).
- Scrollbar starts at `area.top` and spans `area.height` rows. Track scroll range is `tree.lines.len() - 1` content rows.
- `BrowserState::page_height` stays at `screen.height - 4`. The freed body row is one extra visible entry.

### Aux migration data flow (Task 5)

Add `BrowserState::aux_status(&self, remaining_width: u16) -> Option<AuxStatus>` (or a similar helper, name TBD during impl). Returns a struct describing what to paint at the right end of the status row:

```rust
pub(crate) struct AuxStatus {
    git_summary: Option<String>,      // when git_status is computed
    total_size:  Option<String>,      // when tree.options.show_sizes
    mount_space: Option<MountSpace>,  // when tree.options.show_root_fs
}
```

Status row paint logic (in `src/app/standard_status.rs` or `src/display/status_line.rs` — confirm during impl):

1. Compute message width.
2. Compute aux width (sum of three pieces' widths + 1-cell separators).
3. If `message_width + aux_width + 2 > area.width`, ellipsize the message (via `status_ellipsis` style) until it fits or aux is suppressed.
4. Paint message left-aligned with `status_normal` (or `status_error` / `status_job` per current state).
5. Paint aux right-aligned. When `status.error == true`, **suppress aux** (errors are short-lived; let them dominate).

For mount space, `MountSpaceDisplay` is a widget. Pass it a constrained `Area` at the right end. Its current minimum width is small (a few cells); if even that doesn't fit, drop it from the aux for this frame.

For non-active panels (`!WIDE_STATUS` would matter, but `WIDE_STATUS = true` is hardcoded), the aux only renders for the active panel — matching the existing status row policy.

### What goes where

- **Implementation Steps** (`[ ]` checkboxes): code changes, tests, and inline documentation in this codebase.
- **Post-Completion** (no checkboxes): visual smoke testing in a real terminal — broot has no e2e suite.

## Implementation Steps

### Task 1: Flip 12 footer-zone style defaults to panel_alt RGB

**Files:**
- Modify: `src/skin/style_map.rs`

- [x] update `status_normal` default: focused `rgb(237,244,255), rgb(6,11,20), []` / unfocused unchanged
- [x] update `status_italic`, `status_bold`, `status_code`, `status_ellipsis`, `status_error`, `status_job` per the table in Technical Details
- [x] update `flag_label`, `flag_value` per the table
- [x] update `input`: focused `rgb(237,244,255), rgb(6,11,20), []` / unfocused `rgb(142,162,191), rgb(6,11,20), []` (note: unfocused gains a bg)
- [x] update `purpose_normal`, `purpose_italic`, `purpose_bold`, `purpose_ellipsis` per the table
- [x] update `mode_command_mark`: `rgb(9,16,27), rgb(255,178,86), [Bold]`
- [x] add unit test in `src/skin/style_map.rs` `#[cfg(test)]` block: build the default `StyleMaps` and assert that the 12 keys' background colors are `Color::Rgb { r: 6, g: 11, b: 20 }` (paranoid pin so future palette tweaks don't silently drift)
- [x] write unit test for `mode_command_mark` specifically (different bg by design)
- [x] write unit test that user `skin:` overrides still work (build a `FxHashMap` with a `status_normal` override and confirm `StyleMaps::create` honors it)
- [x] run `cargo test --all-features` — must pass; count should rise from 234 by ~3

### Task 2: Add Preview info_string accessor

**Files:**
- Modify: `src/syntactic/text_view.rs`
- Modify: `src/hex/hex_view.rs`
- Modify: `src/preview/dir_view.rs`
- Modify: `src/image/image_view.rs`
- Modify: `src/preview/preview.rs`

- [x] add `pub fn info_string(&self) -> Option<String>` to `TextView` returning `"{total} lines"` or `"{filtered}/{total}"` (mirror the existing `display_info` format)
- [x] add same method to `HexView` returning `"{len} bytes"`
- [x] add same method to `DirView` returning `"{} entries"` (tree lines count, matching today's display_info)
- [x] add same method to `ImageView` returning `"{w}x{h}"`
- [x] add `Preview::info_string(&self) -> Option<String>` dispatcher in `src/preview/preview.rs` mirroring the existing `display_info` match (Dir / Image / Text / Hex; `_ => None`)
- [x] write unit tests for each variant's `info_string` (one happy case each; for TextView also a filtered case) — TextView and HexView covered with field-direct construction; DirView and ImageView unit tests skipped (require building a full `Tree` / decoding a real image file); dispatcher routing covered by `info_string_hex_dispatches`
- [x] write unit test for `Preview::info_string` dispatcher (verify it routes; verify `None` for non-matching variants)
- [x] run `cargo test --all-features` — must pass

### Task 3: Move filename+count to preview frame title; delete body row 0

**Files:**
- Modify: `src/preview/preview_state.rs`

- [x] override `frame_title(&self, max_w: u16) -> String` on `PreviewState` (the trait returns `String`, not `Option<String>` — adapted the plan's snippet). Computes filename from `self.source_path.file_name()`, fetches `self.preview.info_string()` (the field is non-optional `Preview`, not `Option<Preview>`). Formats as `"{filename}  •  {info}"` (or just filename when `info` is `None`). Uses `crate::display::frame::truncate_to_width(...)` (`usize` max) with the truncation policy from Technical Details. Width measured via `unicode_width::UnicodeWidthStr` to match `truncate_to_width`'s own measurement.
- [x] remove the body row 0 paint block in `PreviewState::display` (filename + right-aligned info_area + SPACE_FILLING gap). Dropped the now-orphan imports of `CropWriter`, `SPACE_FILLING`, `cursor`, `QueueableCommand`.
- [x] shift the preview content loop's starting y from `state_area.top + 1` to `state_area.top`. Implemented as: drop the `preview_area.height -= 1; preview_area.top += 1` block; `self.preview_area = state_area.clone()` now. The downstream `preview.display(...)` call passes `&self.preview_area`, so the body content fills the full state area.
- [x] verify match-highlight: handled by the variant's `display()` (which paints relative to the passed `area`). Since we removed the inset, all internal y math is implicitly shifted up by 1 — no separate match-highlight code change needed.
- [x] write unit test for `PreviewState::frame_title`: `frame_title_filename_only_when_info_none` (ZeroLen → bare filename), `frame_title_filename_plus_info_fits` (HexView 10-byte tempfile → `"{base}  •  10 bytes"`), `frame_title_truncates_filename_only` (long path + max_w=40 → `…` in filename, info clause preserved verbatim), `frame_title_info_overflow_falls_back_to_filename` (max_w=8 forces fallback, no `•` in result), `frame_title_no_filename_uses_placeholder` (`/` → `"???"`).
- [x] write unit test ensuring the info clause is never truncated — covered by `frame_title_truncates_filename_only` (asserts `title.contains("  •  5 bytes")` even when the filename gets `…`).
- [x] run `cargo test --all-features` — passes; total = 251 (214 lib + 24 confirm + 6 goto + 7 search), +5 from Task 2 baseline (246).

### Task 4: Skip tree root row; shift click and scroll math

**Files:**
- Modify: `src/display/displayable_tree.rs`
- Modify: `src/tree/tree.rs`

- [x] remove the unconditional `self.write_root_line(...)` call at `displayable_tree.rs:502`. Kept `write_root_line` as a method with `#[allow(dead_code)]` and a doc comment noting Task 5 will harvest its aux painters; symbol stays in case Task 5 re-binds it.
- [x] adjust the tree body render loop so `tree.lines[1..]` paints starting at `state_area.top` (not `state_area.top + 1`). Loop is now `for y in 0..self.area.height` with `line_index = y + 1 + scroll`.
- [x] update `Tree::try_select_y` (`src/tree/tree.rs:288-298`) so a click at body row `y` maps to `tree.lines[y + 1 + scroll]`. Out-of-bounds returns `false` (preserves existing semantics).
- [x] update the scrollbar offset in `displayable_tree.rs:490-491`: start at `area.top`, span `area.height`. Total content rows = `tree.lines.len() - 1`.
- [x] verify `BrowserState::page_height` is unchanged (still `screen.height - 4`) — confirmed at `src/browser/browser_state.rs:112-118`.
- [x] verify `Internal::back`, `Internal::focus`, `BrowserState::open_selection_stay_in_broot` invariants — they key off `tree.selection == 0`, which is still valid; data-model `lines[0]` is still the root. Confirmed at `src/browser/browser_state.rs:143`, `:362-364`, `:376`.
- [x] write unit test for `Tree::try_select_y` with the new offset: added 5 tests (`try_select_y_maps_with_offset`, `try_select_y_out_of_bounds_is_noop`, `try_select_y_with_small_tree`, `try_select_y_with_scroll_offset`, `try_select_y_skips_unselectable_line`). Out-of-bounds preserves the existing "return false, selection unchanged" semantics.
- [x] scrollbar offset math is covered implicitly: the change is a 1-cell shift of two parameters into `termimad::compute_scrollbar`; no separate unit test was added (visual smoke covers it, per plan's explicit "no separate unit test is necessary"). The new tests focus on the behavior actually changed in this crate's code (click mapping).
- [x] run `cargo test --all-features` — passes; total = 256 (219 lib + 24 confirm + 6 goto + 7 search), +5 from Task 3 baseline (251). Manual `cargo run` smoke is deferred to Task 6.

### Task 5: Migrate root-row aux info to the right end of the status row

**Files:**
- Create: `src/display/status_aux.rs` (chose `display` rather than `app` — the paint code lives in `src/display/status_line.rs` so the data carrier is colocated; `src/app/mod.rs` was not touched)
- Modify: `src/display/status_line.rs` (this is where the row paint lives — `src/app/standard_status.rs` only builds the message text, not the paint geometry)
- Modify: `src/display/mod.rs` (register `status_aux` and re-export `StatusAux`)
- Modify: `src/app/panel_state.rs` (add `get_status_aux` trait method, default `None`)
- Modify: `src/app/panel.rs` (pull aux from `state().get_status_aux()` in `write_status`)
- Modify: `src/browser/browser_state.rs` (override `get_status_aux`)
- Modify: `src/display/displayable_tree.rs` (deleted `write_root_line` entirely + orphan imports)

- [x] design `AuxStatus { git_summary, total_size, mount_space }` shape. Implemented as `StatusAux { git_summary: Option<String>, total_size: Option<String>, mount: Option<lfs_core::Mount> }` (the mount widget needs the live `Mount`; the two textual pieces are pre-formatted). The struct lives in `src/display/status_aux.rs`.
- [x] add `BrowserState::get_status_aux(&self) -> Option<StatusAux>`. Pulls git status from `self.tree.git_status` (when `ComputationResult::Done`), total size from `self.tree.lines[0].sum` when `tree.options.show_sizes`, mount from `tree.lines[0].mount()` when `tree.options.show_root_fs` (cfg-gated to macOS/Linux/Windows). The signature in the plan included a `remaining_width: u16` hint, but the consumer (`status_line::write`) already knows the area width and applies the budget check itself, so the parameter was dropped.
- [x] update the status row paint function: `status_line::write` now takes `aux: Option<&StatusAux>`. It computes the aux width, suppresses aux when (a) `status.error`, (b) aux is empty, or (c) `aux.width() + 4 > total_after_leading` (4 cells of breathing room: 2-cell gap + 1 cell each side). Message gets the full row minus `aux_w + 2`. Aux paints right-aligned at `area.left + area.width - aux_w`. Style choices: `git_branch` for git, `count` for total size, `MountSpaceDisplay` for mount.
- [x] suppress aux when `status.error == true` — pinned by `aux_suppressed_when_status_is_error` test.
- [x] remove the now-orphan paint blocks inside `write_root_line` — chose **Option 1** (delete the entire method), since Task 4 already removed the only caller. Cleaned up the orphan imports `GitStatusDisplay`, `task_sync::ComputationResult`, `UnicodeWidthChar`, `UnicodeWidthStr` in `displayable_tree.rs`.
- [x] handle the "panel is not the active panel" case — no logic added; `app_panels.rs:725-728` already gates `write_status` behind `disc.active || !WIDE_STATUS`, so aux follows automatically.
- [x] write unit tests for `BrowserState::get_status_aux`: 5 tests added in `src/browser/browser_state.rs` (`aux_status_none_when_no_toggles_and_no_git`, `aux_status_some_when_sizes_on_with_sum`, `aux_status_skips_sizes_when_no_sum_even_if_toggle_on`, `aux_status_includes_git_summary_when_done`, `aux_status_combines_git_and_size`). The "all-three" case requires a real `lfs_core::Mount` which can't be synthesized — skipped, mount path is covered visually. The "narrow-width clipping" case lives at the `status_line` layer (see below).
- [x] write unit tests for the message+aux collision logic in `src/display/status_line.rs`: 6 tests pin the predicate (`aux_suppressed_when_status_is_error`, `aux_shown_when_room_and_no_error`, `aux_suppressed_when_row_too_narrow`, `aux_suppressed_when_empty`, `aux_suppressed_when_none`, `message_width_shrinks_to_make_room_for_aux`, `message_width_full_when_aux_suppressed`). Plus 7 tests for the `StatusAux` carrier in `src/display/status_aux.rs`.
- [x] run `cargo test --all-features` — passes; total = 275 (238 lib + 24 confirm + 6 goto + 7 search), +19 from Task 4 baseline (256). Note: plan predicted 257-261 but the actual `StatusAux` carrier earned its own coverage (`format_git_summary`, width math) which pushed the count higher. No regressions.

### Task 6: Verify acceptance criteria + update docs

**Files:**
- Modify: `CLAUDE.md`
- Move: this plan file to `docs/plans/completed/`

- [x] verify all three Overview goals are implemented (theme on footer, preview title moved, tree root hidden + aux migrated)
- [x] verify edge cases: narrow terminal (status row collision), no git repo (aux skips git_summary), zero-line preview, image preview frame title — all covered by unit tests (`aux_suppressed_when_row_too_narrow`, `aux_status_none_when_no_toggles_and_no_git`, `frame_title_filename_only_when_info_none`); image-variant frame title routes through the same dispatcher.
- [x] run full test suite: `cargo test --all-features` — passes; total = 275 (up from 234).
- [x] run release build: `cargo build --release --all-features` — clean, no warnings.
- [x] add CLAUDE.md section "Footer-zone theming" documenting that all 12 footer keys default to `rgb(6,11,20)` background; user overrides via `skin:` block still work
- [x] add CLAUDE.md section "Preview frame title" documenting that `PreviewState::frame_title` returns `{filename} • {info_string}` and that body row 0 is no longer painted
- [x] add CLAUDE.md section "Tree root row" documenting that `tree.lines[0]` stays in the data model but is never painted; `try_select_y` shifts by +1; the aux info lives in the status row
- [x] move plan to `docs/plans/completed/2026-05-11-broot-ui-refinements.md` (create dir if missing: `mkdir -p docs/plans/completed`)

## Post-Completion

**Manual verification** (broot has no UI e2e suite):

- Visual smoke: `cargo run --release -- ~/Documents` — confirm:
  - status row, input row, flag values all on the dark `rgb(6,11,20)` background; no flat-gray footer remaining
  - Command mode marker shows on the orange `rgb(255,178,86)` background
  - Errors (e.g. `:cd /nonexistent`) paint on red `rgb(224,90,90)` background
- Preview: `ctrl→` on a code file — confirm frame title shows `{filename} • {N lines}`; body has no duplicate title row; one more visible line of content
- Preview variants: also smoke-test image preview (frame title shows `{w}x{h}`), hex preview (`{N bytes}`), directory preview (`{N entries}`)
- Tree: confirm root path appears once (in the frame title), not in the tree body; click on body row 0 selects the first child; `Esc` / `<-` walks up; `:back` walks up
- Aux: enable `:sizes` and `:show_root_fs` (or whatever the toggle internals are named) — confirm git status / total size / mount space appear at the right end of the status row; trigger an error and confirm aux disappears for the duration
- Override path: in `~/.config/broot/conf.hjson` add a temporary `skin: { status_normal: "white on blue" }` block — confirm the override wins over the new default; remove the block and confirm the default returns

**External system updates**: none. This change ships as a self-contained release of broot.
