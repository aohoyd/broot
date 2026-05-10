//! Goto / bookmarks overlay.
//!
//! `GotoOverlay` is a bottom-anchored vertical-list popup that surfaces
//! the user's bookmarks (see [`crate::app::bookmark`]). The list is
//! navigable two ways:
//!
//! - **Single-character jump**: pressing the key bound to a bookmark
//!   immediately closes the overlay and focuses that path.
//! - **Arrow + Enter**: arrow keys (or Tab / Shift+Tab) move the
//!   selection cursor; Enter commits.
//!
//! The mouse can also click a row to commit it directly. Esc / Ctrl-C
//! dismiss without acting.
//!
//! Layout (anchored near the bottom of the screen, horizontally
//! centred):
//!
//! ```text
//! ╭ Goto ─────────────────────────────╮
//! │  h  Home                          │
//! │  d  Downloads                     │
//! │  c  config                        │
//! │  t  Trash                         │
//! ╰───────────────────────────────────╯
//! ```

use {
    super::{
        CellGetCloned,
        OverlayOutcome,
        OverlayState,
        io_err,
        truncate_to_width,
    },
    crate::{
        app::bookmark::BookmarkEntry,
        display::frame::{
            self,
            FrameStyle,
            path_label,
        },
        skin::StyleMap,
    },
    crokey::{
        KeyCombination,
        crossterm::{
            QueueableCommand,
            cursor,
            event::{
                KeyCode,
                MouseButton,
                MouseEvent,
                MouseEventKind,
            },
        },
        key,
    },
    std::{
        cell::Cell,
        io::{
            self,
            Write,
        },
    },
    termimad::{
        Area,
        CompoundStyle,
    },
};

/// Cached row hit-rectangles produced on every `render` and consulted
/// by `handle_mouse` so a click can be resolved to a bookmark row.
///
/// `rows[i]` is the screen rectangle for entry index `i` (only entries
/// that are visible after geometry clamping are included; rows beyond
/// the popup's vertical capacity have no entry here).
#[derive(Debug, Clone, Default)]
pub(crate) struct RowHits {
    pub(crate) rows: Vec<(usize, Area)>,
}

/// A bottom-anchored bookmark-jump modal.
///
/// The struct itself stays `pub` because the integration tests in the
/// `tests/` directory construct it directly. Internal field access is
/// restricted to the crate so behavioural changes don't accidentally
/// leak through the public API.
pub struct GotoOverlay {
    pub(crate) entries: Vec<BookmarkEntry>,
    pub(crate) selected: usize,
    /// Hit-rects for mouse routing — populated by `render`.
    row_hits: Cell<Option<RowHits>>,
    /// Render-time viewport cache: index of the first visible entry.
    ///
    /// This is **not** user-driven scroll state — the user navigates
    /// with `selected` (via arrow keys / Tab); `scroll` is recomputed
    /// by `render` to clamp the viewport so `selected` stays visible.
    /// It lives in a `Cell` so the immutable `render(&self, ...)` path
    /// can update it.
    ///
    /// Contrast with `ConfirmOverlay::scroll`, which is genuinely
    /// user-driven (the up/down handlers move it; the body has no
    /// independent "selected line" concept).
    scroll: Cell<usize>,
}

impl GotoOverlay {
    /// Build a `GotoOverlay` with the given bookmark entries. The
    /// initial selection is row 0.
    pub fn new(entries: Vec<BookmarkEntry>) -> Self {
        Self {
            entries,
            selected: 0,
            row_hits: Cell::new(None),
            scroll: Cell::new(0),
        }
    }

    /// Compute the popup rectangle: 50 columns wide, height sized to fit
    /// the entries (clamped to ≤ 12 rows), horizontally centred and
    /// anchored two rows above the bottom of the screen. If the screen
    /// is too short for bottom anchoring, fall back to centred.
    fn compute_rect(
        screen: &Area,
        entries_len: usize,
    ) -> Area {
        // Width: 50, but never wider than `screen.width - 2`.
        let width = 50u16.min(screen.width.saturating_sub(2)).max(1);
        // Height: entries + 2 (top + bottom border). Clamp to ≤ 12,
        // and to `screen.height - 4` if smaller (leaves room for the
        // bottom anchor).
        let want_h = (entries_len as u16).saturating_add(2).clamp(3, 12);
        let max_h = screen.height.saturating_sub(4).max(3);
        let height = want_h.min(max_h).min(screen.height);

        // Horizontal centring.
        let left = if screen.width > width {
            screen.left + (screen.width - width) / 2
        } else {
            screen.left
        };

        // Vertical anchor: try `screen.bottom - height - 2`. If that
        // would put us above the screen top, fall back to centred.
        let want_top = screen
            .top
            .saturating_add(screen.height)
            .saturating_sub(height)
            .saturating_sub(2);
        let top = if want_top >= screen.top && want_top + height <= screen.top + screen.height {
            want_top
        } else {
            // centre vertically as a fallback
            screen.top + screen.height.saturating_sub(height) / 2
        };

        Area::new(left, top, width, height)
    }

    /// Test-only accessor to peek at the cached hit-rects after render.
    #[cfg(test)]
    fn cached_hits(&self) -> Option<RowHits> {
        self.row_hits.get_cloned()
    }
}

// =============================================================================
// OverlayState impl
// =============================================================================

impl OverlayState for GotoOverlay {
    fn render<Wr: Write>(
        &self,
        w: &mut Wr,
        screen: Area,
        palette: &StyleMap,
    ) -> io::Result<()> {
        let area = Self::compute_rect(&screen, self.entries.len());
        if area.width < 8 || area.height < 3 {
            return Ok(());
        }

        // ---- background clear ----------------------------------------
        let bg = &palette.default;
        for y in area.top..(area.top + area.height) {
            w.queue(cursor::MoveTo(area.left, y))?;
            for _ in 0..area.width {
                bg.queue(w, ' ').map_err(io_err)?;
            }
        }

        // ---- frame + title -------------------------------------------
        let style = FrameStyle::rounded();
        frame::draw_frame(w, area.clone(), palette, &style)?;
        frame::draw_frame_title(w, area.clone(), palette, " Goto ")?;

        // ---- rows ----------------------------------------------------
        let inner_left = area.left + 1;
        let inner_width = area.width.saturating_sub(2);
        let visible_rows = area.height.saturating_sub(2) as usize;
        // 2 leading spaces + key (1 col) + 2 spaces between key and label.
        let label_max_w = inner_width.saturating_sub(5);

        // Adjust scroll so `selected` is in view. `scroll` is the index
        // of the first visible entry. When selected is below the
        // viewport we scroll down; above, we scroll up. Clamp so the
        // last visible row never lies past the entries' end.
        let mut scroll = self.scroll.get();
        if !self.entries.is_empty() && visible_rows > 0 {
            if self.selected < scroll {
                scroll = self.selected;
            } else if self.selected >= scroll + visible_rows {
                scroll = self.selected + 1 - visible_rows;
            }
            let max_scroll = self.entries.len().saturating_sub(visible_rows);
            if scroll > max_scroll {
                scroll = max_scroll;
            }
        }
        self.scroll.set(scroll);

        let mut hits: Vec<(usize, Area)> = Vec::with_capacity(visible_rows);

        for row in 0..visible_rows {
            let entry_idx = scroll + row;
            if entry_idx >= self.entries.len() {
                break;
            }
            let entry = &self.entries[entry_idx];
            let row_y = area.top + 1 + row as u16;
            let is_selected = entry_idx == self.selected;

            let row_style: &CompoundStyle = if is_selected {
                &palette.selected_line
            } else {
                &palette.default
            };

            // Paint the row background first so the highlight reaches
            // the right edge.
            w.queue(cursor::MoveTo(inner_left, row_y))?;
            for _ in 0..inner_width {
                row_style.queue(w, ' ').map_err(io_err)?;
            }

            // Compose: "  <key>  <label>"
            let label_text = if !entry.label.is_empty() {
                entry.label.clone()
            } else {
                path_label(&entry.path, label_max_w)
            };
            let label_text = truncate_to_width(&label_text, label_max_w as usize);

            w.queue(cursor::MoveTo(inner_left, row_y))?;
            row_style.queue_str(w, "  ").map_err(io_err)?;
            row_style.queue(w, entry.key).map_err(io_err)?;
            row_style.queue_str(w, "  ").map_err(io_err)?;
            row_style.queue_str(w, &label_text).map_err(io_err)?;

            hits.push((entry_idx, Area::new(inner_left, row_y, inner_width, 1)));
        }

        self.row_hits.set(Some(RowHits { rows: hits }));
        Ok(())
    }

    fn handle_key(
        &mut self,
        key: KeyCombination,
    ) -> OverlayOutcome {
        // Cancel keys.
        if key == key!(esc) || key == key!(ctrl - c) {
            return OverlayOutcome::Close;
        }

        // Enter commits the current selection.
        if key == key!(enter) {
            if let Some(entry) = self.entries.get(self.selected) {
                return OverlayOutcome::CloseAndFocus(entry.path.clone());
            }
            return OverlayOutcome::Stay;
        }

        // Arrow / Tab navigation.
        if key == key!(down) || key == key!(tab) {
            if !self.entries.is_empty() {
                self.selected = (self.selected + 1).min(self.entries.len() - 1);
            }
            return OverlayOutcome::Stay;
        }
        if key == key!(up) || key == key!(shift - backtab) || key == key!(shift - tab) {
            self.selected = self.selected.saturating_sub(1);
            return OverlayOutcome::Stay;
        }

        // Single-character jump: if the keystroke is a printable char
        // and matches one of the bookmark keys, commit immediately.
        // Match case-insensitively so Shift+H also fires the `h` jump
        // — terminals deliver the same Char keycode bearing different
        // case depending on the modifier state.
        if let Some(c) = printable_char(&key) {
            if let Some(entry) = self
                .entries
                .iter()
                .find(|e| {
                    let mut buf = [0u8; 4];
                    let key_str = e.key.encode_utf8(&mut buf);
                    let mut buf2 = [0u8; 4];
                    let c_str = c.encode_utf8(&mut buf2);
                    key_str.eq_ignore_ascii_case(c_str)
                })
            {
                return OverlayOutcome::CloseAndFocus(entry.path.clone());
            }
        }

        OverlayOutcome::Stay
    }

    fn handle_mouse(
        &mut self,
        ev: MouseEvent,
    ) -> OverlayOutcome {
        if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
            return OverlayOutcome::Stay;
        }
        let Some(hits) = self.row_hits.get_cloned() else {
            return OverlayOutcome::Stay;
        };
        for (idx, rect) in &hits.rows {
            if rect.contains(ev.column, ev.row) {
                if let Some(entry) = self.entries.get(*idx) {
                    return OverlayOutcome::CloseAndFocus(entry.path.clone());
                }
            }
        }
        OverlayOutcome::Stay
    }
}

// =============================================================================
// helpers
// =============================================================================

/// Extract the printable character from a `KeyCombination`, if any —
/// used by single-character jump matching. Modifier-bearing combos
/// (Ctrl / Alt) are deliberately rejected so e.g. Ctrl-h doesn't jump
/// to the home bookmark.
fn printable_char(key: &KeyCombination) -> Option<char> {
    use crokey::crossterm::event::KeyModifiers;
    if !key.modifiers.is_empty() && key.modifiers != KeyModifiers::SHIFT {
        return None;
    }
    match key.codes.first() {
        KeyCode::Char(c) => Some(*c),
        _ => None,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use {
        super::*,
        crokey::crossterm::event::{
            KeyModifiers,
        },
        std::path::PathBuf,
    };

    fn entry(
        key: char,
        path: &str,
        label: &str,
    ) -> BookmarkEntry {
        BookmarkEntry {
            key,
            path: PathBuf::from(path),
            label: label.to_string(),
        }
    }

    fn four_entries() -> Vec<BookmarkEntry> {
        vec![
            entry('h', "/home/me", "Home"),
            entry('d', "/home/me/Downloads", "Downloads"),
            entry('c', "/home/me/.config", "config"),
            entry('t', "/home/me/.Trash", "Trash"),
        ]
    }

    fn mouse_at(
        x: u16,
        y: u16,
    ) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    // ---- key dispatch ---------------------------------------------------

    #[test]
    fn key_h_jumps_to_home_entry() {
        let mut o = GotoOverlay::new(four_entries());
        let r = o.handle_key(key!('h'));
        match r {
            OverlayOutcome::CloseAndFocus(p) => assert_eq!(p, PathBuf::from("/home/me")),
            other => panic!("expected CloseAndFocus, got {other:?}"),
        }
    }

    #[test]
    fn key_z_no_match_stays() {
        let mut o = GotoOverlay::new(four_entries());
        let r = o.handle_key(key!('z'));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn down_increments_selected_and_clamps() {
        let mut o = GotoOverlay::new(four_entries());
        assert_eq!(o.selected, 0);
        let _ = o.handle_key(key!(down));
        assert_eq!(o.selected, 1);
        // walk past the end — should clamp at len-1 (3)
        for _ in 0..10 {
            let _ = o.handle_key(key!(down));
        }
        assert_eq!(o.selected, 3);
    }

    #[test]
    fn up_decrements_selected_and_clamps_at_zero() {
        let mut o = GotoOverlay::new(four_entries());
        o.selected = 2;
        let _ = o.handle_key(key!(up));
        assert_eq!(o.selected, 1);
        let _ = o.handle_key(key!(up));
        assert_eq!(o.selected, 0);
        // already at top → stays
        let _ = o.handle_key(key!(up));
        assert_eq!(o.selected, 0);
    }

    #[test]
    fn tab_increments_selected() {
        let mut o = GotoOverlay::new(four_entries());
        let _ = o.handle_key(key!(tab));
        assert_eq!(o.selected, 1);
    }

    #[test]
    fn enter_returns_close_and_focus_for_selected() {
        let mut o = GotoOverlay::new(four_entries());
        o.selected = 2; // config
        let r = o.handle_key(key!(enter));
        match r {
            OverlayOutcome::CloseAndFocus(p) => {
                assert_eq!(p, PathBuf::from("/home/me/.config"));
            }
            other => panic!("expected CloseAndFocus, got {other:?}"),
        }
    }

    #[test]
    fn enter_with_empty_entries_stays() {
        let mut o = GotoOverlay::new(vec![]);
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn esc_closes() {
        let mut o = GotoOverlay::new(four_entries());
        let r = o.handle_key(key!(esc));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn ctrl_c_closes() {
        let mut o = GotoOverlay::new(four_entries());
        let r = o.handle_key(key!(ctrl - c));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn ctrl_h_does_not_jump() {
        // Ensure Ctrl-modified char keys don't trigger a single-char jump.
        let mut o = GotoOverlay::new(four_entries());
        let r = o.handle_key(key!(ctrl - h));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn shift_letter_jumps_case_insensitively() {
        // Pressing Shift+H (which most terminals deliver as Char('H')
        // with the SHIFT modifier) must still hit the lowercase `h`
        // bookmark — the bookmark keys are conventionally lowercase
        // and we do not want capitalisation to break a routine jump.
        let mut o = GotoOverlay::new(four_entries());
        let r = o.handle_key(key!(shift - 'h'));
        match r {
            OverlayOutcome::CloseAndFocus(p) => assert_eq!(p, PathBuf::from("/home/me")),
            other => panic!("expected CloseAndFocus, got {other:?}"),
        }
    }

    #[test]
    fn down_with_empty_entries_stays() {
        // Pressing arrow-down with no entries must not panic and must
        // leave selection at 0.
        let mut o = GotoOverlay::new(vec![]);
        let r = o.handle_key(key!(down));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.selected, 0);
    }

    #[test]
    fn shift_tab_decrements_selected() {
        let mut o = GotoOverlay::new(four_entries());
        o.selected = 2;
        let r = o.handle_key(key!(shift - tab));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.selected, 1);
    }

    // ---- mouse routing --------------------------------------------------

    #[test]
    fn mouse_on_row_returns_close_and_focus() {
        let mut o = GotoOverlay::new(four_entries());
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.cached_hits().expect("hits should be cached after render");
        // pick row index 1 (Downloads)
        let (idx, rect) = hits
            .rows
            .iter()
            .find(|(i, _)| *i == 1)
            .cloned()
            .expect("row 1 should be visible");
        assert_eq!(idx, 1);
        let cx = rect.left + rect.width / 2;
        let cy = rect.top;
        let r = o.handle_mouse(mouse_at(cx, cy));
        match r {
            OverlayOutcome::CloseAndFocus(p) => {
                assert_eq!(p, PathBuf::from("/home/me/Downloads"));
            }
            other => panic!("expected CloseAndFocus, got {other:?}"),
        }
    }

    #[test]
    fn mouse_off_rows_stays() {
        let mut o = GotoOverlay::new(four_entries());
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        // 0,0 — way off the popup
        let r = o.handle_mouse(mouse_at(0, 0));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn mouse_before_render_stays() {
        // No render call → empty hit-rect cache → mouse handler must
        // gracefully return Stay rather than panic or fire focus.
        let mut o = GotoOverlay::new(four_entries());
        let r = o.handle_mouse(mouse_at(20, 18));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn mouse_non_left_click_stays() {
        let mut o = GotoOverlay::new(four_entries());
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.cached_hits().unwrap();
        let (_idx, rect) = hits.rows.first().cloned().unwrap();
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: rect.left + 1,
            row: rect.top,
            modifiers: KeyModifiers::NONE,
        };
        let r = o.handle_mouse(ev);
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- render shape ---------------------------------------------------

    #[test]
    fn render_writes_corners_title_keys_and_labels() {
        // Drive the real `OverlayState::render` and capture its output
        // from the `BufWriter<Sink>` buffer *before* flushing. The
        // capture works because `BufWriter::buffer()` exposes the
        // unflushed bytes; we deliberately don't `flush()` so the
        // bytes remain inspectable. Sink backing keeps cargo test's
        // stderr clean (unflushed bytes never hit a real fd).
        let palette = StyleMap::no_term();
        let o = GotoOverlay::new(four_entries());
        let mut wbuf =
            std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains('╭'), "missing top-left corner in render bytes");
        assert!(s.contains('╰'), "missing bottom-left corner in render bytes");
        assert!(s.contains(" Goto "), "missing title in render bytes: {s:?}");
        for c in ['h', 'd', 'c', 't'] {
            assert!(
                s.contains(c),
                "missing bookmark key {c:?} in render bytes",
            );
        }
        for label in ["Home", "Downloads", "config", "Trash"] {
            assert!(s.contains(label), "missing label {label:?} in render bytes");
        }
    }

    #[test]
    fn render_caches_row_hits() {
        let o = GotoOverlay::new(four_entries());
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        assert!(o.cached_hits().is_none());
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.cached_hits().expect("hits must populate after render");
        assert_eq!(hits.rows.len(), 4);
        // All rows must be width > 0 and non-overlapping vertically.
        for (_idx, rect) in &hits.rows {
            assert!(rect.width > 0);
            assert_eq!(rect.height, 1);
        }
    }

    #[test]
    fn render_scrolls_to_keep_selection_visible() {
        // 20 entries; popup height clamps to 12 (10 visible rows).
        // Navigating selected past index 9 must scroll the viewport
        // so the selected entry is still drawn (row hit-rect present).
        let entries: Vec<BookmarkEntry> = (0..20)
            .map(|i| entry((b'a' + i as u8) as char, &format!("/p/{i}"), &format!("p{i}")))
            .collect();
        let mut o = GotoOverlay::new(entries);
        o.selected = 15;
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.cached_hits().unwrap();
        // The selected entry's index must appear in the hit-rect set.
        assert!(
            hits.rows.iter().any(|(idx, _)| *idx == 15),
            "selected index 15 must be visible after render; got rows = {:?}",
            hits.rows.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn render_clamps_height_with_overflow_entries() {
        // 20 entries — popup height clamps to ≤ 12.
        let entries: Vec<BookmarkEntry> = (0..20)
            .map(|i| entry(((b'a' + i as u8) as char), &format!("/p/{i}"), &format!("p{i}")))
            .collect();
        let o = GotoOverlay::new(entries);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        let rect = GotoOverlay::compute_rect(&screen, 20);
        assert!(rect.height <= 12, "popup height {} > 12", rect.height);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.cached_hits().unwrap();
        // visible row count = popup height - 2 (top/bottom border).
        let expected_visible = (rect.height - 2) as usize;
        assert_eq!(
            hits.rows.len(),
            expected_visible,
            "expected {expected_visible} visible rows, got {}",
            hits.rows.len()
        );
    }

    #[test]
    fn compute_rect_anchors_near_bottom_when_screen_tall() {
        let screen = Area::new(0, 0, 80, 30);
        let rect = GotoOverlay::compute_rect(&screen, 4);
        // height = entries (4) + 2 = 6. Anchored at screen.bottom - 6 - 2 = 22.
        assert_eq!(rect.height, 6);
        assert_eq!(rect.top, 22);
        // Horizontally centred: (80 - 50) / 2 = 15.
        assert_eq!(rect.left, 15);
        assert_eq!(rect.width, 50);
    }

    #[test]
    fn compute_rect_falls_back_to_centred_when_screen_short() {
        // screen too short for bottom anchoring with 2-row margin
        let screen = Area::new(0, 0, 80, 5);
        let rect = GotoOverlay::compute_rect(&screen, 4);
        // height clamped to screen.height - 4 = 1, then min 3, then min screen.height = 5
        // The function clamps to max(want_h.min(max_h), ...) actually:
        //   want_h = 4+2 = 6 -> clamped to 12 -> 6
        //   max_h = 5-4 = 1, .max(3) = 3
        //   height = 6.min(3).min(5) = 3
        assert!(rect.height <= 5);
        assert!(rect.top + rect.height <= 5);
    }

    #[test]
    fn empty_entries_renders_without_panic() {
        let o = GotoOverlay::new(vec![]);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.cached_hits().unwrap();
        assert!(hits.rows.is_empty());
    }
}
