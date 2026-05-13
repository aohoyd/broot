use {
    crate::{
        errors::ConfError,
        path::PathAnchor,
    },
    lazy_regex::*,
    std::fmt,
    std::str::FromStr,
};

/// A `{name:flags}` group in a verb definition string, where `name` is the argument name and
/// `flags` is a comma-separated list of flags that modify how the argument is processed.
///
/// This pattern is also used slightly differently in verb invocations, where the flags part
/// can be used to specify a default value.
pub static ARG_DEF_GROUP: Lazy<Regex> = lazy_regex!(r"\{([^{}:]+)(?::([^{}:]+))?\}");

#[derive(Debug, Clone, PartialEq)]
pub struct VerbArgDef {
    pub name: String,
    pub flags: Vec<VerbArgFlag>,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VerbArgFlag {
    CommaSeparated,
    SpaceSeparated,
    PathFromDirectory,
    PathFromParent,
    Theme,
    /// Marker used inside an invocation pattern's `{name:backup-name}`
    /// group. **Intentionally inert** — never consulted by
    /// `has_flag(...)`, `merging_flag()`, or `path_anchor()`. The
    /// `ARG_DEF_GROUP` regex is shared between verb definitions
    /// (where the segment after `:` lists flags) and invocation
    /// patterns (where the same segment names a default-value source
    /// consumed by `invocation_with_default`). Registering
    /// `backup-name` as a flag variant silences the
    /// `from_capture` warn-on-unknown for invocation-pattern strings
    /// without otherwise changing behaviour: the actual substitution
    /// is keyed on the `"backup-name"` string inside
    /// `ExecutionBuilder::get_sel_name_standard_replacement` and
    /// flows through the default-value path, never the flag path.
    /// Sibling to the equally-inert `Theme` variant.
    BackupName,
}

impl VerbArgFlag {
    pub fn is_merging(&self) -> bool {
        matches!(self, Self::CommaSeparated | Self::SpaceSeparated)
    }
    pub fn merge_values(
        &self,
        args: Vec<String>,
    ) -> Option<String> {
        if args.is_empty() {
            return None;
        }
        match self {
            Self::CommaSeparated => Some(args.join(",")),
            Self::SpaceSeparated => Some(args.join(" ")),
            _ => None,
        }
    }
    pub fn path_anchor(&self) -> PathAnchor {
        match self {
            Self::PathFromDirectory => PathAnchor::Directory,
            Self::PathFromParent => PathAnchor::Parent,
            _ => crate::path::PathAnchor::Unspecified,
        }
    }
}

impl VerbArgDef {
    /// Assuming a valid capture from the GROUP regex, parse the argument definition
    pub fn from_capture(capture: &Captures<'_>) -> VerbArgDef {
        let name = capture
            .get(1)
            .map(|m| m.as_str())
            .unwrap_or_else(|| {
                // internal error, the regex should guarantee this group exists
                error!("Invalid capture for argument definition");
                "???"
            })
            .to_string();
        let flags = capture
            .get(2)
            .map(|m| {
                m.as_str()
                    .split(',')
                    .map(str::trim)
                    .filter_map(|s| match s.parse() {
                        Ok(flag) => Some(flag),
                        Err(e) => {
                            warn!("Invalid flag '{}' in argument definition: {}", s, e);
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        VerbArgDef {
            name: name.as_str().to_string(),
            flags,
        }
    }
    pub fn merging_flag(&self) -> Option<VerbArgFlag> {
        for flag in &self.flags {
            if flag.is_merging() {
                return Some(*flag);
            }
        }
        None
    }
    pub fn has_flag(
        &self,
        flag: VerbArgFlag,
    ) -> bool {
        self.flags.contains(&flag)
    }
    pub fn path_anchor(&self) -> PathAnchor {
        for flag in &self.flags {
            let anchor = flag.path_anchor();
            if anchor != PathAnchor::Unspecified {
                return anchor;
            }
        }
        PathAnchor::Unspecified
    }
}

impl FromStr for VerbArgFlag {
    type Err = ConfError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "comma-separated" => Ok(Self::CommaSeparated),
            "space-separated" => Ok(Self::SpaceSeparated),
            "path-from-directory" => Ok(Self::PathFromDirectory),
            "path-from-parent" => Ok(Self::PathFromParent),
            "theme" => Ok(Self::Theme),
            "backup-name" => Ok(Self::BackupName),
            _ => Err(ConfError::UnknownVerbArgFlag {
                name: s.to_string(),
            }),
        }
    }
}

impl fmt::Display for VerbArgFlag {
    fn fmt(
        &self,
        f: &mut fmt::Formatter<'_>,
    ) -> fmt::Result {
        let s = match self {
            Self::CommaSeparated => "comma-separated",
            Self::SpaceSeparated => "space-separated",
            Self::PathFromDirectory => "path-from-directory",
            Self::PathFromParent => "path-from-parent",
            Self::Theme => "theme",
            Self::BackupName => "backup-name",
        };
        write!(f, "{s}")
    }
}
