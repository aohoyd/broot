# Bulk-rename confirm modal: readable diff display

Status: design approved 2026-05-12 (brainstorm), pending breakdown.

## Problem

The bulk-rename confirm overlay renders one row per rename as
`"{from.display()} → {to.display()}"` with absolute paths on both sides
(src/app/app.rs:790-794) and the `ConfirmOverlay` paints that into a
**fixed 50-column** modal (src/app/overlay/confirm.rs:155-208). The
right side gets truncated with `…`, hiding the actual new filename —
the one piece of information the user is being asked to verify.

```text
╭ Rename 2 files? ───────────────────────────────╮
│ /private/tmp/broot-bulk-test/alpha.txt → /pri… │  ← new name hidden
│ /private/tmp/broot-bulk-test/beta.txt → /priv… │
│                                                │
│ [ Cancel ]                          [ Rename ] │
╰────────────────────────────────────────────────╯
```

## Goals

1. The new filename is **always visible** on screen before the user
   confirms.
2. The common case (rename in place) renders compactly — basenames
   only, no path noise.
3. The cross-directory case (absolute path on the `to` side, which
   `bulk_rename::plan` accepts) doesn't silently lose information.
4. The fix is generic enough that other long-body confirm callers
   (none today, but possible — `cp`/`mv` overwrite, future bulk
   operations) benefit automatically.

## Non-goals

- **No char-level diff highlighting.** Considered and declined during
  brainstorm — the basename pair is already visually obvious at the
  widths we'll be rendering at, and inline styled segments would
  require restructuring `ConfirmOverlay::body: Vec<String>` into a
  styled-segment shape.
- **No arrow alignment across rows.** Each row stands alone; aligning
  the `→` column across rows would require a two-pass body builder
  and ragged-right rendering when widths vary.
- **No `to`-side-only emphasis or color.** The pair is plain text on
  both sides — same style as today.

## Design

Two independent changes that compose.

### 1. `bulk_rename::diff_lines(run: &RenameRun) -> Vec<String>`

New pure function in `src/bulk_rename/mod.rs`, sibling to
`serialize` / `parse` / `plan` / `apply`. Builds the body lines for
the confirm overlay.

Per-line rule:

```rust
fn render_pair(from: &Path, to: &Path) -> String {
    let same_parent = match (from.parent(), to.parent()) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    };
    if same_parent {
        let f = from.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let t = to.file_name().and_then(|s| s.to_str()).unwrap_or("");
        format!("{f} → {t}")
    } else {
        format!("{} → {}", from.display(), to.display())
    }
}
```

- Same parent → basenames only.
- Different parent (or any side has no parent) → full paths.
- Non-UTF-8 basenames fall back to `path.display()` form (the
  `to_str()` guard).

Call site at src/app/app.rs:790-794 collapses to:

```rust
let body = bulk_rename::diff_lines(&run);
```

**Tests** in `src/bulk_rename/mod.rs#tests`:

- `diff_lines_same_parent_uses_basenames` — `[/tmp/a.txt → /tmp/a-v2.txt]`
  → `["a.txt → a-v2.txt"]`.
- `diff_lines_cross_dir_uses_full_paths` — `[/tmp/src/c → /tmp/dst/c]`
  → `["/tmp/src/c → /tmp/dst/c"]`.
- `diff_lines_mixed_set_per_line_decision` — two same-parent + one
  cross-dir → mixed output.
- `diff_lines_root_no_parent_falls_back_to_full_path` — `["/a → /b"]`.
- `diff_lines_empty_run` → `vec![]`.

### 2. `ConfirmOverlay`: dynamic width + soft wrap

Two edits to `src/app/overlay/confirm.rs`.

**2a. Dynamic width.** Replace the fixed `50` at line 156 with a
content-driven calculation:

```rust
let max_w = (screen.width as u32 * 8 / 10) as u16;            // 80% cap
let content_w = self.body.iter()
    .map(|l| l.chars().count() as u16)
    .max().unwrap_or(0)
    .saturating_add(4);                                        // L/R padding
let title_w  = self.title.chars().count() as u16 + 4;
let cancel_text  = " [ Cancel ] ";
let confirm_text = format!(" [ {} ] ", self.confirm_label);
let buttons_w = (cancel_text.chars().count()
                + confirm_text.chars().count()) as u16 + 4;
let want_w = content_w.max(title_w).max(buttons_w).max(40);   // floor 40
let want_w = want_w.min(max_w);
let area = frame::centered_rect(screen, want_w, want_h);
```

- Floor 40 prevents collapse for short rm/trash prompts.
- 80%-of-screen cap keeps the modal from going edge-to-edge.
- `content_w` accounts for the longest body line; short bodies stay
  compact.
- This applies to **every** caller of `ConfirmOverlay`. Short ones
  (rm of a single path) stay near 40 cols; bulk-rename grows to fit
  basenames; bulk-rename across dirs grows up to the cap.

**2b. Soft wrap for overflowing lines.** Add a helper:

```rust
fn wrap_diff_line(line: &str, inner_width: usize) -> Vec<String>
```

Logic:

1. If `line.chars().count() <= inner_width` → return `vec![line.to_string()]`.
2. Find the **first** occurrence of `" → "` in `line`.
   - If found at byte position `p`, split at `p`. Let
     `arrow_col = chars().count()` of the prefix. Indent the
     continuation by `min(arrow_col, 10)` spaces, then `"→ "`, then
     the `to` part.
   - If not found (line has no arrow — body isn't a rename
     diff), return `vec![line.to_string()]` and let the existing
     tail-truncate path handle it (back-compat for other callers).
3. The continuation line itself can still overflow — if so, it's
   tail-truncated by the existing `truncate_to_width` path. At 80%
   screen width this is rare.

Wrapping happens inside `render`, after `want_w` is decided. The
**body iteration** (currently `for row in 0..visible`) now walks a
pre-computed `Vec<String>` of rendered rows produced from
`self.body` via `flat_map(wrap_diff_line(_, inner_width))`. The
existing scroll math (`max_scroll`, `visible_body_rows`,
`overflow` ellipsis on the last visible row) operates on this
rendered-rows vec instead of `self.body` directly.

Caching: rendered rows are derived in each `render` call from the
already-resolved `inner_width`. No need to stash on `self` — render
is the only consumer and it's cheap.

**Scroll-handler clamp:** `handle_key` for `key!(down)` currently
clamps `self.scroll` against `self.body.len() - 1`. After wrapping,
the real maximum is the rendered-row count, which `handle_key`
doesn't know without an area. Keep the existing approximate clamp
(`body.len()`); `render` re-clamps to the actual rendered window via
`max_scroll`. This is the same pattern as today and the existing
test `down_clamps_at_body_end` keeps passing.

**Tests** added to `src/app/overlay/confirm.rs#tests`:

- `render_width_grows_to_content` — body with one 60-char line, 80x24
  screen → render bytes show `╭` at column < 10 and `╮` at column > 60
  (modal grew past 50).
- `render_width_clamps_to_screen` — body with a 200-char line, 80x24
  screen → modal width ≤ 64 (= 80% of 80).
- `render_width_has_floor_for_short_body` — body `["a"]`, screen 80x24
  → modal width ≥ 40 (didn't shrink to 5).
- `wrap_diff_line_fits_returns_single` — short line, generous width →
  single-element vec.
- `wrap_diff_line_splits_on_first_arrow` — `"long-from → long-to"` at
  narrow width → two-element vec, second starts with spaces then `"→ "`.
- `wrap_diff_line_no_arrow_unchanged` — `"plain string"` → single
  element regardless of width (back-compat for non-rename bodies).
- `render_wraps_long_diff_line` — body of one over-wide rename line at
  80x24 screen → render bytes contain `→ ` at start of a row other
  than the first.
- `render_scroll_works_with_wrapped_lines` — mix of fitting and
  wrapping lines, `down` scroll → next render shows post-wrap content.

## Files touched

- `src/bulk_rename/mod.rs` — add `diff_lines` + 5 tests.
- `src/app/app.rs:790-794` — swap the body builder to
  `bulk_rename::diff_lines(&run)`.
- `src/app/overlay/confirm.rs` — replace fixed-width geometry with
  content-driven width; add `wrap_diff_line` helper; switch render
  loop to iterate wrapped rows. 8 new tests.

No public API changes outside `bulk_rename` (which gains one new
exported function). No `Command` enum or `ConfirmOverlay` field
additions.

## Risks / open questions

- **Width discoverability:** other callers of `ConfirmOverlay` (the
  bulk-stage confirm and the `cp`/`mv` overwrite prompts) will start
  growing too. For their typical bodies (one or two short paths) the
  growth is small (~50-60 cols vs 50 today). Worth a manual sanity
  check during implementation.
- **`screen.width < 40`:** the floor (`max(40)`) wins, then
  `centered_rect` clips. The existing `area.width < 8` early-return
  at line 157 still catches the truly-tiny case. New `width_floor`
  test should also assert no panic at `screen = 20x10`.
- **Inner-width math after wrap:** `inner_width` is currently
  `area.width.saturating_sub(4) as usize`. Wrapping must use the
  same value so the continuation line aligns correctly. The
  rendered-rows pass takes `inner_width` as input.

## CLAUDE.md updates

If this lands, two CLAUDE.md sections need a note:

- **Overlay routing** — mention that `ConfirmOverlay` now sizes to
  content (40 col floor, 80% screen cap) instead of fixed 50.
- (Implicit) — the existing overlay invariants (single field, single
  render hook, single key hook) are unchanged. No new entry needed
  there, but the width invariant is worth recording.
