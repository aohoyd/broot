# broot ↔ elio UI/UX Convergence — Design

Date: 2026-05-10
Status: design approved, plan TBD
Stance: personal fork — opinionated defaults, no upstream-compat gates

## Goal

Bring broot's visual language closer to elio (`/Users/aohoyd/Git/github.com/aohoyd/elio`):

1. Frames around panels, with the panel's tree-root path embedded in the top border.
2. Nerd Font icons enabled by default.
3. Confirmation modals for destructive verbs.
4. A Goto/Bookmarks modal triggered by a single key, with single-character jumps to user-configured destinations.

## Context — broot today

- broot is hand-rolled (no ratatui). Output goes to `BufWriter<Stderr>` aliased `W` (`src/display/mod.rs:67`). Borders, frames, and overlays do not exist today.
- Panel geometry is computed in `src/display/areas.rs` (`Areas::compute_areas` at line 87). No insets — panels abut at column boundaries.
- `WIDE_STATUS = true` (`src/display/mod.rs:64`) makes the status bar span full screen width and overwrite panel boundaries — must be considered when adding frames.
- `:rm` is registered as an external auto-exec verb at `src/verb/verb_store.rs:322` (`rm -rf {file}`). `:trash` (`src/browser/browser_state.rs:681`) calls `trash::delete()` immediately. Neither has a confirmation step today.
- `auto_exec = false` already exists on `VerbConf` (`src/verb/verb_conf.rs:59`) but only forces a verb into the input bar — it is **not** a yes/no modal.
- `icon_theme` (`src/app/app_context.rs:185`) is `Option<String>`. Default `None` → no icons. The shipped default `resources/default-conf/conf.hjson:97` has `icon_theme: nerdfont` commented out.
- `:goto`, bookmarks, modals — none of these exist in broot.
- `Conf::read_file` (`src/conf/conf.rs:230`) requires both a serde field AND a manual `overwrite_*!` merge line — adding a new top-level config section without the merge silently does nothing (warning at line 169).
- `BrowserState::page_height` (`src/browser/browser_state.rs:112`) is `screen_height - 2`; per CLAUDE.md the cwd-change pipeline depends on `App::tree_pane_height_for_request()` to plumb pane height — frame insets must flow through that helper, not bypass it.
- Test seams that need to be visible to the integration test crate must be gated `#[cfg(any(test, feature = "test-seams"))]` per CLAUDE.md.

## Context — elio (what we are mirroring)

- ratatui-based. Each pane uses `Block` with `BorderType::Rounded` + manual `buffer.set_line()` to inject the path title at row `area.y`, col `area.x + 1`.
- Pane title is `helpers::stable_path_label(cwd, width)` (home → `~`, then truncate-tail to `~/…/basename`).
- Goto modal is bottom-anchored, **hardcoded** to 5 entries (g/d/h/c/t). Cancel-default. Mouse-clickable. Not user-configurable. **We will improve on this** by making it config-driven and supporting arbitrary entry counts via a vertical list.
- Confirmation modals exist only for trash + restore. Two render functions duplicated copy-paste (~176 lines each). Cancel-default. **We will improve on this** with a single generic `ConfirmOverlay`.
- Overlay dispatch is a flat `if/else if` chain in `ui/mod.rs:85-103` requiring edits in 4 places per new overlay. **We will improve on this** with a single `App::overlay: Option<Overlay>` field and a single dispatch site each in render/key/mouse paths.

## Decisions (locked)

| Decision | Choice |
|---|---|
| Fork stance | Personal fork — opinionated defaults, no compat gates |
| Confirmation scope | rm + trash always; mv/cp **only when destination exists**; bulk staging-area ops always |
| Per-verb confirm flag | Add `confirm: bool` to `VerbConf` so user-defined external verbs can opt in (built-in `rm` registered with it explicitly) |
| Bookmark schema | Minimal: `{ key, path }`. No label, no icon — label = basename, icon = path's class icon |
| Frame style | Full elio-style rounded borders (`╭╮╰╯ │ ─`), title in top border |
| Overlay architecture | Floating overlay layer — net-new infra, modals composite over panels |
| Render primitive | Stay hand-rolled — do not introduce ratatui (mixing with current `W: BufWriter<Stderr>` writer is too disruptive) |

---

## Architecture — section by section

### 1. Frames + Title

**New module `src/display/frame.rs`**:

```rust
pub struct FrameStyle { pub corners: [char; 4], pub h: char, pub v: char }
impl FrameStyle { pub fn rounded() -> Self; pub fn square() -> Self; }
pub fn draw_frame(w: &mut W, area: Area, palette: &Palette, style: &FrameStyle) -> io::Result<()>;
pub fn draw_frame_title(w: &mut W, area: Area, palette: &Palette, title: &str) -> io::Result<()>;
pub fn path_label(path: &Path, max_w: u16) -> String; // ~/...basename
pub fn centered_rect(screen: Area, w: u16, h: u16) -> Area;
```

**Geometry change (`src/display/areas.rs`)**:

- `Areas` gains `state_outer: Area` (the rectangle the frame is drawn around) alongside the existing `state` rectangle (interior content).
- `compute_areas` shifts the `state` rect inward by 1 on each side and 1 row from top + 1 from bottom.
- `WIDE_STATUS` stays `true`; the bottom edge of each frame sits 2 rows above the screen bottom (above the global status + input rows). Trade-off accepted: chrome lives under the frames, matching elio's aesthetic.
- A vertical separator column between adjacent panels is drawn in the same drawer pass (the right column of pane N and the left column of pane N+1 happen to land on the same x — they share a single `│`).

**Page-height adjustment**:

- New `App::content_height(screen)` helper centralizes `screen_height - 2 - 2` (status + input + frame top + frame bottom).
- `BrowserState::page_height` and `App::tree_pane_height_for_request()` both route through this helper. The minimum-floor of 8 in `tree_pane_height_for_request` (per CLAUDE.md) stays.

**Title rendering**:

- For each panel, `draw_frame_title(w, areas.state_outer, palette, &path_label(panel.tree.root, areas.state_outer.width - 4))` is called immediately after `draw_frame`.
- Drawn at row `state_outer.y`, col `state_outer.x + 2`, with one space of padding on each side, in `palette.frame_title` (new palette key — defaults to bold accent).
- Title content: panel's tree root, with `$HOME` → `~`, then tail-truncated to `~/…/basename` if it overflows.

**Render order in `display_panels` (`src/app/app_panels.rs:669`)**:

```
for each panel:
  draw_frame(panel.areas.state_outer)
  draw_frame_title(panel.areas.state_outer, path_label(panel.tree.root))
  panel.display(w, panel.areas.state)        // existing — uses interior rect
  panel.write_status(w, panel.areas.status)  // existing
  panel.write_input(w, panel.areas.input)    // existing
if let Some(ov) = &app.overlay { ov.render(w, screen.area(), palette) }
```

### 2. Nerd Font default-on

- `src/app/app_context.rs:185`: `let theme = config.icon_theme.as_deref().unwrap_or("nerdfont"); let icons = icon_plugin(theme);`.
- `src/icon/mod.rs::icon_plugin`: handle `"none"` as an explicit opt-out → returns `None`.
- `resources/default-conf/conf.hjson:97`: uncomment, document `# icon_theme: none  # to disable`.
- Width re-check in `src/display/displayable_tree.rs` around line 320: confirm `unicode_width` returns 2 for the in-use Nerd Font code points and that `CropWriter` accounting matches. Add 2 unit tests.
- New `AppContext::with_icons_for_test(plugin)` test seam.

### 3. Overlay infrastructure

**New module `src/app/overlay/`**:

```
src/app/overlay/
├── mod.rs         // Overlay enum + OverlayState trait + OverlayOutcome
├── confirm.rs     // ConfirmOverlay
└── goto.rs        // GotoOverlay
```

```rust
pub trait OverlayState {
    fn render(&self, w: &mut W, screen: Area, palette: &Palette) -> io::Result<()>;
    fn handle_key(&mut self, key: KeyCombination, app: &mut App) -> OverlayOutcome;
    fn handle_mouse(&mut self, ev: MouseEvent, app: &mut App) -> OverlayOutcome;
}

pub enum Overlay { Goto(GotoOverlay), Confirm(ConfirmOverlay) }
pub enum OverlayOutcome {
    Stay,
    Close,
    CloseAndRun(Command),
    CloseAndFocus(PathBuf),
}
```

**State**: a single `App::overlay: Option<Overlay>` field. Flat — no stack.

**Dispatch**: three single sites:

- `src/app/app_panels.rs::display_panels` post-pass: `if let Some(ov) = &self.overlay { ov.render(...) }`.
- `src/app/app.rs` event loop key handler: `if let Some(ov) = self.overlay.as_mut() { ov.handle_key(...) }` *before* `Panel::apply_command`.
- Same for mouse.

**Frame-state gating**: `App::set_frame_state` skips the `tree_pane_height` commit while `self.overlay.is_some()` to keep underlying-panel height stable across an overlay open/close.

**Modal helpers**: `centered_rect(screen, w, h)` in `frame.rs`; modal draws a `Clear` over its rect, then a frame, then contents. No screen-wide dim.

**Test seams**: `App::open_overlay_for_test(Overlay)`, `App::overlay_for_test() -> Option<&Overlay>`, gated `#[cfg(any(test, feature = "test-seams"))]`.

### 4. Confirmation Modals

**`ConfirmOverlay` (single generic component)**:

```rust
pub struct ConfirmOverlay {
    title: String,
    body: Vec<String>,        // affected paths, scrollable
    confirm_label: String,    // "Delete" / "Trash" / "Overwrite"
    danger: bool,             // styles confirm button red
    pending: Command,
    focus: ConfirmFocus,      // Cancel default
    scroll: u16,
}
```

**Trigger sites** — all funnel through `App::request_confirm(pending: Command, ctx: ConfirmCtx)`:

1. **rm + trash always**: new `verb.requires_confirm: bool` flag. `verb_store.rs:322` registers the built-in `rm` with `.with_confirm(true)`. `Internal::trash` is wrapped at the `Panel::apply_command` dispatch site. Checked before the `auto_exec` short-circuit at `verb_store.rs:549`.
2. **mv/cp when destination exists**: in the verb-execution wrapper for `:cp`, `:mv`, `:cp_to_panel`, `:move_to_panel`, stat the resolved target. If `exists()`: open Confirm with `title = "Overwrite <name>?"`, `danger = true`. If absent: proceed as today.
3. **Bulk staging**: `App::apply_command` checks `app.stage.paths.len() > 1` for any verb run against the staging area; routes through `request_confirm` regardless of verb. Title `"Run :<verb> on N files?"`. `danger = verb.requires_confirm`.

**`VerbConf` extension**: add `confirm: Option<bool>` so users can opt their own external verbs in.

**Visual**: centered modal, 50 cols × `min(15, body.len() + 5)` rows. Rounded frame, title in top-border. Body scrollable with `↑/↓`. Bottom row: `[ Cancel ]   [ Confirm ]`, focused button inverted, danger=true colors `Confirm` red.

**Keys**:
- `Tab` / `←` / `→` toggle focus
- `Enter` commits focused button
- `y` → directly confirm
- `n` / `Esc` / `Ctrl+C` → cancel
- Mouse click on a button → activate

**Output**: `OverlayOutcome::CloseAndRun(pending)` on confirm; `OverlayOutcome::Close` on cancel.

**Tests** (under `tests/`, integration):
- `:rm` triggers overlay, file untouched until confirm
- `:mv` to non-existing dest: no overlay
- `:mv` to existing dest: overlay opens with `"Overwrite ..."` title
- Staging bulk verb opens overlay with correct title
- `y` direct-confirm works; `Esc` cancels and leaves filesystem untouched

### 5. Goto / Bookmarks Modal

**Trigger**: new `Internal::goto_bookmarks` registered with `.with_key(key!('g'))` in `verb_store.rs`. (`g` is currently unbound in broot — confirmed safe by exploration.)

**`GotoOverlay`**:

```rust
pub struct GotoOverlay { entries: Vec<BookmarkEntry>, selected: usize }
pub struct BookmarkEntry { key: char, path: PathBuf, label: String /* basename */ }
```

**Layout**: bottom-anchored vertical-list popup (improves on elio's hardcoded 5-column horizontal layout).

```
                   ╭─ Goto ─────────────────────────╮
                   │  h  ~                          │
                   │  d  ~/Downloads                │
                   │  c  ~/.config                  │
                   │  t  Trash                      │
                   ╰────────────────────────────────╯
```

Width 50 cols, height `min(12, entries + 3)`. Anchored at `screen.height - height - 2`.

**Keys**:
- Single-character match against `entries[].key` → `OverlayOutcome::CloseAndFocus(path)`
- `↑` / `↓` / `Tab` move highlight
- `Enter` activates highlighted entry
- `Esc` / `Ctrl+C` close
- Mouse click activates row (hit-rects stored on `FrameState::overlay_hits` per render)

**Action**: every entry resolves to a `PathBuf` and fires the existing `:focus <path>` verb. No new navigation primitive needed.

**Path expansion (config-load time)**:
- `~`, `${HOME}` → home dir
- `${XDG_CONFIG_HOME}` → falls back to `~/.config`
- `trash://` → platform trash dir (`~/.local/share/Trash` on Linux/BSD, `~/.Trash` on macOS, warn-and-skip on Windows)

**Built-in defaults** (shipped commented-out in `resources/default-conf/conf.hjson`):

```hjson
bookmarks: [
  { key: 'h', path: '~' }
  { key: 'd', path: '~/Downloads' }
  { key: 'c', path: '~/.config' }
  { key: 't', path: 'trash://' }
]
```

When the user defines `bookmarks: [...]`, that list **replaces** defaults (matches conf.hjson convention for `verbs`).

**Conflict policy**:
- Two entries declaring the same `key`: first wins; warning logged at startup.
- Bookmark keys are scoped to the open Goto overlay → no collision with global verb shortcuts (the global `h`-as-parent stays).

**Config plumbing**:
- `pub bookmarks: Option<Vec<BookmarkConf>>` added to `Conf` in `src/conf/conf.rs`.
- `overwrite_vec!(self, bookmarks, conf)` line added to `Conf::read_file` (per the merge-line footgun warning).
- `pub bookmarks: Vec<BookmarkEntry>` added to `AppContext`, populated at load with expansion applied.

**Tests**:
- Built-in defaults parse correctly
- Key-collision logs warning, keeps first
- `~` expansion produces absolute path
- `trash://` resolves per platform
- Overlay-render snapshot test
- Pressing matching key fires `:focus path` and closes overlay

---

## Build order

1. Section 1 — Frames + Title (geometry foundation)
2. Section 2 — Nerd Font default-on (trivial; ships alongside frames)
3. Section 3 — Overlay infrastructure (shared layer)
4. Section 4 — Confirmation modals (overlay consumer #1)
5. Section 5 — Goto / Bookmarks (overlay consumer #2 + new config section)

## Risks / non-obvious

- **`page_height` plumbing.** Per CLAUDE.md, `App::tree_pane_height_for_request()` is the only correct caller for Tree-mode `DirectoryRequest`. The frame inset must reduce *its* return value, not be applied at downstream call sites — otherwise a mismatch will recreate the 500-800 ms blank-tree regression.
- **Conf merge-line footgun.** `Conf::read_file` will silently ignore a `bookmarks` field if `overwrite_vec!(self, bookmarks, conf)` is missing.
- **Nerd Font width hazard.** Two-cell glyphs through `CropWriter` need explicit unit tests.
- **`WIDE_STATUS`.** Frames bottom edge sits *above* the global status row by design — do not draw frame bottoms across the status row.
- **External `rm` verbs.** Detection of "destructive shell command" is heuristic-impossible; rely on the explicit `verb.requires_confirm` flag.

## Out of scope

- Migrating broot to ratatui
- elio's Places sidebar (no Places pane in broot's design)
- Trash *view* (only navigation to the trash directory; restore/permanent-delete is not in scope)
- Theme overhaul beyond adding `palette.frame_title`

---

Plan generation: invoke `/planning:make` against this design to produce ordered tasks.
