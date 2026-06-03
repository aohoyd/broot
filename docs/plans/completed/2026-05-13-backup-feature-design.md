# backup feature — design

## Goal

Add a `:backup` operation that creates a copy of the selection with a
configurable suffix (default `.bak`), letting the user edit the proposed
destination name before applying — mirroring the existing rename UX. Bulk
mode (2+ staged paths) auto-suffixes every staged path and surfaces the
plan in a `ConfirmOverlay` for review.

## Non-goals

- No move/rename semantics — backup always **copies**, original stays.
- No `$EDITOR`-based bulk editing flow (that's `bulk_rename`'s job; for
  fine-grained per-row name editing in backup, the user falls back to
  single-file mode).
- No overwrite confirm UX — the numbered-fallback policy means we never
  clobber by default. Manual collisions reject with status error.
- No new external verb — execution is in-process (`fs::copy` /
  `copy_dir_recursively`), unlike the shell-executed `:rename`.

## UX summary

**Single file/dir (stage < 2)** — user hits `alt-shift-b`:
1. Input bar prefills with `backup_one {selected}.bak` (or the next free
   `.bak.N` if `.bak` exists).
2. User edits the name (or accepts) and presses Enter.
3. The copy is performed in-process. Panels refresh. The new backup is
   focused/selected.

**Bulk (stage ≥ 2)** — user hits `alt-shift-b` with the stage panel
holding 2+ paths:
1. The system builds a `BackupRun` with one `src → dst` per staged path,
   each `dst` computed with the next-free-name rule.
2. A `ConfirmOverlay` opens listing every `src → dst` row.
3. User presses Enter to apply, Esc to cancel.
4. On confirm: copies are performed sequentially. Partial-failure
   semantics match `bulk_rename` (stop on first error, surviving rows
   stay, stage not cleared, status row reports the failing path).

## Architecture

Three new internal verbs:

| Verb | Key | invocation | auto_exec | Role |
|---|---|---|---|---|
| `Internal::backup`       | `alt-shift-b` | none (zero-arg) | `true`  | Trigger; App-level intercept decides single vs bulk |
| `Internal::backup_one`   | none          | `backup_one {new_filename:backup-name}` | `false` | Single-file receiver; prefill bar then apply |
| `Internal::backup_apply` | none          | none           | `true`  | Bulk receiver; consumes `App::pending_backup` |

`Internal::backup_apply` is hidden from completion.

### Key flow

```
alt-shift-b
  ↓
Internal::backup (no args)
  ↓
App::apply_command intercept (top, BEFORE maybe_bulk_stage_confirm):
  ├── stage.len() ≥ 2 → run_backup (bulk path)
  │       build BackupRun via plan_bulk_backup(stage, suffix)
  │       App::pending_backup = Some(run)
  │       open Overlay::Confirm with body = "src → dst" rows
  │       pending command = Command::from_raw(":backup_apply", true)
  │
  └── stage.len() < 2 → run_backup (single path)
          synthesize Command::VerbTrigger { Internal::backup_one }
          recurse into apply_command
          ↓
          panel_input sees auto_exec=false + invocation_parser
            calls invocation_with_default → "backup_one {name}.bak[.N]"
            self.set_content(...)
            returns Command::VerbEdit
          ↓
          user edits the arg, presses Enter
          ↓
          Command::VerbInvocate(backup_one, args=Some("user_name"))
          ↓
          App::apply_command intercept catches Internal::backup_one
            → run_backup_one → fs::copy or copy_dir_recursively

Overlay confirmed → CloseAndRun(":backup_apply")
  ↓
  App::apply_command intercept catches Internal::backup_apply
    → run_backup_apply → mem::take(pending_backup) → apply each copy
```

### Why three internals (not one external receiver)

- `bulk_rename` shells out via `mv`, so its single-file receiver is an
  **external** `rename` verb that broot's exec layer can handle natively.
- Backup must execute in-process (`fs::copy` for files,
  `copy_dir_recursively` for directories) — neither is reachable through
  the external-verb path.
- The split between `backup` (trigger, `auto_exec:true`) and `backup_one`
  (receiver, `auto_exec:false`) is forced by the keypress dispatch: a
  single `auto_exec:false` verb on `alt-shift-b` would prefill the bar
  unconditionally, even when the stage has 2+ paths and we want the
  ConfirmOverlay flow instead.

### Why NOT in `is_stage_consuming_internal`

`Internal::backup` is not added to the bulk-stage deny-list. The
App-level intercept catches it **before** `maybe_bulk_stage_confirm`
runs (same as `bulk_rename` per CLAUDE.md), so the generic
"Run :backup on N files?" confirm never fires. The user sees our own
purpose-built `ConfirmOverlay` with the actual `src → dst` plan instead
of a yes/no count prompt.

## Data types

New module `src/backup/mod.rs`:

```rust
#[derive(Debug, Clone)]
pub struct BackupCopy {
    pub src: PathBuf,
    pub dst: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct BackupRun {
    pub copies: Vec<BackupCopy>,
}

const MAX_BACKUP_BUMP: u32 = 999;

/// Probe `src.parent()` for the first free name of the form:
///   {file_name}{suffix}        (try first — the unbumped name)
///   {file_name}{suffix}.1
///   {file_name}{suffix}.2      (… up to MAX_BACKUP_BUMP)
/// Returns None iff every candidate up to the cap exists.
pub fn next_free_backup_name(
    src: &Path,
    suffix: &str,
) -> Option<PathBuf>;

pub fn plan_bulk_backup(
    paths: &[PathBuf],
    suffix: &str,
) -> BackupRun;

/// Apply a single (src, dst) — fs::copy for files,
/// copy_dir_recursively for directories.
/// Returns Err((failing_path, io::Error)) on first failure.
pub fn apply(
    run: &BackupRun,
) -> Result<(), (PathBuf, io::Error)>;
```

`copy_dir_recursively` already exists at
`src/app/panel_state.rs:1662`. The backup module either re-exports it
or moves it to a shared location — TBD during implementation (lean
toward leaving it in panel_state for v1 and `pub(crate)`-exposing it).

## App state

New field on `App` (next to `pending_bulk_rename` in `src/app/app.rs:75-90`):

```rust
pub pending_backup: Option<BackupRun>,
```

The `OverlayOutcome::Close` arm in `src/app/app.rs:867-898` is extended
to also `self.pending_backup = None`, matching the existing
`pending_bulk_rename` cleanup. Without this, a cancelled overlay
followed by a direct `:backup_apply` invocation would silently apply the
stale plan.

## Placeholder hook

New `"backup-name"` arm in `get_sel_name_standard_replacement`
(`src/verb/execution_builder.rs:126-141`):

```rust
"backup-name" => {
    crate::backup::next_free_backup_name(path, &con.backup_suffix)
        .and_then(|p| p.file_name().map(OsString::from))
        .and_then(|os| os.to_str().map(String::from))
}
```

The placeholder receives `&AppContext` because the existing function
signature already carries `con`. The substitution returns just the
basename — the receiver reconstructs the full path as
`selection.parent() / args`, mirroring the rename invocation contract
(`mv {file} {parent}/{new_filename}`).

If `next_free_backup_name` returns `None` (cap exhausted), the
substitution falls back to the bare `{file_name}{suffix}` so the user
still gets a sensible prefill and can edit it manually.

## Config plumbing

Following the `date_time_format` pattern exactly.

**`src/conf/conf.rs`** — add field (alphabetically positioned near
other scalar string fields):

```rust
#[serde(alias = "backup-suffix")]
pub backup_suffix: Option<String>,
```

**`src/conf/conf.rs`** — merge block (~line 245):

```rust
overwrite!(self, backup_suffix, conf);
```

Forgetting this line silently drops user-supplied values (see the
CLAUDE.md "Bookmarks config plumbing" gotcha).

**`src/app/app_context.rs`** — store the resolved value (default `.bak`):

```rust
let backup_suffix = config.backup_suffix.clone()
    .filter(|s| is_valid_backup_suffix(s))
    .unwrap_or_else(|| ".bak".to_string());
```

Validation (`is_valid_backup_suffix`):
- Reject empty strings (would create `dst == src`, infinite probe loop).
- Reject strings containing `/`, `\\`, or `\0` (path traversal /
  invalid filename chars).
- On rejection, log via `cli_log::warn!` and fall through to the
  default.

`AppContext` gains a `pub backup_suffix: String` field.

**`resources/default-conf/conf.hjson`** — add commented sample near
other options:

```hjson
# Suffix appended by the :backup verb when copying the selection.
# If a destination with this suffix already exists, broot appends
# .1, .2, … up to .999 to find a free name.
# backup_suffix: ".bak"
```

## Handlers (in `src/app/app.rs`)

**`run_backup`** — the dispatcher:
```
fn run_backup(...) -> Result<...> {
    let stage = app_state.stage.paths();
    if stage.len() >= 2 {
        let run = plan_bulk_backup(stage, &con.backup_suffix);
        self.pending_backup = Some(run.clone());
        let body = run.copies.iter()
            .map(|c| format!("{} → {}", c.src.display(), c.dst.display()))
            .collect();
        self.open_confirm_overlay(
            format!("Backup {} files?", run.copies.len()),
            body,
            "Backup",
            /*danger*/ false,
            Command::from_raw(":backup_apply", true),
        );
        Ok(CmdResult::Keep)
    } else {
        let verb_id = self.find_internal_verb_id(Internal::backup_one, con)?;
        let cmd = Command::VerbTrigger { verb_id, input_invocation: None };
        self.apply_command(w, &cmd, panel_skin, app_state, con)
    }
}
```

`find_internal_verb_id` is a new helper that mirrors
`find_external_rename_verb_id` (app.rs:843-849) but matches on
`verb.get_internal() == Some(target)` instead of `has_name`.

**`run_backup_one`** — the single-file receiver:
```
fn run_backup_one(cmd: Command, ...) -> Result<...> {
    let invocation = extract_invocation(cmd)?;
    let new_name = invocation.args.as_deref()
        .ok_or("backup_one called without a name")?;
    let selection = current_selection(panel)?;
    let parent = selection.parent()
        .ok_or("cannot backup the filesystem root")?;
    let target = parent.join(new_name);
    if target.exists() {
        return Ok(CmdResult::error("backup destination already exists"));
    }
    if selection.is_dir() {
        copy_dir_recursively(&selection, &target)?;
    } else {
        fs::copy(&selection, &target)?;
    }
    // refresh + focus the new backup
    Ok(CmdResult::from_path(target))
}
```

**`run_backup_apply`** — the bulk receiver:
```
fn run_backup_apply(...) -> Result<...> {
    let run = self.pending_backup.take()
        .ok_or("no pending backup")?;
    match backup::apply(&run) {
        Ok(()) => {
            // status: "backed up N files"
            // refresh panels; do NOT clear stage
        }
        Err((path, e)) => {
            // status: "{path}: {e}"
            // refresh panels; do NOT clear stage; partial state stays
        }
    }
    Ok(CmdResult::Keep)
}
```

## Overwrite policy

| Path | Trigger | Existing dst? | Behavior |
|---|---|---|---|
| Single, prefilled name accepted as-is | first `.bak` was free | n/a | apply |
| Single, prefilled name accepted as-is | `.bak`, `.bak.1`…`.bak.N-1` all existed | n/a | apply with bumped name |
| Single, prefilled name accepted as-is | all 1000 candidates exist | n/a (prefill is bare `.bak`) | on Enter → exists → reject with status error |
| Single, user manually typed colliding name | yes | reject — status error: `"backup destination already exists"` |
| Bulk, plan stage | first free name found per row | n/a | row uses bumped name |
| Bulk, plan stage | any row has all 1000 collisions | yes | error: `"too many backups exist for {path}"`, bulk planning fails, overlay does not open |

Rationale: **numbered fallback** is the default behavior when the user
accepts the system's proposal. Manual user input is **respected, not
overridden** — if the user typed `foo.bak` themselves, they're refused
(not silently bumped) because the bump would override an explicit choice.
This matches the AddOverlay no-clobber policy.

## Partial-failure semantics (bulk)

Mirrors `bulk_rename::apply` exactly:
- Copies execute sequentially in `run.copies` order.
- On first `io::Error`, stop. Return `Err((failing_src, error))`.
- Successful copies stay on disk (no rollback).
- `pending_backup` is consumed by `take()` so the run is gone.
- Stage is **not** cleared — the user can re-trigger to retry the
  remaining failed entries (though they'd need to manually unstage the
  successful ones, since the plan re-runs the whole stage).
- All panels refresh so the partial state is visible.

## Edge cases

1. **Suffix with slashes/null** — rejected at config load, falls back to
   `.bak` with `cli_log::warn!`.
2. **Empty suffix** — same as above (would create infinite probe loop).
3. **Selection is the filesystem root** — single-file path: status error
   `"cannot backup the filesystem root"`.
4. **Symlinks** — `fs::copy` follows the link by design (copies the
   target's contents). Documented but unchanged for v1.
5. **`.bak.bak` cascades** — if the user already has `foo` and `foo.bak`
   in the stage, backing up the stage produces both `foo.bak` (collision,
   bumps to `foo.bak.1`) and `foo.bak.bak`. Surprising but correct; user
   can cancel the ConfirmOverlay if they don't want it.
6. **Permissions** — read-only destination dir: `fs::copy` returns an
   I/O error, handled by the partial-failure path.

## Testing

**Unit tests** (`src/backup/mod.rs`, using `tempfile`):
- `next_free_backup_name`: zero collisions → `.bak`; collisions 1-9 →
  `.bak.1`-`.bak.9`; cap exhaust → `None`.
- `plan_bulk_backup`: heterogeneous stage (file + dir + nonexistent
  path), spot-checks each `dst`.
- `apply`: success path, partial-failure path (use a read-only dst
  parent to force `fs::copy` error), cycle-irrelevant (no cycles in
  copy, but a regression test that asserts sequential semantics).
- Directory backup: a dir `foo/` produces `foo.bak/` and the recursive
  contents are copied.

**Pin tests** (`src/verb/verb_store.rs`, mirroring
`sort_by_type_dirs_internals_registered`):
- `backup_internals_registered` — asserts all three internals exist by
  name.
- `backup_keybind_on_trigger` — asserts `alt-shift-b` resolves to
  `Internal::backup`, not `backup_one` / `backup_apply`.

**Integration tests** (`src/app/app.rs` or similar):
- Stage of 2+ paths + `Internal::backup` → overlay is `Overlay::Confirm`
  with `pending_backup.is_some()`.
- Same setup, Esc on overlay → `pending_backup.is_none()` after (the
  cancel-leaves-stale-plan regression test).
- `Internal::backup_one` with manually colliding name → status error,
  no fs change.

## Files touched

| File | Change |
|---|---|
| `src/backup/mod.rs` | NEW — `BackupCopy`, `BackupRun`, `next_free_backup_name`, `plan_bulk_backup`, `apply` |
| `src/verb/internal.rs` (or wherever `Internal` is defined) | Three new variants: `backup`, `backup_one`, `backup_apply` |
| `src/verb/verb_store.rs` | Register the three internals; add pin tests |
| `src/verb/execution_builder.rs` | New `"backup-name"` arm in `get_sel_name_standard_replacement` |
| `src/app/app.rs` | Three handlers (`run_backup`, `run_backup_one`, `run_backup_apply`); intercept block; `pending_backup` field; Close-arm cleanup; `find_internal_verb_id` helper |
| `src/conf/conf.rs` | `backup_suffix: Option<String>` field + `overwrite!` merge line |
| `src/app/app_context.rs` | Resolve + validate `backup_suffix`, store on `AppContext` |
| `resources/default-conf/conf.hjson` | Commented `backup_suffix` sample |
| `CLAUDE.md` | New "Backup feature" subsection covering the three-internal split, numbered-fallback policy, MAX_BACKUP_BUMP cap, suffix validation, Close-arm cleanup invariant |
| `src/app/panel_state.rs` | Expose `copy_dir_recursively` as `pub(crate)` if it isn't already |

## Open questions deferred to implementation

- Should `copy_dir_recursively` move to `src/backup/mod.rs` or stay in
  `panel_state.rs`? Decide based on whether any non-backup callers
  exist after the refactor.
- Status messages: exact wording for success / failure / cap-exhaust.
- Whether `Internal::backup_apply` deserves a separate `:backup_apply`
  command alias in addition to being hidden.

## Out of scope / future work

- Per-file timestamp suffixes (e.g. `.bak.20260513-141522`).
- Compression / archive-based backups.
- A `:restore` verb to reverse a backup.
- Configurable numbering format (currently hard-coded `.N`).
- Overwrite-on-explicit-collide policy variants.
