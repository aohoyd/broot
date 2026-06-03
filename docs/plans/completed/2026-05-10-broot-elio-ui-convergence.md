# broot ↔ elio UI/UX Convergence — Implementation Plan

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Make broot visually closer to elio by adding rounded frames with embedded directory titles, defaulting Nerd Font icons on, building a floating overlay layer, and putting confirmation modals (rm/trash, mv/cp overwrite, bulk staging) and a Goto/Bookmarks modal on top of it.

**Architecture:** Hand-rolled rendering (no ratatui). New `src/display/frame.rs` draws borders + titles. New `src/app/overlay/` module hosts a single `App::overlay: Option<Overlay>` field with one render hook in `display_panels` and one key/mouse hook in the event loop. Confirmations and Goto are two `OverlayState` impls reusing the shared layer. New `bookmarks` HJSON section, with built-in defaults shipped in `resources/default-conf/conf.hjson`.

**Tech Stack:** Rust 1.83, crokey, crossterm, hjson + toml configs, `unicode-width` for glyph-cell counting. Test infrastructure: standard `cargo test`; broot has no `test-seams` feature today and we will not introduce one.

## Overview

This plan implements the design at `docs/plans/2026-05-10-broot-elio-ui-design.md`. Five user-visible features ship together, sequenced so each builds on solid ground:

1. **Frames + title** — every panel gets a rounded border with its tree-root path embedded in the top edge.
2. **Nerd Font default-on** — `icon_theme: nerdfont` becomes implicit; users opt out with `icon_theme: none`.
3. **Overlay layer** — net-new floating-modal infrastructure shared by the next two features.
4. **Confirmation modals** — destructive verbs prompt before executing.
5. **Goto / Bookmarks** — `g` opens a single-key jump menu populated from a new `bookmarks` config section.

This is a personal-fork build: defaults flip aggressively, but every behaviour is reversible via config (`icon_theme: none`, custom `bookmarks: []`, custom verb `confirm: false`).

## Context (from discovery)

- **Rendering** is hand-rolled to `BufWriter<Stderr>` (alias `W`) at `src/display/mod.rs:67`. No ratatui. `WIDE_STATUS = true` (line 64) makes the status bar span full width — frames must sit *above* it, not around it.
- **Geometry** is computed in `src/display/areas.rs`, `Areas::compute_areas` at line 87. No insets today; panels abut.
- **Display loop** is `display_panels` in `src/app/app_panels.rs:669` — single top-level draw, the natural place to add the frame pre-pass and the overlay post-pass.
- **Page height**: `BrowserState::page_height(screen)` at `src/browser/browser_state.rs:112` returns `screen_height - 2`. Must be reduced by another 2 for top + bottom frame edges.
- **Verbs**: `:rm` registered as auto-exec external at `src/verb/verb_store.rs:322` (`rm -rf {file}`). `:trash` runs immediately at `src/browser/browser_state.rs:681`. Neither has a confirmation step. `VerbConf` already has `auto_exec: Option<bool>` (`src/verb/verb_conf.rs:59`); we add a parallel `confirm: Option<bool>`.
- **Icons**: `src/app/app_context.rs:185` reads `config.icon_theme`. `None` → no icons. `resources/default-conf/conf.hjson:97` has `icon_theme: nerdfont` commented out.
- **Goto/bookmarks**: do not exist. `g` is currently unbound. `h` is bound globally to `parent` — bookmark keys are scoped *inside* the modal so this is not a collision.
- **Conf parser**: `src/conf/conf.rs:230` `Conf::read_file` requires both a serde field AND a manual `overwrite_*!` merge line; the comment at line 169 explicitly warns about this. Bookmarks must be added in both places.
- **Tests**: integration tests under `tests/` (currently just `tests/search_strings.rs`). Unit tests are inline `#[cfg(test)] mod tests`.

## Development Approach

- **Testing approach**: Regular (code first, then tests in the same task)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - add new test cases for new code paths
  - update existing test cases if behavior changes
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run `cargo test` after each task
- maintain config-file backward compatibility (existing user configs must still parse)

## Testing Strategy

- **Unit tests**: required for every task, inline `#[cfg(test)]` modules in the same file or a sibling module.
- **Integration tests**: add new files under `tests/` for cross-module scenarios — overlay flows, verb confirmation flows, bookmark navigation. Each new feature gets at least one integration test that drives it through the public API.
- **No e2e/UI test framework** in broot — for render code we test the data shape (e.g. `Areas` geometry, modal struct construction) rather than visual diffing.
- Run with `cargo test` per task; full suite must pass before next task.

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

Five features, all coordinated through three new infrastructure pieces:

1. **`src/display/frame.rs`** — small drawer module: `FrameStyle`, `draw_frame`, `draw_frame_title`, `path_label`, `centered_rect`. Pure helpers.
2. **`Areas::state_outer`** — new field; the existing `state` rect shrinks to the interior, `state_outer` is what frames are drawn around.
3. **`src/app/overlay/`** — the shared modal layer; one `App::overlay: Option<Overlay>` field, one render post-pass, one key hook, one mouse hook.

Confirmations and Goto are then two `OverlayState` consumers; bookmarks are a new HJSON section feeding `GotoOverlay`.

## Technical Details

**`FrameStyle`** — defaults to rounded (`╭╮╰╯ │ ─`); `square()` constructor available but unused for now.

**Frame inset**:
- `state` rect: `x += 1`, `y += 1`, `width -= 2`, `height -= 2`.
- `state_outer` rect: the original (pre-shrink) rectangle.
- Bottom of frame sits at `screen.height - 3` (rows `height-2` and `height-1` reserved for status + input as before; row `height-3` is the bottom border).
- Vertical column between adjacent panels: the right `│` of pane N and the left `│` of pane N+1 land on the same x; render once.

**Path label**: `path_label(path, max_w)` — substitute `$HOME` → `~`; if longer than `max_w`, truncate-tail to `~/…/basename`. Returns owned `String`.

**Overlay routing**:

```rust
pub trait OverlayState {
    fn render(&self, w: &mut W, screen: Area, palette: &Palette) -> io::Result<()>;
    fn handle_key(&mut self, key: KeyCombination, app: &mut App) -> OverlayOutcome;
    fn handle_mouse(&mut self, ev: MouseEvent, app: &mut App) -> OverlayOutcome;
}
pub enum Overlay { Goto(GotoOverlay), Confirm(ConfirmOverlay) }
pub enum OverlayOutcome { Stay, Close, CloseAndRun(Command), CloseAndFocus(PathBuf) }
```

Single field `App::overlay: Option<Overlay>`. Render hook in `display_panels` post-pass. Key/mouse hook in `App` event loop *before* `Panel::apply_command`.

**Verb confirmation**:
- New `Verb::requires_confirm: bool` field, parallel to existing `auto_exec`.
- New `VerbConf::confirm: Option<bool>` so users can opt their own external verbs in.
- `verb_store.rs:322` (`rm`) registered with `.with_confirm(true)`.
- `Internal::trash` wrapped at the dispatch site in `Panel::apply_command` — synthesize a `ConfirmOverlay` instead of executing.

**Mv/cp overwrite**: pre-execute `target.exists()` check; if true, open `ConfirmOverlay { danger: true, title: "Overwrite <name>?" }`.

**Bulk staging**: in `App::apply_command`, if `app.stage.paths.len() > 1` and the verb is being run against the staging area, route through `request_confirm` regardless of verb. `danger = verb.requires_confirm`.

**Bookmarks config schema** (HJSON):
```hjson
bookmarks: [
  { key: 'h', path: '~' }
  { key: 'd', path: '~/Downloads' }
  { key: 'c', path: '~/.config' }
  { key: 't', path: 'trash://' }
]
```
- `~`, `${HOME}`, `${XDG_CONFIG_HOME}` expanded at load time.
- `trash://` resolves to platform trash dir (`~/.local/share/Trash` on Linux/BSD, `~/.Trash` on macOS, warn-and-skip on Windows).
- User-defined `bookmarks` *replaces* defaults (matches conf.hjson convention for `verbs`).
- Duplicate `key` → first wins, warning logged.

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): all code, tests, and docs in this repo.
- **Post-Completion** (no checkboxes): manual smoke testing, optional CLAUDE.md authoring for new invariants.

## Implementation Steps

### Task 1: Add frame drawer module + path label

**Files:**
- Create: `src/display/frame.rs`
- Modify: `src/display/mod.rs` (add `pub mod frame;`)

- [ ] create `src/display/frame.rs` with `FrameStyle` (rounded default + square ctor), `draw_frame(w, area, palette, style)`, `draw_frame_title(w, area, palette, title)`, `path_label(path, max_w)`, `centered_rect(screen, w, h)`
- [ ] use `crossterm::cursor::MoveTo` + character writes (match existing broot rendering style); no ratatui
- [ ] export the module from `src/display/mod.rs`
- [ ] write unit tests for `path_label`: home substitution, no-truncation case, tail-truncation case, basename-only case
- [ ] write unit tests for `centered_rect`: even+odd dimensions, edge case where popup ≥ screen
- [ ] write unit tests for `FrameStyle::rounded` character set
- [ ] run `cargo test` — must pass before task 2

### Task 2: Wire frames into Areas geometry

**Files:**
- Modify: `src/display/areas.rs`
- Modify: `src/app/app_panels.rs` (only the call sites that read `panel.areas.state` if the field shape changes)

- [ ] add `state_outer: Area` field to `Areas` (alongside existing `state`)
- [ ] in `Areas::compute_areas` (line 87), compute outer rect first, then shrink to interior `state` (`x += 1`, `y += 1`, `width -= 2`, `height -= 2`); leave `status` and `input` unchanged
- [ ] ensure 0-width / 0-height panels degrade gracefully (clamp to 0, do not underflow)
- [ ] write unit tests covering 1-pane, 2-pane, 3-pane layouts and confirm `state_outer.width == state.width + 2`, `state.height == state_outer.height - 2`
- [ ] write unit test for narrow-terminal degenerate case (terminal width below frame minimum)
- [ ] run `cargo test` — must pass before task 3

### Task 3: Adjust page_height for frame inset

**Files:**
- Modify: `src/browser/browser_state.rs`

- [ ] update `BrowserState::page_height(screen)` (line 112) to subtract 2 more rows (top + bottom frame edges) — i.e. `screen.height - 4`
- [ ] audit every caller of `BrowserState::page_height` and confirm none assume the old value (search: `BrowserState::page_height`)
- [ ] write a regression test confirming `page_height` returns the expected reduced value for several screen heights
- [ ] write a test for the minimum-screen-height case (frames cannot make page_height go negative — clamp)
- [ ] run `cargo test` — must pass before task 4

### Task 4: Render frames + titles in display_panels loop

**Files:**
- Modify: `src/app/app_panels.rs`
- Modify: `src/skin/` (add `frame_title` palette key — find the file that defines palette keys)
- Modify: `src/skin/` default values (find the defaults file — likely `skin_entry.rs` or similar)

- [ ] in `display_panels` (line 669), for each panel: call `draw_frame(w, panel.areas.state_outer, palette, FrameStyle::rounded())` then `draw_frame_title(w, panel.areas.state_outer, palette, &path_label(panel.tree.root, panel.areas.state_outer.width.saturating_sub(4)))`
- [ ] keep existing inner-content render calls as before but ensure they read from `panel.areas.state` (the shrunk interior)
- [ ] add `frame_title` palette key with a sensible default (bold accent / primary color)
- [ ] write a smoke integration test that boots broot in a small dummy directory and renders one frame to a `Vec<u8>` buffer; assert that the output contains a `╭` and `╰` and the home-substituted path
- [ ] manually smoke-test: `cargo run -- ~/Documents` (or any dir) and visually verify frames render
- [ ] run `cargo test` — must pass before task 5

### Task 5: Default-on Nerd Font icons

**Files:**
- Modify: `src/app/app_context.rs`
- Modify: `src/icon/mod.rs`
- Modify: `resources/default-conf/conf.hjson`
- Modify: `src/display/displayable_tree.rs` (only if width handling needs fixing)

- [ ] in `src/app/app_context.rs:185`, default `icon_theme` to `"nerdfont"` when `config.icon_theme` is `None`
- [ ] in `src/icon/mod.rs::icon_plugin`, treat the literal string `"none"` as an explicit opt-out → return `None`
- [ ] in `resources/default-conf/conf.hjson:97`, uncomment the `icon_theme: nerdfont` line and add a comment line documenting `# icon_theme: none  # to disable`
- [ ] verify `unicode_width::UnicodeWidthChar::width()` returns 2 for the in-use Nerd Font code points (probe a few from the icon plugin); if it returns 1, fix the width accounting in `src/display/displayable_tree.rs` line ~320 (`CropWriter` indent path)
- [ ] write unit tests: empty-config-yields-icons; `icon_theme: none` yields no icons; `icon_theme: vscode` still works
- [ ] write a unit test for the wide-icon `CropWriter` path: a line with a 2-cell icon must not exceed the column budget
- [ ] run `cargo test` — must pass before task 6

### Task 6: Overlay layer scaffolding

**Files:**
- Create: `src/app/overlay/mod.rs`
- Modify: `src/app/mod.rs` (add `pub mod overlay;`)
- Modify: `src/app/app.rs` (add `overlay: Option<Overlay>` field + key/mouse routing)
- Modify: `src/app/app_panels.rs` (post-pass overlay render in `display_panels`)

- [ ] create `src/app/overlay/mod.rs` with `Overlay` enum (initially empty / `Stub` variant for testing), `OverlayState` trait (`render`, `handle_key`, `handle_mouse`), `OverlayOutcome` enum (`Stay`, `Close`, `CloseAndRun(Command)`, `CloseAndFocus(PathBuf)`)
- [ ] add `pub overlay: Option<Overlay>` field to `App`
- [ ] in `display_panels`, after the per-panel render loop and before `flush`, call `if let Some(ov) = &self.overlay { ov.render(w, screen.area(), palette)? }`
- [ ] in the `App` key event handler, before dispatching to `Panel::apply_command`, if `self.overlay.is_some()` route the key through `overlay.handle_key`; on `Close` drop the overlay; on `CloseAndRun(cmd)` drop and re-enter dispatch with `cmd`; on `CloseAndFocus(path)` drop and synthesize a `:focus <path>` command
- [ ] mirror the same pattern for mouse events
- [ ] add a `StubOverlay` (minimal `OverlayState` impl) feature-gated behind `#[cfg(test)]` to drive tests
- [ ] write a unit test that opens the stub overlay, sends a key, asserts the outcome routes correctly
- [ ] run `cargo test` — must pass before task 7

### Task 7: Confirm overlay component

**Files:**
- Create: `src/app/overlay/confirm.rs`
- Modify: `src/app/overlay/mod.rs` (add `Overlay::Confirm(ConfirmOverlay)` variant)

- [x] create `ConfirmOverlay` struct: `title: String`, `body: Vec<String>`, `confirm_label: String`, `danger: bool`, `pending: Command`, `focus: ConfirmFocus { Cancel | Confirm }`, `scroll: u16`
- [x] implement `OverlayState::render`: centered rect (50 cols × `min(15, body.len()+5)` rows), `Clear` the rect, draw rounded frame, draw title in top border, draw body lines (scrollable), draw button row `[ Cancel ] [ Confirm ]` with focused button inverted; danger=true colors `Confirm` red
- [x] implement `OverlayState::handle_key`: `Tab/←/→` toggle focus; `Enter` commits focused button; `y` direct-confirm; `n/Esc/Ctrl+C` cancel; `↑/↓` scroll body when overflowing
- [x] implement `OverlayState::handle_mouse`: store hit-rects for the two buttons during render; click on `Confirm` rect → confirm; click on `Cancel` rect → cancel
- [x] write unit tests for: focus toggle keys, `y`/`n` shortcuts, `Esc` cancellation, `OverlayOutcome::CloseAndRun(pending)` on confirm, danger-true vs false rendering pathway
- [x] write a render snapshot-style test: build a `ConfirmOverlay`, render to a buffer, assert it contains the title and both button labels
- [x] run `cargo test` — must pass before task 8

### Task 8: Wire confirmations to rm + trash

**Files:**
- Modify: `src/verb/verb.rs` (add `requires_confirm: bool`)
- Modify: `src/conf/verb_conf.rs` (add `confirm: Option<bool>`)
- Modify: `src/verb/verb_store.rs` (`with_confirm(bool)` builder + apply `confirm` from `VerbConf`; register `rm` with `.with_confirm(true)`)
- Modify: `src/app/panel.rs` or `src/app/app.rs` (intercept on dispatch — find the right point near `Panel::apply_command`)
- Create: `src/app/overlay/request.rs` or add to `mod.rs`: `App::request_confirm(pending: Command, ctx: ConfirmCtx)` helper

- [x] add `requires_confirm: bool` field to `Verb` (default `false`)
- [x] add `confirm: Option<bool>` field to `VerbConf`
- [x] add `Verb::with_confirm(bool)` builder method
- [x] in `verb_store.rs:549` area, apply `verb_conf.confirm` to override `requires_confirm`
- [x] register the built-in `rm` external verb (line 322) with `.with_confirm(true)`
- [x] add `App::request_confirm(pending: Command, ctx: ConfirmCtx)` that constructs a `ConfirmOverlay` and sets `self.overlay = Some(...)`
- [x] in command dispatch, **before** the auto-exec short-circuit: if the resolved verb has `requires_confirm` and `self.overlay.is_none()`, call `request_confirm` instead of executing
- [x] wrap `Internal::trash` dispatch in the same intercept (it's not a `Verb` but goes through the same `apply_command` path — synthesize a `ConfirmOverlay` with title `"Trash <name>?"`)
- [x] write integration test under `tests/`: programmatically run `:rm` against a temp file → assert overlay opens, file untouched; send `Enter` after focus on `Confirm` → assert file removed
- [x] write integration test for `:trash`: same flow against a temp file; verify overlay title contains `"Trash"`
- [x] write a unit test that `confirm: false` in a `VerbConf` overrides a built-in `requires_confirm: true`
- [x] run `cargo test` — must pass before task 9

> **Implementation note (Task 8):**
> - Intercept lives at `App::apply_command` (single point, not in `Panel::apply_command`) — it sees verb resolution before dispatch and shorts-circuits to the overlay.
> - Bypass mechanism: `App.skip_confirm: bool` one-shot flag, set by `handle_overlay_outcome::CloseAndRun` and cleared on next entry. Keeps the overlay's `pending` Command identical to the original (no `Internal::trash_confirmed` marker variant needed).
> - `Internal::trash` recognised both as a direct `Command::Internal { internal: trash, .. }` and as the `:trash` verb resolved via `Command::VerbInvocate` / `Command::VerbTrigger` (because the verb's `execution` is `Internal(Internal::trash)`). The existing `browser_state.rs:685` trash code path runs unchanged after confirmation.
> - `CmdResult::OpenOverlay(Box<Overlay>)` variant added (used by Task 13's Goto wiring; Task 8 uses the `App::request_confirm` direct path).
> - Integration test in `tests/confirm_destructive.rs` exercises the verb registry shape, `ConfirmOverlay` state machine and Cancel-leaves-file-intact behaviour. End-to-end `App` driving is left to manual smoke testing per the plan's "Post-Completion" section, since broot has no headless event-loop test harness today.

### Task 9: Wire confirmations to mv/cp overwrite

**Files:**
- Modify: `src/verb/internal.rs` (find the cp/mv internals or external invocation wrapper)
- Modify: `src/app/app.rs` (or wherever the verb-execution wrapper lives — locate by grep `cp_to_panel|move_to_panel`)

- [x] in the verb-execution path for `:cp`, `:mv`, `:cp_to_panel`, `:move_to_panel`, before invocation, resolve the destination path and call `target.exists()`
- [x] when target exists: build a `ConfirmOverlay` with `title = format!("Overwrite {}?", target.file_name())`, `danger = true`, `pending = original Command`; route through `App::request_confirm`
- [x] when target does not exist: proceed as today (no overlay)
- [x] handle the case where `target` is a directory and the source is a file (collision) vs. moving into a directory (no collision) — match broot's existing `:mv` semantics
- [x] write integration test: `:mv` to non-existing dest → no overlay, file moved
- [x] write integration test: `:mv` to existing dest → overlay opens with `"Overwrite"` title; confirm overwrites
- [x] write integration test: `:cp` overwrite case mirrors `:mv`
- [x] write integration test for `:cp_to_panel` with collision in the second panel
- [x] run `cargo test` — must pass before task 10

> **Implementation note (Task 9):**
> - The conditional overwrite check lives in `App::maybe_destructive_confirm` (extending Task 8's intercept) via a new `resolve_overwrite_target` helper.
> - Family detection: external verb whose first exec-pattern token is one of `mv | cp | rsync | xcopy | cmd`. The verb must additionally use either `{newpath:path-from-parent}` (`:cp`/`:mv`) or `{other-panel-directory}` (`:cpp`/`:mvp`).
> - Destination resolution mirrors `ExecutionBuilder::get_sel_arg_replacement`: `{newpath:path-from-parent}` → `path::path_from(source.parent(), Unspecified, value)`; `{other-panel-directory}` → `closest_dir(other_panel_path).join(source.basename())`.
> - "Move into existing directory" semantics: if the resolved target is an existing directory and the source is a regular file, the actual collision target is `target/<basename>`. We re-stat that joined path and only prompt when it exists.
> - Self-overwrite (`source == target`) is skipped — broot's own `mv`/`cp` will report the error.
> - Symlinks: we use `symlink_metadata` so dangling symlinks are also caught as collisions.
> - End-to-end App-driven testing is gapped (no headless event-loop harness). Coverage is split: `src/app/app.rs::confirm_helper_tests` exercises `resolve_overwrite_target` directly across all four verbs, plus `tests/confirm_destructive.rs` pins the verb-registry shape and the overlay state machine for the overwrite case.

### Task 10: Wire confirmations to bulk staging

**Files:**
- Modify: `src/app/app.rs` (in `apply_command` — find the staging-area dispatch path)

- [ ] in `App::apply_command`, when a verb is being run against the staging area and `app.stage.paths.len() > 1`: build a `ConfirmOverlay` with title `format!("Run {} on {} files?", verb.name, count)`; `danger = verb.requires_confirm` (so non-destructive bulk ops still confirm but without the red styling); route through `request_confirm`
- [ ] ensure single-file staging (count == 1) bypasses the confirmation
- [ ] ensure the confirmation only triggers once per bulk operation (not once per file)
- [ ] write integration test: stage 1 file, run `:rm` → only the rm-verb confirmation opens (the bulk one does not)
- [ ] write integration test: stage 3 files, run `:cp` → bulk overlay opens with title containing "3 files"; confirm executes the verb on all three
- [ ] write integration test: stage 3 files, run a non-destructive verb (e.g. `:open_stay`) → bulk overlay opens; confirm runs verb against all
- [ ] run `cargo test` — must pass before task 11

### Task 11: Bookmark config schema + parsing

**Files:**
- Modify: `src/conf/conf.rs` (add `pub bookmarks: Option<Vec<BookmarkConf>>` + `overwrite_vec!` line in `read_file`)
- Create: `src/conf/bookmark_conf.rs` (or fold into `conf.rs`)
- Modify: `src/app/app_context.rs` (load `bookmarks: Vec<BookmarkEntry>`)
- Create: `src/app/bookmark.rs` (`BookmarkEntry` runtime struct + expansion logic)

- [ ] add `BookmarkConf { key: char, path: String }` deserialization struct with `serde::Deserialize`
- [ ] add `pub bookmarks: Option<Vec<BookmarkConf>>` to `Conf` with `#[serde(default)]`
- [ ] add `overwrite_vec!(self, bookmarks, conf)` line in `Conf::read_file` per the merge-line warning at line 169
- [ ] create `BookmarkEntry { key: char, path: PathBuf, label: String /* basename */ }` runtime type
- [ ] implement path expansion in `bookmark.rs::resolve(raw: &str)`: `~` and `${HOME}` → home dir; `${XDG_CONFIG_HOME}` → falls back to `~/.config`; `trash://` → platform trash dir (Linux/BSD `~/.local/share/Trash`, macOS `~/.Trash`, Windows: warn + skip)
- [ ] implement duplicate-key handling: first wins, log warning via existing logging mechanism
- [ ] in `AppContext::from`, populate `bookmarks: Vec<BookmarkEntry>` from `config.bookmarks` (or built-in defaults if `None`)
- [ ] write unit tests: parse `BookmarkConf` from HJSON; expansion of `~`, `${HOME}`, `trash://` per platform; duplicate-key keeps first + logs warning; missing path file (allowed — bookmarks may point to not-yet-existing dirs)
- [ ] write unit test for built-in defaults present when config has no `bookmarks` field
- [ ] write unit test: user-defined `bookmarks: []` (empty list) replaces defaults with empty list (not silently re-defaulted)
- [ ] run `cargo test` — must pass before task 12

### Task 12: Goto overlay component

**Files:**
- Create: `src/app/overlay/goto.rs`
- Modify: `src/app/overlay/mod.rs` (add `Overlay::Goto(GotoOverlay)` variant)

- [ ] create `GotoOverlay { entries: Vec<BookmarkEntry>, selected: usize, hits: Vec<(usize, Area)> }`
- [ ] implement `OverlayState::render`: bottom-anchored popup at `screen.height - height - 2`, width 50, height `min(12, entries.len() + 3)`; rounded frame; title `" Goto "`; vertical list of `<key>  <path-label>` rows; highlight `selected` row
- [ ] implement `OverlayState::handle_key`: single-character match against `entries[].key` → `OverlayOutcome::CloseAndFocus(path.clone())`; `↑/↓/Tab` move selection; `Enter` activates `selected`; `Esc/Ctrl+C` close
- [ ] implement `OverlayState::handle_mouse`: store row hit-rects during render; click row → activate that entry
- [ ] write unit tests for: single-char match returns expected path; arrow nav clamps at boundaries; `Enter` on selected fires `CloseAndFocus`; `Esc` returns `Close`
- [ ] write a render-shape test: build `GotoOverlay` with 4 entries, render to buffer, assert it contains all four key chars and a `╭` corner
- [ ] write unit test for >12-entry overflow: height clamps, list still renders
- [ ] run `cargo test` — must pass before task 13

### Task 13: Wire :goto_bookmarks verb to 'g' key

**Files:**
- Modify: `src/verb/internal.rs` (add `Internal::goto_bookmarks` variant)
- Modify: `src/verb/verb_store.rs` (register the internal with `.with_key(key!('g'))`)
- Modify: `src/app/cmd_result.rs` or wherever `CmdResult` is defined (add `OpenOverlay(Overlay)` variant if not already present from task 6)
- Modify: `src/browser/browser_state.rs::on_internal` (handle `Internal::goto_bookmarks` → `CmdResult::OpenOverlay(Overlay::Goto(GotoOverlay::new(&app.bookmarks)))`)
- Modify: `resources/default-conf/conf.hjson` (add commented-out default `bookmarks` block)

- [ ] add `Internal::goto_bookmarks` variant
- [ ] register it in `verb_store.rs` with `.with_key(key!('g'))` (verify `g` is still unbound as expected)
- [ ] handle the variant in `on_internal` → produce a `CmdResult` that sets `app.overlay = Some(Overlay::Goto(...))` (route via the existing internal-handling pipeline; if a `OpenOverlay` `CmdResult` variant doesn't exist yet, add it)
- [ ] in `resources/default-conf/conf.hjson`, append a commented `bookmarks: [...]` block with the four built-in defaults (`h`, `d`, `c`, `t`)
- [ ] write integration test under `tests/`: boot broot, send `g` key, assert `app.overlay` is `Some(Overlay::Goto(_))`
- [ ] write integration test: with overlay open, send `h` key → assert overlay closes and tree-root path becomes `$HOME`
- [ ] write integration test: bookmark with non-existent path is loaded but does not crash on activation (path becomes the focus target, broot will then show its standard not-found error)
- [ ] run `cargo test` — must pass before task 14

### Task 14: Verify acceptance criteria

- [ ] verify all five features from Overview work end-to-end via manual smoke test (`cargo run`)
- [ ] verify edge cases: 0 bookmarks defined; very narrow terminal (frames degrade gracefully); icon_theme set to `none`; `confirm: false` user override on a built-in destructive verb
- [ ] run full test suite: `cargo test`
- [ ] verify there is no regression in existing broot integration test (`tests/search_strings.rs` still passes)
- [ ] visual check that `:focus` from the Goto overlay updates the panel frame title as expected
- [ ] verify no panic in headless mode (CI-style `cargo test` with no real TTY)

### Task 15: [Final] Update documentation

**Files:**
- Modify: `CLAUDE.md` (or create if missing)
- Modify: `README.md` if any user-facing key bindings changed
- Move: `docs/plans/2026-05-10-broot-elio-ui-convergence.md` → `docs/plans/completed/`

- [ ] author `CLAUDE.md` for broot capturing the new invariants: (a) frame inset (`state` is interior, `state_outer` is the framed rect — never confuse them); (b) overlay routing is single-field, single-render-hook, single-key-hook; (c) verb `requires_confirm` is the explicit destructive-verb signal — do not heuristic-detect from shell strings; (d) `Conf::bookmarks` requires both serde field AND `overwrite_vec!` line per the existing footgun warning
- [ ] update `README.md` with a note on the new `g` shortcut and the new bookmarks config section (one paragraph, link to a future docs page)
- [ ] move the plan file to `docs/plans/completed/` (create the dir if needed)

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification**:
- run broot in a real terminal with a Nerd Font installed (e.g. WezTerm + JetBrainsMono Nerd Font) and visually confirm icons render at the expected width
- run broot in a terminal *without* a Nerd Font and confirm `icon_theme: none` produces a clean fallback (no replacement chars in the tree)
- exercise rm/trash/mv/cp confirmations against real files in `~/Documents` and confirm Cancel-default behaviour saves you from accidents
- press `g` then each of `h`, `d`, `c`, `t` and confirm navigation lands at the expected directory on Linux, macOS

**External system updates**:
- if downstream packaging (the `.copr` directory was visible in the repo root) has its own changelog, regenerate
- if the `website/` content describes default keys, update there too (out of scope for this plan unless it's a 1-line tweak)
