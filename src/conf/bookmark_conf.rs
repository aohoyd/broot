use serde::Deserialize;

/// A bookmark entry as declared in the user's HJSON config.
///
/// The runtime side (`crate::app::bookmark::BookmarkEntry`) is built
/// from a list of these by `build_bookmarks`.
#[derive(Debug, Clone, Deserialize)]
pub struct BookmarkConf {
    pub key: char,
    pub path: String,
}
