use {
    super::*,
    crate::{
        app::*,
        command::*,
        path::{self, PathAnchor},
        stage::Stage,
    },
    regex::Captures,
    rustc_hash::FxHashMap,
    std::path::{Path, PathBuf},
};

/// a temporary structure gathering selection and invocation
/// parameters and able to generate an executable string from
/// a verb's execution pattern
pub struct ExecutionBuilder<'b> {
    /// the current file selection
    pub sel_info: SelInfo<'b>,

    /// the current root of the app
    root: &'b Path,

    /// the selection in the other panel, when there are exactly two
    other_file: Option<&'b PathBuf>,

    /// the staging area
    stage: &'b Stage,

    /// parsed arguments
    invocation_values: Option<FxHashMap<String, String>>,

    /// whether to keep groups which can't be solved or remove them
    keep_groups: bool,

    target: Target,
}

/// Whether we're trying to build the command as a string or as a vec of tokens (in
/// which case we don't want to do the same escaping, for example)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    String,
    Tokens,
}

impl<'b> ExecutionBuilder<'b> {
    /// constructor to use when there's no invocation string
    /// (because we're in the process of building one, for example
    /// when a verb is triggered from a key shortcut)
    pub fn without_invocation(
        sel_info: SelInfo<'b>,
        app_state: &'b AppState,
    ) -> Self {
        Self {
            sel_info,
            root: &app_state.root,
            other_file: app_state.other_panel_path.as_ref(),
            stage: &app_state.stage,
            invocation_values: None,
            keep_groups: false,
            target: Target::Tokens,
        }
    }
    pub fn with_invocation(
        invocation_parser: Option<&InvocationParser>,
        sel_info: SelInfo<'b>,
        app_state: &'b AppState,
        invocation_args: Option<&String>,
    ) -> Self {
        let invocation_values = invocation_parser
            .as_ref()
            .zip(invocation_args.as_ref())
            .and_then(|(parser, args)| parser.parse(args));
        Self {
            sel_info,
            root: &app_state.root,
            other_file: app_state.other_panel_path.as_ref(),
            stage: &app_state.stage,
            invocation_values,
            keep_groups: false,
            target: Target::Tokens,
        }
    }

    /// Return the replacing value for the whole sel_info
    ///
    /// When you have a multiselection and no merging flag, don't call this function
    /// but get_sel_capture_replacement while building a command per selection.
    fn get_arg_replacement(
        &self,
        arg_def: &VerbArgDef,
        con: &AppContext,
    ) -> Option<String> {
        let merging_flag = arg_def.merging_flag();
        match self.sel_info {
            SelInfo::None => self.get_sel_arg_replacement(arg_def, None, con),
            SelInfo::One(sel) => self.get_sel_arg_replacement(arg_def, Some(sel), con),
            SelInfo::More(stage) => {
                let mut sels = stage.to_selections();
                if let Some(merging_flag) = merging_flag {
                    let mut values = Vec::new();
                    for sel in sels {
                        let rcr = self.get_sel_arg_replacement(arg_def, Some(sel), con);
                        if let Some(rcr) = rcr {
                            values.push(rcr);
                        }
                    }
                    merging_flag.merge_values(values)
                } else {
                    // we're called with no specific selection and there's no merging
                    // strategy, this should probably not really happen, we'll take
                    // the first selection
                    let sel = if sels.is_empty() {
                        None
                    } else {
                        Some(sels.swap_remove(0))
                    };
                    self.get_sel_arg_replacement(arg_def, sel, con)
                }
            }
        }
    }

    /// return the standard replacement (ie not one from the invocation)
    fn get_sel_name_standard_replacement(
        &self,
        name: &str,
        sel: Option<Selection<'_>>,
        con: &AppContext,
    ) -> Option<String> {
        match name {
            "root" => Some(self.path_to_string(self.root)),
            "initial-root" => Some(self.path_to_string(&con.initial_root)),
            "line" => sel.map(|s| s.line.to_string()),
            "file" => sel.map(|s| s.path).map(|p| self.path_to_string(p)),
            "file-name" => sel
                .map(|s| s.path)
                .and_then(|path| path.file_name())
                .and_then(|oss| oss.to_str())
                .map(|s| s.to_string()),
            "file-stem" => sel
                .map(|s| s.path)
                .and_then(|path| path.file_stem())
                .and_then(|oss| oss.to_str())
                .map(|s| s.to_string()),
            "file-extension" => {
                debug!("expending file extension");
                sel.map(|s| s.path)
                    .and_then(|path| path.extension())
                    .and_then(|oss| oss.to_str())
                    .map(|s| s.to_string())
            }
            "file-dot-extension" => {
                debug!("expending file dot extension");
                sel.map(|s| s.path)
                    .and_then(|path| path.extension())
                    .and_then(|oss| oss.to_str())
                    .map(|ext| format!(".{ext}"))
                    .or_else(|| Some("".to_string()))
            }
            "directory" => sel
                .map(|s| path::closest_dir(s.path))
                .map(|p| self.path_to_string(p)),
            "parent" => sel
                .and_then(|s| s.path.parent())
                .map(|p| self.path_to_string(p)),
            "other-panel-file" => self.other_file.map(|p| self.path_to_string(p)),
            "other-panel-filename" => self
                .other_file
                .and_then(|path| path.file_name())
                .and_then(|oss| oss.to_str())
                .map(|s| s.to_string()),
            "other-panel-directory" => self
                .other_file
                .map(|p| path::closest_dir(p))
                .as_ref()
                .map(|p| self.path_to_string(p)),
            "other-panel-parent" => self
                .other_file
                .and_then(|p| p.parent())
                .map(|p| self.path_to_string(p)),
            "git-root" => {
                // path to git repo workdir
                debug!("finding git root");
                sel.and_then(|s| git2::Repository::discover(s.path).ok())
                    .and_then(|repo| repo.workdir().map(|p| self.path_to_string(p)))
            }
            "git-name" => {
                // name of the git repo workdir
                sel.and_then(|s| git2::Repository::discover(s.path).ok())
                    .and_then(|repo| {
                        repo.workdir().and_then(|path| {
                            path.file_name()
                                .and_then(|oss| oss.to_str())
                                .map(|s| s.to_string())
                        })
                    })
            }
            "file-git-relative" => {
                // file path relative to git repo workdir
                let sel = sel?;
                let path = git2::Repository::discover(self.root)
                    .ok()
                    .and_then(|repo| repo.workdir().map(|p| self.path_to_string(p)))
                    .and_then(|gitroot| sel.path.strip_prefix(gitroot).ok())
                    .filter(|p| {
                        // it's empty when the file is both the tree root and the git root
                        !p.as_os_str().is_empty()
                    })
                    .unwrap_or(sel.path);
                Some(self.path_to_string(path))
            }
            "staging" => {
                let paths: Vec<String> = self
                    .stage
                    .paths()
                    .iter()
                    .map(|p| self.path_to_string(p))
                    .collect();
                Some(paths.join(" "))
            }
            "file-root-relative" => {
                // file path relative to the tree root
                sel?.path
                    .strip_prefix(self.root)
                    .ok()
                    .map(|p| self.path_to_string(p))
            }
            "backup-name" => {
                // First-free backup destination filename for the
                // current selection, used to prefill the
                // `:backup_one` invocation. The `con.backup_suffix`
                // is the user-configurable tail (".bak" by default,
                // validated by
                // `crate::app::app_context::is_valid_backup_suffix`).
                // When `next_free_backup_name` exhausts its `.N`
                // candidates we still return the bare
                // `{name}{suffix}` composition so the input bar is
                // populated; the user can edit, and the single-file
                // receiver (`crate::app::App::run_backup_one` via
                // `plan_single_backup`) is what eventually rejects
                // an existing destination. Non-UTF-8 file_name
                // yields `None` so the caller's `unwrap_or_default`
                // produces an empty prefill — there is no honest
                // bare-composition recovery in that case.
                let path = sel.map(|s| s.path)?;
                let file_name = path.file_name()?.to_str()?;
                let suffix = con.backup_suffix.as_str();
                let chosen = crate::backup::next_free_backup_name(path, suffix);
                let name = chosen
                    .as_ref()
                    .and_then(|pb| pb.file_name())
                    .and_then(|os| os.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("{file_name}{suffix}"));
                Some(name)
            }
            #[cfg(unix)]
            "server-name" => con.server_name.clone(),
            _ => None,
        }
    }
    fn get_sel_arg_replacement(
        &self,
        arg_def: &VerbArgDef,
        sel: Option<Selection<'_>>,
        con: &AppContext,
    ) -> Option<String> {
        let name = &arg_def.name;
        self.get_sel_name_standard_replacement(name, sel, con)
            .or_else(|| {
                // it's not one of the standard group names, so we'll look
                // into the ones provided by the invocation pattern
                self.invocation_values
                    .as_ref()
                    .and_then(|map| map.get(name))
                    .and_then(|value| {
                        if arg_def.has_flag(VerbArgFlag::PathFromDirectory) {
                            sel.map(|s| path::closest_dir(s.path))
                                .map(|dir| path::path_from(dir, PathAnchor::Unspecified, value))
                                .map(|pb| self.path_to_string(pb))
                        } else if arg_def.has_flag(VerbArgFlag::PathFromParent) {
                            sel.and_then(|s| s.path.parent())
                                .map(|dir| path::path_from(dir, PathAnchor::Unspecified, value))
                                .map(|pb| self.path_to_string(pb))
                        } else {
                            Some(value.to_string())
                        }
                    })
            })
    }
    fn replace_args(
        &self,
        s: &str,
        replacer: &mut dyn FnMut(&VerbArgDef) -> Option<String>,
    ) -> String {
        ARG_DEF_GROUP
            .replace_all(s, |ec: &Captures<'_>| {
                let arg_def = VerbArgDef::from_capture(ec);
                replacer(&arg_def).unwrap_or_else(|| {
                    if self.keep_groups {
                        ec[0].to_string()
                    } else {
                        "".to_string()
                    }
                })
            })
            .to_string()
    }
    /// fills groups having a default value (after the colon)
    ///
    /// This is used to fill the input in case on non auto_exec
    /// verb triggered with a key.
    ///
    /// In invocation pattern, the part after the colon isn't handled
    /// as a 'flag' but as a default value
    pub fn invocation_with_default(
        &self,
        verb_invocation: &VerbInvocation,
        con: &AppContext,
    ) -> VerbInvocation {
        VerbInvocation {
            name: verb_invocation.name.clone(),
            args: verb_invocation.args.as_ref().map(|a| {
                ARG_DEF_GROUP
                    .replace_all(a.as_str(), |ec: &Captures<'_>| {
                        ec.get(2)
                            .map(|default_name| default_name.as_str())
                            .and_then(|default_name| {
                                self.get_sel_name_standard_replacement(
                                    default_name,
                                    self.sel_info.first_sel(),
                                    con,
                                )
                            })
                            .unwrap_or_default()
                    })
                    .to_string()
            }),
            bang: verb_invocation.bang,
        }
    }

    fn base_dir(&self) -> &Path {
        self.sel_info.one_sel().map_or(self.root, |sel| sel.path)
    }
    /// replace groups in a sequence
    ///
    /// Replacing escapes for the shell for externals, and without
    /// escaping for internals.
    ///
    /// Note that this is *before* asking the (local or remote) panel
    /// state the sequential execution of the different commands. In
    /// this secondary execution, new replacements are expected too,
    /// depending on the verbs.
    pub fn sequence(
        &mut self,
        sequence: &Sequence,
        verb_store: &VerbStore,
        con: &AppContext,
        panel_state_type: Option<PanelStateType>,
    ) -> Sequence {
        let mut inputs = Vec::new();
        for input in sequence.raw.split(&sequence.separator) {
            let raw_parts = CommandParts::from(input.to_string());
            let (_, verb_invocation) = raw_parts.split();
            let verb_is_external = verb_invocation
                .and_then(|vi| {
                    let command = Command::from_parts(vi, true);
                    if let Command::VerbInvocate(invocation) = &command {
                        let search = verb_store.search_prefix(&invocation.name, panel_state_type);
                        if let PrefixSearchResult::Match(_, verb) = search {
                            return Some(verb);
                        }
                    }
                    None
                })
                .map_or(false, |verb| verb.get_internal().is_none());
            let input = if verb_is_external {
                self.shell_exec_string(&ExecPattern::from_string(input), con)
            } else {
                self.string(input, con)
            };
            inputs.push(input);
        }
        Sequence {
            raw: inputs.join(&sequence.separator),
            separator: sequence.separator.clone(),
        }
    }

    fn string(
        &self,
        pattern: &str,
        con: &AppContext,
    ) -> String {
        self.replace_args(pattern, &mut |arg_def| {
            self.get_arg_replacement(arg_def, con)
        })
    }

    /// build a path from a pattern (eg the `working_dir` parameter of a verb definition)
    pub fn path(
        &self,
        pattern: &str,
        con: &AppContext,
    ) -> PathBuf {
        path::path_from(
            self.base_dir(),
            path::PathAnchor::Unspecified,
            &self.replace_args(pattern, &mut |arg_def| {
                self.get_arg_replacement(arg_def, con)
            }),
        )
    }

    /// build a shell compatible command, with escapings
    pub fn shell_exec_string(
        &mut self,
        exec_pattern: &ExecPattern,
        con: &AppContext,
    ) -> String {
        self.target = Target::String; // this ensures proper escaping
        let tokens = self.exec_token(exec_pattern, con);
        tokens.join(" ")
    }

    /// build a shell compatible command, with escapings, for a specific
    /// selection (this is intended for execution on all selections of a
    /// stage)
    pub fn sel_shell_exec_string(
        &mut self,
        exec_pattern: &ExecPattern,
        sel: Option<Selection<'_>>,
        con: &AppContext,
    ) -> String {
        self.target = Target::String; // this ensures proper escaping
        let tokens = self.sel_exec_token(exec_pattern, sel, con);
        tokens.join(" ")
    }

    /// build a vec of tokens which can be passed to Command to
    /// launch an executable.
    pub fn exec_token(
        &self,
        exec_pattern: &ExecPattern,
        con: &AppContext,
    ) -> Vec<String> {
        // When a token is a space-separated arg, and the selection is multiple,
        // we want to build several tokens so that it's received as several args by the
        // executed program, and not as a single arg with spaces.
        // This complex work is needed only when the selection is multiple and there's
        // a "space-separated" flag in the capture
        let mut output = Vec::new();
        for token in exec_pattern.tokens() {
            if let Some(ec) = capture_if_total(&ARG_DEF_GROUP, token) {
                let arg_def = VerbArgDef::from_capture(&ec);
                let space_separated = arg_def.has_flag(VerbArgFlag::SpaceSeparated);
                if space_separated {
                    if let SelInfo::More(stage) = &self.sel_info {
                        let sels = stage.to_selections();
                        for sel in sels {
                            if let Some(s) = self.get_sel_arg_replacement(&arg_def, Some(sel), con)
                            {
                                output.push(s);
                            }
                        }
                        continue; // we did the replacement
                    }
                }
            }
            // as we won't be able to build several tokens from this one, we do the
            // standard replacement
            let replaced =
                self.replace_args(token, &mut |arg_def| self.get_arg_replacement(arg_def, con));
            output.push(fix_token_path(replaced));
        }
        output
    }

    /// build a vec of tokens which can be passed to Command to
    /// launch an executable.
    /// This is intended for execution on all selections of a stage
    /// when the exec pattern isn't merging.
    pub fn sel_exec_token(
        &mut self,
        exec_pattern: &ExecPattern,
        sel: Option<Selection<'_>>,
        con: &AppContext,
    ) -> Vec<String> {
        exec_pattern
            .tokens()
            .iter()
            .map(|s| {
                self.replace_args(s, &mut |arg_def| {
                    self.get_sel_arg_replacement(arg_def, sel, con)
                })
            })
            .map(fix_token_path)
            .collect()
    }

    /// Convert a path (or part of a path) to a string, with escaping if needed (depending on the target)
    fn path_to_string<P: AsRef<Path>>(
        &self,
        path: P,
    ) -> String {
        let s = path.as_ref().to_string_lossy();
        if self.target == Target::Tokens {
            // when building tokens, we don't want to do any escaping,
            // even if there are special characters
            return s.to_string();
        }
        if !regex_is_match!(r#"[\s"']"#, &s) {
            // if there's no special character, we don't need to escape or wrap
            return s.to_string();
        }
        // first we replace single quotes by `'"'"'` (close the single quote, add an escaped
        // single quote, and reopen the single quote)
        let s = s.replace('\'', r#"'"'"#);
        // then we wrap the whole thing in single quotes
        let s = format!("'{}'", s);
        s
    }
}

fn capture_if_total<'h>(
    regex: &Regex,
    s: &'h str,
) -> Option<Captures<'h>> {
    let captures = regex.captures(s)?;
    let overall_match = captures.get(0)?;
    if overall_match.start() == 0 && overall_match.end() == s.len() {
        Some(captures)
    } else {
        None
    }
}

fn fix_token_path<T: Into<String> + AsRef<str>>(token: T) -> String {
    let path = Path::new(token.as_ref());
    if path.exists() {
        if let Some(path) = path.to_str() {
            return path.to_string();
        }
    } else if path::TILDE_REGEX.is_match(token.as_ref()) {
        let path = path::untilde(token.as_ref());
        if path.exists() {
            if let Some(path) = path.to_str() {
                return path.to_string();
            }
        }
    }
    token.into()
}

#[cfg(test)]
mod execution_builder_test {

    // allows writing vo!["a", "b"] to build a vec of strings
    macro_rules! vo {
        ($($item:literal),* $(,)?) => {{
            let mut vec = Vec::new();
            $(
                vec.push($item.to_owned());
            )*
            vec
        }}
    }

    use super::*;

    fn check_build_execution_from_sel(
        exec_patterns: Vec<ExecPattern>,
        path: &str,
        replacements: Vec<(&str, &str)>,
        chk_exec_token: Vec<&str>,
    ) {
        let path = PathBuf::from(path);
        let sel = Selection {
            path: &path,
            line: 0,
            stype: SelectionType::File,
            is_exe: false,
        };
        let app_state = AppState::new(PathBuf::from("/".to_owned()));
        let mut builder = ExecutionBuilder::without_invocation(SelInfo::One(sel), &app_state);
        let mut map = FxHashMap::default();
        for (k, v) in replacements {
            map.insert(k.to_owned(), v.to_owned());
        }
        builder.invocation_values = Some(map);
        let con = AppContext::default();
        for exec_pattern in exec_patterns {
            dbg!("checking pattern: {:#?}", &exec_pattern);
            let exec_token = builder.exec_token(&exec_pattern, &con);
            assert_eq!(exec_token, chk_exec_token);
        }
    }

    /// Shared `context_with_backup_suffix` lives in
    /// `crate::app::app_context::test_helpers`. The thin wrapper here
    /// adapts the `&str` parameter (this module's existing signature)
    /// to the shared helper's `Option<&str>` shape.
    fn context_with_backup_suffix(suffix: &str) -> AppContext {
        crate::app::app_context::test_helpers::context_with_backup_suffix(Some(suffix))
    }

    /// Resolve the `backup-name` placeholder for `selection_path` under
    /// `con`. The selection lives at `selection_path` regardless of
    /// whether the file actually exists on disk; the real-filesystem
    /// probe inside `next_free_backup_name` consults the parent and
    /// the candidate names, not the source itself.
    fn resolve_backup_name(selection_path: &Path, con: &AppContext) -> Option<String> {
        let sel = Selection {
            path: selection_path,
            line: 0,
            stype: SelectionType::File,
            is_exe: false,
        };
        let app_state = AppState::new(PathBuf::from("/".to_owned()));
        let builder = ExecutionBuilder::without_invocation(SelInfo::One(sel), &app_state);
        builder.get_sel_name_standard_replacement("backup-name", Some(sel), con)
    }

    /// No collision: `<name>.bak` is the first free candidate. Pins
    /// the happy path through `next_free_backup_name` to the
    /// `get_sel_name_standard_replacement` arm.
    #[test]
    fn backup_name_returns_bare_suffix_when_no_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        std::fs::write(&src, b"x").unwrap();

        let con = AppContext::default(); // default suffix is ".bak"
        let got = resolve_backup_name(&src, &con).expect("must resolve");
        assert_eq!(got, "foo.bak");
    }

    /// Pre-existing `<name>.bak` forces a `.1` bump. Pins that the
    /// returned string is the `file_name()` (not the full path) of
    /// `next_free_backup_name`'s output.
    #[test]
    fn backup_name_bumps_to_one_on_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        std::fs::write(&src, b"x").unwrap();
        std::fs::write(dir.join("foo.bak"), b"existing").unwrap();

        let con = AppContext::default();
        let got = resolve_backup_name(&src, &con).expect("must resolve");
        assert_eq!(got, "foo.bak.1");
    }

    /// Custom suffix `~` flows through `con.backup_suffix` (not
    /// hardcoded). Pins that the placeholder respects user config.
    #[test]
    fn backup_name_uses_configured_suffix() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo.txt");
        std::fs::write(&src, b"x").unwrap();

        let con = context_with_backup_suffix("~");
        let got = resolve_backup_name(&src, &con).expect("must resolve");
        assert_eq!(got, "foo.txt~");
    }

    /// Cap-exhaust: every `<name>.bak` and `<name>.bak.{1..=MAX}` exists
    /// on disk → `next_free_backup_name` returns `None`. The arm must
    /// fall back to the bare `<name>{suffix}` composition so the input
    /// bar is still populated; the user sees a colliding name and the
    /// receiver rejects it with "backup destination already exists"
    /// (see `crate::app::plan_single_backup`'s `target.exists()` guard).
    #[test]
    fn backup_name_falls_back_to_bare_composition_when_cap_exhausted() {
        use crate::backup::MAX_BACKUP_BUMP;
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo");
        std::fs::write(&src, b"x").unwrap();
        std::fs::write(dir.join("foo.bak"), b"").unwrap();
        for n in 1..=MAX_BACKUP_BUMP {
            std::fs::write(dir.join(format!("foo.bak.{n}")), b"").unwrap();
        }

        let con = AppContext::default();
        let got = resolve_backup_name(&src, &con).expect("fallback must yield Some");
        assert_eq!(
            got, "foo.bak",
            "cap-exhaust falls back to bare composition so the prefill is non-empty",
        );
    }

    /// `SelInfo::None`: the `backup-name` arm's first line is
    /// `let path = sel.map(|s| s.path)?;` — with no selection, the
    /// helper returns `None` and the input bar prefill stays empty.
    /// Pins the no-selection contract so a future refactor that
    /// accidentally falls back to a default path (e.g. `root`) gets
    /// caught.
    #[test]
    fn backup_name_returns_none_when_no_selection() {
        let app_state = AppState::new(PathBuf::from("/".to_owned()));
        let builder = ExecutionBuilder::without_invocation(SelInfo::None, &app_state);
        let con = AppContext::default();
        // Bypass the `resolve_backup_name` test-helper because it
        // constructs a `SelInfo::One` — we want to exercise the
        // `None` arm explicitly.
        let got = builder.get_sel_name_standard_replacement("backup-name", None, &con);
        assert!(
            got.is_none(),
            "SelInfo::None must yield None for backup-name, got {got:?}",
        );
    }

    #[test]
    fn test_build_execution() {
        check_build_execution_from_sel(
            vec![ExecPattern::from_string("vi {file}")],
            "/home/dys/dev",
            vec![],
            vec!["vi", "/home/dys/dev"],
        );
        check_build_execution_from_sel(
            vec![
                ExecPattern::from_string("/bin/e.exe -a {arg} -e {file}"),
                ExecPattern::from_tokens(vo!["/bin/e.exe", "-a", "{arg}", "-e", "{file}"]),
            ],
            "expérimental & 试验性",
            vec![("arg", "deux mots")],
            vec![
                "/bin/e.exe",
                "-a",
                "deux mots",
                "-e",
                "expérimental & 试验性",
            ],
        );
        check_build_execution_from_sel(
            vec![
                ExecPattern::from_string("xterm -e \"kak {file}\""),
                ExecPattern::from_tokens(vo!["xterm", "-e", "kak {file}"]),
            ],
            "/path/to/file",
            vec![],
            vec!["xterm", "-e", "kak /path/to/file"],
        );
    }
}
