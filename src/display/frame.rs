//! Frame drawing helpers — rounded/square boxes with embedded titles.
//!
//! Pure helpers used by the panel render pass (in `display_panels`) and
//! later by the overlay layer. The `draw_frame` and `draw_frame_title`
//! functions are generic over the writer so they can be unit-tested
//! against an in-memory buffer.

use {
    crate::skin::StyleMap,
    crokey::crossterm::{
        QueueableCommand,
        cursor,
    },
    directories::UserDirs,
    std::{
        io::{
            self,
            Write,
        },
        path::Path,
    },
    termimad::Area,
    unicode_width::UnicodeWidthStr,
};

/// Characters used to draw a frame.
///
/// Field order: `corners = [top_left, top_right, bottom_left, bottom_right]`,
/// `h` is the horizontal edge, `v` is the vertical edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameStyle {
    pub(crate) corners: [char; 4],
    pub(crate) h: char,
    pub(crate) v: char,
}

impl FrameStyle {
    /// Rounded frame: `╭ ╮ ╰ ╯`, `─`, `│`.
    pub(crate) fn rounded() -> Self {
        Self {
            corners: ['╭', '╮', '╰', '╯'],
            h: '─',
            v: '│',
        }
    }

    /// Square frame: `┌ ┐ └ ┘`, `─`, `│`. Currently unused at runtime
    /// but retained as the documented alternative to `rounded`.
    #[allow(dead_code)]
    pub(crate) fn square() -> Self {
        Self {
            corners: ['┌', '┐', '└', '┘'],
            h: '─',
            v: '│',
        }
    }
}

/// Draw a frame around `area` using the given style.
///
/// Uses the `default` style from `palette` for the border characters
/// (the title — drawn separately by `draw_frame_title` — uses
/// `palette.frame_title`).
///
/// 0-width or 0-height (or smaller-than-2 in either dimension) areas are
/// silently skipped.
pub(crate) fn draw_frame<W: Write>(
    w: &mut W,
    area: Area,
    palette: &StyleMap,
    style: &FrameStyle,
) -> io::Result<()> {
    if area.width < 2 || area.height < 2 {
        return Ok(());
    }
    let style_ref = &palette.default;
    let left = area.left;
    let top = area.top;
    let right = area.left + area.width - 1;
    let bottom = area.top + area.height - 1;

    // top edge
    w.queue(cursor::MoveTo(left, top))?;
    style_ref
        .queue(w, style.corners[0])
        .map_err(io_err)?;
    for _ in (left + 1)..right {
        style_ref.queue(w, style.h).map_err(io_err)?;
    }
    style_ref
        .queue(w, style.corners[1])
        .map_err(io_err)?;

    // sides
    for y in (top + 1)..bottom {
        w.queue(cursor::MoveTo(left, y))?;
        style_ref.queue(w, style.v).map_err(io_err)?;
        w.queue(cursor::MoveTo(right, y))?;
        style_ref.queue(w, style.v).map_err(io_err)?;
    }

    // bottom edge
    w.queue(cursor::MoveTo(left, bottom))?;
    style_ref
        .queue(w, style.corners[2])
        .map_err(io_err)?;
    for _ in (left + 1)..right {
        style_ref.queue(w, style.h).map_err(io_err)?;
    }
    style_ref
        .queue(w, style.corners[3])
        .map_err(io_err)?;

    Ok(())
}

/// Write `title` into the top edge of `area`, with one space of padding
/// on each side, producing visually `╭─ <title> ─╮`.
///
/// The frame itself must be drawn separately (call `draw_frame` first or
/// after — this function only overwrites the slice in the top row).
///
/// If the title plus padding does not fit, the title is truncated to fit
/// inside `area.width - 4` columns (room for two corners + two spaces).
/// Empty title or area too narrow → no-op.
///
/// Uses `palette.frame_title` for styling (typically a bold accent).
pub(crate) fn draw_frame_title<W: Write>(
    w: &mut W,
    area: Area,
    palette: &StyleMap,
    title: &str,
) -> io::Result<()> {
    if title.is_empty() || area.width < 6 {
        return Ok(());
    }
    let style_ref = &palette.frame_title;
    let max_title_width = area.width.saturating_sub(4) as usize;
    let title = truncate_to_width(title, max_title_width);
    if title.is_empty() {
        return Ok(());
    }
    w.queue(cursor::MoveTo(area.left + 1, area.top))?;
    style_ref.queue(w, ' ').map_err(io_err)?;
    style_ref.queue_str(w, &title).map_err(io_err)?;
    style_ref.queue(w, ' ').map_err(io_err)?;
    Ok(())
}

/// Substitute `$HOME` → `~`, then if the result is wider than `max_w`
/// columns, truncate to `~/…/<basename>`. Returns an owned `String`.
///
/// Width is measured with `unicode_width`. If even the fallback
/// `~/…/<basename>` does not fit, returns just the basename, possibly
/// further truncated with `…` prefix.
pub(crate) fn path_label(
    path: &Path,
    max_w: u16,
) -> String {
    let max_w = max_w as usize;
    let full = path.to_string_lossy().to_string();
    let with_tilde = substitute_home(&full);
    if UnicodeWidthStr::width(with_tilde.as_str()) <= max_w {
        return with_tilde;
    }

    // Fallback: ~/…/basename
    let basename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| with_tilde.clone());
    let candidate = format!("~/…/{}", basename);
    if UnicodeWidthStr::width(candidate.as_str()) <= max_w {
        return candidate;
    }

    // Last resort: just the basename, possibly head-truncated with `…`
    if UnicodeWidthStr::width(basename.as_str()) <= max_w {
        return basename;
    }
    if max_w == 0 {
        return String::new();
    }
    if max_w == 1 {
        return "…".to_string();
    }
    // keep tail of basename, prepend `…`
    let budget = max_w - 1; // room for the leading `…`
    let tail = take_last_columns(&basename, budget);
    format!("…{}", tail)
}

/// Compute a centred sub-rectangle of `screen` with the given width and
/// height. If `w >= screen.width`, the result spans the full screen
/// width (clamped); same for height.
pub(crate) fn centered_rect(
    screen: Area,
    w: u16,
    h: u16,
) -> Area {
    let width = w.min(screen.width);
    let height = h.min(screen.height);
    let left = screen.left + (screen.width - width) / 2;
    let top = screen.top + (screen.height - height) / 2;
    Area::new(left, top, width, height)
}

// ---- private helpers ------------------------------------------------------

fn io_err(e: termimad::Error) -> io::Error {
    match e {
        termimad::Error::IO(io_e) => io_e,
        other => io::Error::other(other.to_string()),
    }
}

/// Substitute the user's home directory prefix with `~` (best-effort).
/// Falls through to the `HOME` env var when `directories` cannot supply a
/// home dir — and silently returns the input unchanged otherwise.
fn substitute_home(s: &str) -> String {
    match home_dir_string() {
        Some(home) => substitute_home_with(s, &home),
        None => s.to_string(),
    }
}

/// Pure-function variant of `substitute_home` that takes the home dir
/// directly, rather than resolving it from `directories::UserDirs` /
/// the `HOME` env var. Lets unit tests exercise the substitution rules
/// without mutating process-wide environment state.
fn substitute_home_with(
    s: &str,
    home: &str,
) -> String {
    if home.is_empty() {
        return s.to_string();
    }
    if s == home {
        return "~".to_string();
    }
    let with_sep_owned;
    let with_sep: &str = if home.ends_with('/') {
        home
    } else {
        with_sep_owned = format!("{home}/");
        &with_sep_owned
    };
    if let Some(rest) = s.strip_prefix(with_sep) {
        return format!("~/{rest}");
    }
    s.to_string()
}

fn home_dir_string() -> Option<String> {
    if let Some(d) = UserDirs::new() {
        return Some(d.home_dir().to_string_lossy().to_string());
    }
    std::env::var("HOME").ok()
}

/// Truncate `s` to fit in `max_w` display columns. If truncation occurs,
/// the result ends with `…`.
fn truncate_to_width(
    s: &str,
    max_w: usize,
) -> String {
    if max_w == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(s) <= max_w {
        return s.to_string();
    }
    if max_w == 1 {
        return "…".to_string();
    }
    let budget = max_w - 1;
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > budget {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

/// Return the suffix of `s` whose display width fits in `max_w` columns.
fn take_last_columns(
    s: &str,
    max_w: usize,
) -> String {
    if max_w == 0 {
        return String::new();
    }
    let mut chars: Vec<char> = s.chars().collect();
    let mut used = 0;
    let mut start = chars.len();
    while start > 0 {
        let ch = chars[start - 1];
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > max_w {
            break;
        }
        used += cw;
        start -= 1;
    }
    chars.drain(..start);
    chars.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // ---- FrameStyle ------------------------------------------------------

    #[test]
    fn rounded_charset() {
        let s = FrameStyle::rounded();
        assert_eq!(s.corners[0], '╭');
        assert_eq!(s.corners[1], '╮');
        assert_eq!(s.corners[2], '╰');
        assert_eq!(s.corners[3], '╯');
        assert_eq!(s.h, '─');
        assert_eq!(s.v, '│');
    }

    #[test]
    fn square_charset() {
        let s = FrameStyle::square();
        assert_eq!(s.corners[0], '┌');
        assert_eq!(s.corners[1], '┐');
        assert_eq!(s.corners[2], '└');
        assert_eq!(s.corners[3], '┘');
        assert_eq!(s.h, '─');
        assert_eq!(s.v, '│');
    }

    // ---- centered_rect ---------------------------------------------------

    #[test]
    fn centered_rect_even_dims() {
        let screen = Area::new(0, 0, 100, 50);
        let r = centered_rect(screen, 40, 20);
        assert_eq!(r.width, 40);
        assert_eq!(r.height, 20);
        assert_eq!(r.left, 30);
        assert_eq!(r.top, 15);
    }

    #[test]
    fn centered_rect_odd_dims() {
        let screen = Area::new(0, 0, 101, 51);
        let r = centered_rect(screen, 41, 21);
        assert_eq!(r.width, 41);
        assert_eq!(r.height, 21);
        // (101-41)/2 = 30, (51-21)/2 = 15
        assert_eq!(r.left, 30);
        assert_eq!(r.top, 15);
    }

    #[test]
    fn centered_rect_mixed_parity() {
        // even screen, odd popup
        let screen = Area::new(0, 0, 100, 50);
        let r = centered_rect(screen, 11, 9);
        assert_eq!(r.width, 11);
        assert_eq!(r.height, 9);
        // (100-11)/2 = 44, (50-9)/2 = 20
        assert_eq!(r.left, 44);
        assert_eq!(r.top, 20);
    }

    #[test]
    fn centered_rect_clamps_width() {
        let screen = Area::new(0, 0, 80, 24);
        let r = centered_rect(screen, 200, 10);
        assert_eq!(r.width, 80);
        assert_eq!(r.height, 10);
        assert_eq!(r.left, 0);
    }

    #[test]
    fn centered_rect_clamps_height() {
        let screen = Area::new(0, 0, 80, 24);
        let r = centered_rect(screen, 20, 200);
        assert_eq!(r.width, 20);
        assert_eq!(r.height, 24);
        assert_eq!(r.top, 0);
    }

    #[test]
    fn centered_rect_clamps_both() {
        let screen = Area::new(5, 5, 40, 20);
        let r = centered_rect(screen, 100, 100);
        assert_eq!(r.width, 40);
        assert_eq!(r.height, 20);
        assert_eq!(r.left, 5);
        assert_eq!(r.top, 5);
    }

    // ---- path_label ------------------------------------------------------

    #[test]
    fn path_label_short_path_unchanged() {
        let p = PathBuf::from("/tmp/x");
        let label = path_label(&p, 80);
        assert_eq!(label, "/tmp/x");
    }

    #[test]
    fn substitute_home_with_replaces_exact_match() {
        // Pure-function variant: no env-var mutation, no flakiness.
        assert_eq!(super::substitute_home_with("/home/me", "/home/me"), "~");
    }

    #[test]
    fn substitute_home_with_replaces_prefix() {
        assert_eq!(
            super::substitute_home_with("/home/me/projects/broot", "/home/me"),
            "~/projects/broot",
        );
    }

    #[test]
    fn substitute_home_with_handles_trailing_slash_home() {
        assert_eq!(
            super::substitute_home_with("/home/me/x", "/home/me/"),
            "~/x",
        );
    }

    #[test]
    fn substitute_home_with_returns_input_when_no_match() {
        assert_eq!(
            super::substitute_home_with("/var/log", "/home/me"),
            "/var/log",
        );
    }

    #[test]
    fn substitute_home_with_empty_home_is_identity() {
        // Defensive: an empty home string must not produce `~` for
        // every input.
        assert_eq!(
            super::substitute_home_with("/anything", ""),
            "/anything",
        );
    }

    #[test]
    fn path_label_does_not_widen_path() {
        // Width-bound assertion that does not depend on the actual
        // home directory: the path may or may not be substituted, but
        // it must stay within max_w columns.
        let p = PathBuf::from("/home/me/projects/broot/some/long/file.rs");
        let label = path_label(&p, 80);
        assert!(UnicodeWidthStr::width(label.as_str()) <= 80);
    }

    #[test]
    fn path_label_tail_truncated() {
        // A path certain to exceed max_w of 16. We cannot rely on the
        // user's actual home dir, so build a path likely outside it.
        let p = PathBuf::from("/var/some/very/deep/and/long/path/file.rs");
        let label = path_label(&p, 16);
        // expected fallback form: ~/…/file.rs (10 cols) — fits in 16
        assert!(
            UnicodeWidthStr::width(label.as_str()) <= 16,
            "label width {} exceeded max 16 ({})",
            UnicodeWidthStr::width(label.as_str()),
            label
        );
        assert!(
            label.ends_with("file.rs"),
            "label should end with basename, got: {label}"
        );
        // Either the fallback form (~/…/file.rs) or just the basename.
        assert!(
            label.contains("…") || label == "file.rs",
            "label should contain ellipsis or be basename, got: {label}"
        );
    }

    #[test]
    fn path_label_basename_only_tiny_max_w() {
        // max_w smaller than even ~/…/<basename>, so we fall through to
        // basename or head-truncated basename.
        let p = PathBuf::from("/var/some/very/deep/and/long/path/extremely_long_filename.rs");
        let label = path_label(&p, 8);
        let w = UnicodeWidthStr::width(label.as_str());
        assert!(
            w <= 8,
            "label width {} exceeded max 8 ({})",
            w,
            label
        );
    }

    #[test]
    fn path_label_max_w_zero() {
        let p = PathBuf::from("/some/long/path/here.rs");
        let label = path_label(&p, 0);
        assert_eq!(UnicodeWidthStr::width(label.as_str()), 0);
    }

    #[test]
    fn path_label_max_w_one() {
        let p = PathBuf::from("/some/long/path/file_with_a_long_name.rs");
        let label = path_label(&p, 1);
        // Single column → "…"
        let w = UnicodeWidthStr::width(label.as_str());
        assert!(w <= 1);
    }

    // ---- draw_frame / draw_frame_title (rendering smoke) ----------------

    #[test]
    fn draw_frame_writes_corners_into_buffer() {
        let palette = StyleMap::no_term();
        let mut buf: Vec<u8> = Vec::new();
        let area = Area::new(0, 0, 10, 4);
        draw_frame(&mut buf, area, &palette, &FrameStyle::rounded()).unwrap();
        let s = String::from_utf8(buf).expect("frame output should be valid UTF-8");
        assert!(s.contains('╭'), "missing top-left corner in: {:?}", s);
        assert!(s.contains('╮'), "missing top-right corner in: {:?}", s);
        assert!(s.contains('╰'), "missing bottom-left corner in: {:?}", s);
        assert!(s.contains('╯'), "missing bottom-right corner in: {:?}", s);
        assert!(s.contains('─'), "missing horizontal edge in: {:?}", s);
        assert!(s.contains('│'), "missing vertical edge in: {:?}", s);
    }

    #[test]
    fn draw_frame_skips_zero_area() {
        let palette = StyleMap::no_term();
        let mut buf: Vec<u8> = Vec::new();
        // 1x1 — too small, must not write any drawing characters
        let area = Area::new(0, 0, 1, 1);
        draw_frame(&mut buf, area, &palette, &FrameStyle::rounded()).unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains('╭'));
        assert!(!s.contains('╯'));
    }

    #[test]
    fn draw_frame_title_writes_title_into_buffer() {
        let palette = StyleMap::no_term();
        let mut buf: Vec<u8> = Vec::new();
        let area = Area::new(0, 0, 30, 4);
        draw_frame_title(&mut buf, area, &palette, "hello").unwrap();
        let s = String::from_utf8(buf).expect("title output should be valid UTF-8");
        assert!(s.contains("hello"), "missing title in: {:?}", s);
    }

    #[test]
    fn draw_frame_title_skips_empty_or_narrow() {
        let palette = StyleMap::no_term();
        // Empty title → nothing visible
        let mut buf: Vec<u8> = Vec::new();
        let area = Area::new(0, 0, 30, 4);
        draw_frame_title(&mut buf, area, &palette, "").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains("hello"));

        // Width < 6 → nothing visible
        let mut buf: Vec<u8> = Vec::new();
        let area = Area::new(0, 0, 5, 4);
        draw_frame_title(&mut buf, area, &palette, "hello").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(!s.contains("hello"));
    }

    #[test]
    fn draw_frame_then_title_combined() {
        // Smoke test: drawing a frame then a title produces a buffer
        // that has both the corner glyphs and the title text.
        let palette = StyleMap::no_term();
        let mut buf: Vec<u8> = Vec::new();
        let area = Area::new(0, 0, 20, 5);
        draw_frame(&mut buf, area.clone(), &palette, &FrameStyle::rounded()).unwrap();
        draw_frame_title(&mut buf, area, &palette, "/tmp/x").unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.contains('╭'));
        assert!(s.contains('╰'));
        assert!(s.contains("/tmp/x"), "title missing in: {:?}", s);
    }
}
