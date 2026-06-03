# broot UI refinements — design

## Goal

Three independent UI improvements landing on top of the recently shipped
elio-convergence work and ratatui-theme port:

1. Extend the dark-blue RGB chrome to the status row, hints row, input
   row, and command-mode marker (currently still flat ANSI 256 gray).
2. Move the preview pane's filename + line/byte/entry indicator into
   the frame top-border title; delete the now-redundant body title row.
3. Stop rendering the root folder in the tree body (it is already shown
   in the frame title) and migrate the auxiliary info it carries
   (git status summary, total size, mount space) to the right end of
   the status row.

## Non-goals

- No new keybinds, verbs, or config keys.
- No change to syntect syntax-token colors.
- No change to panel geometry / `Areas` / `page_height` math.
- No change to overlay routing or verb confirmation systems.
- No light-terminal palette work.

## Section 1 — Status / hints / input theming

**Files**: `src/skin/style_map.rs` only.

Twelve default overrides in the `StyleMap!` macro block. All new
backgrounds use `rgb(6, 11, 20)` (the existing `panel_alt` color), to
visually mark the footer zone and stay consistent with the unfocused
panel chrome.

| Key | New focused fg, bg, [attrs] | Unfocused (if different) |
|---|---|---|
| `status_normal`    | `rgb(237,244,255), rgb(6,11,20), []`       | unchanged-from-focused |
| `status_italic`    | `rgb(255,178,86),  rgb(6,11,20), []`       | |
| `status_bold`      | `rgb(255,178,86),  rgb(6,11,20), [Bold]`   | |
| `status_code`      | `rgb(126,196,255), rgb(6,11,20), []`       | |
| `status_ellipsis`  | `rgb(53,80,111),   rgb(6,11,20), []`       | |
| `status_error`     | `rgb(237,244,255), rgb(224,90,90), []`     | |
| `status_job`       | `rgb(255,178,86),  rgb(6,11,20), [Bold]`   | |
| `flag_label`       | `rgb(142,162,191), rgb(6,11,20), []`       | |
| `flag_value`       | `rgb(255,178,86),  rgb(6,11,20), [Bold]`   | |
| `input`            | `rgb(237,244,255), rgb(6,11,20), []`       | `rgb(142,162,191), rgb(6,11,20), []` |
| `purpose_normal`   | `rgb(237,244,255), rgb(6,11,20), []`       | |
| `purpose_italic`   | `rgb(255,178,86),  rgb(6,11,20), []`       | |
| `purpose_bold`     | `rgb(255,178,86),  rgb(6,11,20), [Bold]`   | |
| `purpose_ellipsis` | `rgb(53,80,111),   rgb(6,11,20), []`       | |
| `mode_command_mark`| `rgb(9,16,27),     rgb(255,178,86), [Bold]`| |

**No renderer code changes.** `panel_skin.styles.<key>` paths
(`src/display/status_line.rs`, `src/display/flags_display.rs`,
`src/command/panel_input.rs`, `src/skin/purpose_mad_skin.rs`,
`src/skin/status_mad_skin.rs`) already consume these slots; the
`MadSkin` wrappers are rebuilt on every `PanelSkin::new`.

## Section 2 — Preview pane title

**Files**: `src/preview/preview_state.rs`, `src/preview/mod.rs` (or
wherever the `Preview` enum lives), `src/preview/text_view.rs`,
`src/preview/hex_view.rs`, `src/preview/dir_view.rs`,
`src/preview/image_view.rs`.

### 2.1 Add `info_string` (text-only counterpart to `display_info`)

Each preview variant already has a `display_info(...)` method that
paints the count into a right-aligned `info_area`. Add a parallel
text-only accessor:

```rust
impl TextView {
    pub fn info_string(&self) -> Option<String> {
        // matches the existing display_info format
        if self.total_lines_count == self.content_lines_count {
            Some(format!("{} lines", self.total_lines_count))
        } else {
            Some(format!("{}/{}", self.content_lines_count, self.total_lines_count))
        }
    }
}
impl HexView    { pub fn info_string(&self) -> Option<String> { Some(format!("{} bytes", self.len)) } }
impl DirView    { pub fn info_string(&self) -> Option<String> { Some(format!("{} entries", self.tree.lines.len())) } }
impl ImageView  { pub fn info_string(&self) -> Option<String> { Some(format!("{}x{}", self.width, self.height)) } }
```

Wire these into a `Preview::info_string(&self) -> Option<String>`
dispatcher.

### 2.2 Implement `frame_title` on `PreviewState`

In `src/preview/preview_state.rs`, override the default:

```rust
fn frame_title(&self, max_width: u16) -> Option<String> {
    let filename = self.source_path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "???".into());
    let info = self.preview.as_ref().and_then(|p| p.info_string());
    Some(match info {
        Some(info) => crate::display::frame::truncate_to_width(
            &format!("{filename}  •  {info}"), max_width,
        ),
        None => crate::display::frame::truncate_to_width(&filename, max_width),
    })
}
```

Truncation policy: filename is truncated with `…` from the right;
the count clause is preserved intact (it is short and informative).

### 2.3 Delete body row 0 in `PreviewState::display`

The current paint block (`preview_state.rs:325-344`) writes filename +
`info_area` into the first content row of the interior. Delete it.
The remaining lines (1, 2, 3, ...) shift up by one — one more visible
content row. Adjust the `state_area.top` start row accordingly: today
the loop iterates `for y in state_area.top+1 ..` (or similar — confirm
exact form during implementation); after the change it iterates from
`state_area.top` directly.

The match-highlight (`preview_match` style) path stays as-is.

## Section 3 — Tree root row hide + aux migration

**Files**: `src/display/displayable_tree.rs`, `src/tree/tree.rs`,
`src/browser/browser_state.rs`, `src/display/status_line.rs`
(or a new `src/display/status_aux.rs`).

### 3.1 Skip rendering row 0 in `write_on`

In `src/display/displayable_tree.rs::write_on` (around line 502), the
unconditional `self.write_root_line(...)` call is removed. The render
loop that follows is shifted: it now iterates `tree.lines[1..]`
starting at the interior top row (`state_area.top`, not
`state_area.top + 1`).

`tree.lines[0]` (the root) remains in memory. All selection / scroll
indices continue to key off `tree.selection`; nothing in the data
model moves.

### 3.2 Adjust click-y → selection in `Tree::try_select_y`

`src/tree/tree.rs:288-298`. The current code maps `y == 0` → `lines[0]`
(the root). After hiding the root row, body row 0 visually contains
`lines[1]`. Shift the mapping: `selected_index = y + 1` (or however the
existing function is structured — implementation detail). The click
gesture to "select the root" becomes unreachable through the tree
body; this is acceptable because the frame title carries the path and
`Internal::back` (Esc / `<-`) still walks up.

### 3.3 Adjust scrollbar offset

`src/display/displayable_tree.rs:490-491`. Today the scrollbar starts
at `area.top + 1` to skip the root row. With the root row gone, it
should start at `area.top` and span `area.height` rows.

### 3.4 Migrate aux info to the status row

Three aux pieces today live in `write_root_line`:

- **Git status summary** — `GitStatusDisplay::from(&git_status, ...)`,
  inline at `displayable_tree.rs:443-445`. Always paints when git
  status is computed.
- **Total size** — `file_size::fit_4(line.sum)`,
  `displayable_tree.rs:415-419`. Paints when `tree.options.show_sizes`.
- **Mount space** — `MountSpaceDisplay::from(&mount, ...)`,
  `displayable_tree.rs:447-454`. Paints when
  `tree.options.show_root_fs`.

Migration target: right end of the status row.

Add a helper `BrowserState::aux_status(&self, remaining_width: u16)`
returning a small struct describing what to paint (one or more
right-aligned strings + optional mount widget).

Update `App::write_status` (or the `StatusBuilder` flow at
`src/app/status_builder.rs` — confirm exact path during
implementation) to:

1. Paint status message on the left.
2. Compute aux width.
3. Truncate the message with `status_ellipsis` if message + aux
   would overlap. Reserve 2 spaces of gap.
4. Paint aux right-aligned.

When `status.error` is true (red background), suppress the aux for
that frame — errors are short-lived and the visual contrast wins.

`MountSpaceDisplay` is a widget, not a string. The simplest
integration: define a `RightAuxItem` enum with `Text(String, Style)`
and `MountWidget(MountSpace)` variants, render in order. The aux
slot must still fit in the status row's height (1 row).

## Test plan

- Build clean: `cargo build --release --all-features` — no warnings.
- Test suite: `cargo test --all-features` — 234 today; new tests
  expected to push the count up, no regressions.

New tests:
- A `style_map` smoke test that asserts the 12 footer keys' fg/bg
  match the panel_alt palette (paranoid pin so future palette tweaks
  don't silently drift).
- A `preview_state::frame_title` test for each variant
  (text/hex/dir/image) with and without count info, plus truncation.
- A `tree::try_select_y` test for the new row offset.
- A status-row aux integration test (probably as a unit test on the
  new `aux_status` helper — full app-level rendering is hard to test).

Visual smoke:
- `cargo run -- ~/Documents` — confirm dark-blue status row and input
  row, orange flag values, no flat-gray footer remaining.
- Preview a file — confirm frame title shows `{filename} • {count}`
  and body row 0 is gone (one more visible content line).
- Confirm tree body no longer shows the root path; clicking row 0
  selects the first child, not the root; `Esc` / `<-` still walks up.
- Toggle `:sizes` and `:show_root_fs` (if those internals exist with
  those names) — confirm aux info appears at the right end of the
  status row.
- Trigger an error (e.g. `:cd /nonexistent`) — confirm error message
  takes the row, aux is suppressed.

## Out of scope / follow-up

- Customizing the `•` separator in preview frame titles (could become
  a `preview_title_separator` config key).
- A toggle to bring back the root body row for users who prefer it
  (could become `show_tree_root: bool` in conf).
- Adjusting `MountSpaceDisplay` width when squeezed in the status
  row vs current root-row width.
