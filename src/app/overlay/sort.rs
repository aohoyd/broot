//! Sort overlay — pick a sort mode with a single keystroke.
//!
//! `SortOverlay` is a compact, centred modal listing the seven sort
//! modes broot supports (size, date, count, type, type-dirs-first,
//! type-dirs-last, none). Each row is prefixed with the single-letter
//! shortcut that picks it; pressing the letter closes the overlay and
//! runs the matching internal verb through the normal `apply_command`
//! machinery (the same `CloseAndRun` plumbing the confirm overlay
//! uses).
//!
//! The overlay holds no mutable state — `SortOverlay` is a unit struct
//! whose `handle_key` is a pure key-to-verb mapping. Esc / `q` cancel;
//! every other key is consumed silently (so a stray keypress doesn't
//! fall through to the panel underneath). Mouse events are ignored.
//!
//! Layout (centred on the screen):
//!
//! ```text
//! ╭─ Sort by ────────────────────────────────────╮
//! │ [s] size                                     │
//! │ [d] date                                     │
//! │ [c] count                                    │
//! │ [t] type                                     │
//! │ [f] type, dirs first                         │
//! │ [l] type, dirs last                          │
//! │ [n] none                                     │
//! ╰──────────────────────────────────────────────╯
//! ```

use {
    super::{
        OverlayOutcome,
        OverlayState,
        io_err,
        truncate_to_width,
    },
    crate::{
        command::Command,
        display::frame::{
            self,
            FrameStyle,
        },
        skin::StyleMap,
    },
    crokey::{
        KeyCombination,
        crossterm::{
            QueueableCommand,
            cursor,
            event::MouseEvent,
        },
        key,
    },
    std::io::{
        self,
        Write,
    },
    termimad::Area,
    unicode_width::UnicodeWidthStr,
};

/// The seven body rows in render order. Each entry is `(letter, label)`
/// — the letter matches the single-key shortcut in `handle_key` and the
/// label is what the user reads. Display-only; `handle_key` does not
/// consult this table.
const SORT_ROWS: &[(char, &str)] = &[
    ('s', "size"),
    ('d', "date"),
    ('c', "count"),
    ('t', "type"),
    ('f', "type, dirs first"),
    ('l', "type, dirs last"),
    ('n', "none"),
];

/// Stateless sort-mode picker. See module docs for layout and behaviour.
pub struct SortOverlay;

impl SortOverlay {
    /// Build an empty `SortOverlay`. Mirrors the other overlay
    /// constructors so callers use a uniform construction pattern.
    pub fn new() -> Self {
        Self
    }
}

// =============================================================================
// OverlayState impl
// =============================================================================

impl OverlayState for SortOverlay {
    fn render<Wr: Write>(
        &self,
        w: &mut Wr,
        screen: Area,
        palette: &StyleMap,
    ) -> io::Result<()> {
        // ---- geometry ---------------------------------------------------
        // Mirror the confirm-overlay short-body sizing: floor 40, cap
        // 80% of screen.width, height = body rows + 5 (frame + padding)
        // capped at 15.
        let title = "Sort by";

        let max_w = (screen.width as u32 * 8 / 10) as u16;
        let content_max = SORT_ROWS
            .iter()
            .map(|(_, label)| UnicodeWidthStr::width(*label) + 4)
            .max()
            .unwrap_or(0);
        let content_w =
            (content_max.min(u16::MAX as usize) as u16).saturating_add(4);
        let title_w = (UnicodeWidthStr::width(title).min(u16::MAX as usize) as u16)
            .saturating_add(4);
        let want_w = content_w.max(title_w).max(40).min(max_w);

        let want_h: u16 = (SORT_ROWS.len().min(u16::MAX as usize) as u16)
            .saturating_add(5)
            .min(15);

        let area = frame::centered_rect(screen, want_w, want_h);
        if area.width < 8 || area.height < 5 {
            // Too small to draw anything sensible — bail. Matches the
            // confirm / add overlays.
            return Ok(());
        }

        // ---- background clear ------------------------------------------
        let bg = &palette.default;
        for y in area.top..(area.top + area.height) {
            w.queue(cursor::MoveTo(area.left, y))?;
            for _ in 0..area.width {
                bg.queue(w, ' ').map_err(io_err)?;
            }
        }

        // ---- frame + title ---------------------------------------------
        let style = FrameStyle::rounded();
        frame::draw_frame(w, area.clone(), palette, &style)?;
        frame::draw_frame_title(w, area.clone(), palette, title, false)?;

        // ---- body rows --------------------------------------------------
        let inner_left = area.left + 2;
        let inner_width = area.width.saturating_sub(4) as usize;
        let body_style = &palette.default;

        // Body sits between top frame (y = area.top) and the bottom
        // frame (y = area.top + area.height - 1). Reserve one blank row
        // above the bottom frame for breathing room.
        let body_top = area.top + 1;
        let body_capacity = area
            .height
            .saturating_sub(3) as usize; // top frame + blank + bottom frame

        for (i, (letter, label)) in SORT_ROWS.iter().enumerate() {
            if i >= body_capacity {
                break;
            }
            let y = body_top + i as u16;
            let row_text = format!("[{}] {}", letter, label);
            w.queue(cursor::MoveTo(inner_left, y))?;
            body_style
                .queue_str(w, truncate_to_width(&row_text, inner_width))
                .map_err(io_err)?;
        }

        Ok(())
    }

    fn handle_key(
        &mut self,
        key: KeyCombination,
    ) -> OverlayOutcome {
        // Cancel keys.
        if key == key!(esc) || key == key!('q') || key == key!(ctrl - c) {
            return OverlayOutcome::Close;
        }
        // Letter -> verb mapping. Each branch synthesizes a verb
        // invocation that the overlay outcome layer re-dispatches via
        // `apply_command`.
        if key == key!('n') {
            return OverlayOutcome::CloseAndRun(
                Command::from_raw(":no_sort".to_string(), true),
            );
        }
        if key == key!('s') {
            return OverlayOutcome::CloseAndRun(
                Command::from_raw(":sort_by_size".to_string(), true),
            );
        }
        if key == key!('d') {
            return OverlayOutcome::CloseAndRun(
                Command::from_raw(":sort_by_date".to_string(), true),
            );
        }
        if key == key!('c') {
            return OverlayOutcome::CloseAndRun(
                Command::from_raw(":sort_by_count".to_string(), true),
            );
        }
        if key == key!('t') {
            return OverlayOutcome::CloseAndRun(
                Command::from_raw(":sort_by_type".to_string(), true),
            );
        }
        if key == key!('f') {
            return OverlayOutcome::CloseAndRun(
                Command::from_raw(":sort_by_type_dirs_first".to_string(), true),
            );
        }
        if key == key!('l') {
            return OverlayOutcome::CloseAndRun(
                Command::from_raw(":sort_by_type_dirs_last".to_string(), true),
            );
        }
        OverlayOutcome::Stay
    }

    fn handle_mouse(
        &mut self,
        _ev: MouseEvent,
    ) -> OverlayOutcome {
        // Mouse routing is intentionally not implemented — the overlay
        // is small enough that keyboard-only is the natural interaction
        // model, and there's no benefit to clickable rows beyond what
        // pressing the letter already provides.
        OverlayOutcome::Stay
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
            MouseButton,
            MouseEventKind,
        },
    };

    /// Extract the verb name from a `CloseAndRun` outcome. The
    /// `Command::from_raw(":verb", true)` shape parses to either a
    /// `VerbInvocate` (when the verb is recognised by name) or an
    /// `Internal` (when the parser maps the leading-`:` form to an
    /// internal directly); either way the verb name is what we want
    /// to assert against. We pull the name from `as_verb_invocation`,
    /// which is the public accessor that handles both shapes.
    fn close_and_run_verb_name(o: &OverlayOutcome) -> Option<String> {
        match o {
            OverlayOutcome::CloseAndRun(cmd) => {
                cmd.as_verb_invocation().map(|vi| vi.name.clone())
            }
            _ => None,
        }
    }

    // ---- letter -> verb mapping ----------------------------------------

    #[test]
    fn n_runs_no_sort() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('n'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
        assert_eq!(close_and_run_verb_name(&r).as_deref(), Some("no_sort"));
    }

    #[test]
    fn s_runs_sort_by_size() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('s'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
        assert_eq!(close_and_run_verb_name(&r).as_deref(), Some("sort_by_size"));
    }

    #[test]
    fn d_runs_sort_by_date() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('d'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
        assert_eq!(close_and_run_verb_name(&r).as_deref(), Some("sort_by_date"));
    }

    #[test]
    fn c_runs_sort_by_count() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('c'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
        assert_eq!(close_and_run_verb_name(&r).as_deref(), Some("sort_by_count"));
    }

    #[test]
    fn t_runs_sort_by_type() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('t'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
        assert_eq!(close_and_run_verb_name(&r).as_deref(), Some("sort_by_type"));
    }

    #[test]
    fn f_runs_sort_by_type_dirs_first() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('f'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
        assert_eq!(
            close_and_run_verb_name(&r).as_deref(),
            Some("sort_by_type_dirs_first"),
        );
    }

    #[test]
    fn l_runs_sort_by_type_dirs_last() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('l'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
        assert_eq!(
            close_and_run_verb_name(&r).as_deref(),
            Some("sort_by_type_dirs_last"),
        );
    }

    // ---- close keys -----------------------------------------------------

    #[test]
    fn esc_closes() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!(esc));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn q_closes() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('q'));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn ctrl_c_closes() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!(ctrl - c));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    // ---- unbound keys ---------------------------------------------------

    #[test]
    fn z_stays() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('z'));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn digit_stays() {
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!('1'));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn enter_stays() {
        // Enter is unbound — picking a sort requires the explicit
        // single-letter shortcut, not "the highlighted row" (no
        // highlight exists; the overlay is a flat menu).
        let mut o = SortOverlay::new();
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- mouse ----------------------------------------------------------

    #[test]
    fn mouse_click_is_ignored() {
        let mut o = SortOverlay::new();
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        let r = o.handle_mouse(ev);
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- render ---------------------------------------------------------

    #[test]
    fn render_writes_title_and_rows() {
        let o = SortOverlay::new();
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains("Sort by"), "missing title in render bytes: {s:?}");
        for (letter, label) in SORT_ROWS {
            assert!(
                s.contains(&format!("[{}] {}", letter, label)),
                "missing row for {letter:?} in render bytes",
            );
        }
    }

    #[test]
    fn render_too_small_is_noop() {
        let o = SortOverlay::new();
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        // Below the 8x5 bail-out threshold.
        let screen = Area::new(0, 0, 6, 4);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        assert!(!s.contains('╭'), "tiny terminal should bail before drawing frame");
    }

    /// Boundary test for the 8x5 bail-out. The bail-out fires when
    /// the computed centred-rect `area` is below 8 wide or 5 tall.
    /// `want_w` is floored at 40 and capped at 80% of `screen.width`,
    /// then `frame::centered_rect` clamps the result to `screen.width`.
    /// To produce an area that's exactly `area.width >= 8 &&
    /// area.height >= 5` we need a screen at least 10 wide (so
    /// `max_w = floor(10 * 8 / 10) = 8`) and at least 5 tall. Pin
    /// that boundary: the frame glyph must paint, proving the
    /// bail-out check uses strict `<` rather than `<=`.
    #[test]
    fn render_at_boundary_paints_frame() {
        let o = SortOverlay::new();
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 10, 5);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        assert!(
            s.contains('╭'),
            "10x5 must be above the bail-out threshold and paint the frame",
        );
    }

    /// When the screen is tall enough only for a strict subset of the
    /// SORT_ROWS, `render` truncates from the bottom: the visible body
    /// rows are the leading prefix of SORT_ROWS, in order, with the
    /// remainder dropped (no scroll, no overflow indicator). Pin the
    /// truncation order so a future "fit all rows by abbreviating
    /// labels" change can't silently swap the visible subset.
    ///
    /// `body_capacity = area.height - 3` (top frame + blank + bottom
    /// frame). For `area.height = 8` (clamped by screen.height), body
    /// capacity is 5 → rows 0..=4 (s, d, c, t, f) are visible; rows 5
    /// (`l`) and 6 (`n`) are cut.
    #[test]
    fn render_subset_when_height_limited() {
        let o = SortOverlay::new();
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 60, 8);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        // Leading prefix (5 rows) must appear.
        for (letter, label) in SORT_ROWS.iter().take(5) {
            let row = format!("[{}] {}", letter, label);
            assert!(s.contains(&row), "missing visible row {row:?}");
        }
        // Trailing rows (`l` and `n`) must be off-screen.
        assert!(
            !s.contains("[l] type, dirs last"),
            "`l` row should be off-screen when body capacity is 5",
        );
        assert!(
            !s.contains("[n] none"),
            "`n` row should be off-screen when body capacity is 5",
        );
    }
}
