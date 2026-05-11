# Frame title as selectable root

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Make the panel frame title behave as the visual representation of the (hidden) tree root row in `BrowserState`: it highlights with the body's selection bg when the root is selected, and clicking it sets `selection = 0`.

**Architecture:** Three small additions — a default-`false` trait hook on `PanelState`, a `selected: bool` parameter on `draw_frame_title`, and a click special-case in `BrowserState::on_click`. No new style keys, no geometry changes, no changes to `tree.lines[0]` data model.

**Tech Stack:** Rust, crossterm (cursor/style), termimad (`CompoundStyle`, `Area`).

## Overview

In broot's current UI the root row is rendered exclusively in the frame title (the body iterates `tree.lines[1..]`). But `tree.selection == 0` still means "root selected" — reached via `line_up` from the first child, or `Internal::back`. Today this state has no visual cue: the title looks identical whether root is selected or not, and clicking the title is inert.

This change closes that gap:
1. When `displayed_tree().selection == 0`, the title is drawn with the body's `selected_line` bg overlaid on the existing `frame_title` style (fg/attrs preserved).
2. Clicking the title row (`y == body_top - 1`) sets `selection = 0`, matching the "title is the root element" mental model.

Scope is `BrowserState` only; other panel types are unaffected (the trait default is `false`).

## Context (from discovery)

- `src/app/panel_state.rs:1148-1162` — `frame_title` default trait method; pattern to follow for the new `is_title_selected` hook.
- `src/browser/browser_state.rs:309-320` — `BrowserState::on_click`. Calls `body_relative_y(y)` then `try_select_y`. We add a title-row special case before that.
- `src/browser/browser_state.rs:31-34` — `body_top` cached during `display`. `body_top - 1 == state_outer.top == title row`.
- `src/display/frame.rs:128-148` — `draw_frame_title`. Add `selected: bool`; overlay `selected_line.bg` onto `frame_title` style when true.
- `src/app/app_panels.rs:707-713` — single production caller of `draw_frame_title`. Pass `panel.state().is_title_selected()`.
- `src/skin/style_map.rs` — `frame_title` and `selected_line` keys already exist; no new entries.
- Tests with current `draw_frame_title` signature: 5 in `src/display/frame.rs` (`draw_frame_title_writes_title_into_buffer`, `..._emits_sgr_when_palette_is_styled`, `..._skips_empty_or_narrow`, `draw_frame_then_title_combined`).

Reference design: `docs/plans/2026-05-11-title-as-root-design.md`.

## Development Approach

- **testing approach**: TDD (tests first → verify fail → implement → verify pass)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run `cargo test --all-features` after each change
- maintain backward compatibility (user `skin:` overrides without `selected_line` bg must still work)

## Testing Strategy

- **unit tests**: required for every task. Pin behaviour, not bytes — use the existing `StyleMap::no_term()` for unstyled and the `SkinEntry::parse("Yellow None Bold")` trick for styled assertions.
- broot has no UI-based e2e suite. Visual smoke testing happens manually in Post-Completion.
- Treat `cargo test --all-features` as the gate. Current count should grow by ~4 tests.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

Five tasks, ordered so each provides a self-contained building block:

1. **Trait hook + BrowserState override** for `is_title_selected`. Behavioural test on `BrowserState`.
2. **Frame title render** — add `selected: bool` to `draw_frame_title`; styled bg overlay when true.
3. **Click handling** — title row sets `selection = 0`; underflow-safe.
4. **Wire it together** in `display_panels`. Pure plumbing; verified by the full test suite + smoke.
5. **Docs** — CLAUDE.md note + move plan to completed.

## Technical Details

### `is_title_selected` trait method

```rust
fn is_title_selected(&self) -> bool { false }
```

`BrowserState` override:

```rust
fn is_title_selected(&self) -> bool {
    self.displayed_tree().selection == 0
}
```

### `draw_frame_title` signature change

```rust
pub(crate) fn draw_frame_title<W: Write>(
    w: &mut W,
    area: Area,
    palette: &StyleMap,
    title: &str,
    selected: bool,
) -> io::Result<()>;
```

When `selected == true`, clone `palette.frame_title`, overlay `palette.selected_line.get_bg()` onto it. If `selected_line` has no bg, fall back to the unstyled `frame_title` look.

### Click handler extension

```rust
if self.body_top > 0 && y == self.body_top - 1 {
    self.displayed_tree_mut().selection = 0;
    return Ok(CmdResult::Keep);
}
```

The `body_top > 0` guard prevents underflow on degenerate tiny terminals where the frame collapses (`state_area.top == 0`).

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): Rust code changes + tests.
- **Post-Completion** (no checkboxes): manual smoke test (resize, filter mode, multi-panel).

## Implementation Steps

### Task 1: Add `is_title_selected` trait hook + `BrowserState` override

**Files:**
- Modify: `src/app/panel_state.rs`
- Modify: `src/browser/browser_state.rs`

**Step 1: Write failing tests**

In `src/browser/browser_state.rs` (`#[cfg(test)] mod tests`):

```rust
#[test]
fn is_title_selected_true_when_root_selected() {
    let state = fake_browser_state_with_children(3, 4);
    // default selection in fake_browser_state_with_children is 0
    assert!(state.is_title_selected());
}

#[test]
fn is_title_selected_false_when_child_selected() {
    let mut state = fake_browser_state_with_children(3, 4);
    state.displayed_tree_mut().selection = 2;
    assert!(!state.is_title_selected());
}
```

**Step 2: Run tests to verify failure**

Run: `cargo test --all-features is_title_selected`
Expected: FAIL — `is_title_selected` method does not exist.

**Step 3: Write minimal implementation**

In `src/app/panel_state.rs`, near `frame_title` (around line 1162) and `status_aux` (around line 1173), add the default trait method:

```rust
/// True when the panel's frame title should render with the
/// body's selection background, signalling that the (hidden)
/// root row is the current selection. Default `false`; only
/// `BrowserState` overrides this.
fn is_title_selected(&self) -> bool {
    false
}
```

In `src/browser/browser_state.rs`, inside `impl PanelState for BrowserState`, add:

```rust
fn is_title_selected(&self) -> bool {
    self.displayed_tree().selection == 0
}
```

**Step 4: Run tests to verify pass**

Run: `cargo test --all-features is_title_selected`
Expected: PASS — both new tests pass; existing tests untouched.

- [ ] write the two `is_title_selected_*` tests in `browser_state.rs`
- [ ] verify tests fail (method missing)
- [ ] add `is_title_selected` default trait method to `PanelState`
- [ ] override on `BrowserState`
- [ ] verify both new tests pass
- [ ] run full suite: `cargo test --all-features` — no regressions

### Task 2: Add `selected: bool` to `draw_frame_title`

**Files:**
- Modify: `src/display/frame.rs`

**Step 1: Write failing test**

Add to `src/display/frame.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn draw_frame_title_selected_emits_selection_bg() {
    use {
        crate::skin::SkinEntry,
        rustc_hash::FxHashMap,
    };
    // Build a StyleMap where both frame_title and selected_line are
    // styled with distinct, terminal-visible colours, so the SGR
    // bytes for "selected" must differ from "unselected".
    let mut overrides: FxHashMap<String, SkinEntry> = FxHashMap::default();
    overrides.insert(
        "frame_title".to_string(),
        SkinEntry::parse("Yellow None Bold").expect("parse frame_title"),
    );
    overrides.insert(
        "selected_line".to_string(),
        SkinEntry::parse("None Blue").expect("parse selected_line"),
    );
    let maps = crate::skin::StyleMaps::create(&overrides);

    let area = Area::new(0, 0, 30, 4);

    let mut buf_unselected: Vec<u8> = Vec::new();
    draw_frame_title(&mut buf_unselected, area.clone(), &maps.focused, "hello", false).unwrap();

    let mut buf_selected: Vec<u8> = Vec::new();
    draw_frame_title(&mut buf_selected, area, &maps.focused, "hello", true).unwrap();

    assert_ne!(
        buf_unselected, buf_selected,
        "selected title must emit different SGR bytes than unselected",
    );
    let s_sel = String::from_utf8_lossy(&buf_selected);
    assert!(s_sel.contains("hello"));
}
```

**Step 2: Run test to verify failure**

Run: `cargo test --all-features draw_frame_title_selected_emits_selection_bg`
Expected: FAIL — `draw_frame_title` takes 4 args, not 5 (compile error). Also the 5 existing call sites in the same module fail to compile.

**Step 3: Write minimal implementation**

Update signature and body in `src/display/frame.rs`:

```rust
pub(crate) fn draw_frame_title<W: Write>(
    w: &mut W,
    area: Area,
    palette: &StyleMap,
    title: &str,
    selected: bool,
) -> io::Result<()> {
    if title.is_empty() || area.width < 6 {
        return Ok(());
    }
    let max_title_width = area.width.saturating_sub(4) as usize;
    let title = truncate_to_width(title, max_title_width);
    if title.is_empty() {
        return Ok(());
    }

    // When selected, overlay the body's selection bg onto the
    // frame_title style. fg/attrs preserved. If selected_line has no
    // bg (custom skin), fall back to unstyled frame_title — no panic.
    let owned;
    let style_ref: &termimad::CompoundStyle = if selected {
        let mut s = palette.frame_title.clone();
        if let Some(bg) = palette.selected_line.get_bg() {
            s.set_bg(bg);
        }
        owned = s;
        &owned
    } else {
        &palette.frame_title
    };

    w.queue(cursor::MoveTo(area.left + 1, area.top))?;
    style_ref.queue(w, ' ').map_err(io_err)?;
    style_ref.queue_str(w, &title).map_err(io_err)?;
    style_ref.queue(w, ' ').map_err(io_err)?;
    Ok(())
}
```

Update the 5 existing `draw_frame_title` call sites in the same file to pass `false`:

- `draw_frame_title_writes_title_into_buffer`
- `draw_frame_title_emits_sgr_when_palette_is_styled`
- `draw_frame_title_skips_empty_or_narrow` (two call sites)
- `draw_frame_then_title_combined`

**Step 4: Run tests to verify pass**

Run: `cargo test --all-features --package broot --lib display::frame`
Expected: PASS — new test passes; all 5 updated tests still pass.

- [ ] write `draw_frame_title_selected_emits_selection_bg` test
- [ ] verify new test fails (signature mismatch)
- [ ] update `draw_frame_title` signature with `selected: bool`
- [ ] implement the selected-bg overlay branch
- [ ] update 5 existing `draw_frame_title_*` call sites to pass `false`
- [ ] verify new test passes
- [ ] run full suite: `cargo test --all-features` — no regressions outside `app_panels.rs` (which will still need the wiring in Task 4)

### Task 3: Click handling — title row selects root

**Files:**
- Modify: `src/browser/browser_state.rs`

**Step 1: Write failing tests**

Add to `src/browser/browser_state.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn on_click_title_row_selects_root() {
    // body_top = 3 → title row sits at y = 2. With selection initially
    // pointing at a child, a click on the title row must reset it to 0.
    let mut state = fake_browser_state_with_children(3, 4);
    state.displayed_tree_mut().selection = 2;

    // Direct in-method logic (no Screen / AppContext required):
    // mirror what on_click does for y == body_top - 1.
    let y: u16 = 2;
    if state.body_top > 0 && y == state.body_top - 1 {
        state.displayed_tree_mut().selection = 0;
    }
    assert_eq!(state.displayed_tree().selection, 0);
}

#[test]
fn on_click_title_row_no_op_when_body_top_zero() {
    // Degenerate terminal: body_top = 0 → no title row exists,
    // and the `body_top > 0` guard must prevent underflow.
    let mut state = fake_browser_state_with_children(0, 4);
    state.displayed_tree_mut().selection = 2;

    let y: u16 = 0;
    if state.body_top > 0 && y == state.body_top - 1 {
        state.displayed_tree_mut().selection = 0;
    }
    // Selection unchanged.
    assert_eq!(state.displayed_tree().selection, 2);
}
```

These tests inline the title-row logic rather than calling `on_click` directly, because `on_click` requires `Screen` and `AppContext`. The inlined code mirrors what we'll put in `on_click` — if we change the production logic, we update the test, and they pin the behaviour together.

➕ alternative: if extracting a small helper `fn try_select_title_row(&mut self, y: u16) -> bool` makes the test cleaner, do that — see Task 3 step 3.

**Step 2: Run tests to verify failure**

Run: `cargo test --all-features on_click_title_row`
Expected: FAIL — initial selection in the fake state is 0 (after construction), so the first test would actually pass trivially before any implementation change. To make the test meaningfully fail-before-pass, extract a helper:

```rust
/// If `y` is the title row (body_top - 1), reset selection to 0 and
/// return true. Otherwise return false. Underflow-safe.
fn try_select_title_row(&mut self, y: u16) -> bool {
    if self.body_top > 0 && y == self.body_top - 1 {
        self.displayed_tree_mut().selection = 0;
        true
    } else {
        false
    }
}
```

Rewrite the tests to call `state.try_select_title_row(y)` and assert on the return value + resulting selection. With the helper missing, tests fail with a compile error.

**Step 3: Write minimal implementation**

Add the helper `try_select_title_row` on `BrowserState` (in the existing `impl BrowserState` block, near `body_relative_y`). Then update `on_click`:

```rust
fn on_click(
    &mut self,
    _x: u16,
    y: u16,
    _screen: Screen,
    _con: &AppContext,
) -> Result<CmdResult, ProgramError> {
    if self.try_select_title_row(y) {
        return Ok(CmdResult::Keep);
    }
    if let Some(body_y) = self.body_relative_y(y) {
        self.displayed_tree_mut().try_select_y(body_y);
    }
    Ok(CmdResult::Keep)
}
```

**Step 4: Run tests to verify pass**

Run: `cargo test --all-features on_click_title_row`
Expected: PASS — both tests pass; existing click tests (`click_translation_*`, `body_relative_y_*`) untouched.

- [ ] write the two `on_click_title_row_*` tests (calling `try_select_title_row`)
- [ ] verify tests fail (helper missing)
- [ ] add `try_select_title_row` helper to `BrowserState`
- [ ] route `on_click` through the helper before `body_relative_y`
- [ ] verify new tests pass
- [ ] run full suite: `cargo test --all-features` — no regressions

### Task 4: Wire `is_title_selected` into `display_panels`

**Files:**
- Modify: `src/app/app_panels.rs`

**Step 1: Establish the failing state**

After Tasks 1–3, the codebase has:
- `draw_frame_title` requires 5 args.
- `app_panels.rs:712` still calls it with 4 args.

So `cargo build` already fails. This task makes it compile and wires the signal.

Run: `cargo build` → expect compile error at `frame::draw_frame_title(w, outer.clone(), &panel_skin.styles, &title)?;`.

**Step 2: Write minimal implementation**

In `src/app/app_panels.rs` (around line 708-713):

```rust
if outer.width >= 6 {
    let title = panel.state().frame_title(outer.width.saturating_sub(4));
    let title_selected = panel.state().is_title_selected();
    frame::draw_frame_title(
        w,
        outer.clone(),
        &panel_skin.styles,
        &title,
        title_selected,
    )?;
}
```

**Step 3: Run full suite to verify pass**

Run: `cargo test --all-features`
Expected: PASS — all tests green. No new test for this task because the wiring is one expression; correctness is covered by:
- Task 1 tests (`is_title_selected` returns the right bool)
- Task 2 tests (`draw_frame_title` renders correctly given the bool)
- This task's compile-and-test-green outcome (signal is plumbed through)

**Note on TDD exception:** No new test is added here because the change is pure plumbing — one extra arg threaded into one call site. The unit-test surface is `is_title_selected()` (Task 1) and `draw_frame_title(..., selected)` (Task 2); both are pinned. An integration test would require spinning up an `App` with mocked rendering, which is well beyond the cost/benefit of this slice. Documented per "partial implementation exception" semantics.

- [ ] update the `draw_frame_title` call in `display_panels` to pass `panel.state().is_title_selected()`
- [ ] run `cargo build` — must compile
- [ ] run full suite: `cargo test --all-features` — all tests pass
- [ ] manual smoke: launch broot, press `Up` repeatedly to reach the root, verify title highlights; click the title from a child-selected state, verify selection moves to root

### Task 5: Update CLAUDE.md + verify acceptance

**Files:**
- Modify: `CLAUDE.md`
- Move: `docs/plans/2026-05-11-title-as-root.md` → `docs/plans/completed/`
- Move: `docs/plans/2026-05-11-title-as-root-design.md` → `docs/plans/completed/`

**Step 1: Update CLAUDE.md**

Append a paragraph to the existing "Tree root row + aux status" section (or as a new sibling section) noting:

- The frame title is now the visual carrier for the root row's selection state. `displayed_tree().selection == 0` highlights the title with `selected_line` bg overlaid on `frame_title`.
- `BrowserState::on_click` special-cases `y == body_top - 1` (the title row) to set `selection = 0`. The `body_top > 0` guard prevents underflow on degenerate tiny terminals.
- `PanelState::is_title_selected()` is the trait hook (default `false`); BrowserState is the only override today. Future tree-like panels can opt in.
- No new style keys — the selected variant clones `frame_title` and overlays `selected_line.get_bg()`. Skin overrides that don't supply a `selected_line` bg fall back to the unstyled `frame_title` look.

**Step 2: Verify acceptance criteria**

- [ ] `is_title_selected` returns true exactly when `displayed_tree().selection == 0` (Task 1 tests pin this)
- [ ] selected title renders with `selected_line` bg, unselected does not (Task 2 test pins this)
- [ ] click on title row sets `selection = 0`; click on title row with `body_top == 0` is a no-op (Task 3 tests pin this)
- [ ] full test suite green: `cargo test --all-features`
- [ ] `cargo clippy --all-features --all-targets -- -D warnings` clean
- [ ] manual smoke (see Post-Completion below)

**Step 3: Move plan to completed**

```
mkdir -p docs/plans/completed
git mv docs/plans/2026-05-11-title-as-root.md docs/plans/completed/
git mv docs/plans/2026-05-11-title-as-root-design.md docs/plans/completed/
```

- [ ] add CLAUDE.md paragraph
- [ ] run final `cargo test --all-features` — all green
- [ ] run `cargo clippy --all-features --all-targets -- -D warnings` — clean
- [ ] move both plan + design to `docs/plans/completed/`

## Post-Completion

*Items requiring manual verification — no checkboxes, informational only*

**Manual smoke test scenarios:**
- Launch broot at a directory with several children. Press `Up` until selection reaches the root. Verify the frame title (which shows the root path) renders with the selection bar background. Press `Down` and verify it loses the highlight.
- Click anywhere on the title row in a BrowserState panel from a non-root selection. Verify the selection jumps to the root (title highlights) and the status row hint flips to the "on tree root" form.
- Repeat the click test on the Preview, Stage, Trash, Help, and Fs panels — confirm clicking their titles does not change any selection (default trait behaviour).
- Open a filtered view (type a pattern). Verify the title still highlights when `selection == 0` in the filtered tree.
- Resize the terminal to a tiny size where the frame collapses (height < 5). Verify no panic and the title-click is inert.

**Skin override sanity:**
- With a custom `conf.hjson` that overrides `selected_line` but not `frame_title`, verify the selected-title rendering uses the overridden bg.
- With a custom `conf.hjson` that *removes* the `selected_line` bg entirely (e.g. transparent), verify the selected-title falls back to the unstyled `frame_title` look (no panic, no visible bg).
