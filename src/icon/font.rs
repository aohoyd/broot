use {
    super::*,
    crate::tree::TreeLineType,
    crokey::crossterm::style::Color,
    rustc_hash::FxHashMap,
};

pub struct FontPlugin {
    icon_name_to_icon_codepoint_map: FxHashMap<&'static str, u32>,
    file_name_to_icon_name_map: FxHashMap<&'static str, &'static str>,
    double_extension_to_icon_name_map: FxHashMap<&'static str, &'static str>,
    extension_to_icon_name_map: FxHashMap<&'static str, &'static str>,
    dir_name_to_icon_name_map: FxHashMap<&'static str, &'static str>,
    default_icon_point: u32,
    colored: bool,
}

impl FontPlugin {
    #[cfg(debug_assertions)]
    fn sanity_check(
        part_to_icon_name_map: &FxHashMap<&str, &str>,
        icon_name_to_icon_codepoint_map: &FxHashMap<&str, u32>,
    ) {
        let offending_entries = part_to_icon_name_map
            .values()
            .map(|icon_name| {
                (
                    icon_name,
                    icon_name_to_icon_codepoint_map.contains_key(icon_name),
                )
            })
            // Find if any entry is not present
            .filter(|(_entry, entry_present)| !entry_present)
            .collect::<Vec<_>>();
        for oe in &offending_entries {
            eprintln!("{} is not a valid icon name", oe.0);
        }
        if !offending_entries.is_empty() {
            eprintln!("Terminating execution");
            std::process::exit(53);
        }
    }

    pub fn new(
        icon_name_to_icon_codepoint_map: &'static [(&'static str, u32)],
        double_extension_to_icon_name_map: &'static [(&'static str, &'static str)],
        extension_to_icon_name_map: &'static [(&'static str, &'static str)],
        file_name_to_icon_name_map: &'static [(&'static str, &'static str)],
        dir_name_to_icon_name_map: &'static [(&'static str, &'static str)],
    ) -> Self {
        let icon_name_to_icon_codepoint_map: FxHashMap<_, _> =
            icon_name_to_icon_codepoint_map.iter().copied().collect();
        let double_extension_to_icon_name_map: FxHashMap<_, _> =
            double_extension_to_icon_name_map.iter().copied().collect();
        let extension_to_icon_name_map: FxHashMap<_, _> =
            extension_to_icon_name_map.iter().copied().collect();
        let file_name_to_icon_name_map: FxHashMap<_, _> =
            file_name_to_icon_name_map.iter().copied().collect();
        let dir_name_to_icon_name_map: FxHashMap<_, _> =
            dir_name_to_icon_name_map.iter().copied().collect();

        #[cfg(debug_assertions)]
        {
            Self::sanity_check(
                &file_name_to_icon_name_map,
                &icon_name_to_icon_codepoint_map,
            );
            Self::sanity_check(
                &double_extension_to_icon_name_map,
                &icon_name_to_icon_codepoint_map,
            );
            Self::sanity_check(
                &extension_to_icon_name_map,
                &icon_name_to_icon_codepoint_map,
            );
            Self::sanity_check(
                &dir_name_to_icon_name_map,
                &icon_name_to_icon_codepoint_map,
            );
        }

        let default_icon_point = *icon_name_to_icon_codepoint_map.get("default_file").unwrap();
        Self {
            icon_name_to_icon_codepoint_map,
            file_name_to_icon_name_map,
            double_extension_to_icon_name_map,
            extension_to_icon_name_map,
            dir_name_to_icon_name_map,
            default_icon_point,
            colored: true,
        }
    }

    pub fn mono(mut self) -> Self {
        self.colored = false;
        self
    }

    fn handle_single_extension(
        &self,
        ext: Option<&str>,
    ) -> &'static str {
        match ext {
            None => "default_file",
            Some(e) => match self.extension_to_icon_name_map.get(e as &str) {
                None => "default_file",
                Some(icon_name) => icon_name,
            },
        }
    }

    fn handle_file(
        &self,
        name: &str,
        double_ext: Option<String>,
        ext: Option<String>,
    ) -> &'static str {
        match self.file_name_to_icon_name_map.get(name) {
            Some(icon_name) => icon_name,
            _ => self.handle_double_extension(double_ext.as_deref(), ext.as_deref()),
        }
    }

    fn handle_double_extension(
        &self,
        double_ext: Option<&str>,
        ext: Option<&str>,
    ) -> &'static str {
        match double_ext {
            None => self.handle_single_extension(ext),
            Some(de) => match self.double_extension_to_icon_name_map.get(de as &str) {
                None => self.handle_single_extension(ext),
                Some(icon_name) => icon_name,
            },
        }
    }

    fn handle_dir(&self, name: &str) -> &'static str {
        let lower = name.to_lowercase();
        match self.dir_name_to_icon_name_map.get(lower.as_str()) {
            Some(icon_name) => icon_name,
            None => "default_folder",
        }
    }
}

impl IconPlugin for FontPlugin {
    fn get_icon(
        &self,
        tree_line_type: &TreeLineType,
        name: &str,
        double_ext: Option<&str>,
        ext: Option<&str>,
    ) -> char {
        let icon_name = match tree_line_type {
            TreeLineType::Dir => self.handle_dir(name),
            TreeLineType::SymLink { .. } => "emoji_type_link", //bad but nothing better
            TreeLineType::File => self.handle_file(
                &name.to_ascii_lowercase(),
                double_ext.map(str::to_ascii_lowercase),
                ext.map(str::to_ascii_lowercase),
            ),
            TreeLineType::Pruning => "file_type_kite", //irrelevant
            TreeLineType::BrokenSymLink(_) => "default_file",
        };

        let entry_icon = unsafe {
            std::char::from_u32_unchecked(
                *self
                    .icon_name_to_icon_codepoint_map
                    .get(icon_name)
                    .unwrap_or(&self.default_icon_point),
            )
        };

        entry_icon
    }

    fn get_icon_color(
        &self,
        line_type: &TreeLineType,
        name: &str,
        ext: Option<&str>,
    ) -> Option<Color> {
        if !self.colored {
            return None;
        }
        crate::icon::colors::resolve(line_type, name, ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::TreeLineType;

    fn make_colored_nerdfont() -> FontPlugin {
        FontPlugin::new(
            &include!("../../resources/icons/nerdfont/data/icon_name_to_icon_code_point_map.rs"),
            &include!("../../resources/icons/nerdfont/data/double_extension_to_icon_name_map.rs"),
            &include!("../../resources/icons/nerdfont/data/extension_to_icon_name_map.rs"),
            &include!("../../resources/icons/nerdfont/data/file_name_to_icon_name_map.rs"),
            &include!("../../resources/icons/nerdfont/data/dir_name_to_icon_name_map.rs"),
        )
    }

    #[test]
    fn colored_plugin_returns_color_for_known_ext() {
        let p = make_colored_nerdfont();
        let c = p.get_icon_color(&TreeLineType::File, "main.rs", Some("rs"));
        assert_eq!(c, Some(Color::Rgb { r: 255, g: 143, b: 64 }));
    }

    #[test]
    fn mono_plugin_always_returns_none() {
        let p = make_colored_nerdfont().mono();
        let c = p.get_icon_color(&TreeLineType::File, "main.rs", Some("rs"));
        assert!(c.is_none());
    }

    #[test]
    fn colored_is_default() {
        let p = make_colored_nerdfont();
        let c = p.get_icon_color(&TreeLineType::Dir, ".git", None);
        assert_eq!(c, Some(Color::Rgb { r: 138, g: 146, b: 168 }));
    }

    fn dir_glyph(name: &str) -> char {
        make_colored_nerdfont().get_icon(&TreeLineType::Dir, name, None, None)
    }

    #[test]
    fn dir_glyph_dotgit() {
        assert_eq!(dir_glyph(".git"), '\u{f02a2}');
    }

    #[test]
    fn dir_glyph_node_modules() {
        assert_eq!(dir_glyph("node_modules"), '\u{f03d7}');
    }

    #[test]
    fn dir_glyph_docs() {
        assert_eq!(dir_glyph("docs"), '\u{f19f7}');
    }

    #[test]
    fn dir_glyph_target() {
        assert_eq!(dir_glyph("target"), '\u{f19fd}');
    }

    #[test]
    fn dir_glyph_unknown_falls_back_to_default_folder() {
        assert_eq!(dir_glyph("RandomDir"), '\u{f07b}');
    }

    #[test]
    fn dir_glyph_lookup_is_case_insensitive() {
        assert_eq!(dir_glyph(".Git"), dir_glyph(".git"));
        assert_eq!(dir_glyph("DOCS"), dir_glyph("docs"));
    }

    #[test]
    fn dir_glyph_lookup_folds_non_ascii_uppercase() {
        // Música → music glyph (lowercase music key in the table)
        assert_eq!(dir_glyph("Música"), dir_glyph("music"));
    }
}
