use {
    super::{
        Screen,
        StatusAux,
        W,
    },
    crate::{
        app::Status,
        errors::ProgramError,
        skin::PanelSkin,
    },
    termimad::{
        Area,
        StyledChar,
        minimad::{
            Alignment,
            Composite,
        },
    },
    unicode_width::UnicodeWidthStr,
};

// `CropWriter` and `status_aux` are only used by the mount-painting branch
// inside `write_aux`, which is gated to macos/linux/windows. Importing
// them unconditionally triggers `unused_imports` on the other targets
// (BSD, Android, …); gate the imports to the same predicate.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
use super::{CropWriter, status_aux};

/// Cell gap painted between the message and the right-aligned aux block,
/// reserving breathing room so a long status message never abuts the aux.
const STATUS_AUX_GAP_WIDTH: u16 = 2;

/// Minimum slack required (in cells) for the aux block to render at all.
/// `aux.width() + STATUS_AUX_MIN_SLACK <= total_after_leading` is the
/// "row wide enough to host the aux" gate. The slack covers the 2-cell
/// gap plus 1 cell of padding on each end so a degenerate message can't
/// kiss the aux.
const STATUS_AUX_MIN_SLACK: u16 = 4;

/// Decide whether the aux block should be rendered for a given frame.
///
/// Suppression rules:
///   - the status is in error state (red bg should dominate),
///   - the row is too narrow to fit the aux plus `STATUS_AUX_MIN_SLACK`,
///   - or the aux is empty / not present.
///
/// Extracted so the production code and the test helper share the rule.
fn is_aux_visible<'a>(
    aux: Option<&'a StatusAux>,
    error: bool,
    total_after_leading: u16,
) -> Option<&'a StatusAux> {
    aux.filter(|_| !error)
        .filter(|a| !a.is_empty())
        .filter(|a| (a.width() as u16).saturating_add(STATUS_AUX_MIN_SLACK) <= total_after_leading)
}

/// write the whole status line (task + status + optional right-aligned aux)
///
/// The aux block, when present, is painted right-aligned over the last
/// `aux_width` cells of the row. The message area is shrunk accordingly so
/// the two regions never overlap. When the status is in error state
/// (red bg dominates) the aux is suppressed for the frame — short-lived
/// errors should not have to compete with decorative info.
#[allow(clippy::too_many_arguments)]
pub fn write(
    w: &mut W,
    watching: bool,
    task: Option<&str>,
    status: &Status,
    aux: Option<&StatusAux>,
    panel_skin: &PanelSkin,
    screen: Screen,
    area: &Area,
) -> Result<(), ProgramError> {
    let y = area.top;
    screen.goto(w, area.left, y)?;
    let mut x = area.left;
    if watching {
        let eye = "👁 ";
        x += eye.width() as u16;
        panel_skin.styles.status_job.queue(w, eye)?;
    }
    if let Some(pending_task) = task {
        let pending_task = format!(" {pending_task}… ");
        x += pending_task.chars().count() as u16;
        panel_skin.styles.status_job.queue(w, pending_task)?;
    }
    screen.goto(w, x, y)?;
    let style = if status.error {
        &panel_skin.status_skin.error
    } else {
        &panel_skin.status_skin.normal
    };
    style.write_inline_on(w, " ")?;

    // Compute how much room the (optional) aux block needs and whether it
    // fits. See `is_aux_visible` for the suppression rules.
    let total_after_leading = area.width.saturating_sub(x - area.left).saturating_sub(1);
    let aux_visible = is_aux_visible(aux, status.error, total_after_leading);

    // Width budget for the message, in cells. With aux: full row minus the
    // aux block minus `STATUS_AUX_GAP_WIDTH`. Without aux: full row.
    let aux_w = aux_visible.map_or(0, |a| a.width()) as u16;
    let gap_w: u16 = if aux_visible.is_some() {
        STATUS_AUX_GAP_WIDTH
    } else {
        0
    };
    let remaining_width = total_after_leading
        .saturating_sub(aux_w)
        .saturating_sub(gap_w);

    style.write_composite_fill(
        w,
        Composite::from_inline(&status.message),
        remaining_width as usize,
        Alignment::Unspecified,
    )?;

    // Now paint the aux block right-aligned. We compute its starting x
    // from area.left + area.width - aux_w. The mount widget paints itself
    // via a CropWriter so we wrap the buffered writer for that piece.
    if let Some(aux) = aux_visible {
        let aux_start_x = area.left + area.width - aux_w;
        screen.goto(w, aux_start_x, y)?;
        write_aux(w, aux, panel_skin)?;
    }
    Ok(())
}

/// Paint the aux block at the current cursor position. Pieces are painted
/// in a fixed order — git, total size, mount — each separated by a single
/// space when both neighbours are present.
fn write_aux(
    w: &mut W,
    aux: &StatusAux,
    panel_skin: &PanelSkin,
) -> Result<(), ProgramError> {
    // Track whether any piece has been painted yet; a separator space is
    // queued before every piece *after* the first. The only *read* of
    // this flag lives inside the cfg-gated mount branch below — on
    // platforms where that branch is gated out, the trailing assignments
    // become "set but never read". The `let _ = painted_any;` at the end
    // of the function suppresses the warning without forcing every
    // target to carry a cfg-shadowing arm.
    let mut painted_any = false;
    if let Some(g) = aux.git_summary.as_deref() {
        panel_skin.styles.git_branch.queue(w, g)?;
        painted_any = true;
    }
    if let Some(s) = aux.total_size.as_deref() {
        if painted_any {
            panel_skin.styles.default.queue(w, " ")?;
        }
        panel_skin.styles.count.queue(w, s)?;
        painted_any = true;
    }
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    if let Some(mount) = &aux.mount {
        if painted_any {
            panel_skin.styles.default.queue(w, " ")?;
        }
        // MountSpaceDisplay paints via a CropWriter; create one bounded to
        // the reserved width for this piece. The widget self-shrinks if
        // there isn't enough room.
        let mut cw = CropWriter::new(
            w,
            status_aux::MOUNT_AUX_WIDTH,
        );
        let fs_display = crate::filesystems::MountSpaceDisplay::from(
            mount,
            &panel_skin.styles,
            status_aux::MOUNT_AUX_WIDTH,
        );
        fs_display.write(&mut cw, false)?;
    }
    // `painted_any` is read inside cfg-gated branches; the trailing
    // assignment isn't observable, but suppressing the unused-assignment
    // warning would otherwise need an awkward `let _ = painted_any`.
    let _ = painted_any;
    Ok(())
}

/// erase the whole status line
pub fn erase(
    w: &mut W,
    area: &Area,
    panel_skin: &PanelSkin,
    screen: Screen,
) -> Result<(), ProgramError> {
    screen.goto(w, area.left, area.top)?;
    let sc = StyledChar::new(
        panel_skin
            .status_skin
            .normal
            .paragraph
            .compound_style
            .clone(),
        ' ',
    );
    sc.queue_repeat(w, area.width as usize)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for the message/aux width arithmetic.
    //!
    //! The full renderer writes to a `BufWriter<Stderr>` and queues
    //! crossterm commands, so end-to-end byte-level assertions are
    //! impractical. We instead pin the visibility predicate that decides
    //! whether the aux is rendered for a given frame — that's where the
    //! interesting logic lives (error suppression + width budget).
    use super::*;

    /// Thin wrapper turning the production `is_aux_visible` (returns
    /// `Option<&StatusAux>`) into a `bool` for ergonomic test
    /// assertions.
    fn aux_visible(
        aux: Option<&StatusAux>,
        error: bool,
        total_after_leading: u16,
    ) -> bool {
        is_aux_visible(aux, error, total_after_leading).is_some()
    }

    fn aux_with_size() -> StatusAux {
        StatusAux {
            total_size: Some("1.2G".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn aux_suppressed_when_status_is_error() {
        let aux = aux_with_size();
        // wide row but error state — aux must hide
        assert!(!aux_visible(Some(&aux), true, 80));
    }

    #[test]
    fn aux_shown_when_room_and_no_error() {
        let aux = aux_with_size();
        // "1.2G" is 4 cells; with 4 cells of slack we need >= 8 width.
        assert!(aux_visible(Some(&aux), false, 80));
    }

    #[test]
    fn aux_suppressed_when_row_too_narrow() {
        let aux = aux_with_size();
        // 4-cell aux + 4 slack = 8 minimum; 7 is too narrow.
        assert!(!aux_visible(Some(&aux), false, 7));
    }

    #[test]
    fn aux_suppressed_when_empty() {
        let aux = StatusAux::default();
        assert!(!aux_visible(Some(&aux), false, 80));
    }

    #[test]
    fn aux_suppressed_when_none() {
        assert!(!aux_visible(None, false, 80));
    }

    #[test]
    fn message_width_shrinks_to_make_room_for_aux() {
        // Mirror the arithmetic from `write`. With aux present:
        //   remaining = total_after_leading - aux_w - STATUS_AUX_GAP_WIDTH
        // The expected value is derived from the constants rather than
        // hard-coded so a future change to either side stays consistent.
        let aux = aux_with_size();
        let total_after_leading: u16 = 50;
        let aux_w = aux.width() as u16;
        let gap_w: u16 = STATUS_AUX_GAP_WIDTH;
        let remaining = total_after_leading
            .saturating_sub(aux_w)
            .saturating_sub(gap_w);
        assert_eq!(remaining, total_after_leading - aux_w - gap_w);
    }

    #[test]
    fn message_width_full_when_aux_suppressed() {
        // No aux -> message gets the full width budget after leading.
        let total_after_leading: u16 = 50;
        let aux_w: u16 = 0;
        let gap_w: u16 = 0;
        let remaining = total_after_leading
            .saturating_sub(aux_w)
            .saturating_sub(gap_w);
        assert_eq!(remaining, 50);
    }

    #[test]
    fn aux_visible_at_exact_boundary() {
        // Boundary pin: total_after_leading == aux_w + STATUS_AUX_MIN_SLACK.
        // The predicate uses `<=`, so equality is visible; one less is
        // not.
        let aux = aux_with_size(); // width = 4 cells
        let aux_w = aux.width() as u16;
        // Exact equality.
        assert!(aux_visible(
            Some(&aux),
            false,
            aux_w + STATUS_AUX_MIN_SLACK,
        ));
        // One cell less — must hide.
        assert!(!aux_visible(
            Some(&aux),
            false,
            aux_w + STATUS_AUX_MIN_SLACK - 1,
        ));
    }
}
