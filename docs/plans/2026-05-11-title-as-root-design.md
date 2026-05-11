# Frame title as selectable root — design

## Goal

In `BrowserState` panels, make the frame title (which already displays the
tree root path) behave as the visual representation of the root row:

1. When the root is selected (`displayed_tree().selection == 0`), the
   title is rendered with the same background as the body's selection
   bar (`selected_line` bg), so the user sees *which* row is selected.
2. Clicking the title row selects the root (sets `selection = 0`).

Scope is `BrowserState` only. Other panel types (Preview, Stage, Trash,
Help, Fs) keep the existing title rendering and click behaviour.

## Non-goals

- No new style keys. Re-use the existing `frame_title` + `selected_line`
  pair; the selected variant clones `frame_title` and overlays the
  `selected_line` bg.
- No geometry changes. The frame title still paints over the top edge
  of the panel frame, with one cell of padding on each side.
- No change to `tree.lines[0]` data model — root remains a line in
  memory; only rendering changes are visual. Body still iterates
  `tree.lines[1..]`.
- No change to keyboard navigation. `Internal::back`, `Internal::focus`,
  `open_selection_stay_in_broot`, etc., already key off
  `tree.selection == 0` and continue to work unchanged.
- Double-click on the title row is **not** plumbed to "open root /
  go to parent" in this slice (could be a follow-up).

## Section 1 — `PanelState` trait hook

**File:** `src/app/panel_state.rs`

Add a default-`false` accessor next to the existing `frame_title` and
`status_aux` defaults (~line 1170):

```rust
/// True when the panel's frame title should render with the
/// body's selection background, signalling that the (hidden) root
/// row is the current selection. Default `false`; only `BrowserState`
/// overrides this.
fn is_title_selected(&self) -> bool {
    false
}
```

## Section 2 — `BrowserState` override

**File:** `src/browser/browser_state.rs`

```rust
fn is_title_selected(&self) -> bool {
    self.displayed_tree().selection == 0
}
```

`displayed_tree()` already resolves to the filtered tree when one
exists, so the highlight works the same in filter mode.

## Section 3 — Render: `draw_frame_title` gains `selected: bool`

**File:** `src/display/frame.rs`

```rust
pub(crate) fn draw_frame_title<W: Write>(
    w: &mut W,
    area: Area,
    palette: &StyleMap,
    title: &str,
    selected: bool,
) -> io::Result<()> {
    if title.is_empty() || area.width < 6 {
        return Ok(());
    }
    let max_title_width = area.width.saturating_sub(4) as usize;
    let title = truncate_to_width(title, max_title_width);
    if title.is_empty() {
        return Ok(());
    }

    // When selected, overlay the body selection bg onto the frame_title
    // style — fg/attrs preserved. If selected_line has no bg, fall back
    // to the unstyled frame_title look (no panic, no visible change).
    let owned;
    let style_ref: &CompoundStyle = if selected {
        let mut s = palette.frame_title.clone();
        if let Some(bg) = palette.selected_line.get_bg() {
            s.set_bg(bg);
        }
        owned = s;
        &owned
    } else {
        &palette.frame_title
    };

    w.queue(cursor::MoveTo(area.left + 1, area.top))?;
    style_ref.queue(w, ' ').map_err(io_err)?;
    style_ref.queue_str(w, &title).map_err(io_err)?;
    style_ref.queue(w, ' ').map_err(io_err)?;
    Ok(())
}
```

Note: bring `CompoundStyle` into scope (it's already used in
`displayable_tree.rs`; import from termimad).

## Section 4 — Call site: pass `is_title_selected()` from the panel render

**File:** `src/app/app_panels.rs`

Around line 707-713 in `display_panels`:

```rust
if outer.width >= 3 && outer.height >= 3 {
    let frame_style = frame::FrameStyle::rounded();
    frame::draw_frame(w, outer.clone(), &panel_skin.styles, &frame_style)?;
    if outer.width >= 6 {
        let title = panel.state().frame_title(outer.width.saturating_sub(4));
        let title_selected = panel.state().is_title_selected();
        frame::draw_frame_title(
            w,
            outer.clone(),
            &panel_skin.styles,
            &title,
            title_selected,
        )?;
    }
}
```

## Section 5 — Click handling: title row sets `selection = 0`

**File:** `src/browser/browser_state.rs`

Extend `on_click`:

```rust
fn on_click(
    &mut self,
    _x: u16,
    y: u16,
    _screen: Screen,
    _con: &AppContext,
) -> Result<CmdResult, ProgramError> {
    // Title row sits at body_top - 1 (the frame top edge with the
    // title painted over it). Clicking it selects the root, matching
    // the "title is the root element" mental model.
    if self.body_top > 0 && y == self.body_top - 1 {
        self.displayed_tree_mut().selection = 0;
        return Ok(CmdResult::Keep);
    }
    if let Some(body_y) = self.body_relative_y(y) {
        self.displayed_tree_mut().try_select_y(body_y);
    }
    Ok(CmdResult::Keep)
}
```

The `body_top > 0` guard prevents underflow on degenerate tiny
terminals (where the frame collapses and `body_top` is 0).

## Section 6 — Tests

### New tests

1. **`draw_frame_title_selected_emits_selection_bg`** in
   `src/display/frame.rs`:
   - Build a `StyleMap` with both `frame_title` and `selected_line`
     styled (use the same `Yellow None Bold` parse trick as the
     existing `..._emits_sgr_when_palette_is_styled` test).
   - Render the title twice (once with `selected=false`, once with
     `selected=true`) into two buffers.
   - Assert the two byte strings differ — the selected variant must
     emit additional SGR bytes for the body selection bg.
   - Both contain the title text.

2. **`is_title_selected_true_when_root_selected`** and
   **`is_title_selected_false_when_child_selected`** in
   `src/browser/browser_state.rs`:
   - Re-use `fake_browser_state_with_children`. Set `selection = 0`
     vs `selection = 2`. Assert `is_title_selected()` matches.

3. **`on_click_title_row_selects_root`** in
   `src/browser/browser_state.rs`:
   - `body_top = 3`, tree has root + 4 children, `selection = 2`.
   - Call `on_click(0, 2, ...)` (y = body_top - 1).
   - Assert `displayed_tree().selection == 0`.

4. **`on_click_title_row_no_op_when_body_top_zero`** in
   `src/browser/browser_state.rs`:
   - `body_top = 0`, `selection = 2`.
   - Call `on_click(0, 0, ...)`.
   - Assert selection unchanged (still 2). No underflow panic.

### Updated tests

The five existing `draw_frame_title_*` tests in `src/display/frame.rs`
get an extra `false` argument:

- `draw_frame_title_writes_title_into_buffer`
- `draw_frame_title_emits_sgr_when_palette_is_styled`
- `draw_frame_title_skips_empty_or_narrow` (two call sites)
- `draw_frame_then_title_combined`

## Section 7 — Docs

Add a paragraph to `CLAUDE.md` near the existing "Tree root row + aux
status" section noting that the frame title is now the visual carrier
for the root row's selection state, and clicking the title row sets
`selection = 0`. List the trait hook (`PanelState::is_title_selected`)
so future panel types know how to opt in.

## Risk

- Low. All four touch points are localised, behind small additions.
- The trait method is default-`false`, so unaffected panels carry on
  unchanged.
- The render parameter additions are a tiny breaking change to the
  `draw_frame_title` signature, fully contained inside the crate; the
  one production caller and five test call-sites are updated together.
- Skin overrides that don't set `selected_line` bg fall through to the
  unstyled `frame_title` look — no panic, no broken layout.
