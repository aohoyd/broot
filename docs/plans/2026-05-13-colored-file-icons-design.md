# Colored file icons — design

Bring elio-style per-type colored file icons to broot.

## Goal

Today broot's file icons are painted with the same style as the filename
(derived from `skin.file` / `skin.directory` / `skin.exe` / `skin.link`).
Result: icons are effectively monochrome.

Elio colors icons by file type — `.rs` gets orange, `.py` yellow, `.md` tan,
`.git` directories get a distinct color, etc. — using a hardcoded palette
plus a per-extension/per-class lookup. We want the same look in broot.

## Decisions (from brainstorm)

1. **Paint scope**: icon glyph only. The filename keeps its existing skin
   style (`file_name` / `directory` / `exe` / `link`). Only the icon cell
   gets the per-type fg color.
2. **Color source**: hardcoded port of elio's effective resolved palette
   (Rust baseline `rules/classes.rs` + `rules/extensions.rs` combined with
   the embedded `assets/themes/default/theme.toml`). No new TOML config
   surface in broot.
3. **Default behavior**: colored is the new default for
   `icon_theme: nerdfont`. A new `icon_theme: nerdfont-mono` is the
   escape hatch for the previous monochrome look. `icon_theme: none`
   continues to disable icons entirely.
4. **Directory scope**: directories also get per-name colors
   (`.git`, `node_modules`, `Downloads`, `Documents`, `src`, ...).
   Unmatched directories use the generic `directory` class color.
5. **`ext_colors` interaction**: existing user `ext_colors` config keeps
   its current behavior — it overrides both icon color **and** name color.
   The built-in icon palette only kicks in when `ext_colors` has no match
   for the file's extension.

## Architecture — trait extension with sibling plugins

Extend the existing `IconPlugin` trait rather than introducing a parallel
abstraction.

### `src/icon/icon_plugin.rs`

```rust
pub trait IconPlugin {
    fn get_icon(
        &self,
        tree_line_type: &TreeLineType,
        name: &str,
        double_ext: Option<&str>,
        ext: Option<&str>,
    ) -> char;

    // NEW — default impl keeps existing plugins unchanged.
    fn get_icon_color(
        &self,
        _tree_line_type: &TreeLineType,
        _name: &str,
        _double_ext: Option<&str>,
        _ext: Option<&str>,
    ) -> Option<crossterm::style::Color> {
        None
    }
}
```

### `src/icon/font.rs`

Add a `colored: bool` field to `FontPlugin`. Two constructors:

```rust
impl FontPlugin {
    pub fn new() -> Self { Self { /* …, */ colored: true } }
    pub fn new_mono() -> Self { Self { /* …, */ colored: false } }
}

impl IconPlugin for FontPlugin {
    fn get_icon_color(&self, line_type, name, double_ext, ext) -> Option<Color> {
        if !self.colored { return None; }
        crate::icon::font::colors::resolve(line_type, name, double_ext, ext)
    }
}
```

### Factory — `src/icon/mod.rs`

```rust
pub fn icon_plugin(icon_set: &str) -> Option<Box<dyn IconPlugin + Send + Sync>> {
    match icon_set.to_ascii_lowercase().as_str() {
        "nerdfont"      => Some(Box::new(FontPlugin::new())),       // colored (default)
        "nerdfont-mono" => Some(Box::new(FontPlugin::new_mono())),  // mono escape hatch
        "none"          => None,
        other => { cli_log::warn!("unknown icon_theme {other:?}"); None }
    }
}
```

`AppContext::from` at `src/app/app_context.rs:193` keeps defaulting
`icon_theme` to `"nerdfont"` — now that means colored.

## Color table — `src/icon/font/colors.rs` (new file)

```rust
use crossterm::style::Color;
use crate::tree::TreeLineType;

pub enum FileClass {
    Directory, Code, Document, Image, Video, Audio,
    Archive, Config, Data, Binary, Script,
    Lock, License, Markdown, Font, Other,
}

// All static, populated at compile time from elio's resolved palette.
// Use phf if broot's existing font.rs uses it; else plain match arms.
pub static EXT_COLORS:      …;  // e.g. "rs" → rgb(255,143,64), "py" → rgb(255,214,102)
pub static FILENAME_COLORS: …;  // e.g. "Cargo.toml" → tan, "LICENSE" → tan, "Dockerfile" → blue
pub static DIRNAME_COLORS:  …;  // e.g. ".git", "node_modules", "src", "Downloads"
pub static CLASS_COLORS:    …;  // FileClass → Color

pub fn resolve(
    line_type: &TreeLineType,
    name: &str,
    double_ext: Option<&str>,
    ext: Option<&str>,
) -> Option<Color> { … }

pub fn infer_class(name: &str, ext: Option<&str>) -> FileClass { … }
```

### Resolution priority

```
Dir:          DIRNAME_COLORS[name]
              ?? CLASS_COLORS[Directory]
File:         FILENAME_COLORS[name]
              ?? EXT_COLORS[double_ext]   // ".tar.gz" handling
              ?? EXT_COLORS[ext]
              ?? CLASS_COLORS[infer_class(name, ext)]
SymLinkToX:   resolve the target using the same chain
              ?? CLASS_COLORS[Other]      // broken/missing target
Pruning:      None                         // keep existing dimmed style
```

### Data source

One-time translation of elio's:

- `src/ui/theme/appearance/rules/classes.rs` — class defaults (icon + color)
- `src/ui/theme/appearance/rules/extensions.rs` — per-ext overrides
- `assets/themes/default/theme.toml` — final layer that produces the
  visible elio palette

into a single Rust file. Expected size: ~150–250 entries; flat data.
This is mechanical-but-large work and should be its own subtask.

## Paint-site change — `src/display/displayable_tree.rs`

Current site (`src/display/displayable_tree.rs:314-316`):

```rust
if let Some(icon) = line.icon {
    cw.queue_char(style, icon)?;
    cw.queue_char(style, ' ')?;
}
```

New site — split into two passes, but keep the 2-cell prefix exactly:

```rust
if let Some(icon) = line.icon {
    let icon_style = compute_icon_style(line, ext, label_style, skin, icon_plugin);
    cw.queue_char(&icon_style, icon)?;
    cw.queue_char(label_style, ' ')?;   // trailing space stays on label_style for bg continuity
}
```

`compute_icon_style` lives near `label_style()` at
`src/display/displayable_tree.rs:85-112`. Sketch:

```rust
fn compute_icon_style(
    line: &TreeLine,
    ext: Option<&str>,
    label_style: &CompoundStyle,
    skin: &StyleMap,
    icon_plugin: Option<&dyn IconPlugin>,
) -> CompoundStyle {
    let mut s = label_style.clone();   // inherit bg (selection, panel, etc.)

    // ext_colors wins for both icon and name — already baked into label_style.
    if ext_colors_matched(ext) {
        return s;
    }

    if let Some(plugin) = icon_plugin {
        if let Some(c) = plugin.get_icon_color(&line.line_type, &line.name, double_ext, ext) {
            s.set_fg(c);
        }
    }
    s.add_attr(Attribute::Bold);   // matches elio's `Modifier::BOLD` on the icon
    s
}
```

### Invariants preserved (per CLAUDE.md)

- 2-cell `icon + space` prefix is untouched (same two `queue_char` calls,
  same widths). Existing `cw.allowed` budgets stay correct.
- Selection band: `icon_style` clones from `label_style`, which already
  has selection bg applied at `displayable_tree.rs:496`.
- The trailing space stays on `label_style` so the seam between icon and
  name has the same bg.

## Tests

1. **`src/icon/font.rs`** — unit tests for `FontPlugin::get_icon_color`:
   - `rs` → orange; `py` → yellow; `md` → tan; `Cargo.toml` → tan;
     `.git` (dir) → orange; unknown extension → class fallback color.
   - `FontPlugin::new_mono().get_icon_color(...)` always returns `None`.
2. **`src/icon/mod.rs`** — `icon_plugin("nerdfont")` returns a plugin
   whose `get_icon_color` is non-None for a known ext;
   `icon_plugin("nerdfont-mono")` returns one whose `get_icon_color`
   is `None`; `icon_plugin("none")` returns `None`.
3. **`src/display/displayable_tree.rs`** — a render test that builds a
   small fake tree (one file, one directory, one symlink) and asserts
   the styled output contains the expected fg color escape for the icon
   glyph but the existing skin's fg for the filename. Also assert a
   selected row's icon cell carries the selection bg.

## Edge cases

- **Pruning lines** (`TreeLineType::Pruning`): resolve returns `None`;
  current dimmed style preserved.
- **Broken symlinks**: `TreeLineType::SymLinkToFile` with missing
  target → fall through to `CLASS_COLORS[Other]`.
- **Selection**: comes free via clone-from-`label_style`. Pinned by test.
- **Unicode width**: 2-cell prefix preserved.
- **Custom skins**: user `skin.file_name` etc. still styles the name.
  Only the icon cell uses the palette.

## File inventory

| File | Change |
|---|---|
| `src/icon/icon_plugin.rs` | Add `get_icon_color` method with default `None` |
| `src/icon/font.rs` | Add `colored: bool` field, `new()` / `new_mono()` ctors, implement `get_icon_color` |
| `src/icon/font/colors.rs` (new) | Static palette tables + `FileClass` enum + `infer_class` + `resolve` |
| `src/icon/mod.rs` | Add `"nerdfont-mono"` to factory match arm |
| `src/display/displayable_tree.rs` | Split icon paint into helper; two-pass `queue_char` |
| `CLAUDE.md` | Document colored vs mono, resolution priority, `ext_colors` interaction |

## Out of scope

- TOML-based user themes for icon colors (rejected — would duplicate
  broot's existing skin + ext_colors config surface).
- New config key like `icon_colors` (`ext_colors` covers user override).
- Image-preview or syntax-highlight color changes.

## References

- elio paint sites:
  `src/ui/browser/tree_view.rs:186-198`,
  `src/ui/browser/entries.rs:112-116`
- elio resolution:
  `src/ui/theme/appearance/resolve.rs:21-82`
- elio palette source of truth:
  `src/ui/theme/appearance/rules/classes.rs`,
  `src/ui/theme/appearance/rules/extensions.rs`,
  `assets/themes/default/theme.toml`
- broot today:
  `src/icon/icon_plugin.rs`,
  `src/icon/font.rs`,
  `src/icon/mod.rs:16` (icon_plugin factory),
  `src/display/displayable_tree.rs:85-112` (label_style),
  `src/display/displayable_tree.rs:314-316` (icon paint site),
  `src/app/app_context.rs:193` (default icon_theme)
