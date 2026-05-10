//! Bookmarks: single-character jump targets surfaced by the Goto
//! modal (Task 12+). Pure data plumbing here — no UI.
//!
//! - `BookmarkConf` (in `crate::conf`) is the HJSON-serde shape.
//! - `BookmarkEntry` is the resolved runtime shape.
//! - `build_bookmarks` materialises the user list (or built-in defaults)
//!   with path expansion and duplicate-key handling.

use {
    crate::conf::BookmarkConf,
    directories::UserDirs,
    std::path::PathBuf,
};

/// A resolved bookmark, ready for use by the Goto modal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkEntry {
    /// Single-character jump key.
    pub key: char,
    /// Resolved filesystem (or trash) path.
    pub path: PathBuf,
    /// Short display label — usually the basename of `path`, with a few
    /// human-friendly overrides (Home / Trash / ...).
    pub label: String,
}

/// Resolve a raw bookmark path string into a `PathBuf`.
///
/// Recognises:
/// - `~` and `~/...`            → user home dir
/// - `${HOME}` / `${HOME}/...`  → user home dir (or `$HOME` env var as fallback)
/// - `${XDG_CONFIG_HOME}`       → that env var if set, else `~/.config`
/// - `trash://`                 → platform trash directory (or `None` on Windows)
/// - everything else            → parsed as a `PathBuf` verbatim
///
/// Returns `None` for unresolvable inputs (and logs a warning).
pub(crate) fn resolve(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        warn!("empty bookmark path");
        return None;
    }

    if raw == "trash://" {
        return resolve_trash();
    }

    // ${HOME} substitution (handle both `${HOME}` alone and `${HOME}/...`)
    if let Some(rest) = raw.strip_prefix("${HOME}") {
        let home = home_dir()?;
        return Some(join_rest(home, rest));
    }
    if let Some(rest) = raw.strip_prefix("${XDG_CONFIG_HOME}") {
        let xdg = xdg_config_home()?;
        return Some(join_rest(xdg, rest));
    }

    // Tilde expansion: only when ~ is the first path segment.
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        let home = home_dir()?;
        return Some(home.join(rest));
    }

    // Plain path — keep as-is. Existence is *not* checked; the Goto
    // modal will surface broot's standard not-found error at use time.
    Some(PathBuf::from(raw))
}

fn join_rest(
    base: PathBuf,
    rest: &str,
) -> PathBuf {
    if rest.is_empty() {
        return base;
    }
    // strip a single leading '/' so we don't lose `base` to absolute-path
    // joining semantics.
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    if rest.is_empty() {
        base
    } else {
        base.join(rest)
    }
}

fn home_dir() -> Option<PathBuf> {
    if let Some(user_dirs) = UserDirs::new() {
        return Some(user_dirs.home_dir().to_path_buf());
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return Some(PathBuf::from(home));
        }
    }
    warn!("no home dir found while resolving bookmark");
    None
}

fn xdg_config_home() -> Option<PathBuf> {
    let env_value = std::env::var("XDG_CONFIG_HOME").ok();
    xdg_config_home_with(env_value.as_deref(), home_dir().as_deref())
}

/// Pure-function variant of `xdg_config_home` for unit tests. Returns
/// `Some(env_value)` when the env value is non-empty; otherwise falls
/// back to `<home>/.config`. `None` only when both inputs are missing.
fn xdg_config_home_with(
    env_value: Option<&str>,
    home: Option<&std::path::Path>,
) -> Option<PathBuf> {
    if let Some(v) = env_value {
        if !v.is_empty() {
            return Some(PathBuf::from(v));
        }
    }
    home.map(|h| h.join(".config"))
}

#[cfg(target_os = "macos")]
fn resolve_trash() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".Trash"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn resolve_trash() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".local/share/Trash"))
}

/// Windows fallback for `trash://`. Windows does not have a stable
/// per-user trash directory exposed as a single path (the Recycle Bin
/// surfaces through Shell APIs, not a directory). Broot does not
/// open a trash view on Windows either, so we deliberately drop the
/// bookmark and emit a warning rather than silently mapping it to
/// some plausible-looking but wrong path.
#[cfg(target_os = "windows")]
fn resolve_trash() -> Option<PathBuf> {
    warn!("trash:// bookmark is not supported on Windows; broot has no trash view there");
    None
}

/// Other-platform fallback. Same shape as the Windows branch — drop
/// the bookmark with a warning rather than fabricating a path.
#[cfg(not(any(unix, target_os = "windows")))]
fn resolve_trash() -> Option<PathBuf> {
    warn!("trash:// bookmark is not supported on this platform");
    None
}

/// Build the runtime bookmark list from optional user config.
///
/// - `None` (no `bookmarks` key in conf) ⇒ built-in defaults.
/// - `Some(list)` (even empty) ⇒ that explicit list, fully replacing defaults.
///
/// Duplicate keys log a warning and keep the *first* occurrence.
/// Unresolvable paths are dropped (already warned by `resolve`).
pub(crate) fn build_bookmarks(user: Option<&[BookmarkConf]>) -> Vec<BookmarkEntry> {
    match user {
        None => default_bookmarks(),
        Some(list) => materialise(list.iter().cloned()),
    }
}

fn default_bookmarks() -> Vec<BookmarkEntry> {
    let raw = [
        ('h', "~"),
        ('d', "~/Downloads"),
        ('c', "${XDG_CONFIG_HOME}"),
        ('t', "trash://"),
    ];
    materialise(raw.iter().map(|(k, p)| BookmarkConf {
        key: *k,
        path: (*p).to_string(),
    }))
}

fn materialise<I: IntoIterator<Item = BookmarkConf>>(items: I) -> Vec<BookmarkEntry> {
    let mut out: Vec<BookmarkEntry> = Vec::new();
    for conf in items {
        if out.iter().any(|e| e.key == conf.key) {
            warn!(
                "duplicate bookmark key {:?} (path {:?}); keeping first definition",
                conf.key, conf.path
            );
            continue;
        }
        let raw = conf.path.trim().to_string();
        let Some(path) = resolve(&raw) else {
            warn!("dropping bookmark {:?} → {:?}: unresolvable path", conf.key, raw);
            continue;
        };
        let label = label_for(&raw, &path);
        out.push(BookmarkEntry {
            key: conf.key,
            path,
            label,
        });
    }
    out
}

fn label_for(
    raw: &str,
    path: &std::path::Path,
) -> String {
    if raw == "trash://" {
        return "Trash".to_string();
    }
    if raw == "~" || raw == "${HOME}" {
        return "Home".to_string();
    }
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        home_dir().expect("a home dir to exist in the test env")
    }

    fn bm(
        key: char,
        path: &str,
    ) -> BookmarkConf {
        BookmarkConf {
            key,
            path: path.to_string(),
        }
    }

    #[test]
    fn resolves_tilde_alone() {
        assert_eq!(resolve("~"), Some(home()));
    }

    #[test]
    fn resolves_tilde_with_subpath() {
        assert_eq!(resolve("~/Downloads"), Some(home().join("Downloads")));
    }

    #[test]
    fn resolves_braced_home() {
        assert_eq!(resolve("${HOME}"), Some(home()));
        assert_eq!(resolve("${HOME}/Downloads"), Some(home().join("Downloads")));
    }

    #[test]
    fn resolves_xdg_config_home_default() {
        // We don't mutate env state here; either the env var is set
        // (then result is that path) or it falls back to ~/.config.
        let resolved = resolve("${XDG_CONFIG_HOME}").expect("xdg config home should resolve");
        if let Ok(env_value) = std::env::var("XDG_CONFIG_HOME") {
            if !env_value.is_empty() {
                assert_eq!(resolved, PathBuf::from(env_value));
                return;
            }
        }
        assert_eq!(resolved, home().join(".config"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn resolves_trash_macos() {
        assert_eq!(resolve("trash://"), Some(home().join(".Trash")));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn resolves_trash_linux() {
        assert_eq!(resolve("trash://"), Some(home().join(".local/share/Trash")));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn trash_is_unsupported_on_windows() {
        assert_eq!(resolve("trash://"), None);
    }

    #[test]
    fn resolves_absolute_path_unchanged() {
        assert_eq!(
            resolve("/absolute/path"),
            Some(PathBuf::from("/absolute/path"))
        );
    }

    #[test]
    fn does_not_panic_on_weird_input() {
        // Should produce *some* PathBuf without panicking.
        let _ = resolve("invalid//path:with:colons");
    }

    #[test]
    fn resolve_empty_string_returns_none() {
        // Defensive: an empty path string is unresolvable and must not
        // be treated as the current directory.
        assert_eq!(resolve(""), None);
    }

    #[test]
    fn resolve_whitespace_only_returns_none() {
        // Whitespace-only strings are also unresolvable. The trim in
        // `resolve` reduces them to the empty string and bails.
        assert_eq!(resolve("   "), None);
        assert_eq!(resolve("\t\n"), None);
    }

    #[test]
    fn xdg_config_home_with_env_takes_precedence() {
        let home = std::path::Path::new("/home/me");
        let r = xdg_config_home_with(Some("/custom/cfg"), Some(home));
        assert_eq!(r, Some(PathBuf::from("/custom/cfg")));
    }

    #[test]
    fn xdg_config_home_with_empty_env_falls_back_to_home_config() {
        let home = std::path::Path::new("/home/me");
        let r = xdg_config_home_with(Some(""), Some(home));
        assert_eq!(r, Some(PathBuf::from("/home/me/.config")));
    }

    #[test]
    fn xdg_config_home_with_missing_env_falls_back_to_home_config() {
        let home = std::path::Path::new("/home/me");
        let r = xdg_config_home_with(None, Some(home));
        assert_eq!(r, Some(PathBuf::from("/home/me/.config")));
    }

    #[test]
    fn xdg_config_home_with_no_home_returns_none() {
        let r = xdg_config_home_with(None, None);
        assert_eq!(r, None);
    }

    #[test]
    fn malformed_bookmark_conf_drops_dropped_entries_silently() {
        // Empty path -> resolve returns None -> entry dropped, no panic.
        // Other entries in the same list survive intact.
        let confs = [bm('a', ""), bm('b', "/tmp/some_path")];
        let bookmarks = build_bookmarks(Some(&confs));
        assert_eq!(bookmarks.len(), 1, "empty-path entry must be dropped");
        assert_eq!(bookmarks[0].key, 'b');
    }

    #[test]
    fn build_bookmarks_none_yields_defaults() {
        let bookmarks = build_bookmarks(None);
        // Trash on Windows resolves to None and is dropped — keep the test
        // tolerant of that platform difference, but the other 3 must always
        // be present.
        let keys: Vec<char> = bookmarks.iter().map(|b| b.key).collect();
        assert!(keys.contains(&'h'), "default bookmarks should include 'h'");
        assert!(keys.contains(&'d'), "default bookmarks should include 'd'");
        assert!(keys.contains(&'c'), "default bookmarks should include 'c'");
        #[cfg(not(target_os = "windows"))]
        assert!(keys.contains(&'t'), "default bookmarks should include 't'");
    }

    #[test]
    fn build_bookmarks_explicit_empty_replaces_defaults() {
        let bookmarks = build_bookmarks(Some(&[]));
        assert!(
            bookmarks.is_empty(),
            "explicit empty bookmarks list must NOT silently re-default; got {bookmarks:?}",
        );
    }

    #[test]
    fn duplicate_keys_first_wins() {
        let confs = [bm('h', "/foo"), bm('h', "/bar")];
        let bookmarks = build_bookmarks(Some(&confs));
        assert_eq!(bookmarks.len(), 1, "duplicate keys collapse to one entry");
        assert_eq!(bookmarks[0].key, 'h');
        assert_eq!(bookmarks[0].path, PathBuf::from("/foo"));
    }

    #[test]
    fn home_bookmark_uses_friendly_label() {
        let bookmarks = build_bookmarks(Some(&[bm('h', "~")]));
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].label, "Home");
    }

    #[test]
    fn trash_bookmark_uses_friendly_label() {
        let bookmarks = build_bookmarks(Some(&[bm('t', "trash://")]));
        if cfg!(target_os = "windows") {
            assert!(bookmarks.is_empty(), "windows drops trash bookmark");
        } else {
            assert_eq!(bookmarks.len(), 1);
            assert_eq!(bookmarks[0].label, "Trash");
        }
    }

    #[test]
    fn basename_label_for_regular_path() {
        let bookmarks = build_bookmarks(Some(&[bm('p', "/tmp/projects")]));
        assert_eq!(bookmarks.len(), 1);
        assert_eq!(bookmarks[0].label, "projects");
    }

    #[test]
    fn deserializes_bookmarks_from_hjson() {
        // Round-trip a snippet through the same HJSON deserializer the
        // real Conf uses, confirming the shape lines up.
        let hjson = r#"{
            bookmarks: [
                { key: 'h', path: '~' }
                { key: 'd', path: '~/Downloads' }
            ]
        }"#;
        let conf: crate::conf::Conf = deser_hjson::from_str(hjson)
            .expect("hjson with bookmarks should deserialize");
        let list = conf
            .bookmarks
            .expect("bookmarks field should be Some when present in hjson");
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].key, 'h');
        assert_eq!(list[0].path, "~");
        assert_eq!(list[1].key, 'd');
        assert_eq!(list[1].path, "~/Downloads");
    }
}
