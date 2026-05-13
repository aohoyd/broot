//! Pure-function support for the `:backup` verb.
//!
//! This module owns name composition, bulk planning, and apply for the
//! backup-the-selection flow. The planner side
//! ([`next_free_backup_name`], [`plan_bulk_backup`]) has no knowledge
//! of app state. The applier side ([`apply`]) depends on
//! [`crate::app::copy_dir_recursively`] for recursive directory copies,
//! a small filesystem utility that today lives in `src/app/panel_state.rs`
//! alongside the verb that originally introduced it. Moving it to a
//! standalone utility module would be cleaner, but the current placement
//! is intentional: there's only one other caller (the cp/mv verb path)
//! and no third caller is planned.
//!
//! The naming rule is: given a source `parent/file_name` and a suffix
//! string (e.g. `.bak`), the first candidate destination is
//! `parent/file_name{suffix}`. If that exists, fall back to
//! `parent/file_name{suffix}.1`, `.2`, …, `.{MAX_BACKUP_BUMP}`. Only
//! when every candidate already exists does the planner give up.
//!
//! The probe is filesystem-backed: [`next_free_backup_name`] uses
//! `Path::exists()` on each candidate, so a TOCTOU window exists
//! between planning and applying. [`apply`] surfaces any write-time
//! failure as a `(PathBuf, io::Error)` tuple identifying the offending
//! row, mirroring [`crate::bulk_rename::apply`]'s partial-failure
//! semantics (no rollback; copies before the failure stay on disk).

use std::{
    io,
    path::{Path, PathBuf},
};

/// One planned copy: `src` is the file/dir to back up, `dst` is the
/// free destination path computed by [`next_free_backup_name`].
///
/// The sentinel `dst == src` means the planner could not find a free
/// candidate within [`MAX_BACKUP_BUMP`] — [`apply`] detects this and
/// surfaces an error rather than running a self-copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupCopy {
    pub src: PathBuf,
    pub dst: PathBuf,
}

/// The validated set of backup copies to run.
///
/// Built by [`plan_bulk_backup`]; consumed by [`apply`]. Entries
/// where `dst == src` are sentinel rows for "no free name in the
/// bumped range"; the apply step is responsible for catching them
/// and emitting the user-facing error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupRun {
    pub copies: Vec<BackupCopy>,
}

/// Maximum numeric bump appended after the base `{suffix}` candidate.
///
/// 999 was chosen as the practical ceiling: it's three digits (so it
/// fits in a readable filename), it's far higher than any realistic
/// hand-managed backup set, and it doubles as a soft brake on
/// pathological inputs (a million-iteration probe would freeze the
/// UI thread on the planning hop).
pub(crate) const MAX_BACKUP_BUMP: u32 = 999;

/// Compute the first free destination filename for backing up `src`
/// using `suffix`.
///
/// The probe order is:
///
/// 1. `parent/{file_name}{suffix}` (no numeric bump)
/// 2. `parent/{file_name}{suffix}.1`
/// 3. `parent/{file_name}{suffix}.2`
/// 4. … through `parent/{file_name}{suffix}.{MAX_BACKUP_BUMP}`
///
/// The first candidate whose `exists()` returns `false` wins. If every
/// candidate exists, `None` is returned. `None` is also returned if
/// `src` has no parent (filesystem root) or no file_name, or if the
/// file_name is not valid UTF-8 — the format! composition path through
/// `&str` would otherwise lose bytes silently.
///
/// Non-UTF-8 names: consistent with `bulk_rename::diff_lines`, paths
/// whose `file_name()` is not representable as `&str` are skipped at
/// the planning layer rather than rendered through `display()`. A
/// future iteration could route through `OsString` end-to-end if a
/// real-world request surfaces.
pub fn next_free_backup_name(src: &Path, suffix: &str) -> Option<PathBuf> {
    let parent = src.parent()?;
    let file_name = src.file_name()?.to_str()?;

    // Candidate 0: bare `{name}{suffix}`.
    let base = parent.join(format!("{file_name}{suffix}"));
    if !base.exists() {
        return Some(base);
    }

    // Candidates 1..=MAX_BACKUP_BUMP: `{name}{suffix}.N`.
    for n in 1..=MAX_BACKUP_BUMP {
        let candidate = parent.join(format!("{file_name}{suffix}.{n}"));
        if !candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

/// Plan a bulk-backup run from a list of source paths.
///
/// For each path:
///
/// - Paths with no parent (filesystem root like `/`) or no file_name
///   are skipped entirely — they are not representable as a backup
///   source, and producing a `dst == src` row would be wrong (we
///   never had a name to back up under).
/// - Paths whose file_name is not valid UTF-8 are also skipped, for
///   the same reason [`next_free_backup_name`] returns `None` on them.
/// - Otherwise, [`next_free_backup_name`] is called. If it finds a
///   free name, a `BackupCopy { src, dst }` is appended.
/// - **Sentinel contract**: if every candidate is taken
///   ([`MAX_BACKUP_BUMP`] exhausted), the path is **still included**
///   in the run with `dst = src.clone()`. [`apply`] pattern-matches
///   on `dst == src` and emits a user-facing error for that row
///   rather than running a self-copy.
pub fn plan_bulk_backup(paths: &[PathBuf], suffix: &str) -> BackupRun {
    let mut copies: Vec<BackupCopy> = Vec::with_capacity(paths.len());
    for src in paths {
        if src.parent().is_none() {
            continue;
        }
        let Some(file_name) = src.file_name() else {
            continue;
        };
        if file_name.to_str().is_none() {
            // Non-UTF-8 file_name: skipped at planning. See
            // module-level doc for the rationale.
            continue;
        }
        let dst = next_free_backup_name(src, suffix)
            .unwrap_or_else(|| src.clone());
        copies.push(BackupCopy {
            src: src.clone(),
            dst,
        });
    }
    BackupRun { copies }
}

/// Execute a planned [`BackupRun`].
///
/// Iterates `run.copies` in order. For each row:
///
/// - If `dst == src`, the planner ran out of `.N` slots — return
///   `Err((src, io::Error::AlreadyExists("too many backups exist for this path")))`.
/// - If `src.is_dir()`, recurse via
///   [`crate::app::copy_dir_recursively`].
/// - Otherwise, `fs::copy(src, dst)`.
///
/// On the first error the function returns immediately with the
/// failing path and the underlying `io::Error`. Copies that completed
/// before the failure stay on disk — there is no rollback. Subsequent
/// rows are NOT attempted. This mirrors the partial-failure semantics
/// of [`crate::bulk_rename::apply`].
pub fn apply(run: &BackupRun) -> Result<(), (PathBuf, io::Error)> {
    for copy in &run.copies {
        let BackupCopy { src, dst } = copy;
        if dst == src {
            return Err((
                src.clone(),
                io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "too many backups exist for this path",
                ),
            ));
        }
        if src.is_dir() {
            crate::app::copy_dir_recursively(src, dst)
                .map_err(|e| (src.clone(), e))?;
        } else {
            std::fs::copy(src, dst)
                .map(drop)
                .map_err(|e| (src.clone(), e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        std::fs,
    };

    /// Zero collisions: the bare `{name}{suffix}` candidate wins on
    /// the first probe.
    #[test]
    fn next_free_no_collision_returns_bare_suffix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        fs::write(&src, b"x").unwrap();

        let got = next_free_backup_name(&src, ".bak")
            .expect("first candidate must be free");
        assert_eq!(got, dir.join("foo.bak"));
    }

    /// Collision on `.bak` only — the planner bumps once to `.bak.1`.
    #[test]
    fn next_free_single_collision_bumps_to_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        fs::write(&src, b"x").unwrap();
        fs::write(dir.join("foo.bak"), b"existing").unwrap();

        let got = next_free_backup_name(&src, ".bak")
            .expect("bump candidate must be free");
        assert_eq!(got, dir.join("foo.bak.1"));
    }

    /// Collisions on `.bak`, `.bak.1`, `.bak.2` — planner lands on
    /// `.bak.3`.
    #[test]
    fn next_free_multiple_collisions_bumps_past_them() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        fs::write(&src, b"x").unwrap();
        fs::write(dir.join("foo.bak"), b"e0").unwrap();
        fs::write(dir.join("foo.bak.1"), b"e1").unwrap();
        fs::write(dir.join("foo.bak.2"), b"e2").unwrap();

        let got = next_free_backup_name(&src, ".bak")
            .expect("third bump must be free");
        assert_eq!(got, dir.join("foo.bak.3"));
    }

    /// Every candidate `.bak` through `.bak.MAX_BACKUP_BUMP` exists →
    /// `None`. We create all `1 + MAX_BACKUP_BUMP` real files; on a
    /// modern machine `tempfile` + 1000 small writes runs in well
    /// under a second. Sticking to the real filesystem (rather than a
    /// closure-injected `exists` predicate) keeps the test honest
    /// against [`next_free_backup_name`]'s actual `Path::exists`
    /// call.
    #[test]
    fn next_free_returns_none_when_every_candidate_exists() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        fs::write(&src, b"x").unwrap();

        fs::write(dir.join("foo.bak"), b"").unwrap();
        for n in 1..=MAX_BACKUP_BUMP {
            fs::write(dir.join(format!("foo.bak.{n}")), b"").unwrap();
        }

        assert_eq!(next_free_backup_name(&src, ".bak"), None);
    }

    /// Directory source: the planner doesn't care about file type,
    /// only name composition. `foo/` produces `foo.bak/` as the
    /// destination path; the apply step is what eventually copies the
    /// tree contents.
    #[test]
    fn next_free_works_for_directory_source() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        fs::create_dir(&src).unwrap();

        let got = next_free_backup_name(&src, ".bak")
            .expect("directory source must plan a destination");
        assert_eq!(got, dir.join("foo.bak"));
    }

    /// Suffix-agnostic: a `~`-style suffix produces `foo.txt~` then
    /// `foo.txt~.1`. The `.N` separator is always dot-prefixed
    /// regardless of the suffix shape.
    #[test]
    fn next_free_format_agnostic_tilde_suffix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo.txt");
        fs::write(&src, b"x").unwrap();

        // First call: bare `foo.txt~` is free.
        let got = next_free_backup_name(&src, "~")
            .expect("first ~ candidate must be free");
        assert_eq!(got, dir.join("foo.txt~"));

        // Create it, then the next probe must bump to `.1`.
        fs::write(&got, b"").unwrap();
        let got2 = next_free_backup_name(&src, "~")
            .expect("second probe must bump");
        assert_eq!(got2, dir.join("foo.txt~.1"));
    }

    /// Heterogeneous bulk plan: a regular file, a directory, and a
    /// nonexistent path each produce a row with the bare
    /// `{name}.bak` destination. The nonexistent path also plans
    /// successfully — `next_free_backup_name` doesn't require the
    /// source to exist, only the destination candidates' parent.
    #[test]
    fn plan_bulk_backup_heterogeneous_inputs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let file = dir.join("a.txt");
        let subdir = dir.join("b");
        let missing = dir.join("c-does-not-exist");
        fs::write(&file, b"A").unwrap();
        fs::create_dir(&subdir).unwrap();
        // `missing` is intentionally not created.

        let paths = vec![file.clone(), subdir.clone(), missing.clone()];
        let run = plan_bulk_backup(&paths, ".bak");
        assert_eq!(run.copies.len(), 3);
        assert_eq!(
            run.copies[0],
            BackupCopy {
                src: file.clone(),
                dst: dir.join("a.txt.bak"),
            },
        );
        assert_eq!(
            run.copies[1],
            BackupCopy {
                src: subdir.clone(),
                dst: dir.join("b.bak"),
            },
        );
        assert_eq!(
            run.copies[2],
            BackupCopy {
                src: missing.clone(),
                dst: dir.join("c-does-not-exist.bak"),
            },
        );
    }

    /// Filesystem root (no parent) is skipped entirely — no row in
    /// the run at all.
    #[test]
    fn plan_bulk_backup_skips_paths_with_no_parent() {
        let paths = vec![PathBuf::from("/")];
        let run = plan_bulk_backup(&paths, ".bak");
        assert!(
            run.copies.is_empty(),
            "filesystem root must not produce a row, got {:?}",
            run.copies,
        );
    }

    /// `next_free_backup_name("/", suffix)` returns `None` because
    /// the filesystem root has no parent. Pin the helper's
    /// rejection at the same shape `plan_bulk_backup` skips on, so
    /// the two layers stay in agreement.
    #[test]
    fn next_free_returns_none_for_filesystem_root() {
        let got = next_free_backup_name(Path::new("/"), ".bak");
        assert!(
            got.is_none(),
            "filesystem root must yield None (no parent), got {got:?}",
        );
    }

    /// `next_free_backup_name(".", suffix)` returns `None`. A bare
    /// `.` has no `file_name()` component, so composition can't
    /// proceed. Pinning this matches the planner's skip behaviour
    /// for the same path shape.
    #[test]
    fn next_free_returns_none_for_dot() {
        let got = next_free_backup_name(Path::new("."), ".bak");
        assert!(
            got.is_none(),
            "`.` must yield None (no file_name), got {got:?}",
        );
    }

    /// `next_free_backup_name("..", suffix)` also returns `None`,
    /// for the same `file_name()` reason as the `.` case above.
    #[test]
    fn next_free_returns_none_for_dotdot() {
        let got = next_free_backup_name(Path::new(".."), ".bak");
        assert!(
            got.is_none(),
            "`..` must yield None (no file_name), got {got:?}",
        );
    }

    /// A path with no file_name (`.` or `..`) is also skipped.
    #[test]
    fn plan_bulk_backup_skips_dot_and_dotdot() {
        let paths = vec![PathBuf::from("."), PathBuf::from("..")];
        let run = plan_bulk_backup(&paths, ".bak");
        assert!(
            run.copies.is_empty(),
            "dot/dotdot have no file_name; rows must be skipped",
        );
    }

    /// Empty `BackupRun`: `apply` must return `Ok(())` without
    /// touching the filesystem. Pins the contract that a no-op plan
    /// (e.g. `plan_bulk_backup` skipped every input as unbackable)
    /// is safe to apply.
    #[test]
    fn apply_empty_run_returns_ok() {
        let run = BackupRun { copies: vec![] };
        apply(&run).expect("empty BackupRun must apply successfully");
    }

    /// When `next_free_backup_name` returns `None` (every candidate
    /// exists), the planner still emits a sentinel row with
    /// `dst == src`. The apply step is expected to detect and reject
    /// this case.
    #[test]
    fn plan_bulk_backup_uses_self_sentinel_when_exhausted() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        fs::write(&src, b"x").unwrap();

        // Exhaust every candidate so `next_free_backup_name` returns None.
        fs::write(dir.join("foo.bak"), b"").unwrap();
        for n in 1..=MAX_BACKUP_BUMP {
            fs::write(dir.join(format!("foo.bak.{n}")), b"").unwrap();
        }

        let run = plan_bulk_backup(&[src.clone()], ".bak");
        assert_eq!(run.copies.len(), 1);
        assert_eq!(
            run.copies[0],
            BackupCopy {
                src: src.clone(),
                dst: src.clone(),
            },
            "exhausted candidates must produce dst==src sentinel",
        );
    }

    /// Happy-path apply: three planned copies, all writable, all
    /// succeed. Each destination must exist after the apply and its
    /// bytes must match the source exactly.
    #[test]
    fn apply_all_succeed_for_regular_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        let c = dir.join("c.txt");
        fs::write(&a, b"AAA").unwrap();
        fs::write(&b, b"BBBB").unwrap();
        fs::write(&c, b"CCCCC").unwrap();

        let run = BackupRun {
            copies: vec![
                BackupCopy { src: a.clone(), dst: dir.join("a.txt.bak") },
                BackupCopy { src: b.clone(), dst: dir.join("b.txt.bak") },
                BackupCopy { src: c.clone(), dst: dir.join("c.txt.bak") },
            ],
        };
        apply(&run).expect("apply must succeed for writable destinations");

        assert_eq!(fs::read(dir.join("a.txt.bak")).unwrap(), b"AAA");
        assert_eq!(fs::read(dir.join("b.txt.bak")).unwrap(), b"BBBB");
        assert_eq!(fs::read(dir.join("c.txt.bak")).unwrap(), b"CCCCC");
    }

    /// Directory-source apply: nested structure must be replicated
    /// under the destination directory. Delegates to
    /// `copy_dir_recursively`; this test pins that the dispatch on
    /// `src.is_dir()` actually wires to the recursive path.
    #[test]
    fn apply_copies_directory_recursively() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src_dir = dir.join("src");
        fs::create_dir(&src_dir).unwrap();
        fs::write(src_dir.join("top.txt"), b"top").unwrap();
        let nested = src_dir.join("nested");
        fs::create_dir(&nested).unwrap();
        fs::write(nested.join("inner.txt"), b"inner").unwrap();

        let dst_dir = dir.join("src.bak");
        let run = BackupRun {
            copies: vec![BackupCopy { src: src_dir, dst: dst_dir.clone() }],
        };
        apply(&run).expect("directory apply must succeed");

        assert!(dst_dir.is_dir(), "destination dir must exist");
        assert_eq!(fs::read(dst_dir.join("top.txt")).unwrap(), b"top");
        assert!(dst_dir.join("nested").is_dir(), "nested dir must exist");
        assert_eq!(
            fs::read(dst_dir.join("nested").join("inner.txt")).unwrap(),
            b"inner",
        );
    }

    /// Cap-exhaust sentinel: a `BackupCopy` where `dst == src` must
    /// produce `AlreadyExists` with the canonical message and not
    /// touch the filesystem. We place the sentinel as the only row so
    /// the assertion "no prior row was applied" is trivially true.
    #[test]
    fn apply_detects_cap_exhaust_sentinel() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        fs::write(&src, b"x").unwrap();

        let run = BackupRun {
            copies: vec![BackupCopy { src: src.clone(), dst: src.clone() }],
        };
        let err = apply(&run).expect_err("sentinel must produce Err");
        assert_eq!(err.0, src, "error must carry the source path");
        assert_eq!(err.1.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            err.1.to_string().contains("too many backups"),
            "error message must mention 'too many backups', got: {}",
            err.1,
        );
        // No write was attempted: the source file is unchanged.
        assert_eq!(fs::read(&src).unwrap(), b"x");
    }

    /// Partial-failure semantics: when row 2 fails (unwritable
    /// destination directory), row 1's copy stays on disk and row 3
    /// is never attempted. Unix-only because we use `chmod(0o555)` to
    /// force the failure; Windows would need different fixturing.
    #[cfg(unix)]
    #[test]
    fn apply_stops_on_first_failure_partial_results_persist() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();

        // Three sources, one shared writable dir, plus a read-only
        // subdirectory that hosts row 2's destination.
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        let c = dir.join("c.txt");
        fs::write(&a, b"AAA").unwrap();
        fs::write(&b, b"BBB").unwrap();
        fs::write(&c, b"CCC").unwrap();

        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();

        let dst_a = dir.join("a.txt.bak");
        let dst_b = locked.join("b.txt.bak");
        let dst_c = dir.join("c.txt.bak");

        // 0o555 = read+execute, no write. fs::copy into this dir will fail.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o555))
            .expect("chmod 0o555 must succeed");

        let run = BackupRun {
            copies: vec![
                BackupCopy { src: a.clone(), dst: dst_a.clone() },
                BackupCopy { src: b.clone(), dst: dst_b.clone() },
                BackupCopy { src: c.clone(), dst: dst_c.clone() },
            ],
        };
        let result = apply(&run);

        // Restore permissions BEFORE assertions so tempdir cleanup
        // and any panic-path teardown can succeed regardless of the
        // assertion outcome.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755))
            .expect("chmod 0o755 restore must succeed");

        let err = result.expect_err("locked-dir destination must fail");
        assert_eq!(err.0, b, "error must carry row 2's source path");

        // Row 1 succeeded and stays on disk.
        assert!(dst_a.exists(), "row 1's destination must exist");
        assert_eq!(fs::read(&dst_a).unwrap(), b"AAA");
        // Row 3 was never attempted.
        assert!(
            !dst_c.exists(),
            "row 3 must not have been attempted after row 2 failed",
        );
    }
}
