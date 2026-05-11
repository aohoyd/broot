//! Pure-function support for the bulk-rename flow.
//!
//! This module owns three operations and has no knowledge of the
//! filesystem, no app state, and no editor integration:
//!
//! - [`serialize`] — turn a stage (`&[PathBuf]`) into the text payload
//!   shown to the user in `$EDITOR`. One `path.display()` per line,
//!   `\n` terminator after every entry (including the last).
//! - [`parse`] — turn the edited text back into a `Vec<String>` of
//!   one entry per non-blank, non-comment line. Trailing whitespace is
//!   trimmed; leading whitespace is preserved (only the comment check
//!   inspects the first non-whitespace char).
//! - [`plan`] — validate the parsed lines against the original stage
//!   and return a [`RenameRun`] ready for the apply phase (Task 7).
//!
//! The `existing` predicate is injected into [`plan`] (`&dyn Fn(&Path)
//! -> bool`) so unit tests can fake the filesystem-existence check;
//! real callers will pass `|p| p.exists()`.
//!
//! Validation rules are ordered and short-circuit — the first failure
//! wins. Cycles (`a → b, b → a`) are NOT a validation failure: they
//! reach the apply phase intact and Task 7's two-phase rename resolves
//! them. Unchanged pairs (target equals source) are filtered out before
//! returning so the diff modal only shows real changes.

use {
    std::{
        collections::{HashMap, HashSet},
        fs,
        io,
        path::{Path, PathBuf},
    },
};

/// Serialize a stage of paths into the editable text payload.
///
/// One `path.display()` per line, each terminated by `\n` (including
/// the final entry). Round-trips with [`parse`] for paths that have no
/// embedded newlines and no surrounding whitespace.
pub fn serialize(stage: &[PathBuf]) -> String {
    let mut s = String::new();
    for p in stage {
        s.push_str(&p.display().to_string());
        s.push('\n');
    }
    s
}

/// Parse the user-edited text back into one entry per line.
///
/// - Splits on `\n` (using [`str::lines`], which also tolerates `\r\n`).
/// - Trims trailing whitespace from each line (leading whitespace is
///   preserved — a filename may begin with a space).
/// - Skips fully blank lines and lines whose first non-whitespace
///   character is `#` (comments).
pub fn parse(edited: &str) -> Vec<String> {
    edited
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

/// The validated set of renames to apply.
///
/// Built by [`plan`]; consumed by `apply` in Task 7. Pairs where the
/// target equals the source have already been filtered out, so every
/// entry is a real rename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameRun {
    pub renames: Vec<(PathBuf, PathBuf)>,
}

/// Validation failures surfaced by [`plan`].
///
/// Each variant's [`Display`] impl renders a one-line message suitable
/// for the status row. Variants are ordered to match the rule order in
/// [`plan`]: the first failure wins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BulkRenameError {
    /// The edited line count doesn't match the original stage size.
    LineCountMismatch { expected: usize, got: usize },
    /// An edited target is empty (after trimming). 1-based line index.
    EmptyTarget { line: usize },
    /// Two or more edited targets resolve to the same path.
    DuplicateTarget { name: String },
    /// An edited target points at a file that already exists on disk
    /// and is not itself one of the source paths (which would be a
    /// cycle and is accepted).
    ExternalCollision { target: PathBuf },
}

impl std::fmt::Display for BulkRenameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use BulkRenameError::*;
        match self {
            LineCountMismatch { expected, got } => write!(
                f,
                "bulk rename: expected {expected} lines, got {got}",
            ),
            EmptyTarget { line } => write!(
                f,
                "bulk rename: empty target on line {line}",
            ),
            DuplicateTarget { name } => write!(
                f,
                "bulk rename: duplicate target `{name}`",
            ),
            ExternalCollision { target } => write!(
                f,
                "bulk rename: target `{}` already exists",
                target.display(),
            ),
        }
    }
}

impl std::error::Error for BulkRenameError {}

/// Validate the edited lines against the stage and return a
/// [`RenameRun`] of real changes.
///
/// Resolution: each edited line is treated as a target path. If the
/// line is absolute, it is used as-is; otherwise it's resolved relative
/// to the source path's parent (so the user can type a bare filename
/// and have it land alongside the original).
///
/// Rules run in order; the first failure short-circuits:
///
/// 1. `stage.len() == edited_lines.len()` (else `LineCountMismatch`).
/// 2. No edited line is empty after trimming (else `EmptyTarget`).
/// 3. No two resolved targets are equal (else `DuplicateTarget`).
/// 4. No resolved target both `existing()` and absent from `stage`
///    (else `ExternalCollision`). Targets that exist *because* they're
///    one of the source paths are accepted — that's a cycle.
///
/// Unchanged pairs (`from == to`) are filtered out before returning.
pub fn plan(
    stage: &[PathBuf],
    edited_lines: &[String],
    existing: &dyn Fn(&Path) -> bool,
) -> Result<RenameRun, BulkRenameError> {
    // Rule 1: line count must match.
    if stage.len() != edited_lines.len() {
        return Err(BulkRenameError::LineCountMismatch {
            expected: stage.len(),
            got: edited_lines.len(),
        });
    }

    // Rule 2: no empty target. Defensive — `parse` strips empties
    // already, but `plan` is called with arbitrary input.
    for (i, line) in edited_lines.iter().enumerate() {
        if line.trim().is_empty() {
            return Err(BulkRenameError::EmptyTarget { line: i + 1 });
        }
    }

    // Resolve each edited line to a target path. Absolute → as-is.
    // Relative → join onto the source's parent. No parent (root path)
    // → use the edited line verbatim.
    let pairs: Vec<(PathBuf, PathBuf)> = stage
        .iter()
        .zip(edited_lines.iter())
        .map(|(from, edited)| {
            let to_path = if Path::new(edited).is_absolute() {
                PathBuf::from(edited)
            } else if let Some(parent) = from.parent() {
                parent.join(edited)
            } else {
                PathBuf::from(edited)
            };
            (from.clone(), to_path)
        })
        .collect();

    // Rule 3: no duplicate targets.
    let mut counts: HashMap<PathBuf, usize> = HashMap::new();
    for (_, to) in &pairs {
        *counts.entry(to.clone()).or_insert(0) += 1;
    }
    for (to, n) in &counts {
        if *n > 1 {
            return Err(BulkRenameError::DuplicateTarget {
                name: to.display().to_string(),
            });
        }
    }

    // Rule 4: no external collision. A target that exists on disk is
    // only rejected if it isn't also one of the source paths (which
    // would be a cycle and is accepted).
    let source_set: std::collections::HashSet<&PathBuf> = stage.iter().collect();
    for (_, to) in &pairs {
        if existing(to) && !source_set.contains(to) {
            return Err(BulkRenameError::ExternalCollision {
                target: to.clone(),
            });
        }
    }

    // Filter unchanged pairs — the diff modal only shows real moves.
    let renames: Vec<(PathBuf, PathBuf)> = pairs
        .into_iter()
        .filter(|(from, to)| from != to)
        .collect();

    Ok(RenameRun { renames })
}

/// Execute the validated renames on the real filesystem in two phases.
///
/// Phase 1 walks `run.renames` in order. When a target path already
/// exists *and* is itself one of the source paths (i.e. a cycle, e.g.
/// `a → b, b → a`), the source is renamed to `<source>.broot-bulk-tmp-{idx}`
/// and a `(temp, target)` entry is queued for phase 2. Otherwise the
/// rename runs directly.
///
/// Phase 2 walks the queued temps and renames each one onto its final
/// target. By construction every target in phase 2 is free (its
/// original occupant was moved to a temp in phase 1).
///
/// Failure semantics: on the first `fs::rename` error, return
/// `Err((path, io::Error))` immediately. Entries before the failure
/// stay applied — there is no rollback. The caller surfaces the path
/// and the error to the status row. Phase-1 temp entries that survive
/// a phase-2 failure are NOT cleaned up; they remain on disk under
/// their `.broot-bulk-tmp-{idx}` names.
pub fn apply(run: &RenameRun) -> Result<(), (PathBuf, io::Error)> {
    let from_set: HashSet<PathBuf> = run.renames.iter().map(|(f, _)| f.clone()).collect();
    let mut second_phase: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (idx, (from, to)) in run.renames.iter().enumerate() {
        if to.exists() && from_set.contains(to) {
            // Cycle: rename `from` to a sibling temp file, queue the
            // (temp, to) pair for phase 2. The temp name is derived
            // from `from` (not `to`) so two cycle pairs cannot pick
            // the same temp; the `{idx}` suffix is additional defense.
            let mut tmp = from.clone();
            let stem = tmp
                .file_name()
                .map(|s| s.to_os_string())
                .unwrap_or_default();
            let mut name = stem;
            name.push(format!(".broot-bulk-tmp-{idx}"));
            tmp.set_file_name(name);
            fs::rename(from, &tmp).map_err(|e| (from.clone(), e))?;
            second_phase.push((tmp, to.clone()));
        } else {
            fs::rename(from, to).map_err(|e| (from.clone(), e))?;
        }
    }
    for (tmp, to) in second_phase {
        fs::rename(&tmp, &to).map_err(|e| (tmp.clone(), e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_parse_round_trip() {
        let stage = vec![
            PathBuf::from("/tmp/a.txt"),
            PathBuf::from("/tmp/b.txt"),
            PathBuf::from("/tmp/sub/c.txt"),
        ];
        let serialized = serialize(&stage);
        let parsed = parse(&serialized);
        let expected: Vec<String> = stage
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(parsed, expected);
    }

    #[test]
    fn plan_line_count_mismatch() {
        let stage = vec![
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
            PathBuf::from("/tmp/c"),
        ];
        let edited = vec!["x".to_string(), "y".to_string()];
        let result = plan(&stage, &edited, &|_| false);
        assert_eq!(
            result,
            Err(BulkRenameError::LineCountMismatch {
                expected: 3,
                got: 2,
            }),
        );
    }

    #[test]
    fn plan_empty_target_fires_on_blank_line() {
        // Call `plan` directly with a blank entry — `parse` would have
        // stripped it, but `plan` is also reachable from non-parse
        // callers and must defend its own invariant.
        let stage = vec![
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
            PathBuf::from("/tmp/c"),
        ];
        let edited = vec!["a".to_string(), "".to_string(), "c".to_string()];
        let result = plan(&stage, &edited, &|_| false);
        assert_eq!(result, Err(BulkRenameError::EmptyTarget { line: 2 }));
    }

    #[test]
    fn plan_duplicate_target_fires() {
        let stage = vec![
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        ];
        let edited = vec!["c".to_string(), "c".to_string()];
        let result = plan(&stage, &edited, &|_| false);
        match result {
            Err(BulkRenameError::DuplicateTarget { name }) => {
                assert_eq!(name, "/tmp/c");
            }
            other => panic!("expected DuplicateTarget, got {other:?}"),
        }
    }

    #[test]
    fn plan_external_collision_fires_when_target_exists_outside_stage() {
        let stage = vec![PathBuf::from("/tmp/a")];
        let edited = vec!["b".to_string()];
        // /tmp/b exists on the (fake) filesystem and is NOT in the
        // stage — external collision.
        let existing = |p: &Path| p == Path::new("/tmp/b");
        let result = plan(&stage, &edited, &existing);
        match result {
            Err(BulkRenameError::ExternalCollision { target }) => {
                assert_eq!(target, PathBuf::from("/tmp/b"));
            }
            other => panic!("expected ExternalCollision, got {other:?}"),
        }
    }

    #[test]
    fn plan_cycle_swap_is_accepted_even_when_both_exist() {
        // Stage [a, b] with edited [b, a] — both targets "exist" on
        // disk, but both are in the stage, so this is a cycle, not an
        // external collision.
        let stage = vec![
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        ];
        let edited = vec!["b".to_string(), "a".to_string()];
        let existing = |p: &Path| {
            p == Path::new("/tmp/a") || p == Path::new("/tmp/b")
        };
        let result = plan(&stage, &edited, &existing);
        let run = result.expect("cycle case must validate");
        assert_eq!(run.renames.len(), 2);
        assert_eq!(
            run.renames[0],
            (PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")),
        );
        assert_eq!(
            run.renames[1],
            (PathBuf::from("/tmp/b"), PathBuf::from("/tmp/a")),
        );
    }

    #[test]
    fn plan_cycle_produces_two_entries() {
        // Same shape as the cycle test above but reads more like a
        // standalone "two entries are returned" assertion — kept
        // separate because the plan checkbox lists it explicitly.
        let stage = vec![
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        ];
        let edited = vec!["b".to_string(), "a".to_string()];
        let existing = |p: &Path| {
            p == Path::new("/tmp/a") || p == Path::new("/tmp/b")
        };
        let run = plan(&stage, &edited, &existing)
            .expect("cycle must produce a RenameRun");
        assert_eq!(run.renames.len(), 2);
    }

    #[test]
    fn plan_unchanged_pairs_filtered() {
        // Stage [a, b], edited [a, c] — first pair is unchanged and
        // must be dropped from `renames`. Second is a real move.
        let stage = vec![
            PathBuf::from("/tmp/a"),
            PathBuf::from("/tmp/b"),
        ];
        let edited = vec!["a".to_string(), "c".to_string()];
        let run = plan(&stage, &edited, &|_| false)
            .expect("plan must validate");
        assert_eq!(run.renames.len(), 1);
        assert_eq!(
            run.renames[0],
            (PathBuf::from("/tmp/b"), PathBuf::from("/tmp/c")),
        );
    }

    #[test]
    fn parse_skips_comments_and_blanks() {
        let input = "a\n#comment\n\n  b  \n\t# indented comment\n";
        let parsed = parse(input);
        assert_eq!(parsed, vec!["a".to_string(), "  b".to_string()]);
    }

    #[test]
    fn display_messages_render_one_line_status_strings() {
        // Pin the user-facing wording so the status row stays terse
        // and consistent across the four variants.
        let line_count = BulkRenameError::LineCountMismatch {
            expected: 3,
            got: 2,
        };
        assert_eq!(
            line_count.to_string(),
            "bulk rename: expected 3 lines, got 2",
        );

        let empty = BulkRenameError::EmptyTarget { line: 4 };
        assert_eq!(empty.to_string(), "bulk rename: empty target on line 4");

        let dup = BulkRenameError::DuplicateTarget {
            name: "/tmp/c".to_string(),
        };
        assert_eq!(dup.to_string(), "bulk rename: duplicate target `/tmp/c`");

        let collision = BulkRenameError::ExternalCollision {
            target: PathBuf::from("/tmp/b"),
        };
        assert_eq!(
            collision.to_string(),
            "bulk rename: target `/tmp/b` already exists",
        );
    }
}

#[cfg(test)]
mod apply_tests {
    use {
        super::*,
        std::fs as stdfs,
    };

    /// Three real files, three real renames; assert each lands at the
    /// expected post-rename path. Exercises the non-cycle phase-1 fast
    /// path of `apply`.
    #[test]
    fn apply_happy_path_renames_three_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let a = dir.join("a");
        let b = dir.join("b");
        let c = dir.join("c");
        stdfs::write(&a, b"A").unwrap();
        stdfs::write(&b, b"B").unwrap();
        stdfs::write(&c, b"C").unwrap();

        let run = RenameRun {
            renames: vec![
                (a.clone(), dir.join("a1")),
                (b.clone(), dir.join("b1")),
                (c.clone(), dir.join("c1")),
            ],
        };
        apply(&run).expect("apply must succeed");

        assert!(!a.exists());
        assert!(!b.exists());
        assert!(!c.exists());
        assert_eq!(stdfs::read(dir.join("a1")).unwrap(), b"A");
        assert_eq!(stdfs::read(dir.join("b1")).unwrap(), b"B");
        assert_eq!(stdfs::read(dir.join("c1")).unwrap(), b"C");
    }

    /// Two files `a` and `b`, rename them onto each other. The
    /// two-phase code path must move both to a temp and back to land
    /// at the swapped names with their contents preserved.
    #[test]
    fn apply_swaps_two_files_via_two_phase() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let a = dir.join("a");
        let b = dir.join("b");
        stdfs::write(&a, b"A").unwrap();
        stdfs::write(&b, b"B").unwrap();

        let run = RenameRun {
            renames: vec![
                (a.clone(), b.clone()),
                (b.clone(), a.clone()),
            ],
        };
        apply(&run).expect("cycle apply must succeed");

        // After swap: file at path `a` has content "B", path `b` has "A".
        assert_eq!(stdfs::read(&a).unwrap(), b"B");
        assert_eq!(stdfs::read(&b).unwrap(), b"A");
    }

    /// Three files; middle target points into a non-existent directory
    /// so the rename fails. Assert the first rename stayed applied and
    /// the third was not attempted (its source still exists at the
    /// original name). The returned error must name the failed source.
    #[test]
    fn apply_partial_failure_stops_at_first_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let a = dir.join("a");
        let b = dir.join("b");
        let c = dir.join("c");
        stdfs::write(&a, b"A").unwrap();
        stdfs::write(&b, b"B").unwrap();
        stdfs::write(&c, b"C").unwrap();

        // Middle rename targets a path inside a non-existent dir.
        let bad_target = dir.join("missing_dir/b1");
        let run = RenameRun {
            renames: vec![
                (a.clone(), dir.join("a1")),
                (b.clone(), bad_target.clone()),
                (c.clone(), dir.join("c1")),
            ],
        };
        let err = apply(&run).expect_err("middle rename must fail");
        let (failed_path, _io_err) = err;
        assert_eq!(failed_path, b);

        // First rename applied.
        assert!(!a.exists());
        assert_eq!(stdfs::read(dir.join("a1")).unwrap(), b"A");
        // Middle source still there (rename failed before move).
        assert_eq!(stdfs::read(&b).unwrap(), b"B");
        // Third never attempted.
        assert_eq!(stdfs::read(&c).unwrap(), b"C");
        assert!(!dir.join("c1").exists());
    }
}
