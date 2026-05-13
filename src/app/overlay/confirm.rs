//! Yes/no confirmation overlay.
//!
//! `ConfirmOverlay` is a generic, reusable modal that asks the user to
//! confirm or cancel a pending [`Command`]. It is the foundation for
//! destructive-verb prompts (rm/trash), mv/cp overwrite, and bulk
//! staging confirmation.
//!
//! Layout (centred on the screen):
//!
//! ```text
//! ╭─ <title> ─────────────────────╮
//! │ <body line 1>                 │
//! │ <body line 2>                 │
//! │ …                             │
//! │     [ <confirm> ]   [ Cancel ]     │
//! ╰───────────────────────────────╯
//! ```
//!
//! The action button sits on the left, `Cancel` on the right, and the
//! pair is centred horizontally. Focus defaults to the action button —
//! `Enter` commits it. The safe-exit keys (`n`, `Esc`, `Ctrl-C`) stay
//! independent of focus; `y` likewise always confirms. `Tab`, `←`,
//! and `→` toggle focus. `↑` / `↓` scroll the body when it overflows.

use {
    super::{
        ButtonHits,
        CellGetCloned,
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
            event::{
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
    unicode_width::UnicodeWidthStr,
};

/// Which button currently has keyboard focus.
///
/// Crate-internal: the overlay's focus state is an implementation
/// detail of the overlay layer, not part of the public surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmFocus {
    Cancel,
    Confirm,
}

/// Cells of horizontal space between the action and Cancel buttons when
/// they share the bottom row. The button-row geometry centres
/// `action + GAP + cancel` inside the frame; on pathologically narrow
/// modals the gap collapses to 1.
const BUTTON_GAP: u16 = 3;

/// A yes/no confirmation modal.
///
/// `pending` is the command that fires when the user confirms;
/// `body` is the affected paths or any other context lines (vertical
/// scrollable when there are more than fit). `confirm_label` lets the
/// caller pick the verb shown on the confirm button (e.g. "Delete",
/// "Trash", "Overwrite"). `danger` switches the confirm button to a
/// red/error palette.
pub struct ConfirmOverlay {
    pub(crate) title: String,
    pub(crate) body: Vec<String>,
    pub(crate) confirm_label: String,
    pub(crate) danger: bool,
    pub(crate) pending: Command,
    pub(crate) focus: ConfirmFocus,
    pub(crate) scroll: u16,
    /// Hit-rects for mouse routing — populated by `render` so
    /// `handle_mouse` can test clicks against them. `Cell` is enough
    /// because the value is `Copy`-able as a small struct.
    button_hits: Cell<Option<ButtonHits>>,
    /// Most-recent modal `Area` from `render`. Tests rely on this to
    /// pin modal width independently of where the buttons land —
    /// the centred-group layout makes button positions symmetric
    /// around the screen centre, so they alone don't distinguish
    /// width 40 from width 64 on an 80-col screen.
    #[cfg(test)]
    last_area: Cell<Option<Area>>,
}

impl ConfirmOverlay {
    /// Build a `ConfirmOverlay` with sensible defaults: focus on the
    /// action button (`Confirm`), scroll at 0, no cached hit-rects.
    pub fn new(
        title: impl Into<String>,
        body: Vec<String>,
        confirm_label: impl Into<String>,
        danger: bool,
        pending: Command,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            confirm_label: confirm_label.into(),
            danger,
            pending,
            focus: ConfirmFocus::Confirm,
            scroll: 0,
            button_hits: Cell::new(None),
            #[cfg(test)]
            last_area: Cell::new(None),
        }
    }

    /// Visible body height in rows for the given outer area, accounting
    /// for the 4 rows of chrome (top frame, bottom frame, button row,
    /// separator above buttons).
    fn visible_body_rows(area: Area) -> u16 {
        area.height.saturating_sub(4)
    }

    /// Maximum legal `scroll` value so the last visible row still
    /// shows real content rather than blank.
    fn max_scroll(
        body_len: usize,
        visible: u16,
    ) -> u16 {
        let body_len = body_len as u32;
        let visible = visible as u32;
        if body_len <= visible {
            0
        } else {
            (body_len - visible) as u16
        }
    }
}

// =============================================================================
// OverlayState impl
// =============================================================================

impl OverlayState for ConfirmOverlay {
    fn render<Wr: Write>(
        &self,
        w: &mut Wr,
        screen: Area,
        palette: &StyleMap,
    ) -> io::Result<()> {
        // ---- geometry ---------------------------------------------------
        //
        // Order:
        //   1. compute want_w from body line widths, title width, and
        //      button row width (each + 4-cell horizontal padding);
        //   2. derive inner_width from want_w (post-cap);
        //   3. soft-wrap the body using inner_width — rendered.len() may
        //      exceed self.body.len();
        //   4. derive want_h from the POST-WRAP rendered count so the
        //      modal grows to fit wrapped lines.
        let cancel_text = " [ Cancel ] ";
        let confirm_text = format!(" [ {} ] ", self.confirm_label);

        let max_w = (screen.width as u32 * 8 / 10) as u16;
        let saturating_to_u16 = |n: usize| -> u16 { n.min(u16::MAX as usize) as u16 };
        let content_w = saturating_to_u16(
            self.body
                .iter()
                .map(|l| UnicodeWidthStr::width(l.as_str()))
                .max()
                .unwrap_or(0),
        )
        .saturating_add(4);
        let title_w = saturating_to_u16(UnicodeWidthStr::width(self.title.as_str()))
            .saturating_add(4);
        let buttons_w = saturating_to_u16(
            UnicodeWidthStr::width(cancel_text) + UnicodeWidthStr::width(confirm_text.as_str()),
        )
        .saturating_add(BUTTON_GAP)
        .saturating_add(2);
        let want_w = content_w.max(title_w).max(buttons_w).max(40).min(max_w);

        // Post-wrap row count drives want_h. We need a provisional area
        // width to compute inner_width — centered_rect clamps to screen
        // width, so use min(want_w, screen.width).
        let provisional_w = want_w.min(screen.width);
        let inner_width = provisional_w.saturating_sub(4) as usize;
        let rendered: Vec<String> = self
            .body
            .iter()
            .flat_map(|l| wrap_diff_line(l, inner_width))
            .collect();
        let want_h: u16 = saturating_to_u16(rendered.len())
            .saturating_add(5)
            .min(15);

        let area = frame::centered_rect(screen, want_w, want_h);
        if area.width < 8 || area.height < 5 {
            // Too small to draw anything sensible — bail. This avoids
            // a corrupted modal on tiny terminals.
            return Ok(());
        }
        #[cfg(test)]
        self.last_area.set(Some(area.clone()));

        // ---- background clear ------------------------------------------
        // Paint spaces row-by-row so we don't depend on `Clear` (which
        // resets attributes globally). Use the default style so the
        // modal sits on a clean canvas.
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
        frame::draw_frame_title(w, area.clone(), palette, &self.title, false)?;

        // ---- body -------------------------------------------------------
        // `rendered` and `inner_width` were computed in the geometry block
        // above (we need rendered.len() to size want_h). Re-use them here.
        let visible = Self::visible_body_rows(area.clone());
        let max_scroll = Self::max_scroll(rendered.len(), visible);
        let scroll = self.scroll.min(max_scroll) as usize;
        let body_top = area.top + 1;
        let inner_left = area.left + 2;
        let body_style = &palette.default;

        let total = rendered.len();
        let visible_usize = visible as usize;
        let last_visible_idx = scroll + visible_usize;
        let overflow = total > last_visible_idx;

        for row in 0..visible {
            let body_idx = scroll + row as usize;
            if body_idx >= total {
                break;
            }
            w.queue(cursor::MoveTo(inner_left, body_top + row))?;
            let line = &rendered[body_idx];
            // Truncate the line to the available width. Append `…` only
            // when ALL THREE hold: rendered count overflows the visible
            // window (`overflow`), this is the last paintable row in the
            // window (`is_last_visible`), AND there is still un-rendered
            // body after this row (`body_idx + 1 < total`).
            let is_last_visible = row + 1 == visible || body_idx + 1 == total;
            let display = if overflow && is_last_visible && body_idx + 1 < total {
                truncate_with_ellipsis(line, inner_width.saturating_sub(2))
            } else {
                truncate_to_width(line, inner_width)
            };
            body_style.queue_str(w, display).map_err(io_err)?;
        }

        // ---- button row -------------------------------------------------
        // Row sits at area.top + area.height - 2 (one above the bottom
        // border). `cancel_text` and `confirm_text` were already bound in
        // the geometry block above and are reused for both measurement
        // and paint.
        //
        // Layout: action button on the left, Cancel on the right,
        // separated by `BUTTON_GAP` cells, with the whole group centred
        // inside the frame. Display widths use `UnicodeWidthStr` (NOT
        // `chars().count()`) so a CJK confirm label like `削除` measures
        // as 4 cells, not 2 chars — mis-measuring here both clips the
        // painted label and leaves the cached `ButtonHits` rect out of
        // sync with the glyphs the user sees, so mouse routing would
        // land on the wrong hit.
        let button_row_y = area.top + area.height - 2;
        let confirm_w_natural = UnicodeWidthStr::width(confirm_text.as_str()) as u16;
        let cancel_w_natural = UnicodeWidthStr::width(cancel_text) as u16;
        // 1-cell margin inside the frame on each side.
        let avail = area.width.saturating_sub(2);
        let natural = confirm_w_natural
            .saturating_add(BUTTON_GAP)
            .saturating_add(cancel_w_natural);
        let (confirm_w, gap, cancel_w) = if natural <= avail {
            (confirm_w_natural, BUTTON_GAP, cancel_w_natural)
        } else {
            // Pathologically narrow modal: drop to a 1-cell gap and share
            // the remaining width between the two buttons.
            let rem = avail.saturating_sub(1);
            let cw = confirm_w_natural.min(rem / 2 + rem % 2);
            let kw = cancel_w_natural.min(rem.saturating_sub(cw));
            (cw, 1, kw)
        };
        let group_w = confirm_w + gap + cancel_w;
        let confirm_x = area.left + (area.width.saturating_sub(group_w) / 2);
        let cancel_x = confirm_x + confirm_w + gap;

        let cancel_focused = matches!(self.focus, ConfirmFocus::Cancel);
        let confirm_focused = matches!(self.focus, ConfirmFocus::Confirm);

        let cancel_style: &CompoundStyle = if cancel_focused {
            &palette.selected_line
        } else {
            &palette.default
        };
        let confirm_style: &CompoundStyle = if self.danger {
            // Danger: prefer file_error (red); when focused, layer with
            // selected_line for a visible "pressed" state.
            if confirm_focused {
                &palette.status_error
            } else {
                &palette.file_error
            }
        } else if confirm_focused {
            &palette.selected_line
        } else {
            &palette.default
        };

        w.queue(cursor::MoveTo(cancel_x, button_row_y))?;
        cancel_style
            .queue_str(w, truncate_to_width(cancel_text, cancel_w as usize))
            .map_err(io_err)?;

        w.queue(cursor::MoveTo(confirm_x, button_row_y))?;
        confirm_style
            .queue_str(w, truncate_to_width(&confirm_text, confirm_w as usize))
            .map_err(io_err)?;

        // Cache hit-rects.
        let cancel_rect = Area::new(cancel_x, button_row_y, cancel_w, 1);
        let confirm_rect = Area::new(confirm_x, button_row_y, confirm_w, 1);
        self.button_hits.set(Some(ButtonHits {
            cancel: cancel_rect,
            confirm: confirm_rect,
        }));

        Ok(())
    }

    fn handle_key(
        &mut self,
        key: KeyCombination,
    ) -> OverlayOutcome {
        // Cancel-class keys.
        if key == key!(esc) || key == key!(ctrl - c) {
            return OverlayOutcome::Close;
        }
        if key == key!('n') || key == key!(shift - 'n') {
            return OverlayOutcome::Close;
        }

        // Direct-confirm.
        if key == key!('y') || key == key!(shift - 'y') {
            return OverlayOutcome::CloseAndRun(self.pending.clone());
        }

        // Focus toggle.
        if key == key!(tab) || key == key!(left) || key == key!(right) {
            self.focus = match self.focus {
                ConfirmFocus::Cancel => ConfirmFocus::Confirm,
                ConfirmFocus::Confirm => ConfirmFocus::Cancel,
            };
            return OverlayOutcome::Stay;
        }

        // Enter commits the focused button.
        if key == key!(enter) {
            return match self.focus {
                ConfirmFocus::Confirm => OverlayOutcome::CloseAndRun(self.pending.clone()),
                ConfirmFocus::Cancel => OverlayOutcome::Close,
            };
        }

        // Body scroll. We don't know the visible-row count or the
        // post-wrap rendered-row count without a render area, so we
        // clamp against an upper bound (`body.len() * 2`, since
        // `wrap_diff_line` produces at most 2 rows per input). `render`
        // re-clamps precisely against `rendered.len()`.
        if key == key!(down) {
            let max = self
                .body
                .len()
                .saturating_mul(2)
                .saturating_sub(1)
                .min(u16::MAX as usize) as u16;
            if self.scroll < max {
                self.scroll += 1;
            }
            return OverlayOutcome::Stay;
        }
        if key == key!(up) {
            self.scroll = self.scroll.saturating_sub(1);
            return OverlayOutcome::Stay;
        }

        // Anything else: consumed silently.
        OverlayOutcome::Stay
    }

    fn handle_mouse(
        &mut self,
        ev: MouseEvent,
    ) -> OverlayOutcome {
        if !matches!(ev.kind, MouseEventKind::Down(MouseButton::Left)) {
            return OverlayOutcome::Stay;
        }
        let Some(hits) = self.button_hits.get_cloned() else {
            return OverlayOutcome::Stay;
        };
        if hits.confirm.contains(ev.column, ev.row) {
            return OverlayOutcome::CloseAndRun(self.pending.clone());
        }
        if hits.cancel.contains(ev.column, ev.row) {
            return OverlayOutcome::Close;
        }
        OverlayOutcome::Stay
    }
}

// =============================================================================
// helpers
// =============================================================================

fn truncate_with_ellipsis(
    s: &str,
    max_w: usize,
) -> String {
    // Leave at least 2 cols for " …" suffix.
    if max_w < 2 {
        return truncate_to_width(s, max_w);
    }
    let body = truncate_to_width(s, max_w.saturating_sub(2));
    format!("{body} …")
}

/// Soft-wrap a `from → to` diff line that overflows `inner_width`.
/// Indent details documented in CLAUDE.md.
fn wrap_diff_line(line: &str, inner_width: usize) -> Vec<String> {
    if UnicodeWidthStr::width(line) <= inner_width {
        return vec![line.to_string()];
    }
    let Some(byte_idx) = line.find(" → ") else {
        return vec![line.to_string()];
    };
    let (from_part, rest) = line.split_at(byte_idx);
    // `rest` begins with " → "; strip the leading space to land "→ to".
    let to_part = rest.trim_start();
    // Continuation indent aligns the new `→` one column to the right of
    // the original arrow (`from_width + 1` — under the space, NOT
    // directly under the arrow). Capped at 10 cols to limit wasted
    // left margin. Collapses to 0 when the indented continuation would
    // itself overflow `inner_width` — visibility of the new path beats
    // column alignment.
    let preferred_indent = (UnicodeWidthStr::width(from_part) + 1).min(10);
    let to_width = UnicodeWidthStr::width(to_part);
    let indent_len = if preferred_indent + to_width <= inner_width {
        preferred_indent
    } else {
        0
    };
    let indent = " ".repeat(indent_len);
    vec![from_part.to_string(), format!("{indent}{to_part}")]
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use {
        super::*,
        crokey::crossterm::event::KeyModifiers,
    };

    fn cmd() -> Command {
        Command::from_raw(":help".to_string(), true)
    }

    fn make(
        body: Vec<&str>,
        danger: bool,
    ) -> ConfirmOverlay {
        ConfirmOverlay::new(
            "Delete file?",
            body.into_iter().map(String::from).collect(),
            "Delete",
            danger,
            cmd(),
        )
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

    fn render_capture(o: &ConfirmOverlay, screen: Area) -> String {
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        o.render(&mut wbuf, screen, &palette).unwrap();
        String::from_utf8_lossy(wbuf.buffer()).into_owned()
    }

    // ---- key dispatch ---------------------------------------------------

    #[test]
    fn tab_toggles_focus() {
        let mut o = make(vec!["/tmp/x"], false);
        assert_eq!(o.focus, ConfirmFocus::Confirm);
        let r = o.handle_key(key!(tab));
        assert!(matches!(r, OverlayOutcome::Stay));
        assert_eq!(o.focus, ConfirmFocus::Cancel);
        let _ = o.handle_key(key!(tab));
        assert_eq!(o.focus, ConfirmFocus::Confirm);
    }

    #[test]
    fn arrow_keys_toggle_focus() {
        let mut o = make(vec!["/tmp/x"], false);
        assert_eq!(o.focus, ConfirmFocus::Confirm);
        let _ = o.handle_key(key!(right));
        assert_eq!(o.focus, ConfirmFocus::Cancel);
        let _ = o.handle_key(key!(left));
        assert_eq!(o.focus, ConfirmFocus::Confirm);
    }

    #[test]
    fn enter_on_cancel_closes() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Cancel;
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn enter_on_confirm_runs_command() {
        let mut o = make(vec!["/tmp/x"], false);
        assert_eq!(o.focus, ConfirmFocus::Confirm);
        let r = o.handle_key(key!(enter));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn y_always_confirms() {
        let mut o = make(vec!["/tmp/x"], false);
        // even when focus is forced onto Cancel
        o.focus = ConfirmFocus::Cancel;
        let r = o.handle_key(key!('y'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn shift_y_also_confirms() {
        let mut o = make(vec!["/tmp/x"], false);
        let r = o.handle_key(key!(shift - 'y'));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn n_always_cancels() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Confirm;
        let r = o.handle_key(key!('n'));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn esc_cancels() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Confirm;
        let r = o.handle_key(key!(esc));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn ctrl_c_cancels() {
        let mut o = make(vec!["/tmp/x"], false);
        o.focus = ConfirmFocus::Confirm;
        let r = o.handle_key(key!(ctrl - c));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn down_scrolls_when_body_overflows() {
        let body: Vec<&str> = (0..20).map(|_| "row").collect();
        let mut o = make(body, false);
        assert_eq!(o.scroll, 0);
        let _ = o.handle_key(key!(down));
        assert_eq!(o.scroll, 1);
    }

    #[test]
    fn render_after_scroll_renders_later_body_lines() {
        // Drive the real render path with a body that overflows the
        // visible window, advance scroll past the first row, and assert
        // the captured bytes contain a body line that only appears
        // *after* scrolling. `BufWriter::buffer()` lets us inspect
        // the unflushed bytes — this is the actual render output, not
        // a sidecar buffer.
        let body: Vec<String> = (0..30).map(|i| format!("UNIQUE_LINE_{i}")).collect();
        let mut o = ConfirmOverlay::new("title", body, "OK", false, cmd());
        // Scroll past the first 10 rows.
        for _ in 0..15 {
            let _ = o.handle_key(key!(down));
        }
        let palette = StyleMap::no_term();
        let mut wbuf =
            std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        // Compute the post-scroll visible window explicitly so the
        // assertion fails loudly if the geometry changes:
        //   want_h = body.len()+5 capped at 15 → 15
        //   visible_body_rows = 15 - 4 = 11
        //   scroll = 15 (15 ↓ presses, clamped to max_scroll = 30-11 = 19)
        //   visible range = [15, 26).
        //
        // The last visible row can be ellipsis-truncated when the body
        // overflows ("UNIQUE_LINE_25 …"). To keep the assertion
        // robust we exclude index 25 — the line immediately before
        // (index 24) is rendered in full, which is enough to prove
        // scrolling actually happened.
        //
        // Tightening: a previous version of this test allowed any
        // index in `[15, 25)`, but UNIQUE_LINE_15 ALSO appears at
        // scroll = 5..15, so a partial-scroll regression (e.g.
        // `min(scroll, body.len()/2)`) would silently pass. Pin both:
        //
        //   * `UNIQUE_LINE_24` is in the rendered window — proves the
        //     scroll reached at least 14 (window includes [scroll,
        //     scroll+11) and 24 first becomes visible at scroll=14).
        //   * `UNIQUE_LINE_5` is NOT in the rendered window — proves
        //     the scroll exceeded 5 (a regression that clamped at
        //     scroll=5 would leak _5 into the visible bytes).
        //
        // Together these straddle the intended scroll position of 15
        // with a one-step tolerance on each side.
        assert!(
            s.contains("UNIQUE_LINE_24"),
            "expected UNIQUE_LINE_24 in render at scroll=15 — \
             scroll did not advance far enough: {s:?}",
        );
        assert!(
            !s.contains("UNIQUE_LINE_5"),
            "UNIQUE_LINE_5 must not be visible at scroll=15 — a \
             partial-scroll regression is leaking earlier rows: {s:?}",
        );
    }

    #[test]
    fn up_clamps_at_zero() {
        let mut o = make(vec!["a", "b"], false);
        assert_eq!(o.scroll, 0);
        let _ = o.handle_key(key!(up));
        assert_eq!(o.scroll, 0);
    }

    #[test]
    fn down_clamps_at_body_end() {
        // Body has 3 rows. `handle_key(down)` clamps against the
        // upper bound `body.len() * 2 - 1` = 5 (room for wrap), since
        // it has no render-area info. `render` re-clamps precisely.
        let mut o = make(vec!["a", "b", "c"], false);
        for _ in 0..10 {
            let _ = o.handle_key(key!(down));
        }
        assert!(o.scroll <= 5, "scroll {} exceeded upper-bound clamp", o.scroll);
    }

    #[test]
    fn unrecognized_key_is_consumed_silently() {
        let mut o = make(vec!["/tmp/x"], false);
        let r = o.handle_key(key!('q'));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- mouse routing --------------------------------------------------

    #[test]
    fn mouse_on_confirm_runs_command() {
        // The trait `render` writes to a `BufWriter<Sink>` here so the
        // bytes are discarded entirely — tests don't observe them, and
        // cargo's stderr stays clean. We only need the *cached
        // hit-rects* the render path populates as a side effect.
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o
            .button_hits
            .get_cloned()
            .expect("hit-rects must be cached after render");
        let cx = hits.confirm.left + hits.confirm.width / 2;
        let cy = hits.confirm.top;
        let r = o.handle_mouse(mouse_at(cx, cy));
        assert!(matches!(r, OverlayOutcome::CloseAndRun(_)));
    }

    #[test]
    fn mouse_on_cancel_closes() {
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.button_hits.get_cloned().unwrap();
        let cx = hits.cancel.left + hits.cancel.width / 2;
        let cy = hits.cancel.top;
        let r = o.handle_mouse(mouse_at(cx, cy));
        assert!(matches!(r, OverlayOutcome::Close));
    }

    #[test]
    fn mouse_off_buttons_stays() {
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        // top-left corner of screen is well outside both button rects.
        let r = o.handle_mouse(mouse_at(0, 0));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn mouse_non_left_click_stays() {
        let mut o = make(vec!["/tmp/x"], false);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.button_hits.get_cloned().unwrap();
        let cx = hits.confirm.left + hits.confirm.width / 2;
        let cy = hits.confirm.top;
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: cx,
            row: cy,
            modifiers: KeyModifiers::NONE,
        };
        let r = o.handle_mouse(ev);
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    #[test]
    fn mouse_before_render_stays() {
        // Without a render call the hit-rect cache is empty; clicks
        // must be ignored gracefully (Stay), not panic or fire a
        // command. Pins the `Cell::take`-based cache invariant.
        let mut o = make(vec!["/tmp/x"], false);
        let r = o.handle_mouse(mouse_at(40, 12));
        assert!(matches!(r, OverlayOutcome::Stay));
    }

    // ---- render shape ---------------------------------------------------

    #[test]
    fn render_writes_corners_and_title_and_buttons() {
        // Drive the real `OverlayState::render` and capture its output
        // from the `BufWriter<Sink>` buffer *before* flushing. This is
        // the actual render path, not a sidecar buffer mimicking it.
        // The capture works because `BufWriter::buffer()` returns a
        // borrow of unflushed bytes — we simply skip the `flush` call.
        // Backing the buffer with `sink()` keeps cargo test's stderr
        // free of noise (unflushed bytes never reach a real fd).
        let palette = StyleMap::no_term();
        let o = make(vec!["/tmp/a", "/tmp/b"], false);
        // 64 KiB is comfortably larger than the 80x24 modal output, so
        // the buffer never auto-flushes during the render.
        let mut wbuf =
            std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let screen = Area::new(0, 0, 80, 24);
        o.render(&mut wbuf, screen, &palette).unwrap();
        let bytes = wbuf.buffer().to_vec();
        let s = String::from_utf8_lossy(&bytes);
        assert!(s.contains('╭'), "missing top-left corner in render bytes");
        assert!(s.contains('╰'), "missing bottom-left corner in render bytes");
        assert!(s.contains("Delete file?"), "missing title in render bytes: {s:?}");
        assert!(s.contains("Cancel"), "missing Cancel button label in render bytes");
        assert!(s.contains("Delete"), "missing Delete confirm label in render bytes");
    }

    // The previous `danger_render_differs_from_safe` test only asserted
    // on the `danger` flag (because `StyleMap::no_term` flattens the
    // palette to empty styles, the two renders are byte-identical). It
    // was removed because it does not actually exercise the styling
    // code path. The real render path is exercised by
    // `render_caches_button_hits` and the structural-pin behaviour
    // tests above.

    #[test]
    fn render_caches_button_hits() {
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let o = make(vec!["/tmp/a"], false);
        let screen = Area::new(0, 0, 80, 24);
        assert!(o.button_hits.get_cloned().is_none());
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o
            .button_hits
            .get_cloned()
            .expect("hits should be populated after render");
        // Both buttons must have non-zero width.
        assert!(hits.cancel.width > 0);
        assert!(hits.confirm.width > 0);
        // Action sits to the LEFT of Cancel; the two rects must not
        // overlap horizontally.
        let confirm_right = hits.confirm.left + hits.confirm.width;
        assert!(
            confirm_right <= hits.cancel.left,
            "confirm and cancel rects overlap or are mis-ordered: {hits:?}"
        );
    }

    #[test]
    fn render_clamps_oversize_body() {
        // 30 lines of body, screen 24 rows — height clamps to 15.
        let body: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let o = ConfirmOverlay::new("t", body, "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        // Should not panic, should populate hits.
        o.render(&mut wbuf, screen, &palette).unwrap();
        assert!(o.button_hits.get_cloned().is_some());
    }

    // ---- focus + layout pins -------------------------------------------

    #[test]
    fn default_focus_is_confirm() {
        // Pin: `ConfirmOverlay::new` must default focus to the action
        // button. Reverting to `ConfirmFocus::Cancel` would silently
        // change the Enter-on-default semantics for every caller
        // (destructive verbs included) — catch that here, not via
        // downstream behaviour tests.
        let o = make(vec!["/tmp/x"], false);
        assert_eq!(o.focus, ConfirmFocus::Confirm);
        let o_danger = make(vec!["/tmp/x"], true);
        assert_eq!(
            o_danger.focus,
            ConfirmFocus::Confirm,
            "danger overlays must also default to Confirm — the focus \
             default does not branch on the danger flag",
        );
    }

    #[test]
    fn render_centers_button_group() {
        // Pin the centred-group layout:
        //   * action button sits left of Cancel (confirm_x < cancel_x);
        //   * exactly BUTTON_GAP cells separate them;
        //   * the whole group is centred inside the frame, with the
        //     left and right margins differing by at most 1 cell
        //     (integer-division asymmetry on odd modal widths).
        //
        // Computation for an 80-col screen with "t" title, "OK" action,
        // and "a" body (floor width 40):
        //   area.left = 20, area.width = 40
        //   confirm_w = 8 (" [ OK ] "), cancel_w = 12 (" [ Cancel ] ")
        //   group_w = 8 + 3 + 12 = 23
        //   confirm_x = 20 + (40-23)/2 = 28
        //   cancel_x  = 28 + 8 + 3   = 39
        //   left margin  = 28 - 20 = 8
        //   right margin = (20+40) - (39+12) = 9
        let o = make(vec!["/tmp/x"], false);
        let screen = Area::new(0, 0, 80, 24);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        o.render(&mut wbuf, screen, &palette).unwrap();
        let hits = o.button_hits.get_cloned().expect("hits cached");
        let area = o.last_area.get_cloned().expect("area cached");
        assert!(
            hits.confirm.left < hits.cancel.left,
            "action must sit LEFT of Cancel: {hits:?}",
        );
        let gap = hits.cancel.left - (hits.confirm.left + hits.confirm.width);
        assert_eq!(
            gap, BUTTON_GAP,
            "expected {BUTTON_GAP}-cell gap between buttons; got {gap}",
        );
        let left_margin = hits.confirm.left - area.left;
        let right_margin =
            (area.left + area.width) - (hits.cancel.left + hits.cancel.width);
        let delta = left_margin.abs_diff(right_margin);
        assert!(
            delta <= 1,
            "button group must be centred; left_margin={left_margin}, \
             right_margin={right_margin}, delta={delta}",
        );
    }

    // ---- wrap_diff_line -------------------------------------------------

    #[test]
    fn wrap_diff_line_fits_returns_single() {
        let out = wrap_diff_line("a → b", 40);
        assert_eq!(out, vec!["a → b".to_string()]);
    }

    #[test]
    fn wrap_diff_line_splits_on_first_arrow() {
        // "long-from → long-to" — total 19 chars, inner_width 15 forces
        // wrap. Continuation "{10 spaces}→ long-to" = 19 chars > 15, so
        // adaptive indent collapses to 0 → "→ long-to". To exercise the
        // PREFERRED 10-col indent path, use a wider inner_width where the
        // indent still fits.
        let out = wrap_diff_line("long-from → long-to", 15);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "long-from");
        // 10-col indent (10) + "→ long-to" (9) = 19 > 15 → indent collapses.
        assert_eq!(out[1], "→ long-to");
    }

    #[test]
    fn wrap_diff_line_uses_preferred_indent_when_it_fits() {
        // Longer input so the line forces a wrap but the preferred 10-col
        // indent still fits the continuation:
        //   "long-from-name → long-to-name" = 29 chars (>25 → wrap)
        //   indent 10 + "→ long-to-name" (14) = 24 chars (≤25 → fits)
        // exercises the PREFERRED-indent branch of wrap_diff_line.
        let out = wrap_diff_line("long-from-name → long-to-name", 25);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "long-from-name");
        assert_eq!(out[1], "          → long-to-name");
    }

    #[test]
    fn wrap_diff_line_no_arrow_unchanged() {
        let out = wrap_diff_line("plain string with no arrow", 5);
        assert_eq!(out, vec!["plain string with no arrow".to_string()]);
    }

    #[test]
    fn wrap_diff_line_indent_clamps_to_10() {
        // "very-long-prefix-that-exceeds-10-cols → x" — arrow at col 38,
        // indent must clamp to 10.
        let out = wrap_diff_line("very-long-prefix-that-exceeds-10-cols → x", 20);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], "          → x");
    }

    #[test]
    fn wrap_diff_line_indent_uses_from_width_plus_one() {
        // Pin the +1 in `preferred_indent = (from_width + 1).min(10)`.
        //
        // Construction:
        //   from = "abcdefgh" (width 8), to_part = "→ y" (width 3).
        //   line_width = 8 + " → " (3) + 1 = 12.
        //   inner_width = 11 → line overflows by exactly 1 → wrap fires.
        //   With  +1: preferred_indent = 9 → 9 + 3 = 12 > 11 → COLLAPSES to 0.
        //   Without +1: preferred_indent = 8 → 8 + 3 = 11 ≤ 11 → stays as 8 spaces.
        // So this test FAILS if the +1 is dropped (`out[1]` would be
        // `"        → y"` instead of `"→ y"`).
        let out = wrap_diff_line("abcdefgh → y", 11);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "abcdefgh");
        assert_eq!(out[1], "→ y");
    }

    #[test]
    fn wrap_diff_line_only_first_arrow_splits() {
        // Two arrows in input — split on the FIRST.
        let out = wrap_diff_line("a → b → c", 5);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], "a");
        // Preferred 2-col indent + "→ b → c" (7) = 9 > 5 → collapses to 0.
        assert_eq!(out[1], "→ b → c");
    }

    // ---- dynamic width --------------------------------------------------

    #[test]
    fn render_width_grows_to_content() {
        // 60-char body line in an 80-col screen — content_w = 60 + 4 = 64,
        // capped at 80% of screen (64). Modal width must be EXACTLY 64.
        //
        // Pinned via `last_area.width` rather than button-coordinate math.
        // The centred-group layout makes confirm/cancel x-positions
        // symmetric around the screen centre, so on an 80-col screen the
        // button rects are the same for any modal width that fits the
        // group — they alone can't distinguish a 64-wide modal from the
        // 40-wide floor. `last_area` is the direct observable.
        let long = "a".repeat(60);
        let o = ConfirmOverlay::new("t", vec![long], "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        o.render(&mut wbuf, screen, &palette).unwrap();
        let area = o.last_area.get_cloned().expect("area cached");
        assert_eq!(
            area.width, 64,
            "expected modal width 64 (content-driven); got {} — \
             content-grow regression",
            area.width,
        );
        // Sanity: the full 60-char content line really lands in the body.
        let s = String::from_utf8_lossy(wbuf.buffer()).into_owned();
        assert!(s.contains(&"a".repeat(60)));
    }

    #[test]
    fn render_width_clamps_to_screen() {
        // 200-char line in an 80-col screen — modal width must NOT exceed
        // 80% of screen (64 cols), so inner width is ≤ 60. Verify by ensuring
        // a single body row does NOT contain 70+ consecutive 'x' chars.
        let long = "x".repeat(200);
        let o = ConfirmOverlay::new("t", vec![long.clone()], "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        let s = render_capture(&o, screen);
        assert!(
            !s.contains(&"x".repeat(70)),
            "body line must be clipped within the 80%-of-screen (64 col) cap",
        );
    }

    #[test]
    fn render_width_has_floor_for_short_body() {
        // Single 1-char body in an 80-col screen — modal width must be
        // EXACTLY 40 (the floor), inner width 36.
        //
        // Pinned via `last_area.width`. The button-coord-based assertion
        // used previously can't catch a floor regression under the
        // centred-group layout: confirm/cancel rects are symmetric
        // around the screen centre for any modal width that fits the
        // group, so a regression to width 25 (driven by buttons_w alone)
        // would produce IDENTICAL button positions and pass silently.
        // `last_area.width` is the unambiguous observable.
        let o = ConfirmOverlay::new("t", vec!["a".to_string()], "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        let palette = StyleMap::no_term();
        o.render(&mut wbuf, screen, &palette).unwrap();
        let area = o.last_area.get_cloned().expect("area cached");
        assert_eq!(
            area.width, 40,
            "expected modal width 40 (floor); got {} — floor regression",
            area.width,
        );
    }

    #[test]
    fn render_no_panic_at_tiny_screen() {
        // 20x10 screen — width calc must not panic. The guard
        // `area.width < 8 || area.height < 5` does NOT fire here
        // (clamped area is 20x10 → no bail), but a smaller screen
        // would. This pins the "no panic" property at a realistic
        // boundary; the actual guard firing is exercised in
        // `render_bails_at_truly_tiny_screen`.
        let o = ConfirmOverlay::new("t", vec!["a → b".to_string()], "OK", false, cmd());
        let screen = Area::new(0, 0, 20, 10);
        let _ = render_capture(&o, screen); // must not panic
    }

    #[test]
    fn render_bails_at_truly_tiny_screen() {
        // 4x3 screen — centered_rect clamps area to 4x3, the guard
        // `area.width < 8 || area.height < 5` fires, and render returns
        // Ok(()) without writing the frame. The button-hit cache must
        // therefore stay empty.
        let o = ConfirmOverlay::new("t", vec!["a → b".to_string()], "OK", false, cmd());
        let screen = Area::new(0, 0, 4, 3);
        let palette = StyleMap::no_term();
        let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
        o.render(&mut wbuf, screen, &palette).unwrap();
        assert!(
            o.button_hits.get_cloned().is_none(),
            "guard should fire — render must bail before caching hits",
        );
    }

    // ---- render wrap integration ----

    #[test]
    fn render_wraps_long_diff_line() {
        // Single rename line that exceeds the 80%-of-80 = 64 col cap.
        // After wrap, render bytes must contain BOTH the full `from` path
        // and the full `to` tail (proving nothing was silently truncated).
        let from = "/tmp/very-long-source-dir/quite-long-name.txt";
        let to = "/tmp/very-long-archive-dir/quite-long-name-renamed-v2.txt";
        let line = format!("{from} → {to}");
        let o = ConfirmOverlay::new("t", vec![line.clone()], "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        let s = render_capture(&o, screen);
        assert!(s.contains(from), "from path missing from render: {s:?}");
        assert!(
            s.contains("quite-long-name-renamed-v2.txt"),
            "to path tail missing — wrap did not happen: {s:?}",
        );
    }

    #[test]
    fn render_scroll_works_with_wrapped_lines() {
        // 11 distinct lines (N=11) where every line wraps to 2 rows →
        // rendered.len() = 22. visible = 11 (height capped at 15, body
        // rows = 15-4 = 11). render_max_scroll = 22-11 = 11.
        //
        // The OLD bug clamped `handle_key(down)` against `body.len()-1`
        // = 10, so the very last rendered row (the continuation of line
        // 10) was unreachable: scroll=10 shows rendered[10..21], but
        // rendered[21] requires scroll=11. The fix clamps against
        // `body.len()*2 - 1` = 21, so scroll can reach 11.
        //
        // Pin: tag each rename's TO path with a UNIQUE marker. Each line
        // is constructed so the continuation row's marker survives the
        // inner_width=60 truncate. The marker for line 10 lives in
        // rendered[21] and is ONLY visible when scroll reaches 11.
        let body: Vec<String> = (0..11)
            .map(|i| {
                // from_part width 35, to_part width 36 (incl "→ ").
                // continuation = 10-indent + to_part = 46 cols ≤ 60.
                format!(
                    "/tmp/source-dir/file-from-{i:02}-end.txt → \
                     /tmp/archive-dir/MARK{i:02}-end.txt"
                )
            })
            .collect();
        let mut o = ConfirmOverlay::new("t", body, "OK", false, cmd());
        for _ in 0..100 {
            let _ = o.handle_key(key!(down));
        }
        // With the fix, scroll reaches the render-side cap (11), so
        // scroll > 10. Old behaviour clamped at 10.
        assert!(
            o.scroll > 10,
            "scroll {} stuck at old clamp; expected > 10",
            o.scroll,
        );
        let screen = Area::new(0, 0, 80, 24);
        let s = render_capture(&o, screen);
        // The UNIQUE marker of the LAST rename's TO path (continuation)
        // must be in the render bytes — proves rendered[21] is visible.
        assert!(
            s.contains("MARK10"),
            "last continuation row missing from render — keyboard scroll \
             cannot reach the final wrapped row: {s:?}",
        );
    }

    #[test]
    fn render_height_grows_to_rendered_count_no_scroll_needed() {
        // Pin the geometry fix: `want_h` is sized from `rendered.len()`
        // (the post-wrap row count), NOT `self.body.len()`. With N=3
        // body lines that all wrap to 2 rows each:
        //
        //   * NEW formula: want_h = rendered.len()+5 = 6+5 = 11 (≤15).
        //     visible body rows = 11-4 = 7. max_scroll = max(0, 6-7) = 0.
        //     All 6 rendered rows are visible at scroll=0 — every line's
        //     continuation marker reaches the output.
        //
        //   * OLD formula: want_h = body.len()+5 = 3+5 = 8.
        //     visible = 4. max_scroll = 6-4 = 2. At scroll=0 only the
        //     first 4 rendered rows paint (lines 0-1 fully, line 2
        //     missing entirely). The MARK02 marker of line 2's
        //     continuation row would be absent from render bytes.
        //
        // So this test FAILS if `want_h` reverts to using `body.len()`.
        //
        // Each `from` is exactly 35 cols wide, `to` is 36 cols wide
        // (including the arrow): total line width = 35 + " → " (3) + 33
        // marker = 71 cols. With screen=80, max_w=64, want_w pegged at
        // 64, inner_width=60. 71 > 60 → wrap fires for every line.
        // Continuation (10-col indent + 33-col marker = 43 cols) ≤ 60 →
        // PREFERRED-indent branch keeps the marker visible.
        let body: Vec<String> = (0..3)
            .map(|i| {
                format!(
                    "/tmp/source-dir/file-from-{i:02}-end.txt → \
                     /tmp/archive-dir/MARK{i:02}-end.txt"
                )
            })
            .collect();
        let o = ConfirmOverlay::new("t", body, "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        let s = render_capture(&o, screen);
        // scroll=0 (default); every line's continuation marker must be
        // in the painted bytes. With the OLD formula MARK02 is missing.
        for i in 0..3 {
            let marker = format!("MARK{i:02}");
            assert!(
                s.contains(&marker),
                "marker {marker} missing at scroll=0 — modal height did \
                 not grow to fit wrapped rows (want_h must size from \
                 rendered.len(), not body.len()): {s:?}",
            );
        }
    }

    #[test]
    fn render_emits_ellipsis_when_body_overflows() {
        // Pin the overflow-ellipsis path: when the body is too tall for
        // the visible window AND there are more rendered rows beyond
        // the last paintable one, the last visible row must end with
        // `" …"`. Without this assertion the entire
        // `truncate_with_ellipsis` branch could be deleted and no test
        // would notice.
        //
        // 20 short single-row lines → rendered.len() = 20.
        // want_h = 20+5 = 25, capped at 15. visible = 11.
        // last_visible_idx (scroll=0) = 11; overflow = 20 > 11 → true.
        // The 11th row (body_idx=10) is the last visible AND
        // body_idx+1 = 11 < 20 → ellipsis branch fires.
        let body: Vec<String> = (0..20).map(|i| format!("row-{i:02}")).collect();
        let o = ConfirmOverlay::new("t", body, "OK", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        let s = render_capture(&o, screen);
        assert!(
            s.contains('…'),
            "ellipsis glyph missing — overflow-truncate branch did not \
             fire even though body (20 rows) overflows the 11-row \
             visible window: {s:?}",
        );
    }

    #[test]
    fn render_short_body_renders_all_lines() {
        // Backward-compat pin for non-rename callers (rm/trash, etc.):
        // a short body without arrows must reach the output unchanged.
        let body = vec!["/tmp/a".to_string(), "/tmp/b".to_string()];
        let o = ConfirmOverlay::new("Delete?", body, "Delete", false, cmd());
        let screen = Area::new(0, 0, 80, 24);
        let s = render_capture(&o, screen);
        assert!(s.contains("/tmp/a"), "first body line missing: {s:?}");
        assert!(s.contains("/tmp/b"), "second body line missing: {s:?}");
    }

}
