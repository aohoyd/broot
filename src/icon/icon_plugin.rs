use crate::tree::TreeLineType;
use crokey::crossterm::style::Color;

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
        _ext: Option<&str>,
    ) -> Option<Color> {
        None
    }
}
