mod anchor;
mod closest;
mod common;
mod from;
mod normalize;
mod special_path;

pub use {
    anchor::*,
    closest::*,
    common::*,
    from::*,
    normalize::*,
    special_path::*,
};

use std::{
    ffi::OsStr,
    path::Path,
};

pub fn path_has_ext<P: AsRef<Path>>(
    path: P,
    ext: &str,
) -> bool {
    path.as_ref()
        .extension()
        .and_then(OsStr::to_str)
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}
