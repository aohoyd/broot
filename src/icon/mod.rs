mod font;
mod icon_plugin;

use font::FontPlugin;

pub use icon_plugin::IconPlugin;

/// Build the icon plugin matching the requested icon-set name.
///
/// The literal string `"none"` (case-insensitive) is recognised as
/// an explicit opt-out and yields `None` — this lets users disable
/// icons in their config without deleting the line, by writing
/// `icon_theme: none`.
///
/// Any unknown name also yields `None`.
pub fn icon_plugin(icon_set: &str) -> Option<Box<dyn IconPlugin + Send + Sync>> {
    if icon_set.eq_ignore_ascii_case("none") {
        return None;
    }
    match icon_set {
        "vscode" => Some(Box::new(FontPlugin::new(
            &include!("../../resources/icons/vscode/data/icon_name_to_icon_code_point_map.rs"),
            &include!("../../resources/icons/vscode/data/double_extension_to_icon_name_map.rs"),
            &include!("../../resources/icons/vscode/data/extension_to_icon_name_map.rs"),
            &include!("../../resources/icons/vscode/data/file_name_to_icon_name_map.rs"),
        ))),
        "nerdfont" => Some(Box::new(FontPlugin::new(
            &include!("../../resources/icons/nerdfont/data/icon_name_to_icon_code_point_map.rs"),
            &include!("../../resources/icons/nerdfont/data/double_extension_to_icon_name_map.rs"),
            &include!("../../resources/icons/nerdfont/data/extension_to_icon_name_map.rs"),
            &include!("../../resources/icons/nerdfont/data/file_name_to_icon_name_map.rs"),
        ))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nerdfont_resolves_to_a_plugin() {
        assert!(icon_plugin("nerdfont").is_some());
    }

    #[test]
    fn vscode_resolves_to_a_plugin() {
        assert!(icon_plugin("vscode").is_some());
    }

    #[test]
    fn explicit_none_disables_icons() {
        assert!(icon_plugin("none").is_none());
    }

    #[test]
    fn explicit_none_is_case_insensitive() {
        assert!(icon_plugin("None").is_none());
        assert!(icon_plugin("NONE").is_none());
    }

    #[test]
    fn unknown_theme_disables_icons() {
        assert!(icon_plugin("not-a-real-theme").is_none());
    }
}
