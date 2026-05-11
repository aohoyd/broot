//! Auxiliary status-row payload.
//!
//! The tree's root row used to carry three decorative pieces: a git
//! status summary, a total-size badge (when `show_sizes` was on), and a
//! mount-space progress bar (when `show_root_fs` was on). The root row is
//! now hidden from the body, so that aux info migrated to the right end
//! of the active panel's status row.
//!
//! `StatusAux` is the carrier type: every piece is optional, and the active
//! `PanelState` builds one (or returns `None`) on every render frame via
//! `PanelState::status_aux`. The status row paint code consumes it and:
//!   - paints the message left-aligned within `area.width - aux_width - 2`,
//!   - paints the aux right-aligned starting at `area.left + area.width - aux_width`,
//!   - suppresses the aux entirely when the status is in error state
//!     (red bg) or when there's not enough room.
//!
//! The struct owns its data (the git summary and total size are
//! pre-formatted strings, the mount is a cloned `lfs_core::Mount`). Status
//! rows repaint once per render frame, so the extra allocations are
//! immaterial and the ownership story stays simple.

use unicode_width::UnicodeWidthStr;

/// Optional pieces of aux info shown at the right end of the active panel's
/// status row.
#[derive(Debug, Default, Clone)]
pub struct StatusAux {
    /// Pre-formatted git status summary (e.g. `" main +5-2"`).
    /// Present when `tree.git_status` is computed.
    pub git_summary: Option<String>,
    /// Pre-formatted total tree size (e.g. `"1.2G"`).
    /// Present when `tree.options.show_sizes` and the root has a sum.
    pub total_size: Option<String>,
    /// A clone of the mount carrying `show_root_fs` widget data.
    /// `None` when the toggle is off or the platform doesn't support it.
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    pub mount: Option<lfs_core::Mount>,
}

impl StatusAux {
    /// `true` when none of the aux pieces are present — caller can skip
    /// the right-alignment math entirely.
    pub fn is_empty(&self) -> bool {
        let no_git = self.git_summary.is_none();
        let no_size = self.total_size.is_none();
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        let no_mount = self.mount.is_none();
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        let no_mount = true;
        no_git && no_size && no_mount
    }

    /// Estimated rendered width in cells. The two textual pieces use
    /// `unicode_width`; the mount widget is given a fixed budget
    /// (`MOUNT_AUX_WIDTH`) because it's a self-adapting widget. Pieces are
    /// separated by a single space, so we add 1 cell per gap between
    /// present pieces.
    pub fn width(&self) -> usize {
        // Accumulate width + piece count in a single pass; the gap budget
        // is `pieces.saturating_sub(1)` (one space between each adjacent
        // pair). No allocations.
        let mut pieces: usize = 0;
        let mut sum: usize = 0;
        if let Some(s) = self.git_summary.as_deref() {
            pieces += 1;
            sum += UnicodeWidthStr::width(s);
        }
        if let Some(s) = self.total_size.as_deref() {
            pieces += 1;
            sum += UnicodeWidthStr::width(s);
        }
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        if self.mount.is_some() {
            pieces += 1;
            sum += MOUNT_AUX_WIDTH;
        }
        // Each inter-piece gap is a single space (see `status_line::write_aux`).
        sum + pieces.saturating_sub(1)
    }
}

/// Fixed width budget for the mount-space widget when shown in the status
/// row. The widget self-adapts down to `4` cells, but we reserve a bit more
/// so the percentage and the progress bar are readable.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub const MOUNT_AUX_WIDTH: usize = 24;

/// Build a compact git summary string from a `TreeGitStatus`.
///
/// Compact plain-text version: optional branch name + `+ins-del` stats,
/// both space-prefixed. The historical glyph-based renderer
/// (`GitStatusDisplay`) was deleted along with the root-row painter that
/// hosted it; this plain-text formatter is the sole producer used by the
/// status-row aux. Returns `None` when there's nothing to show.
pub fn format_git_summary(status: &crate::git::TreeGitStatus) -> Option<String> {
    let mut out = String::new();
    if let Some(branch) = &status.current_branch_name {
        out.push(' ');
        out.push_str(branch);
    }
    if status.insertions > 0 || status.deletions > 0 {
        out.push_str(&format!(" +{}-{}", status.insertions, status.deletions));
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::TreeGitStatus;

    #[test]
    fn status_aux_empty_when_no_pieces() {
        let aux = StatusAux::default();
        assert!(aux.is_empty());
        assert_eq!(aux.width(), 0);
    }

    #[test]
    fn status_aux_width_size_only() {
        let aux = StatusAux {
            total_size: Some("1.2G".to_string()),
            ..Default::default()
        };
        assert!(!aux.is_empty());
        // "1.2G" = 4 cells, no separators (single piece)
        assert_eq!(aux.width(), 4);
    }

    #[test]
    fn status_aux_width_git_only() {
        let aux = StatusAux {
            git_summary: Some(" main +1-0".to_string()),
            ..Default::default()
        };
        // " main +1-0" = 10 cells
        assert_eq!(aux.width(), 10);
    }

    #[test]
    fn status_aux_width_two_pieces_has_separator() {
        let aux = StatusAux {
            git_summary: Some("g".to_string()),
            total_size: Some("s".to_string()),
            ..Default::default()
        };
        // 1 + 1 + 1 gap = 3
        assert_eq!(aux.width(), 3);
    }

    // `status_aux_width_size_only` above already pins the one-piece-no-gap
    // case (single `total_size` field, no separator added). The
    // `n=3 with two gaps` case is not unit-tested because the third
    // piece is a `lfs_core::Mount`, which has no test-friendly
    // constructor. The arithmetic is exercised indirectly: every test
    // here pins one of the gap counts (0 / 1) and the formula
    // `pieces.saturating_sub(1)` is linear in `pieces`, so n=3 follows
    // by induction.

    #[test]
    fn format_git_summary_with_branch_and_stats() {
        let status = TreeGitStatus {
            current_branch_name: Some("main".to_string()),
            insertions: 3,
            deletions: 1,
        };
        let s = format_git_summary(&status).expect("non-empty");
        assert!(s.contains("main"));
        assert!(s.contains("+3-1"));
    }

    #[test]
    fn format_git_summary_empty_when_no_branch_no_stats() {
        let status = TreeGitStatus {
            current_branch_name: None,
            insertions: 0,
            deletions: 0,
        };
        assert!(format_git_summary(&status).is_none());
    }

    #[test]
    fn format_git_summary_stats_only() {
        let status = TreeGitStatus {
            current_branch_name: None,
            insertions: 5,
            deletions: 0,
        };
        let s = format_git_summary(&status).expect("non-empty");
        assert!(s.contains("+5-0"));
        assert!(!s.contains("main"));
    }

    #[test]
    fn format_git_summary_branch_only_no_stats() {
        // Pin: branch present, insertions == deletions == 0 → the stats
        // suffix is omitted entirely, leaving just `" branchname"`.
        let status = TreeGitStatus {
            current_branch_name: Some("trunk".to_string()),
            insertions: 0,
            deletions: 0,
        };
        let s = format_git_summary(&status).expect("non-empty");
        assert_eq!(s, " trunk");
        assert!(!s.contains('+'));
        assert!(!s.contains('-'));
    }
}
