/// Defines the StyleMap structure with its default value.
///
/// A style_map is a collection of termimad compound_style. It's
/// either defined for the focused panel state or the unfocused
/// one (there are thus two instances in the application)
use {
    super::*,
    crate::errors::ProgramError,
    crokey::crossterm::{
        QueueableCommand,
        style::{
            Attribute::*,
            Attributes,
            Color::*,
            SetBackgroundColor,
        },
    },
    rustc_hash::FxHashMap,
    std::{
        fmt,
        io::Write,
    },
    termimad::CompoundStyle,
};

// this macro, which must be called once, creates
// the StyleMap struct with its creation functions handling
// both default values defined in the macro call and
// overriding values defined in TOML
macro_rules! StyleMap {
    (
        $(
            $name:ident: $fg:expr, $bg:expr, [$($attr:expr)*] $( / $fgu:expr, $bgu:expr , [$($attru:expr)*] )*
        )*
    ) => {
        /// a struct whose fields are
        /// - a boolean telling whether it's a no-style map
        /// - the styles to apply to various parts/cases
        pub struct StyleMap {
            styled: bool,
            $(pub $name: CompoundStyle,)*
        }
        /// a set of two style_maps: one for the focused panel and one for the other panels
        ///
        /// This struct is just a vessel for the skin initialization process.
        pub struct StyleMaps {
            pub focused: StyleMap,
            pub unfocused: StyleMap,
        }
        impl StyleMap {
            /// build a skin without any terminal control character (for file output)
            pub fn no_term() -> Self {
                Self {
                    styled: false,
                    $($name: CompoundStyle::default(),)*
                }
            }
            /// ensures the "default" skin entry is used as base for all other
            /// entries (this processus is part of the skin initialization)
            fn diffuse_default(&mut self) {
                $(
                    let mut base = self.default.clone();
                    base.overwrite_with(&self.$name);
                    self.$name = base;
                )*
            }
        }
        impl StyleMaps {
            pub fn create(skin_conf: &FxHashMap<String, SkinEntry>) -> Self {
                let mut focused = StyleMap {
                    styled: true,
                    $($name: skin_conf
                        .get(stringify!($name))
                        .map(|sec| sec.get_focused().clone())
                        .unwrap_or(
                            CompoundStyle::new(
                                $fg,
                                $bg,
                                Attributes::from(vec![$($attr),*].as_slice()),
                            )
                        )
                    ,)*
                };
                focused.diffuse_default();
                let mut unfocused = StyleMap {
                    styled: true,
                    $($name: CompoundStyle::default(),)*
                };
                $(
                    unfocused.$name = CompoundStyle::new(
                        $fg,
                        $bg,
                        Attributes::from(vec![$($attr),*].as_slice()),
                    );
                    $(
                        unfocused.$name = CompoundStyle::new(
                            $fgu,
                            $bgu,
                            Attributes::from(vec![$($attru),*].as_slice()),
                        );
                    )*
                    if let Some(sec) = skin_conf.get(stringify!($name)) {
                        unfocused.$name = sec.get_unfocused().clone();
                    }
                )*
                unfocused.diffuse_default();
                Self {
                    focused,
                    unfocused,
                }
            }
        }
        impl Clone for StyleMap {
            fn clone(&self) -> Self {
                Self {
                    styled: self.styled,
                    $($name: self.$name.clone(),)*
                }
            }
        }
    }
}

impl StyleMap {
    pub fn queue_reset<W: Write>(
        &self,
        f: &mut W,
    ) -> Result<(), ProgramError> {
        if self.styled {
            f.queue(SetBackgroundColor(Color::Reset))?;
        }
        Ok(())
    }
    pub fn good_to_bad_color(
        &self,
        value: f64,
    ) -> Color {
        debug_assert!((0.0..=1.0).contains(&value));
        const N: usize = 10;
        let idx = (value * N as f64) as usize;
        let cs = match idx {
            0 => &self.good_to_bad_0,
            1 => &self.good_to_bad_1,
            2 => &self.good_to_bad_2,
            3 => &self.good_to_bad_3,
            4 => &self.good_to_bad_4,
            5 => &self.good_to_bad_5,
            6 => &self.good_to_bad_6,
            7 => &self.good_to_bad_7,
            8 => &self.good_to_bad_8,
            _ => &self.good_to_bad_9,
        };
        cs.object_style.foreground_color.unwrap_or(Color::Blue)
    }
}

// Default styles defined as
//    name: forecolor, backcolor, [attributes]
// The optional part after a '/' is the style for unfocused panels
// (if missing the style is the same than for focused panels)
StyleMap! {
    default: rgb(237, 244, 255), rgb(9, 16, 27), [] / rgb(142, 162, 191), rgb(6, 11, 20), []
    tree: rgb(142, 162, 191), None, [] / rgb(53, 80, 111), None, []
    parent: rgb(237, 244, 255), None, [] / rgb(142, 162, 191), None, []
    file: rgb(237, 244, 255), None, [] / rgb(142, 162, 191), None, []
    directory: rgb(126, 196, 255), None, [Bold] / rgb(126, 196, 255), None, []
    exe: rgb(255, 178, 86), None, []
    link: Some(Magenta), None, []
    pruning: rgb(142, 162, 191), None, [Italic]
    perm__: gray(5), None, []
    perm_r: ansi(94), None, []
    perm_w: ansi(132), None, []
    perm_x: ansi(65), None, []
    owner: ansi(138), None, []
    group: ansi(131), None, []
    count: rgb(142, 162, 191), rgb(6, 11, 20), []
    dates: rgb(142, 162, 191), None, []
    sparse: ansi(214), None, []
    content_extract: ansi(29), None, []
    content_match: ansi(34), None, []
    device_id_major: ansi(138), None, []
    device_id_sep: ansi(102), None, []
    device_id_minor: ansi(138), None, []
    git_branch: ansi(178), None, []
    git_insertions: ansi(28), None, []
    git_deletions: ansi(160), None, []
    git_status_current: gray(5), None, []
    git_status_modified: ansi(28), None, []
    git_status_new: ansi(94), None, [Bold]
    git_status_ignored: gray(17), None, []
    git_status_conflicted: ansi(88), None, []
    git_status_other: ansi(88), None, []
    selected_line: None, rgb(32, 64, 100), [] / None, rgb(20, 54, 87), []
    char_match: rgb(126, 196, 255), None, [Bold]
    file_error: rgb(224, 90, 90), None, []
    flag_label: rgb(142, 162, 191), rgb(6, 11, 20), []
    flag_value: rgb(255, 178, 86), rgb(6, 11, 20), [Bold]
    input: rgb(237, 244, 255), rgb(6, 11, 20), [] / rgb(142, 162, 191), rgb(6, 11, 20), []
    status_error: rgb(237, 244, 255), rgb(224, 90, 90), []
    status_job: rgb(255, 178, 86), rgb(6, 11, 20), [Bold]
    status_normal: rgb(237, 244, 255), rgb(6, 11, 20), [] / rgb(142, 162, 191), rgb(6, 11, 20), []
    status_italic: rgb(255, 178, 86), rgb(6, 11, 20), [] / rgb(255, 178, 86), rgb(6, 11, 20), []
    status_bold: rgb(255, 178, 86), rgb(6, 11, 20), [Bold] / rgb(255, 178, 86), rgb(6, 11, 20), [Bold]
    status_code: rgb(126, 196, 255), rgb(6, 11, 20), [] / rgb(126, 196, 255), rgb(6, 11, 20), []
    status_ellipsis: rgb(53, 80, 111), rgb(6, 11, 20), [] / rgb(53, 80, 111), rgb(6, 11, 20), []
    purpose_normal: rgb(237, 244, 255), rgb(6, 11, 20), []
    purpose_italic: rgb(255, 178, 86), rgb(6, 11, 20), []
    purpose_bold: rgb(255, 178, 86), rgb(6, 11, 20), [Bold]
    purpose_ellipsis: rgb(53, 80, 111), rgb(6, 11, 20), []
    scrollbar_track: rgb(53, 80, 111), None, [] / rgb(20, 54, 87), None, []
    scrollbar_thumb: rgb(126, 196, 255), None, [] / rgb(142, 162, 191), None, []
    help_paragraph: gray(20), None, []
    help_bold: ansi(178), None, [Bold]
    help_italic: ansi(229), None, []
    help_code: gray(21), gray(3), []
    help_headers: ansi(178), None, []
    help_table_border: ansi(239), None, []
    preview: rgb(215, 227, 244), rgb(10, 13, 18), [] / rgb(142, 162, 191), rgb(6, 11, 20), []
    preview_title: rgb(237, 244, 255), rgb(9, 16, 27), [] / rgb(142, 162, 191), rgb(6, 11, 20), []
    preview_line_number: rgb(123, 144, 167), rgb(10, 13, 18), []
    preview_separator: rgb(53, 80, 111), None, []
    preview_match: None, rgb(18, 42, 63), []
    hex_null: gray(8), None, []
    hex_ascii_graphic: gray(18), None, []
    hex_ascii_whitespace: ansi(143), None, []
    hex_ascii_other: ansi(215), None, []
    hex_non_ascii: ansi(167), None, []
    staging_area_title: gray(22), gray(2), [] / gray(20), gray(3), []
    mode_command_mark: rgb(9, 16, 27), rgb(255, 178, 86), [Bold]
    frame_title: rgb(126, 196, 255), None, [Bold] / rgb(142, 162, 191), None, []
    good_to_bad_0: ansi(28), None, []
    good_to_bad_1: ansi(29), None, []
    good_to_bad_2: ansi(29), None, []
    good_to_bad_3: ansi(29), None, []
    good_to_bad_4: ansi(29), None, []
    good_to_bad_5: ansi(100), None, []
    good_to_bad_6: ansi(136), None, []
    good_to_bad_7: ansi(172), None, []
    good_to_bad_8: ansi(166), None, []
    good_to_bad_9: ansi(196), None, []
}

impl fmt::Debug for StyleMap {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        write!(f, "Skin")
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::skin::SkinEntry,
        crokey::crossterm::style::Color,
        rustc_hash::FxHashMap,
    };

    /// the panel_alt background — the colour that all footer-zone keys
    /// (except status_error and mode_command_mark) must default to.
    const PANEL_ALT: Color = Color::Rgb {
        r: 6,
        g: 11,
        b: 20,
    };

    /// orange — the accent_warn / selection_bar colour used as bg of
    /// mode_command_mark and as fg for several emphasis keys.
    const ACCENT_WARN: Color = Color::Rgb {
        r: 255,
        g: 178,
        b: 86,
    };

    /// the inverted text colour used inside the command-mode marker.
    const PANEL_BASE: Color = Color::Rgb {
        r: 9,
        g: 16,
        b: 27,
    };

    /// red — the cut_bar colour used as the error background.
    const CUT_BAR: Color = Color::Rgb {
        r: 224,
        g: 90,
        b: 90,
    };

    /// the default text fg used over panel_alt.
    const TEXT: Color = Color::Rgb {
        r: 237,
        g: 244,
        b: 255,
    };

    #[test]
    fn footer_zone_uses_panel_alt_bg() {
        let maps = StyleMaps::create(&FxHashMap::default());
        // 13 keys that must all use rgb(6, 11, 20) as background.
        // status_error has a red bg by design; mode_command_mark has
        // an orange bg by design — both excluded here.
        let pairs: [(&str, Option<Color>); 13] = [
            ("status_normal", maps.focused.status_normal.get_bg()),
            ("status_italic", maps.focused.status_italic.get_bg()),
            ("status_bold", maps.focused.status_bold.get_bg()),
            ("status_code", maps.focused.status_code.get_bg()),
            ("status_ellipsis", maps.focused.status_ellipsis.get_bg()),
            ("status_job", maps.focused.status_job.get_bg()),
            ("flag_label", maps.focused.flag_label.get_bg()),
            ("flag_value", maps.focused.flag_value.get_bg()),
            ("input", maps.focused.input.get_bg()),
            ("purpose_normal", maps.focused.purpose_normal.get_bg()),
            ("purpose_italic", maps.focused.purpose_italic.get_bg()),
            ("purpose_bold", maps.focused.purpose_bold.get_bg()),
            ("purpose_ellipsis", maps.focused.purpose_ellipsis.get_bg()),
        ];
        for (name, bg) in pairs {
            assert_eq!(
                bg,
                Some(PANEL_ALT),
                "expected {name} to have panel_alt bg",
            );
        }
    }

    #[test]
    fn footer_zone_unfocused_input_gains_panel_alt_bg() {
        // Regression pin: pre-flip, `input` unfocused had no bg.
        // Post-flip it must have the same panel_alt bg as focused.
        let maps = StyleMaps::create(&FxHashMap::default());
        assert_eq!(maps.unfocused.input.get_bg(), Some(PANEL_ALT));
    }

    #[test]
    fn footer_zone_unfocused_all_keys_have_panel_alt_bg() {
        // All thirteen footer-zone keys must default to panel_alt bg
        // for the unfocused panel as well as the focused one, so the
        // dark-blue chrome stays consistent regardless of which panel
        // holds focus.
        let maps = StyleMaps::create(&FxHashMap::default());
        let pairs: [(&str, Option<Color>); 13] = [
            ("status_normal", maps.unfocused.status_normal.get_bg()),
            ("status_italic", maps.unfocused.status_italic.get_bg()),
            ("status_bold", maps.unfocused.status_bold.get_bg()),
            ("status_code", maps.unfocused.status_code.get_bg()),
            ("status_ellipsis", maps.unfocused.status_ellipsis.get_bg()),
            ("status_job", maps.unfocused.status_job.get_bg()),
            ("flag_label", maps.unfocused.flag_label.get_bg()),
            ("flag_value", maps.unfocused.flag_value.get_bg()),
            ("input", maps.unfocused.input.get_bg()),
            ("purpose_normal", maps.unfocused.purpose_normal.get_bg()),
            ("purpose_italic", maps.unfocused.purpose_italic.get_bg()),
            ("purpose_bold", maps.unfocused.purpose_bold.get_bg()),
            (
                "purpose_ellipsis",
                maps.unfocused.purpose_ellipsis.get_bg(),
            ),
        ];
        for (name, bg) in pairs {
            assert_eq!(
                bg,
                Some(PANEL_ALT),
                "expected unfocused {name} to have panel_alt bg",
            );
        }
    }

    #[test]
    fn mode_command_mark_uses_accent_warn_bg() {
        let maps = StyleMaps::create(&FxHashMap::default());
        assert_eq!(
            maps.focused.mode_command_mark.get_bg(),
            Some(ACCENT_WARN),
        );
        assert_eq!(
            maps.focused.mode_command_mark.get_fg(),
            Some(PANEL_BASE),
        );
    }

    #[test]
    fn status_error_uses_cut_bar_bg() {
        let maps = StyleMaps::create(&FxHashMap::default());
        assert_eq!(maps.focused.status_error.get_bg(), Some(CUT_BAR));
        assert_eq!(maps.focused.status_error.get_fg(), Some(TEXT));
    }

    #[test]
    fn user_skin_override_wins_over_footer_default() {
        // Sanity check: a user override of `status_normal` must beat
        // the new panel_alt default. Parse a SkinEntry from the same
        // textual form that conf.hjson uses.
        let entry = SkinEntry::parse("White Blue")
            .expect("parse of `White Blue` should succeed");
        let mut overrides: FxHashMap<String, SkinEntry> = FxHashMap::default();
        overrides.insert("status_normal".to_string(), entry);
        let maps = StyleMaps::create(&overrides);
        // The bg must NOT be panel_alt — it should be whatever
        // `parse_compound_style("White Blue")` decoded "Blue" to.
        // We don't pin the exact Color variant (Ansi vs Rgb depends on
        // the parser), we just assert it's no longer panel_alt.
        assert_ne!(
            maps.focused.status_normal.get_bg(),
            Some(PANEL_ALT),
            "user override should win over default panel_alt bg",
        );
    }
}
