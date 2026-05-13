# Colored File Icons

> **For Claude:** use `/planning:execute` to implement this plan task-by-task with fresh subagents.

**Goal:** Port elio's per-type colored file icons to broot — `.rs` orange, `.py` yellow, `.git` distinct, etc. — by extending the existing `IconPlugin` trait and adding a static palette resolver, leaving the filename style untouched.

**Architecture:** Add a default-`None` `get_icon_color` method to `IconPlugin`. Introduce `FontPlugin::new_mono()` for the monochrome escape hatch. Add a new `src/icon/colors.rs` containing a `FileClass` enum, four static lookup maps (extensions / filenames / dirnames / classes), and a `resolve()` function whose priority chain mirrors elio. The paint site in `displayable_tree.rs` splits the existing single-style icon paint into a two-pass write: icon cell gets its own per-type fg + Bold; the trailing space and the filename keep their existing `label_style`. User `ext_colors` continues to win over the built-in palette.

**Tech Stack:** Rust, `crossterm::style::Color`, `FxHashMap` (existing pattern in `font.rs`).

## Overview

Today broot paints file icons with the same `label_style` as the filename, so icons look monochrome (one color per `skin.file`/`directory`/`exe`/`link`). Elio paints each icon with a per-file-type fg color plus Bold, producing the distinctive colored-icon look (`.rs` orange, `.py` yellow, `.git` directories tinted, etc.).

This plan brings that look to broot with no new user-facing config surface:

- `icon_theme: nerdfont` (the current default) becomes the **colored** variant.
- A new `icon_theme: nerdfont-mono` is the escape hatch for the old monochrome look.
- `icon_theme: none` continues to disable icons entirely.
- The user's existing `ext_colors` config wins over the built-in palette for both icon and name when it matches an extension.

Only the icon glyph is recolored. The filename keeps its existing skin style. The 2-cell `icon + space` prefix invariant from CLAUDE.md is preserved exactly.

## Context (from discovery)

- **Files involved**:
  - `src/icon/icon_plugin.rs` — trait definition (currently glyph-only `get_icon`)
  - `src/icon/font.rs` — `FontPlugin` struct with four glyph-lookup maps; used by both `nerdfont` and `vscode` themes
  - `src/icon/mod.rs:16` — `icon_plugin(name)` factory; currently recognises `"vscode"`, `"nerdfont"`, `"none"`, and falls through to `None`
  - `src/display/displayable_tree.rs:85-112` — `label_style()` helper
  - `src/display/displayable_tree.rs:314-316` — the icon paint site (two `queue_char` calls)
  - `src/app/app_context.rs:193` — default `icon_theme = "nerdfont"`
- **Related patterns**:
  - `FontPlugin` builds `FxHashMap`s from static `&[(&str, &str)]` slices at construction time. The colored variant can follow the same pattern (static slice → map) OR use `match` arms directly.
  - `TreeLineType` variants are `Dir`, `File`, `SymLink { .. }`, `BrokenSymLink(_)`, `Pruning`. The design doc's reference to "SymLinkToFile/SymLinkToDir" was wrong — there's only one `SymLink` variant carrying the target path.
  - The current `FontPlugin::new(...)` constructor takes four glyph-table slices; this is shared by both `nerdfont` and `vscode`. The colored behaviour belongs only on `nerdfont`, so a `colored: bool` field plus a `new_mono` constructor is the right shape.
- **Dependencies**: no new crates. `crossterm::style::Color` is already in scope throughout the codebase.

## Development Approach

- **Testing approach**: **TDD (tests first)**
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task** — `cargo test --all` is the gate
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- maintain backward compatibility — existing `icon_theme: nerdfont` users see the colored variant as a visual change, but the config string itself still works

## Testing Strategy

- **unit tests**: required for every task, colocated in `#[cfg(test)] mod tests` blocks as is broot's convention.
- **integration / render tests**: broot has render-style tests near `displayable_tree.rs`. A small new render test asserts the icon cell carries the expected fg color and the filename retains its skin fg.
- **e2e tests**: broot has no Playwright-style e2e suite. Manual smoke verification is part of the final verification task (Task 8).

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

1. Extend `IconPlugin` with `get_icon_color` (default `None`).
2. Create `src/icon/colors.rs` with `FileClass`, four lookup maps, `infer_class`, and `resolve`.
3. Add a `colored: bool` field to `FontPlugin` plus a `new_mono()` constructor; implement `get_icon_color` to delegate to `colors::resolve` when colored.
4. Register `"nerdfont-mono"` in the `icon_plugin` factory.
5. Cache `icon_color: Option<Color>` on `TreeLine` alongside the existing `icon` field (or thread the plugin into the paint site — TDD will surface which is cleaner).
6. Split the paint site into two passes: icon cell uses a derived `icon_style` (label_style + per-type fg + Bold, short-circuited by `ext_colors`); space + name keep `label_style`.
7. Expand the palette to elio-equivalent coverage (~150-250 entries).
8. Document the new behaviour in `CLAUDE.md`.

## Technical Details

### Data structures

```rust
// src/icon/colors.rs
pub enum FileClass {
    Directory, Code, Document, Image, Video, Audio,
    Archive, Config, Data, Binary, Script,
    Lock, License, Markdown, Font, Other,
}

// Use FxHashMap with static slice inputs, matching FontPlugin's pattern.
pub static EXT_COLOR_PAIRS:      &[(&str, (u8, u8, u8))] = &[ ... ];
pub static FILENAME_COLOR_PAIRS: &[(&str, (u8, u8, u8))] = &[ ... ];
pub static DIRNAME_COLOR_PAIRS:  &[(&str, (u8, u8, u8))] = &[ ... ];
pub static CLASS_COLOR_PAIRS:    &[(FileClass, (u8, u8, u8))] = &[ ... ];

pub fn resolve(
    line_type: &TreeLineType,
    name: &str,
    double_ext: Option<&str>,
    ext: Option<&str>,
) -> Option<crossterm::style::Color>;

pub fn infer_class(name: &str, ext: Option<&str>) -> FileClass;
```

### Resolution priority

```
Dir:                 DIRNAME_COLORS[name]
                     ?? CLASS_COLORS[Directory]
File:                FILENAME_COLORS[name]
                     ?? EXT_COLORS[double_ext]   (".tar.gz", etc.)
                     ?? EXT_COLORS[ext]
                     ?? CLASS_COLORS[infer_class(name, ext)]
SymLink { target }:  resolve target name + ext using the file chain
                     ?? CLASS_COLORS[Other]
BrokenSymLink(_):    CLASS_COLORS[Other]
Pruning:             None  (keep current dimmed style)
```

### Paint flow

```
write_line_label(line, label_style, ...) {
    if let Some(icon) = line.icon {
        let icon_style = compute_icon_style(line, ext, label_style, icon_plugin);
        cw.queue_char(&icon_style, icon)?;
        cw.queue_char(label_style, ' ')?;   // unchanged
    }
    // existing name paint code, unchanged
}

compute_icon_style(line, ext, label_style, icon_plugin) -> CompoundStyle {
    let mut s = label_style.clone();
    if user_ext_colors_matched(ext) { return s; }   // ext_colors wins on both icon + name
    if let Some(plugin) = icon_plugin {
        if let Some(c) = plugin.get_icon_color(&line.line_type, &line.name, double_ext, ext) {
            s.set_fg(c);
        }
    }
    s.add_attr(Attribute::Bold);
    s
}
```

The `user_ext_colors_matched` check needs the `ExtColorMap` reference, which is already accessible at the paint site (it's part of `label_style()` resolution today at `displayable_tree.rs:103-105`).

## What Goes Where

- **Implementation Steps** (`[ ]` checkboxes): trait extension, palette module, plugin wiring, factory registration, paint-site change, palette transcription, doc update.
- **Post-Completion** (no checkboxes): manual visual smoke test in a real terminal; spot-check on dark and light skins; confirm `icon_theme: nerdfont-mono` and `icon_theme: none` still work.

## Implementation Steps

### Task 1: Extend `IconPlugin` trait with `get_icon_color`

**Files:**
- Modify: `src/icon/icon_plugin.rs`
- Modify: `src/icon/mod.rs` (test additions)

**Step 1: Write the failing test**

In `src/icon/mod.rs` `#[cfg(test)]` block, add:

```rust
#[test]
fn default_icon_color_is_none() {
    // A stub plugin that only implements get_icon should get None from get_icon_color.
    struct Stub;
    impl IconPlugin for Stub {
        fn get_icon(&self, _: &TreeLineType, _: &str, _: Option<&str>, _: Option<&str>) -> char { '?' }
    }
    let s = Stub;
    assert!(s.get_icon_color(&TreeLineType::File, "x.rs", None, Some("rs")).is_none());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo test --lib -p broot icon`
Expected: FAIL — `get_icon_color` is not defined on `IconPlugin`.

**Step 3: Add the trait method with default `None` impl**

In `src/icon/icon_plugin.rs`:

```rust
use crossterm::style::Color;

pub trait IconPlugin {
    fn get_icon(
        &self,
        tree_line_type: &TreeLineType,
        name: &str,
        double_ext: Option<&str>,
        ext: Option<&str>,
    ) -> char;

    fn get_icon_color(
        &self,
        _tree_line_type: &TreeLineType,
        _name: &str,
        _double_ext: Option<&str>,
        _ext: Option<&str>,
    ) -> Option<Color> {
        None
    }
}
```

**Step 4: Run test to verify it passes**

Run: `cargo test --lib -p broot icon`
Expected: PASS — default impl returns `None`.

- [ ] write failing test for default `get_icon_color` returning `None`
- [ ] verify test fails (no method defined)
- [ ] add `get_icon_color` to `IconPlugin` with default `None` impl
- [ ] verify test passes
- [ ] run `cargo build` to ensure no compile errors in existing callers
- [ ] run `cargo test --lib` — all tests must pass before next task

### Task 2: Create `src/icon/colors.rs` with seed palette + `resolve`

**Files:**
- Create: `src/icon/colors.rs`
- Modify: `src/icon/mod.rs` (declare the new module)

**Step 1: Write failing tests**

In a new `#[cfg(test)] mod tests` block inside `colors.rs`:

```rust
use crate::tree::TreeLineType;
use crossterm::style::Color;
use super::*;

fn rgb(r: u8, g: u8, b: u8) -> Color { Color::Rgb { r, g, b } }

#[test]
fn rs_extension_resolves_to_orange() {
    let c = resolve(&TreeLineType::File, "main.rs", None, Some("rs"));
    assert_eq!(c, Some(rgb(255, 143, 64)));   // elio's #ff8f40
}

#[test]
fn cargo_toml_filename_wins_over_ext() {
    // Cargo.toml → tan (filename match), NOT the .toml extension color
    let c = resolve(&TreeLineType::File, "Cargo.toml", None, Some("toml"));
    assert_eq!(c, Some(rgb(211, 170, 124)));   // tan
}

#[test]
fn unknown_extension_falls_back_to_class() {
    let c = resolve(&TreeLineType::File, "weird.xyzzy", None, Some("xyzzy"));
    // Falls through to the Other class color.
    assert_eq!(c, Some(rgb(170, 170, 170)));   // class Other (pick a placeholder)
}

#[test]
fn dir_resolves_by_name_then_class() {
    let c = resolve(&TreeLineType::Dir, ".git", None, None);
    assert_eq!(c, Some(rgb(255, 143, 64)));   // .git → orange

    let c = resolve(&TreeLineType::Dir, "RandomDir", None, None);
    // Falls through to Directory class color.
    assert!(c.is_some());
}

#[test]
fn pruning_resolves_to_none() {
    let c = resolve(&TreeLineType::Pruning, "...", None, None);
    assert_eq!(c, None);
}

#[test]
fn symlink_resolves_via_target_name() {
    let c = resolve(
        &TreeLineType::SymLink { has_error: false, target: "main.rs".into() },
        "link",
        None,
        Some("rs"),
    );
    assert_eq!(c, Some(rgb(255, 143, 64)));
}

#[test]
fn broken_symlink_resolves_to_other_class() {
    let c = resolve(&TreeLineType::BrokenSymLink("gone".into()), "link", None, None);
    assert!(c.is_some());   // Other class color
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib colors`
Expected: FAIL — module does not exist.

**Step 3: Write minimal implementation**

Create `src/icon/colors.rs` with:
- `FileClass` enum
- A seed palette covering the entries the tests reference (rs, Cargo.toml, .git, Other class, Directory class)
- `infer_class(name, ext)` — minimal logic to map known extensions to classes
- `resolve(line_type, name, double_ext, ext) -> Option<Color>` implementing the priority chain above

Use static slices + `FxHashMap` constructed in a `LazyLock`, OR plain `match` arms — pick whichever is simpler. Both are fine; the palette expansion task (Task 7) can refactor if needed.

In `src/icon/mod.rs`, add `mod colors;` (private — only `font.rs` and tests need it).

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib colors`
Expected: all PASS.

- [ ] write failing tests for `resolve`: extension, filename-precedence, fallback, dir, pruning, symlink, broken symlink
- [ ] verify tests fail (module missing)
- [ ] create `src/icon/colors.rs` with `FileClass`, seed palette, `infer_class`, `resolve`
- [ ] declare module in `src/icon/mod.rs`
- [ ] write tests for `infer_class` covering at least 3 classes
- [ ] verify all tests pass
- [ ] run `cargo test --lib` — must pass before next task

### Task 3: Implement `FontPlugin::get_icon_color` and `new_mono` constructor

**Files:**
- Modify: `src/icon/font.rs`

**Step 1: Write failing tests**

In `src/icon/font.rs` `#[cfg(test)] mod tests` block:

```rust
use crate::tree::TreeLineType;

fn make_colored_nerdfont() -> FontPlugin {
    FontPlugin::new(
        &include!("../../resources/icons/nerdfont/data/icon_name_to_icon_code_point_map.rs"),
        &include!("../../resources/icons/nerdfont/data/double_extension_to_icon_name_map.rs"),
        &include!("../../resources/icons/nerdfont/data/extension_to_icon_name_map.rs"),
        &include!("../../resources/icons/nerdfont/data/file_name_to_icon_name_map.rs"),
    )
}

#[test]
fn colored_plugin_returns_color_for_known_ext() {
    let p = make_colored_nerdfont();
    let c = p.get_icon_color(&TreeLineType::File, "main.rs", None, Some("rs"));
    assert!(c.is_some());
}

#[test]
fn mono_plugin_always_returns_none() {
    let p = make_colored_nerdfont().mono();
    let c = p.get_icon_color(&TreeLineType::File, "main.rs", None, Some("rs"));
    assert!(c.is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib font`
Expected: FAIL — no `get_icon_color` impl, no `mono` method.

**Step 3: Write minimal implementation**

In `src/icon/font.rs`:

```rust
pub struct FontPlugin {
    // existing fields ...
    colored: bool,
}

impl FontPlugin {
    pub fn new(...) -> Self {
        // existing logic ...
        Self { ..., colored: true }   // colored by default
    }

    pub fn mono(mut self) -> Self {
        self.colored = false;
        self
    }
}

impl IconPlugin for FontPlugin {
    // existing get_icon ...

    fn get_icon_color(
        &self,
        line_type: &TreeLineType,
        name: &str,
        double_ext: Option<&str>,
        ext: Option<&str>,
    ) -> Option<crossterm::style::Color> {
        if !self.colored { return None; }
        crate::icon::colors::resolve(line_type, name, double_ext, ext)
    }
}
```

Builder-style `.mono()` is preferred over a separate `new_mono(...)` constructor because it avoids duplicating the four `&[(...)]` argument lists at every call site.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib font`
Expected: PASS.

- [ ] write failing tests: colored plugin returns Some for known ext; mono plugin returns None
- [ ] verify tests fail
- [ ] add `colored: bool` field to `FontPlugin` (defaults to `true`)
- [ ] add `mono(self) -> Self` builder method
- [ ] implement `get_icon_color` delegating to `colors::resolve` when `colored`
- [ ] verify tests pass
- [ ] run `cargo test --lib` — must pass before next task

### Task 4: Register `nerdfont-mono` in factory

**Files:**
- Modify: `src/icon/mod.rs`

**Step 1: Write failing tests**

In `src/icon/mod.rs` `#[cfg(test)] mod tests`:

```rust
use crate::tree::TreeLineType;

#[test]
fn nerdfont_mono_resolves_to_a_plugin() {
    assert!(icon_plugin("nerdfont-mono").is_some());
}

#[test]
fn nerdfont_returns_colored_plugin() {
    let p = icon_plugin("nerdfont").unwrap();
    assert!(p.get_icon_color(&TreeLineType::File, "main.rs", None, Some("rs")).is_some());
}

#[test]
fn nerdfont_mono_returns_uncolored_plugin() {
    let p = icon_plugin("nerdfont-mono").unwrap();
    assert!(p.get_icon_color(&TreeLineType::File, "main.rs", None, Some("rs")).is_none());
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib icon::tests`
Expected: FAIL — `nerdfont-mono` not recognised.

**Step 3: Add the match arm**

In `src/icon/mod.rs` `icon_plugin` factory, add a `"nerdfont-mono"` arm that builds the same `FontPlugin` as `"nerdfont"` then calls `.mono()`:

```rust
"nerdfont-mono" => Some(Box::new(FontPlugin::new(
    &include!("../../resources/icons/nerdfont/data/icon_name_to_icon_code_point_map.rs"),
    &include!("../../resources/icons/nerdfont/data/double_extension_to_icon_name_map.rs"),
    &include!("../../resources/icons/nerdfont/data/extension_to_icon_name_map.rs"),
    &include!("../../resources/icons/nerdfont/data/file_name_to_icon_name_map.rs"),
).mono())),
```

Don't add colored behaviour for `"vscode"` — it stays whatever `colors::resolve` reports (Task 3's wiring means vscode also returns colors, since the trait impl is on `FontPlugin`, not gated by theme name). Decision: **vscode plugin keeps the colored palette too**, because the palette is keyed by file type, not by glyph set. If we wanted vscode-mono, that would be a separate variant.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib icon::tests`
Expected: PASS.

- [ ] write failing tests for `nerdfont-mono` factory recognition and color behaviour
- [ ] verify tests fail
- [ ] add `"nerdfont-mono"` arm to `icon_plugin` factory
- [ ] verify tests pass
- [ ] confirm `"nerdfont"`, `"vscode"`, `"none"`, and unknown still behave as before
- [ ] run `cargo test --lib` — must pass before next task

### Task 5: Split icon paint site to use per-icon style

**Files:**
- Modify: `src/display/displayable_tree.rs`
- Modify: `src/icon/icon_plugin.rs` (no further changes; trait already in place)
- Possibly modify: a `TreeLine` field (if caching `icon_color` is cleaner than threading the plugin into the paint site — to be decided during implementation)

**Step 1: Write failing render test**

Add a new test in `src/display/displayable_tree.rs` test module (or in a sibling `displayable_tree_tests.rs` file) that builds a small `Tree` with one Rust file and renders it via `DisplayableTree::write_on` into a buffer, then asserts:

- The byte sequence for the icon cell contains the `rs` extension's expected fg color (e.g. orange `255;143;64`).
- The byte sequence for the filename cell uses the existing `skin.file_name` fg color, NOT orange.
- The trailing space between icon and name has the same bg as the rest of the row.

The test should also cover a selected row to confirm selection bg is preserved on the icon cell.

If no render-test pattern exists in this file today, the test can target the helper directly: pass a fake `TreeLine`, `label_style`, and `FontPlugin::new(...)`, then assert `compute_icon_style(...)` produces a `CompoundStyle` with the expected fg + Bold attribute.

**Step 2: Run test to verify it fails**

Run: `cargo test --lib displayable_tree`
Expected: FAIL — helper not defined, or rendered output uses single style.

**Step 3: Implement the paint-site split**

Near `label_style()` at `src/display/displayable_tree.rs:85-112`, add a new helper:

```rust
fn compute_icon_style(
    line: &TreeLine,
    ext: Option<&str>,
    label_style: &CompoundStyle,
    ext_colors: &ExtColorMap,
    icon_plugin: Option<&dyn IconPlugin>,
) -> CompoundStyle {
    let mut s = label_style.clone();
    if ext_colors.get(ext).is_some() {
        return s;   // user override wins on icon + name; no Bold added
    }
    if let Some(plugin) = icon_plugin {
        if let Some(c) = plugin.get_icon_color(&line.line_type, &line.name, /*double_ext*/ None, ext) {
            s.set_fg(c);
        }
    }
    s.add_attr(Attribute::Bold);
    s
}
```

Update the paint site at `:314-316`:

```rust
if let Some(icon) = line.icon {
    let icon_style = compute_icon_style(line, ext, &label_style, ext_colors, icon_plugin);
    cw.queue_char(&icon_style, icon)?;
    cw.queue_char(&label_style, ' ')?;
}
```

The double_ext value at the paint site may not be readily available; if not, pass `None` (single-ext lookup still covers `.rs`, `.py`, etc.). Tarball-class colors via `.tar.gz` are optional — note as a follow-up if the data isn't accessible without a refactor.

**Step 4: Run test to verify it passes**

Run: `cargo test --lib displayable_tree`
Expected: PASS.

**Step 5: Manual visual smoke test**

Run `cargo run -- src/` and confirm `.rs` files show an orange icon while the filename stays the default file color. Press a key to select another row; confirm the icon repaints with the selection bg. Set `icon_theme: nerdfont-mono` in `~/.config/broot/conf.hjson` and confirm icons return to monochrome.

- [ ] write failing test for icon-style helper (or render-level fg assertion)
- [ ] verify test fails
- [ ] add `compute_icon_style` helper near `label_style`
- [ ] thread `icon_plugin` + `ext_colors` to the paint site as needed
- [ ] split paint into two `queue_char` calls — icon with icon_style, space with label_style
- [ ] write test for ext_colors short-circuit (no Bold, no plugin color)
- [ ] write test for Bold attribute on icon when no ext_colors match
- [ ] verify all tests pass
- [ ] manual visual smoke: `cargo run -- src/` looks correct on colored / mono / selection / `icon_theme: none`
- [ ] run `cargo test --all` — must pass before next task

### Task 6: Expand palette to elio-equivalent coverage

**Files:**
- Modify: `src/icon/colors.rs`

This is the mechanical transcription task explicitly flagged in the design. Source-of-truth references in elio:

- `/Users/aohoyd/Git/github.com/aohoyd/elio/src/ui/theme/appearance/rules/classes.rs` — class defaults
- `/Users/aohoyd/Git/github.com/aohoyd/elio/src/ui/theme/appearance/rules/extensions.rs` — per-extension overrides
- `/Users/aohoyd/Git/github.com/aohoyd/elio/assets/themes/default/theme.toml` — final overlay layer

The **effective resolved palette** is what we want: apply each layer in order and take the final color per key. The seed palette from Task 2 is a strict subset of this.

**Step 1: Add tests for representative new entries**

Add tests in `src/icon/colors.rs` covering at least 10 new entries spread across categories — e.g. `py` (yellow), `go` (cyan), `md` (tan), `json` (blue), `yaml` (yellow), `sh` (near-white), `lock` (green), `xml` (purple), `pdf` (green), `dart` (cyan); plus 3 directory entries — `node_modules`, `src`, `Downloads`; plus 2 filename entries — `LICENSE`, `Dockerfile`.

**Step 2: Run tests to verify they fail**

Run: `cargo test --lib colors`
Expected: FAIL — palette doesn't cover these entries yet (resolve returns class fallback, not the expected color).

**Step 3: Transcribe the effective palette**

Expand the four lookup tables in `colors.rs` to cover ~150-250 entries. Source values come from the three elio files above; the final value per key is what `Theme::apply_config_on(base_theme(), DEFAULT_THEME_TOML)` produces (the embedded TOML overrides the Rust baseline). A small Python or shell helper can extract the data, but a manual transcription is acceptable given the table is static.

**Step 4: Run tests to verify they pass**

Run: `cargo test --lib colors`
Expected: PASS.

- [ ] write tests for at least 10 new extension entries, 3 directory entries, 2 filename entries
- [ ] verify tests fail (palette is too sparse)
- [ ] transcribe elio's effective palette into the four lookup tables in `colors.rs`
- [ ] verify all new tests pass
- [ ] run `cargo test --all` — must pass before next task
- [ ] manual smoke: `cargo run -- .` in a polyglot directory; spot-check 5+ extensions visually match elio's look

### Task 7: Verify acceptance criteria

- [ ] verify `icon_theme: nerdfont` (or unset) produces colored icons
- [ ] verify `icon_theme: nerdfont-mono` produces monochrome icons (existing behaviour)
- [ ] verify `icon_theme: none` produces no icons
- [ ] verify user `ext_colors` entry overrides both the icon and the filename color
- [ ] verify Pruning lines keep their dimmed style (no Bold, no color)
- [ ] verify broken symlinks render with the Other class color
- [ ] verify selection bg paints across the icon cell, the space, and the filename
- [ ] run full test suite: `cargo test --all`
- [ ] run clippy: `cargo clippy --all-targets -- -D warnings`
- [ ] verify no regressions in `cargo build --release`

### Task 8: Update documentation

**Files:**
- Modify: `CLAUDE.md`
- Move: this plan file to `docs/plans/completed/`

- [ ] add a subsection under "Icon defaults" in `CLAUDE.md` covering:
  - `icon_theme: nerdfont` is now colored by default
  - `icon_theme: nerdfont-mono` is the monochrome escape hatch
  - resolution priority order
  - `ext_colors` interaction (still wins over the built-in palette)
  - the new `src/icon/colors.rs` module and its lookup tables
  - the 2-cell prefix invariant still holds (`displayable_tree.rs:314-316`)
- [ ] update README.md if a user-facing config doc references `icon_theme` values
- [ ] move this plan to `docs/plans/completed/2026-05-13-colored-file-icons.md`:
  `mkdir -p docs/plans/completed && git mv docs/plans/2026-05-13-colored-file-icons.md docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification**:

- Visual review in at least two terminals (e.g., iTerm2 + the system Terminal.app, or terminal + tmux) to confirm Nerd Font PUA glyphs render with the new fg colors and that no terminal renders the colors strangely.
- Spot-check on both `skin: dark` and `skin: light` (if applicable) — the per-type colors should remain readable on either background.
- Confirm the colored icons don't interfere with broot's search-match highlighting in the same row.

**Out of scope (deliberately deferred)**:

- TOML-based user themes for icon colors. `ext_colors` covers the user-override use case; a parallel theme format would duplicate broot's existing skin + ext_colors config surface.
- A new top-level config key like `icon_colors`. Not needed.
- Coloring inside the preview pane, the help pane, or the stage panel — those don't render the icon column today, so there's nothing to change.
- Image-preview / syntax-highlight color changes — unrelated.
