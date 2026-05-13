# Bulk-rename confirm modal: readable diff display

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Make the confirm overlay's diff readable when bulk-renaming — show basenames only when both sides share a parent, full paths when they don't, grow the overlay width to fit content (40 col floor, 80% screen cap), and soft-wrap the `to` half onto a continuation line when a single rename still doesn't fit.

**Architecture:** Two independent changes that compose. A new pure function `bulk_rename::diff_lines` builds the body lines and lives next to `serialize`/`parse`/`plan`/`apply` so it's testable without the App. `ConfirmOverlay` (src/app/overlay/confirm.rs) replaces its fixed 50-col width with content-driven sizing and gains a `wrap_diff_line` helper that the render loop applies before iterating rows. No public API changes beyond the new `bulk_rename::diff_lines` export. No `Command` enum or `ConfirmOverlay` field additions.

**Tech Stack:** Rust, termimad (Area / CompoundStyle), crossterm. No new dependencies.

## Overview

The current bulk-rename confirm overlay renders one row per rename as `"{from.display()} → {to.display()}"` with absolute paths on both sides (src/app/app.rs:790-794) into a fixed 50-column modal (src/app/overlay/confirm.rs:155-208). The right side is truncated with `…` — hiding the one piece of information the user needs to verify.

This plan:
- Strips the shared parent so the typical case becomes `"alpha.txt → alpha-v2.txt"`.
- Falls back to full paths per-line when parents differ (cross-directory move via absolute edited path).
- Grows the overlay to fit content up to 80% of screen width.
- Soft-wraps the `to` half onto an indented continuation line when even the grown width isn't enough.

Reference design: `docs/plans/2026-05-12-bulk-rename-diff-display-design.md`.

## Context (from discovery)

- **Body source**: src/app/app.rs:790-794 builds `body: Vec<String>` from `RenameRun.renames` then passes it to `App::request_confirm`. The `RenameRun` type lives at src/bulk_rename/mod.rs:75-82.
- **Plan-time path resolution**: `bulk_rename::plan` (src/bulk_rename/mod.rs:148-219) accepts absolute paths in the edited text verbatim — so `(from, to)` can land in different parents. Same-parent is the common case; cross-dir is the edge case the wrap path covers.
- **Overlay geometry**: `ConfirmOverlay::render` at src/app/overlay/confirm.rs:148-269. Fixed width at line 156: `frame::centered_rect(screen, 50, want_h)`. Body iteration at lines 194-210 uses `truncate_to_width` which clips from the right. Height already grows up to 15 rows.
- **Vertical scroll math**: `visible_body_rows` and `max_scroll` (src/app/overlay/confirm.rs:123-140) operate on `self.body.len()` today. After wrap they must operate on rendered-row count.
- **Render-bytes test pattern**: existing tests use `std::io::BufWriter::with_capacity(64 * 1024, std::io::sink())` and inspect `buffer()` pre-flush — see src/app/overlay/confirm.rs:640-708. Re-use this pattern for new width/wrap tests.
- **CLAUDE.md invariant**: the "Overlay routing — single field, single render hook, single key hook" section pins the dispatch shape; nothing in this plan touches that. The fixed-50 width is **not** mentioned in CLAUDE.md, so no existing invariant is being violated by changing it — but the new dynamic-width contract is worth recording (Task 6).

## Development Approach

- **Testing approach**: TDD (tests first, run-fail, implement, run-pass)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional — they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run `cargo test` after each task
- run `cargo build` after structural changes
- maintain backward compatibility — `ConfirmOverlay` callers (rm, trash, cp/mv overwrite, bulk-stage, future) keep working unchanged

## Testing Strategy

- **unit tests**: required for every task (see Development Approach)
- **render-output capture**: re-use the `BufWriter<Sink>::buffer()` pattern at src/app/overlay/confirm.rs:644-664 for width/wrap assertions. Don't introduce new test scaffolding.
- **no UI e2e tests**: broot is a TUI. Manual smoke verification is the e2e equivalent — covered in Task 6 + Post-Completion.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

Six tasks in dependency order. Tasks 1–2 land the body-shape change end-to-end (function + call-site swap) so the visible improvement ships before any overlay work. Tasks 3–5 are the overlay sizing/wrap path. Task 6 verifies and documents.

```
Task 1: diff_lines (pure fn + tests)
    │
    ▼
Task 2: hook diff_lines into app.rs call site
    │
    ▼  (visible improvement complete — overlay still 50 cols)
    │
Task 3: wrap_diff_line helper (pure fn + tests)
    │
    ▼
Task 4: dynamic overlay width
    │
    ▼
Task 5: wire wrap into render loop
    │
    ▼  (overflow & cross-dir cases now render readably)
    │
Task 6: verify + CLAUDE.md + move plan
```

## Technical Details

**`diff_lines` per-pair rule:**

```rust
pub fn diff_lines(run: &RenameRun) -> Vec<String> {
    run.renames.iter().map(|(from, to)| render_pair(from, to)).collect()
}

fn render_pair(from: &Path, to: &Path) -> String {
    let same_parent = match (from.parent(), to.parent()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if same_parent {
        let f = from.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let t = to.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        format!("{f} → {t}")
    } else {
        format!("{} → {}", from.display(), to.display())
    }
}
```

**Dynamic-width calculation (replaces src/app/overlay/confirm.rs:156):**

```rust
let max_w = (screen.width as u32 * 8 / 10) as u16;
let cancel_text  = " [ Cancel ] ";
let confirm_text = format!(" [ {} ] ", self.confirm_label);
let content_w = self.body.iter()
    .map(|l| l.chars().count() as u16)
    .max().unwrap_or(0).saturating_add(4);
let title_w  = (self.title.chars().count() as u16).saturating_add(4);
let buttons_w = ((cancel_text.chars().count()
                + confirm_text.chars().count()) as u16).saturating_add(4);
let want_w = content_w.max(title_w).max(buttons_w).max(40).min(max_w);
let area = frame::centered_rect(screen, want_w, want_h);
```

**Soft-wrap helper signature:**

```rust
fn wrap_diff_line(line: &str, inner_width: usize) -> Vec<String>
```

- Fits → `vec![line.into()]`.
- Doesn't fit and contains `" → "` → splits at the first occurrence, returns `vec![from, format!("{indent}→ {to}")]` where `indent = " ".repeat(min(arrow_col, 10))` and `arrow_col` is the char-count of the prefix.
- Doesn't fit and has no arrow → `vec![line.into()]` (the existing tail-truncate path handles it — back-compat for non-rename bodies).

**Render-loop integration**: produce `rendered: Vec<String> = self.body.iter().flat_map(|l| wrap_diff_line(l, inner_width)).collect()` once at the top of the body section. `visible_body_rows` stays the same; `max_scroll` uses `rendered.len()` not `self.body.len()`. The `for row in 0..visible` loop indexes into `rendered` instead of `self.body`.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): all code + tests + plan-file move. All achievable inside this repo.
- **Post-Completion** (no checkboxes): manual TUI smoke verification across the cases the dynamic width / wrap path is meant to fix.

## Implementation Steps

### Task 1: Add `bulk_rename::diff_lines` pure function

**Files:**
- Modify: `src/bulk_rename/mod.rs` (add `pub fn diff_lines` + tests)

**Step 1: Write the failing tests**

Add to the `tests` module in `src/bulk_rename/mod.rs`:

```rust
#[test]
fn diff_lines_same_parent_uses_basenames() {
    let run = RenameRun {
        renames: vec![
            (PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/a-v2.txt")),
            (PathBuf::from("/tmp/b.txt"), PathBuf::from("/tmp/b-v2.txt")),
        ],
    };
    assert_eq!(
        diff_lines(&run),
        vec!["a.txt → a-v2.txt".to_string(), "b.txt → b-v2.txt".to_string()],
    );
}

#[test]
fn diff_lines_cross_dir_uses_full_paths() {
    let run = RenameRun {
        renames: vec![(PathBuf::from("/tmp/src/c"), PathBuf::from("/tmp/dst/c"))],
    };
    assert_eq!(diff_lines(&run), vec!["/tmp/src/c → /tmp/dst/c".to_string()]);
}

#[test]
fn diff_lines_mixed_set_per_line_decision() {
    let run = RenameRun {
        renames: vec![
            (PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/a-v2.txt")),
            (PathBuf::from("/tmp/src/c"), PathBuf::from("/tmp/dst/c")),
        ],
    };
    assert_eq!(
        diff_lines(&run),
        vec![
            "a.txt → a-v2.txt".to_string(),
            "/tmp/src/c → /tmp/dst/c".to_string(),
        ],
    );
}

#[test]
fn diff_lines_root_no_parent_falls_back_to_full_path() {
    let run = RenameRun {
        renames: vec![(PathBuf::from("/a"), PathBuf::from("/b"))],
    };
    // both have parent `/`, so same-parent — basenames are `a` and `b`.
    assert_eq!(diff_lines(&run), vec!["a → b".to_string()]);
}

#[test]
fn diff_lines_empty_run() {
    let run = RenameRun { renames: vec![] };
    assert!(diff_lines(&run).is_empty());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib bulk_rename::tests::diff_lines`
Expected: FAIL — `diff_lines` not defined.

**Step 3: Write minimal implementation**

Add to `src/bulk_rename/mod.rs` (between `plan` and `apply` is a natural spot):

```rust
/// Build the confirm-modal body lines for a planned `RenameRun`.
///
/// Per-pair rule: if `from.parent() == to.parent()`, render basenames
/// only (the common case — rename in place); otherwise render full
/// paths on both sides (cross-directory move via absolute edited path).
/// Non-UTF-8 basenames fall back to empty strings; callers that need
/// stricter handling should consume the `RenameRun` directly.
pub fn diff_lines(run: &RenameRun) -> Vec<String> {
    run.renames.iter().map(|(from, to)| render_pair(from, to)).collect()
}

fn render_pair(from: &Path, to: &Path) -> String {
    let same_parent = match (from.parent(), to.parent()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if same_parent {
        let f = from.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let t = to.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        format!("{f} → {t}")
    } else {
        format!("{} → {}", from.display(), to.display())
    }
}
```

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib bulk_rename`
Expected: PASS for all `diff_lines_*` tests and all existing bulk_rename tests.

- [ ] write the five `diff_lines_*` tests in src/bulk_rename/mod.rs#tests
- [ ] run `cargo test --lib bulk_rename::tests::diff_lines` — FAIL
- [ ] add `pub fn diff_lines` + private `render_pair` to src/bulk_rename/mod.rs
- [ ] run `cargo test --lib bulk_rename` — all PASS
- [ ] run `cargo test` — full suite green before Task 2

### Task 2: Swap call site to use `diff_lines`

**Files:**
- Modify: `src/app/app.rs:790-794`

The existing inline `format!` loop collapses to a single call.

**Step 1: Confirm there is no app-layer assertion on the body shape**

Run: `grep -n 'from.display()\|to.display()\|→' src/app/app.rs src/app/overlay/confirm.rs`
Expected: only the literal at src/app/app.rs:793 (the line being replaced). If anything else surfaces (a test pinning the old shape), update it in the same task.

**Step 2: Replace the inline builder**

In `src/app/app.rs`, locate the block at lines 789-794:

```rust
let count = run.renames.len();
let body: Vec<String> = run
    .renames
    .iter()
    .map(|(from, to)| format!("{} → {}", from.display(), to.display()))
    .collect();
```

Replace with:

```rust
let count = run.renames.len();
let body = bulk_rename::diff_lines(&run);
```

(`bulk_rename` is already imported at src/app/app.rs:5.)

**Step 3: Run full test suite**

Run: `cargo test`
Expected: PASS. No new tests required at this layer — Task 1's coverage proves the function; the call-site swap is mechanical.

- [ ] grep for body-shape assertions across the crate (none expected)
- [ ] replace the inline `format!` loop at src/app/app.rs:790-794 with `bulk_rename::diff_lines(&run)`
- [ ] run `cargo build` — clean
- [ ] run `cargo test` — full suite PASS before Task 3
- [ ] manual smoke (optional but encouraged): build, stage 2 files in same dir, F2, confirm overlay now shows basenames

### Task 3: Add `wrap_diff_line` helper

**Files:**
- Modify: `src/app/overlay/confirm.rs` (new private fn + tests)

**Step 1: Write the failing tests**

Add to the `tests` module in `src/app/overlay/confirm.rs`:

```rust
#[test]
fn wrap_diff_line_fits_returns_single() {
    let out = wrap_diff_line("a → b", 40);
    assert_eq!(out, vec!["a → b".to_string()]);
}

#[test]
fn wrap_diff_line_splits_on_first_arrow() {
    // "long-from → long-to" — total 19 chars, inner_width 12 → must wrap.
    let out = wrap_diff_line("long-from → long-to", 12);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], "long-from");
    // Continuation indented by min(arrow_col=10, 10) = 10 spaces, then "→ to".
    assert!(out[1].starts_with("          → "), "got: {:?}", out[1]);
    assert!(out[1].ends_with("long-to"));
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
fn wrap_diff_line_only_first_arrow_splits() {
    // Two arrows in input — split on the FIRST.
    let out = wrap_diff_line("a → b → c", 5);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0], "a");
    assert_eq!(out[1], "  → b → c");
}
```

**Step 2: Run tests — FAIL**

Run: `cargo test --lib confirm::tests::wrap_diff_line`
Expected: FAIL — `wrap_diff_line` not defined.

**Step 3: Implement the helper**

Add near the existing `truncate_with_ellipsis` at the bottom of `src/app/overlay/confirm.rs`:

```rust
/// Soft-wrap a diff line that doesn't fit `inner_width`.
///
/// If the line fits, returns it as the single element. If it doesn't
/// fit and contains `" → "`, splits at the first occurrence: the prefix
/// becomes the first row, and the continuation row is the arrow plus
/// the suffix, indented to align under the arrow (capped at 10 cols).
/// If the line doesn't fit but has no arrow, returns it unchanged —
/// non-rename body callers fall back to the existing tail-truncate
/// path in `render`.
fn wrap_diff_line(line: &str, inner_width: usize) -> Vec<String> {
    if line.chars().count() <= inner_width {
        return vec![line.to_string()];
    }
    let Some(byte_idx) = line.find(" → ") else {
        return vec![line.to_string()];
    };
    let (from_part, rest) = line.split_at(byte_idx);
    // `rest` begins with " → "; strip the leading space to land "→ to".
    let to_part = rest.trim_start();
    let arrow_col = from_part.chars().count().min(10);
    let indent = " ".repeat(arrow_col);
    vec![from_part.to_string(), format!("{indent}{to_part}")]
}
```

**Step 4: Run tests — PASS**

Run: `cargo test --lib confirm::tests::wrap_diff_line`
Expected: all five tests PASS.

- [ ] add the five `wrap_diff_line_*` tests to src/app/overlay/confirm.rs#tests
- [ ] run `cargo test --lib confirm::tests::wrap_diff_line` — FAIL
- [ ] add `wrap_diff_line` private fn to src/app/overlay/confirm.rs
- [ ] run `cargo test --lib confirm` — PASS
- [ ] run `cargo test` — full suite green before Task 4

### Task 4: Replace fixed overlay width with content-driven sizing

**Files:**
- Modify: `src/app/overlay/confirm.rs:155-156` (the geometry block in `render`)

**Step 1: Write the failing tests**

Add to the `tests` module in `src/app/overlay/confirm.rs`. These read the rendered bytes and verify width by locating the top-left `╭` and top-right `╮` corners:

```rust
/// Helper: render to a sink-backed buffer and return the unflushed bytes.
fn render_capture(o: &ConfirmOverlay, screen: Area) -> String {
    let palette = StyleMap::no_term();
    let mut wbuf = std::io::BufWriter::with_capacity(64 * 1024, std::io::sink());
    o.render(&mut wbuf, screen, &palette).unwrap();
    String::from_utf8_lossy(&wbuf.buffer().to_vec()).into_owned()
}

#[test]
fn render_width_grows_to_content() {
    // 60-char body line in an 80-col screen — modal width must exceed 50.
    let long = "a".repeat(60);
    let o = ConfirmOverlay::new("t", vec![long], "OK", false, cmd());
    let screen = Area::new(0, 0, 80, 24);
    let s = render_capture(&o, screen);
    // The modal's width is encoded in the gap between '╭' and '╮' on the
    // top frame row. Their column positions appear in `cursor::MoveTo`
    // sequences; assert both are present and far enough apart.
    assert!(s.contains('╭'));
    assert!(s.contains('╮'));
    // Indirect check: the long content line is present in the body.
    assert!(s.contains(&"a".repeat(40)),
        "body content should be wider than the old 50-col modal allowed");
}

#[test]
fn render_width_clamps_to_screen() {
    // 200-char line in an 80-col screen — modal width must NOT exceed
    // 80% of screen (64 cols). We verify by ensuring NOT all 200 chars
    // render on a single body row.
    let long = "x".repeat(200);
    let o = ConfirmOverlay::new("t", vec![long.clone()], "OK", false, cmd());
    let screen = Area::new(0, 0, 80, 24);
    let s = render_capture(&o, screen);
    assert!(
        !s.contains(&"x".repeat(70)),
        "body line must be clipped or wrapped within the 64-col cap"
    );
}

#[test]
fn render_width_has_floor_for_short_body() {
    // Single 1-char body — modal must still render and not collapse.
    let o = ConfirmOverlay::new("t", vec!["a".to_string()], "OK", false, cmd());
    let screen = Area::new(0, 0, 80, 24);
    let s = render_capture(&o, screen);
    // Both buttons must fit alongside each other → modal ≥ 40 cols.
    assert!(s.contains("Cancel"));
    assert!(s.contains("OK"));
}

#[test]
fn render_no_panic_at_tiny_screen() {
    // 20x10 screen — width calc shouldn't panic. Bails via the existing
    // `area.width < 8 || area.height < 5` guard at line 157.
    let o = ConfirmOverlay::new("t", vec!["a → b".to_string()], "OK", false, cmd());
    let screen = Area::new(0, 0, 20, 10);
    let _ = render_capture(&o, screen); // must not panic
}
```

**Step 2: Run tests — FAIL**

Run: `cargo test --lib confirm::tests::render_width`
Expected: at least `render_width_grows_to_content` and `render_width_clamps_to_screen` FAIL (current code always uses 50).

**Step 3: Replace the geometry block**

At src/app/overlay/confirm.rs:155-156, replace:

```rust
let want_h: u16 = (self.body.len() as u16).saturating_add(5).min(15);
let area = frame::centered_rect(screen, 50, want_h);
```

with:

```rust
let want_h: u16 = (self.body.len() as u16).saturating_add(5).min(15);

let max_w = (screen.width as u32 * 8 / 10) as u16;
let cancel_text  = " [ Cancel ] ";
let confirm_text_for_size = format!(" [ {} ] ", self.confirm_label);
let content_w = self.body.iter()
    .map(|l| l.chars().count() as u16)
    .max().unwrap_or(0).saturating_add(4);
let title_w  = (self.title.chars().count() as u16).saturating_add(4);
let buttons_w = ((cancel_text.chars().count()
                + confirm_text_for_size.chars().count()) as u16)
                .saturating_add(4);
let want_w = content_w.max(title_w).max(buttons_w).max(40).min(max_w);
let area = frame::centered_rect(screen, want_w, want_h);
```

Note: `cancel_text` and `confirm_text` are already defined further down (lines 218-219). Use a different local name (`confirm_text_for_size`) at the top to avoid shadowing, OR move the existing definitions up. Pick whichever is cleaner during implementation.

**Step 4: Run tests — PASS**

Run: `cargo test --lib confirm`
Expected: all new width tests PASS; existing tests (`render_writes_corners_and_title_and_buttons`, `render_caches_button_hits`, `render_clamps_oversize_body`) continue to PASS.

- [ ] add the four `render_width_*` / `render_no_panic_at_tiny_screen` tests + `render_capture` helper to src/app/overlay/confirm.rs#tests
- [ ] run `cargo test --lib confirm::tests::render_width` — FAIL
- [ ] replace the fixed-50 geometry at src/app/overlay/confirm.rs:155-156 with content-driven calc
- [ ] run `cargo test --lib confirm` — PASS
- [ ] run `cargo test` — full suite green before Task 5

### Task 5: Wire `wrap_diff_line` into the render body loop

**Files:**
- Modify: `src/app/overlay/confirm.rs` (the body section of `render`, currently lines 180-210, and `max_scroll` clamp at 182)

**Step 1: Write the failing tests**

```rust
#[test]
fn render_wraps_long_diff_line() {
    // Single rename line that exceeds the 80%-of-80 = 64 col cap.
    // After wrap, render bytes must contain a "→ " on a row that is
    // NOT the first body row.
    let from = "/tmp/very-long-source-dir/quite-long-name.txt";
    let to   = "/tmp/very-long-archive-dir/quite-long-name-renamed-v2.txt";
    let line = format!("{from} → {to}");
    let o = ConfirmOverlay::new("t", vec![line.clone()], "OK", false, cmd());
    let screen = Area::new(0, 0, 80, 24);
    let s = render_capture(&o, screen);
    // The `from` and `to` substrings must both appear in the output
    // (proving no part is silently truncated past visibility).
    assert!(s.contains(from), "from path missing from render: {s:?}");
    assert!(s.contains("quite-long-name-renamed-v2.txt"),
        "to path tail missing — wrap did not happen: {s:?}");
}

#[test]
fn render_scroll_works_with_wrapped_lines() {
    // 10 long lines → after wrap each produces 2 rendered rows = 20 rows.
    // Visible window is only ~10 rows. Scroll must clamp to the rendered
    // count, not the body length.
    let from = "/tmp/very-long-source-dir/quite-long-name.txt";
    let to   = "/tmp/very-long-archive-dir/quite-long-name-renamed-v2.txt";
    let line = format!("{from} → {to}");
    let body: Vec<String> = (0..10).map(|_| line.clone()).collect();
    let mut o = ConfirmOverlay::new("t", body, "OK", false, cmd());
    // Press down many times; scroll must clamp without panicking.
    for _ in 0..100 {
        let _ = o.handle_key(key!(down));
    }
    let screen = Area::new(0, 0, 80, 24);
    let s = render_capture(&o, screen);
    // Last rendered row should still contain content (post-clamp).
    assert!(s.contains("quite-long-name-renamed-v2.txt"));
}
```

**Step 2: Run tests — FAIL**

Run: `cargo test --lib confirm::tests::render_wraps_long_diff_line confirm::tests::render_scroll_works_with_wrapped_lines`
Expected: `render_wraps_long_diff_line` FAILs — without wrap wired in, the `to` tail is tail-truncated.

**Step 3: Replace the body iteration**

Locate the body section at src/app/overlay/confirm.rs:180-210 (starting `// ---- body ----`). Replace with this structure:

```rust
// ---- body -------------------------------------------------------
let inner_width = area.width.saturating_sub(4) as usize;
let rendered: Vec<String> = self.body.iter()
    .flat_map(|l| wrap_diff_line(l, inner_width))
    .collect();

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
    let is_last_visible = row + 1 == visible || body_idx + 1 == total;
    let display = if overflow && is_last_visible && body_idx + 1 < total {
        truncate_with_ellipsis(line, inner_width.saturating_sub(2))
    } else {
        truncate_to_width(line, inner_width)
    };
    body_style.queue_str(w, display).map_err(io_err)?;
}
```

Key changes vs current:
- New `inner_width` computed once at the top.
- New `rendered: Vec<String>` produced via `flat_map(wrap_diff_line)`.
- `max_scroll` and `overflow` use `rendered.len()` (was `self.body.len()`).
- The render loop indexes `rendered` (was `self.body`).

The `handle_key(down)` clamp stays approximate (`self.body.len() - 1`); `render` re-clamps via `max_scroll(rendered.len(), visible)`. This matches the existing pattern documented at src/app/overlay/confirm.rs:307-308.

**Step 4: Run tests — PASS**

Run: `cargo test --lib confirm`
Expected: new wrap tests PASS, scroll tests PASS, all existing tests (including `render_after_scroll_renders_later_body_lines`, `down_clamps_at_body_end`, `render_clamps_oversize_body`) PASS.

- [ ] add the two new tests (`render_wraps_long_diff_line`, `render_scroll_works_with_wrapped_lines`) to src/app/overlay/confirm.rs#tests
- [ ] run those tests — FAIL
- [ ] replace the body section at src/app/overlay/confirm.rs:180-210 with the wrap-aware version
- [ ] run `cargo test --lib confirm` — PASS (all 22+ tests)
- [ ] run `cargo test` — full suite PASS before Task 6

### Task 6: Verify acceptance criteria + update CLAUDE.md + move plan

**Files:**
- Modify: `CLAUDE.md` (add note to the overlay-routing section about dynamic width)
- Move: `docs/plans/2026-05-12-bulk-rename-diff-display.md` → `docs/plans/completed/`
- Move: `docs/plans/2026-05-12-bulk-rename-diff-display-design.md` → `docs/plans/completed/`

**Acceptance criteria (from Overview):**

- [ ] verify Goal 1 — new filename always visible: stage 2 same-dir files, F2, edit names, confirm overlay shows full basenames without truncation
- [ ] verify Goal 2 — common case compact: same-dir rename → basenames only, no `/tmp/.../` prefix
- [ ] verify Goal 3 — cross-dir case readable: edit one row to an absolute path in another directory, confirm shows full paths (wrapped if too long)
- [ ] verify Goal 4 — generic improvement: stage 2 files and trigger `:trash` — bulk-stage confirm now uses dynamic width (not 50 cols)

**Final tests:**

- [ ] run `cargo build --all-targets` — clean
- [ ] run `cargo test` — full suite PASS
- [ ] run `cargo clippy --all-targets -- -D warnings` if the project enables clippy in CI (check Cargo.toml / CI config)

**Documentation updates:**

- [ ] in `CLAUDE.md` under "Overlay routing — single field, single render hook, single key hook", add a paragraph noting: `ConfirmOverlay` modals now size their width to content (floor 40 cols, cap 80% of screen). Long diff lines containing ` → ` soft-wrap onto an indented continuation row; lines without an arrow fall back to tail truncation. Body iteration in `render` walks rendered rows (post-wrap), not raw `body` entries — `max_scroll` operates on the post-wrap count.
- [ ] update README.md only if it mentions modal sizing (it likely doesn't — quick grep `rg -i 'modal|confirm' README.md` to check)

**Plan housekeeping:**

- [ ] `mkdir -p docs/plans/completed`
- [ ] `git mv docs/plans/2026-05-12-bulk-rename-diff-display.md docs/plans/completed/`
- [ ] `git mv docs/plans/2026-05-12-bulk-rename-diff-display-design.md docs/plans/completed/`

## Post-Completion

*Items requiring manual TUI verification — no checkboxes, informational only.*

**Manual smoke tests** (build with `cargo build --release` first):

- **Same-dir rename (the typical case)**: `cd /tmp && mkdir -p broot-bulk && cd broot-bulk && touch a.txt b.txt c.txt`, launch broot, stage all three, F2, rename to `a-v2.txt`/`b-v2.txt`/`c-v2.txt`, verify confirm overlay shows:
  ```
  a.txt → a-v2.txt
  b.txt → b-v2.txt
  c.txt → c-v2.txt
  ```
  with no truncation.

- **Long-name same-dir**: stage 2 files with ~60-char basenames; verify overlay grows wide enough to show both names in full (or wraps gracefully if screen narrow).

- **Cross-dir move via absolute path**: `mkdir /tmp/broot-bulk-dst`, stage `/tmp/broot-bulk/{a,b}.txt`, F2, edit second line to `/tmp/broot-bulk-dst/b.txt`, verify confirm shows:
  ```
  a.txt → a.txt        (or whatever the in-place rename is)
  /tmp/broot-bulk/b.txt → /tmp/broot-bulk-dst/b.txt
  ```
  full paths on the cross-dir row, basenames on the same-dir row.

- **Long cross-dir path forcing wrap**: stage `/tmp/very-long-source-directory-name/file.txt`, edit to `/tmp/very-long-archive-directory-name/file-archived.txt`, verify the `to` half wraps onto a continuation line indented under the `→`.

- **Tiny terminal**: shrink terminal to ~30 cols wide, trigger any confirm overlay, verify no panic (the existing `area.width < 8` early-return still fires for the truly-tiny case).

- **Backwards compat — other confirm callers**: trigger `:rm` on a single file (per-verb destructive confirm), trigger bulk `:trash` on 2+ staged files (bulk-stage confirm), and trigger `:mv` with an overwrite (overwrite confirm). Each should render at >= 40 cols and look at least as good as before — no truncation regression.

**External system updates**: none. This is a pure-render change; no config schema, no public verb, no keybinding, no on-disk file format affected.
