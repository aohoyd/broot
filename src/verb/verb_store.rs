use {
    super::{
        Internal,
        Verb,
        VerbId,
    },
    crate::{
        app::*,
        command::Sequence,
        conf::{
            Conf,
            VerbConf,
        },
        errors::ConfError,
        keys::{
            self,
            KEY_FORMAT,
        },
        verb::*,
    },
    crokey::*,
};

/// Provide access to the verbs:
/// - the built-in ones
/// - the user defined ones
///
/// A user defined verb can replace a built-in.
///
/// When the user types some keys, we select a verb
/// - if the input exactly matches a shortcut or the name
/// - if only one verb name starts with the input
pub struct VerbStore {
    verbs: Vec<Verb>,
    unbound_keys: Vec<KeyCombination>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PrefixSearchResult<'v, T> {
    NoMatch,
    Match(&'v str, T),
    Matches(Vec<&'v str>),
}

impl VerbStore {
    pub fn new(conf: &mut Conf) -> Result<Self, ConfError> {
        let mut store = Self {
            verbs: Vec::new(),
            unbound_keys: Vec::new(),
        };
        for vc in &conf.verbs {
            if let Err(e) = store.add_from_conf(vc) {
                eprintln!("Invalid verb configuration: {}", e);
                warn!("Faulty parsed configuration: {:#?}", vc);
                if let Ok(toml) = toml::to_string(&vc) {
                    eprintln!("Faulty configuration:\n{}", toml);
                }
                eprintln!("Configuration files:");
                for path in &conf.files {
                    eprintln!("  - {}", path.display());
                }
            }
        }
        store.add_builtin_verbs()?; // at the end so that we can override them
        for key in store.unbound_keys.clone() {
            store.unbind_key(key)?;
        }
        Ok(store)
    }

    fn add_builtin_verbs(&mut self) -> Result<(), ConfError> {
        use super::{
            ExternalExecutionMode::*,
            Internal::*,
        };
        self.add_internal(escape).with_key(key!(esc));

        // input actions, not visible in doc, but available for
        // example in remote control
        self.add_internal(input_clear).no_doc();
        self.add_internal(input_del_char_left).no_doc();
        self.add_internal(input_del_char_below).no_doc();
        self.add_internal(input_del_word_left).no_doc();
        self.add_internal(input_del_word_right).no_doc();
        self.add_internal(input_go_to_end)
            .with_key(key!(end))
            .no_doc();
        self.add_internal(input_go_left).no_doc();
        self.add_internal(input_go_right).no_doc();
        self.add_internal(input_go_to_start)
            .with_key(key!(home))
            .no_doc();
        self.add_internal(input_go_word_left).no_doc();
        self.add_internal(input_go_word_right).no_doc();

        // arrow keys bindings
        self.add_internal(back);
        self.add_internal(open_stay);
        self.add_internal(line_down)
            .with_key(key!(down))
            .with_key(key!('j'));
        self.add_internal(line_up)
            .with_key(key!(up))
            .with_key(key!('k'));

        // changing display
        self.add_internal(set_syntax_theme);
        self.add_internal(apply_flags).with_name("apply_flags")?;
        self.add_internal(set_panel_width);
        self.add_internal(default_layout);

        // those two operations are mapped on ALT-ENTER, one
        // for directories and the other one for the other files
        self.add_internal(open_leave) // calls the system open
            .with_condition(FileTypeCondition::File)
            .with_key(key!(alt - enter))
            .with_shortcut("ol");
        self.add_external("cd", "cd {directory}", FromParentShell)
            .with_condition(FileTypeCondition::Directory)
            .with_key(key!(alt - enter))
            .with_shortcut("ol")
            .with_description("change directory and quit");

        #[cfg(unix)]
        self.add_external("chmod {args}", "chmod {args} {file}", StayInBroot)
            .with_condition(FileTypeCondition::File);
        #[cfg(unix)]
        self.add_external("chmod {args}", "chmod -R {args} {file}", StayInBroot)
            .with_condition(FileTypeCondition::Directory);
        self.add_internal(open_preview);
        self.add_internal(close_preview);
        self.add_internal(toggle_preview).with_key(key!(alt - p));
        self.add_internal(preview_image).with_shortcut("img");
        self.add_internal(preview_text).with_shortcut("txt");
        self.add_internal(preview_binary).with_shortcut("hex");
        self.add_internal(preview_tty).with_shortcut("tty");
        self.add_internal(close_panel_ok);
        self.add_internal(close_panel_cancel)
            .with_key(key!(ctrl - w));
        self.add_internal(close_panel_ok_if_not_last);
        self.add_internal(close_panel_cancel_if_not_last);
        #[cfg(unix)]
        self.add_external(
            "copy {newpath}",
            "cp -r {file} {newpath:path-from-parent}",
            StayInBroot,
        )
        .with_shortcut("cp");
        #[cfg(windows)]
        self.add_external(
            "copy {newpath}",
            "xcopy /Q /H /Y /I {file} {newpath:path-from-parent}",
            StayInBroot,
        )
        .with_shortcut("cp");
        #[cfg(feature = "clipboard")]
        self.add_internal(copy_file_content).with_key(key!(shift - y));
        #[cfg(feature = "clipboard")]
        self.add_internal(copy_line).with_key(key!(alt - c));
        #[cfg(feature = "clipboard")]
        self.add_internal(copy_name).with_key(key!('c'));
        #[cfg(feature = "clipboard")]
        self.add_internal(copy_path).with_key(key!(shift - c));
        #[cfg(unix)]
        self.add_external(
            "copy_to_panel",
            "cp -r {file} {other-panel-directory}",
            StayInBroot,
        )
        .with_shortcut("cpp");
        #[cfg(windows)]
        self.add_external(
            "copy_to_panel",
            "xcopy /Q /H /Y /I {file} {other-panel-directory}",
            StayInBroot,
        )
        .with_shortcut("cpp");
        self.add_internal(trash).with_key(key!('d'));
        #[cfg(any(
            target_os = "windows",
            all(unix, not(any(target_os = "ios", target_os = "android")))
        ))]
        {
            self.add_internal(open_trash).with_shortcut("ot");
            self.add_internal(restore_trashed_file).with_shortcut("rt");
            self.add_internal(delete_trashed_file).with_shortcut("dt");
            self.add_internal(purge_trash).with_shortcut("et");
        }
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        self.add_internal(filesystems).with_shortcut("fs");
        self.add_internal(focus_staging_area_no_open);
        // :focus is also hardcoded on Enter on directories
        // but ctrl-f is useful for focusing on a file's parent
        // (and keep the filter)
        self.add_internal(focus)
            .with_key(key!(L)) // hum... why this one ?
            .with_key(key!(ctrl - f));
        self.add_internal(help)
            .with_key(key!(F1))
            .with_shortcut("?");
        #[cfg(feature = "clipboard")]
        self.add_internal(input_paste).with_key(key!(ctrl - v));
        #[cfg(unix)]
        self.add_external(
            "mkdir {subpath}",
            "mkdir -p {subpath:path-from-directory}",
            StayInBroot,
        )
        .with_shortcut("md");
        #[cfg(windows)]
        self.add_external(
            "mkdir {subpath}",
            "cmd /c mkdir {subpath:path-from-directory}",
            StayInBroot,
        )
        .with_shortcut("md");
        #[cfg(unix)]
        self.add_external(
            "move {newpath}",
            "mv {file} {newpath:path-from-parent}",
            StayInBroot,
        )
        .with_shortcut("mv");
        #[cfg(windows)]
        self.add_external(
            "move {newpath}",
            "cmd /c move /Y {file} {newpath:path-from-parent}",
            StayInBroot,
        )
        .with_shortcut("mv");
        #[cfg(unix)]
        self.add_external(
            "move_to_panel",
            "mv {file} {other-panel-directory}",
            StayInBroot,
        )
        .with_shortcut("mvp");
        #[cfg(windows)]
        self.add_external(
            "move_to_panel",
            "cmd /c move /Y {file} {other-panel-directory}",
            StayInBroot,
        )
        .with_shortcut("mvp");
        // `bulk_rename` is registered BEFORE the external `rename` verb
        // (which also binds F2) so that `find_key_verb` — which scans
        // verbs in registration order and returns the first match —
        // resolves F2 to the internal first. The internal always
        // collects paths via `collect_bulk_paths`
        // (`stage || [selection]`) and routes through the
        // `$EDITOR`-backed bulk flow. N=1 is just a one-row bulk run —
        // there is no separate fast path. The continuation
        // `bulk_rename_apply` is NOT bound to any key — it is only ever
        // reached from the confirm overlay's `CloseAndRun` path.
        self.add_internal(bulk_rename)
            .with_key(key!(f2))
            .with_key(key!('r'));
        self.add_internal(bulk_rename_apply).no_doc();
        #[cfg(unix)]
        self.add_external(
            "rename {new_filename:file-name}",
            "mv {file} {parent}/{new_filename}",
            StayInBroot,
        )
        .with_auto_exec(false)
        .with_key(key!(f2));
        #[cfg(windows)]
        self.add_external(
            "rename {new_filename:file-name}",
            "cmd /c move /Y {file} {parent}/{new_filename}",
            StayInBroot,
        )
        .with_auto_exec(false)
        .with_key(key!(f2));
        // `backup` mirrors `bulk_rename`'s two-internal split:
        //   - `Internal::backup` is the keyed trigger; the App-level
        //     intercept always plans a bulk run (N=1 when the stage is
        //     empty and there's a single selection, otherwise N=stage)
        //     and opens a confirm overlay.
        //   - `Internal::backup_apply` is the bulk receiver, consuming
        //     `App::pending_backup` after the confirm overlay accepts.
        //     It has no key and is hidden from docs — only reachable
        //     via the confirm overlay's `CloseAndRun` re-dispatch.
        self.add_internal(backup)
            .with_key(key!(alt - shift - b));
        self.add_internal(backup_apply).no_doc();
        self.add_internal_bang(start_end_panel)
            .with_key(key!(ctrl - p));
        // the char keys for mode_input are handled differently as they're not
        // consumed by the command
        self.add_internal(mode_input)
            .with_key(key!(' '))
            .with_key(key!(':'))
            .with_key(key!('/'));
        self.add_internal(previous_match)
            .with_key(key!(shift - backtab))
            .with_key(key!(backtab))
            .with_key(key!(shift - n));
        self.add_internal(next_match)
            .with_key(key!(tab))
            .with_key(key!('n'));
        self.add_internal(no_sort).with_shortcut("ns");
        self.add_internal(open_stay)
            .with_key(key!(enter))
            .with_shortcut("os");
        self.add_internal(open_stay_filter).with_shortcut("osf");
        self.add_internal(parent)
            .with_key(key!(h))
            .with_shortcut("p");
        self.add_internal(bookmarks)
            .with_key(key!(alt - b))
            .with_key(key!('b'));
        self.add_internal(add).with_key(key!(alt - n));
        self.add_internal(open_sort_overlay).with_key(key!('o'));
        self.add_internal(page_down)
            .with_key(key!(ctrl - d))
            .with_key(key!(pagedown));
        self.add_internal(page_up)
            .with_key(key!(ctrl - u))
            .with_key(key!(pageup));
        self.add_internal(focus_panel_left);
        self.add_internal(focus_panel_right);
        self.add_internal(panel_left_no_open)
            .with_key(key!(shift - left));
        self.add_internal(panel_right).with_key(key!(shift - right));
        self.add_internal(print_path).with_shortcut("pp");
        self.add_internal(print_relative_path).with_shortcut("prp");
        self.add_internal(print_tree).with_shortcut("pt");
        self.add_internal(quit)
            .with_key(key!(ctrl - c))
            .with_key(key!(ctrl - q))
            .with_key(key!('q'))
            .with_shortcut("q");
        self.add_internal(refresh)
            .with_key(key!(f5))
            .with_key(key!(shift - r));
        self.add_internal(root_up).with_key(key!(ctrl - up));
        self.add_internal(root_down).with_key(key!(ctrl - down));
        self.add_internal(select_first).with_key(key!('g'));
        self.add_internal(select_last).with_key(key!(shift - g));
        self.add_internal(select);
        self.add_internal(show);
        self.add_internal(clear_stage).with_shortcut("cls");
        self.add_internal(copy_from_staging)
            .with_key(key!(shift - p))
            .with_shortcut("cfs");
        self.add_internal(move_from_staging)
            .with_key(key!(shift - x))
            .with_shortcut("mfs");
        // `+`, `=` and `ctrl-g` all bind `stage` (add-only + advance).
        // `toggle_stage` stays registered (callable as `:toggle_stage` from
        // user conf) but has no default key binding — see the BrowserState
        // arms in `src/browser/browser_state.rs` for the unified semantics.
        self.add_internal(stage)
            .with_key(key!('+'))
            .with_key(key!('='))
            .with_key(key!(ctrl - g));
        self.add_internal(unstage).with_key(key!('-'));
        self.add_internal(stage_all_directories);
        self.add_internal(stage_all_files).with_key(key!(ctrl - a));
        self.add_internal(toggle_stage);
        self.add_internal(open_staging_area).with_shortcut("osa");
        self.add_internal(close_staging_area).with_shortcut("csa");
        self.add_internal(toggle_staging_area)
            .with_key(key!(alt - s))
            .with_shortcut("tsa");
        self.add_internal(toggle_tree)
            .with_key(key!(alt - t))
            .with_shortcut("tree");
        self.add_internal(toggle_watch)
            .with_shortcut("watch")
            .with_key(key!(alt - w));
        self.add_internal(sort_by_count).with_shortcut("sc");
        self.add_internal(sort_by_date).with_shortcut("sd");
        self.add_internal(sort_by_size).with_shortcut("ss");
        self.add_internal(sort_by_type).with_shortcut("st");
        self.add_internal(sort_by_type_dirs_first).with_shortcut("sdf");
        self.add_internal(sort_by_type_dirs_last).with_shortcut("sdl");
        #[cfg(unix)]
        self.add_external("rm", "rm -rf {file}", StayInBroot)
            .with_confirm(true)
            .with_key(key!(shift - d));
        #[cfg(windows)]
        self.add_external("rm", "cmd /c rmdir /Q /S {file}", StayInBroot)
            .with_condition(FileTypeCondition::Directory)
            .with_confirm(true)
            .with_key(key!(shift - d));
        #[cfg(windows)]
        self.add_external("rm", "cmd /c del /Q {file}", StayInBroot)
            .with_condition(FileTypeCondition::File)
            .with_confirm(true)
            .with_key(key!(shift - d));
        self.add_internal(toggle_counts).with_shortcut("counts");
        self.add_internal(toggle_dates)
            .with_key(key!(alt - d))
            .with_shortcut("dates");
        self.add_internal(toggle_device_id).with_shortcut("dev");
        self.add_internal(toggle_files).with_shortcut("files");
        self.add_internal(toggle_ignore)
            .with_key(key!(alt - i))
            .with_shortcut("gi");
        self.add_internal(toggle_git_file_info)
            .with_key(key!(alt - g))
            .with_shortcut("gf");
        self.add_internal(toggle_git_status)
            .with_key(key!(alt - shift - g))
            .with_shortcut("gs");
        self.add_internal(toggle_root_fs).with_shortcut("rfs");
        self.add_internal(toggle_whale_spotting)
            .with_key(key!(alt - shift - w))
            .with_shortcut("ws");
        self.add_internal(next_same_depth).with_key(key!(alt - down));
        self.add_internal(previous_same_depth).with_key(key!(alt - up));
        self.add_internal(next_dir).with_key(key!(shift - down));
        self.add_internal(previous_dir).with_key(key!(shift - up));
        self.add_internal(set_max_depth);
        self.add_internal(unset_max_depth);
        self.add_internal(toggle_hidden)
            .with_key(key!(alt - '.'))
            .with_shortcut("h");
        #[cfg(unix)]
        self.add_internal(toggle_perm).with_shortcut("perm");
        self.add_internal(toggle_sizes).with_shortcut("sizes");
        self.add_internal(toggle_trim_root);
        self.add_internal(total_search);
        self.add_internal(search_again).with_key(key!(ctrl - s));
        self.add_internal(up_tree).with_shortcut("up");

        self.add_internal_with_args(move_panel_divider, "0 1")
            .with_key(key!(alt - '>'));
        self.add_internal_with_args(move_panel_divider, "0 -1")
            .with_key(key!(alt - '<'));

        self.add_internal(clear_output);
        self.add_internal(write_output);
        Ok(())
    }

    fn build_add_internal(
        &mut self,
        internal: Internal,
        bang: bool,
    ) -> &mut Verb {
        let invocation = internal.invocation_pattern();
        let execution =
            VerbExecution::Internal(InternalExecution::from_internal_bang(internal, bang));
        let description = VerbDescription::from_text(internal.description().to_string());
        self.add_verb(Some(invocation), execution, description)
            .unwrap()
    }

    fn add_internal(
        &mut self,
        internal: Internal,
    ) -> &mut Verb {
        self.build_add_internal(internal, false)
    }

    fn add_internal_with_args(
        &mut self,
        internal: Internal,
        args: &str,
    ) -> &mut Verb {
        let command = format!("{} {}", internal.name(), args);
        let execution = VerbExecution::Internal(InternalExecution {
            internal,
            bang: false,
            arg: Some(args.to_string()),
        });
        let description = VerbDescription::from_text(command.clone());
        self.add_verb(Some(&command), execution, description)
            .unwrap()
    }

    fn add_internal_bang(
        &mut self,
        internal: Internal,
    ) -> &mut Verb {
        self.build_add_internal(internal, true)
    }

    fn add_external(
        &mut self,
        invocation_str: &str,
        execution_str: &str,
        exec_mode: ExternalExecutionMode,
    ) -> &mut Verb {
        let execution = VerbExecution::External(ExternalExecution::new(
            ExecPattern::from_string(execution_str),
            exec_mode,
        ));
        self.add_verb(
            Some(invocation_str),
            execution,
            VerbDescription::from_code(execution_str.to_string()),
        )
        .unwrap()
    }

    pub fn add_verb(
        &mut self,
        invocation_str: Option<&str>,
        execution: VerbExecution,
        description: VerbDescription,
    ) -> Result<&mut Verb, ConfError> {
        let id = self.verbs.len();
        self.verbs
            .push(Verb::new(id, invocation_str, execution, description)?);
        Ok(&mut self.verbs[id])
    }

    /// Create a verb from its configuration, adding it to its store
    pub fn add_from_conf(
        &mut self,
        vc: &VerbConf,
    ) -> Result<(), ConfError> {
        if vc.leave_broot == Some(false) && vc.from_shell == Some(true) {
            return Err(ConfError::InvalidVerbConf {
                details: "You can't simultaneously have leave_broot=false and from_shell=true"
                    .to_string(),
            });
        }

        // we accept both key and keys. We merge both here
        let mut unchecked_keys = vc.keys.clone();
        if let Some(key) = &vc.key {
            unchecked_keys.push(key.clone());
        }
        let mut checked_keys = Vec::new();
        for key in &unchecked_keys {
            let key = crokey::parse(key)?;
            if keys::is_reserved(key) {
                return Err(ConfError::ReservedKey {
                    key: keys::KEY_FORMAT.to_string(key),
                });
            }
            checked_keys.push(key);
        }

        let invocation = vc.invocation.clone().filter(|i| !i.is_empty());
        let internal = vc.internal.as_ref().filter(|i| !i.is_empty());
        let external = vc.external.as_ref().filter(|i| !i.is_empty());
        let cmd = vc.cmd.as_ref().filter(|i| !i.is_empty());
        let cmd_separator = vc.cmd_separator.as_ref().filter(|i| !i.is_empty());
        let execution = vc.execution.as_ref().filter(|i| !i.is_empty());
        let make_external_execution = |s| {
            let working_dir = match (vc.set_working_dir, &vc.working_dir) {
                (Some(false), _) => None,
                (_, Some(s)) => Some(s.clone()),
                (Some(true), None) => Some("{directory}".to_owned()),
                (None, None) => None,
            };
            let mut external_execution = ExternalExecution::new(
                s,
                ExternalExecutionMode::from_conf(vc.from_shell, vc.leave_broot),
            )
            .with_working_dir(working_dir);
            if let Some(b) = vc.switch_terminal {
                external_execution.switch_terminal = b;
            }
            external_execution
        };
        let mut execution = match (execution, internal, external, cmd) {
            // old definition with "execution": we guess whether it's an internal or
            // an external
            (Some(ep), None, None, None) => {
                if let Some(internal_pattern) = ep.to_internal_pattern() {
                    if let Some(previous_verb) =
                        self.verbs.iter().find(|&v| v.has_name(&internal_pattern))
                    {
                        previous_verb.execution.clone()
                    } else {
                        VerbExecution::Internal(InternalExecution::try_from(&internal_pattern)?)
                    }
                } else {
                    VerbExecution::External(make_external_execution(ep.clone()))
                }
            }
            // "internal": the leading `:` or ` ` is optional
            (None, Some(s), None, None) => {
                VerbExecution::Internal(if s.starts_with(':') || s.starts_with(' ') {
                    InternalExecution::try_from(&s[1..])?
                } else {
                    InternalExecution::try_from(s)?
                })
            }
            // "external": it can be about any form
            (None, None, Some(ep), None) => {
                VerbExecution::External(make_external_execution(ep.clone()))
            }
            // "cmd": it's a sequence
            (None, None, None, Some(s)) => VerbExecution::Sequence(SequenceExecution {
                sequence: Sequence::new(s, cmd_separator),
            }),
            _ => {
                // there's no execution, this 'verbconf' is supposed to be dedicated to
                // unbind keys
                for key in checked_keys {
                    self.unbound_keys.push(key);
                }
                return Ok(());
            }
        };
        if let Some(refresh_after) = vc.refresh_after {
            if let VerbExecution::External(external_execution) = &mut execution {
                external_execution.refresh_after = refresh_after;
            } else {
                warn!("refresh_after is only relevant for external commands");
            }
        }
        let description = vc
            .description
            .clone()
            .map(VerbDescription::from_text)
            .unwrap_or_else(|| VerbDescription::from_code(execution.to_string()));
        let verb = self.add_verb(invocation.as_deref(), execution, description)?;
        for extension in &vc.extensions {
            verb.file_extensions.push(extension.clone());
        }
        if !checked_keys.is_empty() {
            verb.add_keys(checked_keys);
        }
        if let Some(shortcut) = &vc.shortcut {
            verb.names.push(shortcut.clone());
        }
        if vc.auto_exec == Some(false) {
            verb.auto_exec = false;
        }
        if let Some(confirm) = vc.confirm {
            // user override of the verb's default confirmation behaviour:
            // - `confirm: true` opts an external verb in
            // - `confirm: false` opts a built-in destructive verb out
            verb.requires_confirm = confirm;
        }
        if !vc.panels.is_empty() {
            verb.panels.clone_from(&vc.panels);
        }
        verb.impacted_panel = vc.impacted_panel;
        verb.selection_condition = vc.apply_to;
        Ok(())
    }

    pub fn unbind_key(
        &mut self,
        key: KeyCombination,
    ) -> Result<(), ConfError> {
        debug!("unbinding key {:?}", key);
        for verb in &mut self.verbs {
            verb.keys.retain(|&k| k != key);
        }
        Ok(())
    }
    pub fn unbind_name(
        &mut self,
        name: &str,
    ) -> Result<(), ConfError> {
        for verb in &mut self.verbs {
            verb.names.retain(|n| n != name);
        }
        Ok(())
    }

    pub fn search_sel_info<'v>(
        &'v self,
        prefix: &str,
        sel_info: SelInfo<'_>,
        panel_state_type: Option<PanelStateType>,
        stage_is_empty: bool,
    ) -> PrefixSearchResult<'v, &'v Verb> {
        self.search(
            prefix,
            Some(sel_info),
            true,
            panel_state_type,
            Some(stage_is_empty),
        )
    }

    pub fn search_prefix<'v>(
        &'v self,
        prefix: &str,
        panel_state_type: Option<PanelStateType>,
    ) -> PrefixSearchResult<'v, &'v Verb> {
        self.search(prefix, None, true, panel_state_type, None)
    }

    /// Return either the only match, or None if there's not
    /// exactly one match
    pub fn search_sel_info_unique<'v>(
        &'v self,
        prefix: &str,
        sel_info: SelInfo<'_>,
        panel_state_type: Option<PanelStateType>,
        stage_is_empty: bool,
    ) -> Option<&'v Verb> {
        match self.search_sel_info(prefix, sel_info, panel_state_type, stage_is_empty) {
            PrefixSearchResult::Match(_, verb) => Some(verb),
            _ => None,
        }
    }

    pub fn search<'v>(
        &'v self,
        prefix: &str,
        sel_info: Option<SelInfo>,
        short_circuit: bool,
        panel_state_type: Option<PanelStateType>,
        stage_is_empty: Option<bool>,
    ) -> PrefixSearchResult<'v, &'v Verb> {
        let mut found_index = 0;
        let mut nb_found = 0;
        let mut completions: Vec<&str> = Vec::new();
        let extension = sel_info.as_ref().and_then(|si| si.extension());
        let sel_count = sel_info.map(|si| si.count_paths());
        for (index, verb) in self.verbs.iter().enumerate() {
            if let Some(sel_info) = sel_info {
                if !sel_info.is_accepted_by(verb.selection_condition) {
                    continue;
                }
            }
            if let Some(panel_state_type) = panel_state_type {
                if !verb.can_be_called_in_panel(panel_state_type) {
                    continue;
                }
            }
            if let Some(count) = sel_count {
                if count > 1 && verb.is_sequence() {
                    continue;
                }
                if count == 0 && verb.needs_selection {
                    continue;
                }
            }
            if !verb.accepts_extension(extension) {
                continue;
            }
            if let Some(empty) = stage_is_empty {
                if empty && verb.needs_staging {
                    continue;
                }
            }
            for name in &verb.names {
                if name.starts_with(prefix) {
                    if short_circuit && name == prefix {
                        return PrefixSearchResult::Match(name, verb);
                    }
                    found_index = index;
                    nb_found += 1;
                    completions.push(name);
                }
            }
        }
        match nb_found {
            0 => PrefixSearchResult::NoMatch,
            1 => PrefixSearchResult::Match(completions[0], &self.verbs[found_index]),
            _ => PrefixSearchResult::Matches(completions),
        }
    }

    pub fn key_desc_of_internal_stype(
        &self,
        internal: Internal,
        stype: SelectionType,
    ) -> Option<String> {
        for verb in &self.verbs {
            if verb.get_internal() == Some(internal)
                && verb.selection_condition.accepts_selection_type(stype)
            {
                return verb.keys.first().map(|&k| KEY_FORMAT.to_string(k));
            }
        }
        None
    }

    pub fn key_desc_of_internal(
        &self,
        internal: Internal,
    ) -> Option<String> {
        for verb in &self.verbs {
            if verb.get_internal() == Some(internal) {
                return verb.keys.first().map(|&k| KEY_FORMAT.to_string(k));
            }
        }
        None
    }

    pub fn verbs(&self) -> &[Verb] {
        &self.verbs
    }

    pub fn verb(
        &self,
        id: VerbId,
    ) -> &Verb {
        &self.verbs[id]
    }
}

#[test]
fn check_builtin_verbs() {
    let mut conf = Conf::default();
    let _store = VerbStore::new(&mut conf).unwrap();
}

#[cfg(test)]
mod confirm_tests {
    use super::*;

    /// The built-in `rm` external verb is registered with
    /// `with_confirm(true)` and so is destructive by default.
    #[test]
    fn builtin_rm_requires_confirm() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let rm = store
            .verbs()
            .iter()
            .find(|v| v.has_name("rm"))
            .expect("rm verb must be registered");
        assert!(
            rm.requires_confirm,
            "built-in rm should have requires_confirm=true"
        );
    }

    /// A user `VerbConf` with `confirm: false` overrides the built-in
    /// `requires_confirm: true` on `rm` (or any other destructive verb).
    /// We exercise this by registering a fresh external verb whose
    /// `VerbConf` opts out of confirmation, and asserting the verb
    /// stored has `requires_confirm == false`.
    #[test]
    fn verb_conf_confirm_false_overrides_default() {
        let mut conf = Conf::default();
        let mut store = VerbStore::new(&mut conf).unwrap();
        // Build a VerbConf for a custom destructive shell command and
        // explicitly opt OUT of the confirmation.
        let vc = VerbConf {
            invocation: Some("zap".to_string()),
            external: Some(ExecPattern::from_string("rm -rf {file}")),
            confirm: Some(false),
            ..Default::default()
        };
        store.add_from_conf(&vc).unwrap();
        let zap = store
            .verbs()
            .iter()
            .find(|v| v.has_name("zap"))
            .expect("zap verb must be registered");
        // External verbs default to requires_confirm=false, so this
        // primarily checks that the override path doesn't crash.
        assert!(!zap.requires_confirm);
    }

    /// A user `VerbConf` with `confirm: true` opts a non-destructive
    /// external verb into confirmation.
    #[test]
    fn verb_conf_confirm_true_opts_in() {
        let mut conf = Conf::default();
        let mut store = VerbStore::new(&mut conf).unwrap();
        let vc = VerbConf {
            invocation: Some("careful".to_string()),
            external: Some(ExecPattern::from_string("touch {file}")),
            confirm: Some(true),
            ..Default::default()
        };
        store.add_from_conf(&vc).unwrap();
        let careful = store
            .verbs()
            .iter()
            .find(|v| v.has_name("careful"))
            .expect("careful verb must be registered");
        assert!(
            careful.requires_confirm,
            "confirm: true should opt the verb in"
        );
    }
}

#[cfg(test)]
mod bulk_rename_routing_tests {
    use {
        super::*,
        crokey::key,
    };

    /// `find_key_verb` returns verbs in registration order, but with
    /// additional per-verb filters: `can_be_called_in_panel`,
    /// `selection_condition`, and `file_extensions`. This test pins
    /// BOTH the registration order AND the filter shape so a future
    /// change to either path keeps F2 (and `r`) routed to
    /// `Internal::bulk_rename`.
    ///
    /// Specifically, we assert that the first F2-bound verb has:
    ///   - registration order before the external `rename`
    ///   - no `file_extensions` restriction (would skip dot-less files)
    ///   - `selection_condition` of `Any` (would otherwise skip the
    ///     no-selection case where the user opens F2 with the stage
    ///     populated but no tree selection)
    ///
    /// `r` is the Command-mode bare-letter alias for F2 (registered
    /// alongside it on `Internal::bulk_rename`); since the external
    /// `rename` verb does NOT bind `r`, the assertion that the first
    /// `r`-bound verb is the internal also pins that no future
    /// refactor accidentally moves `r` onto the external.
    #[test]
    fn f2_resolves_to_internal_bulk_rename_before_external_rename() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let f2 = key!(f2);
        let first_f2_verb = store
            .verbs()
            .iter()
            .find(|v| v.keys.contains(&f2))
            .expect("at least one verb must bind F2");
        assert_eq!(
            first_f2_verb.get_internal(),
            Some(Internal::bulk_rename),
            "F2 must resolve to Internal::bulk_rename first; \
             check the registration order in add_builtin_verbs",
        );
        assert!(
            first_f2_verb.file_extensions.is_empty(),
            "Internal::bulk_rename must not restrict by file extension; \
             a filter would let find_key_verb skip past it to the external rename",
        );
        assert!(
            matches!(first_f2_verb.selection_condition, FileTypeCondition::Any),
            "Internal::bulk_rename must accept any selection; \
             a stricter condition would let find_key_verb skip past it",
        );
        assert!(
            first_f2_verb.panels.is_empty(),
            "Internal::bulk_rename must not restrict to specific panels; \
             a panel filter would let find_key_verb skip past it (the \
             three filters in find_key_verb are selection_condition, \
             file_extensions, and panels — all three must be permissive)",
        );

        // `r` (the Command-mode bare-letter alias) must also resolve
        // to `Internal::bulk_rename`. Use the same first-in-registration-
        // order resolution as `find_key_verb`.
        let first_r_verb = store
            .verbs()
            .iter()
            .find(|v| v.keys.contains(&key!('r')))
            .expect("at least one verb must bind `r`");
        assert_eq!(
            first_r_verb.get_internal(),
            Some(Internal::bulk_rename),
            "`r` must resolve to Internal::bulk_rename first; \
             check the registration order in add_builtin_verbs",
        );
    }

    /// The continuation `bulk_rename_apply` is intentionally unbound:
    /// it should only be reachable via the confirm overlay's
    /// `CloseAndRun` re-dispatch. If anyone binds it to a key by
    /// accident, the user could end up calling it with no
    /// `pending_bulk_rename` set, which would surface as a status-row
    /// error rather than silent breakage — but we'd rather catch the
    /// mistake at registration time.
    #[test]
    fn bulk_rename_apply_has_no_key_binding() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let apply = store
            .verbs()
            .iter()
            .find(|v| v.is_internal(Internal::bulk_rename_apply))
            .expect("bulk_rename_apply must be registered");
        assert!(
            apply.keys.is_empty(),
            "bulk_rename_apply must not bind any key",
        );
    }
}

#[cfg(test)]
mod vim_bindings_tests {
    //! Pin tests for the bare-letter Command-mode bindings introduced
    //! for the vim-style keymap. These verify that each binding is
    //! attached to the right verb (internal name or external invocation),
    //! so a future verb-store refactor can't silently drop a binding.
    //!
    //! Bare-letter keys fire only in Command mode by virtue of
    //! `is_key_allowed_for_verb` (`src/command/panel_input.rs:414-427`),
    //! which blocks bare-letter `KeyCombination`s while the input field
    //! is active. No test in this module exercises that gate — it lives
    //! in `panel_input.rs` and is covered by its own pin test.
    use super::*;

    /// Helper: find the first verb in registration order whose `keys`
    /// list contains the given `KeyCombination`. Mirrors the
    /// registration-order semantics that `find_key_verb` relies on,
    /// minus the per-verb filters (which are exercised by the
    /// `bulk_rename_routing_tests` module).
    fn first_verb_for_key(
        store: &VerbStore,
        key: KeyCombination,
    ) -> Option<&Verb> {
        store.verbs().iter().find(|v| v.keys.contains(&key))
    }

    /// Table-driven check for the 18 always-on Command-mode bindings
    /// (17 internals + 1 external `:rm`). The set includes bare letters,
    /// shifted letters, and `=`. Each row binds a `KeyCombination` to
    /// either an `Internal` (for internal verbs) or to a verb name (for
    /// externals like `:rm`).
    #[test]
    fn vim_bindings_resolve() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();

        // Internals — assert the first verb bound to each key is the
        // expected internal. Note: crokey's `key!` macro lower-cases
        // bare char literals (`key!('G')` is parsed as `Char('g')`
        // with no shift modifier — see crokey-proc_macros 1.3.0).
        // For uppercase letters we must use `key!(shift - g)` so the
        // produced `KeyCombination` matches the SHIFT-normalized form
        // that crossterm emits for `Shift+G`.
        let internal_bindings: &[(KeyCombination, Internal)] = &[
            (key!('r'), Internal::bulk_rename),
            // Shift+P / Shift+X are NOT clipboard-gated —
            // `copy_from_staging` and `move_from_staging` move files
            // between paths, they don't touch the system clipboard. The
            // four `clipboard` verbs (copy_name, copy_path,
            // copy_file_content, plus shift-Y) live in
            // `vim_bindings_resolve_clipboard` below.
            (key!(shift - p), Internal::copy_from_staging),
            (key!(shift - x), Internal::move_from_staging),
            (key!('o'), Internal::open_sort_overlay),
            (key!('b'), Internal::bookmarks),
            (key!('='), Internal::stage),
            (key!('g'), Internal::select_first),
            (key!(shift - g), Internal::select_last),
            (key!('q'), Internal::quit),
            (key!(shift - r), Internal::refresh),
            (key!('n'), Internal::next_match),
            (key!(shift - n), Internal::previous_match),
            (key!('d'), Internal::trash),
            // Vim-style navigation. `focus` is registered with
            // `key!(L)` (uppercase-char form, no SHIFT modifier in
            // the KeyCombination) — distinct from `key!(shift - l)`
            // which would add SHIFT and not match.
            (key!('j'), Internal::line_down),
            (key!('k'), Internal::line_up),
            (key!('h'), Internal::parent),
            (key!(L), Internal::focus),
        ];
        for (key, expected) in internal_bindings {
            let verb = first_verb_for_key(&store, *key).unwrap_or_else(|| {
                panic!(
                    "no verb bound to key {:?} (expected {:?})",
                    key, expected,
                )
            });
            assert_eq!(
                verb.get_internal(),
                Some(*expected),
                "key {:?} should resolve to internal {:?}, got {:?}",
                key,
                expected,
                verb.get_internal(),
            );
            // Filter-shape assertion: bare-letter bindings must be
            // permissive enough that `find_key_verb` doesn't skip
            // past them. A future `.with_condition(...)`, a
            // `.with_panel(...)`, or a `file_extensions` entry on
            // these verbs would silently break the binding without
            // tripping the get_internal check above. Pin the three
            // filters that `find_key_verb` consults.
            assert!(
                matches!(verb.selection_condition, FileTypeCondition::Any),
                "verb for key {:?} must accept any selection \
                 (selection_condition == Any) — got {:?}",
                key,
                verb.selection_condition,
            );
            assert!(
                verb.file_extensions.is_empty(),
                "verb for key {:?} must not restrict by file extension",
                key,
            );
            assert!(
                verb.panels.is_empty(),
                "verb for key {:?} must not restrict to specific panels",
                key,
            );
        }

        // External `:rm` (the only external in the bare-letter set).
        let d_upper_verb = first_verb_for_key(&store, key!(shift - d))
            .expect("shift-D must be bound");
        assert!(
            d_upper_verb.has_name("rm"),
            "shift-D should resolve to the external `rm` verb",
        );
    }

    /// Clipboard-gated bindings: these three verbs are only registered
    /// when the `clipboard` feature is enabled. Their key bindings live
    /// alongside the registrations and so are also feature-gated.
    ///
    /// Note: Shift+P (`copy_from_staging`) and Shift+X
    /// (`move_from_staging`) are NOT in this list — they copy/move files
    /// between paths and are registered unconditionally. They're pinned
    /// in `vim_bindings_resolve`.
    #[cfg(feature = "clipboard")]
    #[test]
    fn vim_bindings_resolve_clipboard() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let bindings: &[(KeyCombination, Internal)] = &[
            (key!(shift - y), Internal::copy_file_content),
            (key!('c'), Internal::copy_name),
            (key!(shift - c), Internal::copy_path),
        ];
        for (key, expected) in bindings {
            let verb = first_verb_for_key(&store, *key).unwrap_or_else(|| {
                panic!(
                    "no verb bound to key {:?} (expected {:?})",
                    key, expected,
                )
            });
            assert_eq!(
                verb.get_internal(),
                Some(*expected),
                "key {:?} should resolve to internal {:?}, got {:?}",
                key,
                expected,
                verb.get_internal(),
            );
        }
    }

    /// Table-driven check for the alt-modifier bindings (panel
    /// toggles plus the bookmarks and add modals, plus tree-navigation
    /// modifiers and the whale-spotting toggle). Alt-modifier bindings
    /// work in both Input and Command modes (alt-* keys bypass
    /// `is_key_only_modal`), so these are always live regardless of
    /// `modal:` config. Shift+arrow bindings live here too — they
    /// already carry a modifier, so they aren't gated by Command mode.
    #[test]
    fn vim_alt_bindings_resolve() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();

        let bindings: &[(KeyCombination, Internal)] = &[
            (key!(alt - '.'), Internal::toggle_hidden),
            (key!(alt - d), Internal::toggle_dates),
            (key!(alt - g), Internal::toggle_git_file_info),
            (key!(alt - shift - g), Internal::toggle_git_status),
            (key!(alt - i), Internal::toggle_ignore),
            (key!(alt - s), Internal::toggle_staging_area),
            (key!(alt - p), Internal::toggle_preview),
            (key!(alt - t), Internal::toggle_tree),
            (key!(alt - b), Internal::bookmarks),
            (key!(alt - n), Internal::add),
            (key!(alt - shift - w), Internal::toggle_whale_spotting),
            (key!(alt - down), Internal::next_same_depth),
            (key!(alt - up), Internal::previous_same_depth),
            (key!(shift - down), Internal::next_dir),
            (key!(shift - up), Internal::previous_dir),
        ];
        for (key, expected) in bindings {
            let verb = first_verb_for_key(&store, *key).unwrap_or_else(|| {
                panic!(
                    "no verb bound to key {:?} (expected {:?})",
                    key, expected,
                )
            });
            assert_eq!(
                verb.get_internal(),
                Some(*expected),
                "key {:?} should resolve to internal {:?}, got {:?}",
                key,
                expected,
                verb.get_internal(),
            );
        }
    }

    /// Pin test: `alt-h` was previously bound to `toggle_hidden`, but
    /// the vim keymap moved that binding to `alt-.` so that `h` (and
    /// alt-h variants in some configs) can be used for navigation /
    /// other purposes. This test ensures no verb in the store carries
    /// `alt-h` in its keys list, catching an accidental re-add of the
    /// old binding via a future refactor or merge.
    #[test]
    fn alt_h_no_longer_resolves_to_toggle_hidden() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let alt_h = key!(alt - h);
        for verb in store.verbs() {
            assert!(
                !verb.keys.contains(&alt_h),
                "verb {:?} still has alt-h in its keys list; the vim \
                 keymap migrated this binding to alt-. on toggle_hidden",
                verb.names,
            );
        }
    }

    /// Pin test: alt-g now binds `toggle_git_file_info`. `toggle_git_status`
    /// moved to alt-G (shift-alt-g). This guards against an accidental
    /// re-route back to the prior wiring.
    #[test]
    fn alt_g_no_longer_resolves_to_toggle_git_status() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let alt_g = key!(alt - g);
        for verb in store.verbs() {
            if verb.get_internal() == Some(Internal::toggle_git_status) {
                assert!(
                    !verb.keys.contains(&alt_g),
                    "toggle_git_status still has alt-g; should be on alt-shift-g now",
                );
            }
        }
    }

    /// Pin test: the two `sort_by_type_dirs_*` internals must be
    /// registered as verbs so the sort overlay's `f` / `l` keystrokes
    /// (which re-dispatch via `Command::from_raw(":sort_by_type_dirs_first")`
    /// and `:sort_by_type_dirs_last`) actually resolve to a verb and
    /// run the sort. Without registration, the apply path would set
    /// the status row to "verb not found" and the sort would silently
    /// never apply.
    #[test]
    fn sort_by_type_dirs_internals_registered() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let first = store
            .verbs()
            .iter()
            .find(|v| v.is_internal(Internal::sort_by_type_dirs_first))
            .expect("sort_by_type_dirs_first must be registered");
        assert!(
            first.has_name("sort_by_type_dirs_first"),
            "sort_by_type_dirs_first must be invocable by name (\
             SortOverlay's `f` key dispatches via name)",
        );
        let last = store
            .verbs()
            .iter()
            .find(|v| v.is_internal(Internal::sort_by_type_dirs_last))
            .expect("sort_by_type_dirs_last must be registered");
        assert!(
            last.has_name("sort_by_type_dirs_last"),
            "sort_by_type_dirs_last must be invocable by name (\
             SortOverlay's `l` key dispatches via name)",
        );
    }

    /// Dispatch-path pin test: the sort overlay's `f` and `l` keystrokes
    /// produce `Command::from_raw(":sort_by_type_dirs_first", true)` and
    /// `Command::from_raw(":sort_by_type_dirs_last", true)`. The
    /// `apply_command` re-dispatch path resolves those leading-`:` verb
    /// invocations through `VerbStore::search_sel_info_unique`, NOT
    /// through `v.is_internal(...)` (which the test above checks). If a
    /// future refactor changes `search_sel_info_unique` to drop these
    /// names — for example by adding a `needs_selection`-style filter
    /// that excludes the sort internals — the registration-check test
    /// above will still pass while `f` / `l` silently break. This test
    /// exercises the actual lookup function, so the dispatch path is
    /// pinned end-to-end.
    ///
    /// `SelInfo::None` + `panel_state_type: None` + `stage_is_empty: true`
    /// is the most permissive call shape; the sort internals MUST resolve
    /// under that shape, because the overlay can be opened from any
    /// panel and the sort affects the tree regardless of selection or
    /// stage state.
    #[test]
    fn sort_by_type_dirs_dispatch_via_search() {
        use crate::app::SelInfo;
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();

        let first = store
            .search_sel_info_unique(
                "sort_by_type_dirs_first",
                SelInfo::None,
                None,
                true,
            )
            .expect(
                "`:sort_by_type_dirs_first` must resolve via the verb-name \
                 search path used by apply_command re-dispatch",
            );
        assert_eq!(
            first.get_internal(),
            Some(Internal::sort_by_type_dirs_first),
        );

        let last = store
            .search_sel_info_unique(
                "sort_by_type_dirs_last",
                SelInfo::None,
                None,
                true,
            )
            .expect(
                "`:sort_by_type_dirs_last` must resolve via the verb-name \
                 search path used by apply_command re-dispatch",
            );
        assert_eq!(
            last.get_internal(),
            Some(Internal::sort_by_type_dirs_last),
        );
    }

    /// Pin test: the two surviving backup internals must be registered.
    /// The trigger `backup` is keyed (alt-shift-b); the bulk
    /// continuation `backup_apply` is unbound and hidden. Without
    /// these registrations the alt-shift-b keystroke and the
    /// confirm-overlay `CloseAndRun` re-dispatch would silently fail.
    ///
    /// Negative assertion: `backup_one` (the deleted single-file
    /// receiver) must NOT appear in the store under its old name. If
    /// anything ever resurrects the variant AND registers it, this
    /// test fails so the dead receiver gets removed rather than
    /// silently shadowing the unified flow.
    #[test]
    fn backup_internals_registered() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let trigger = store
            .verbs()
            .iter()
            .find(|v| v.is_internal(Internal::backup))
            .expect("backup must be registered");
        assert!(
            trigger.has_name("backup"),
            "backup must be invocable by name",
        );
        let apply = store
            .verbs()
            .iter()
            .find(|v| v.is_internal(Internal::backup_apply))
            .expect("backup_apply must be registered");
        assert!(
            apply.has_name("backup_apply"),
            "backup_apply must be invocable by name (the confirm \
             overlay's CloseAndRun re-dispatch resolves by name)",
        );
        assert!(
            apply.keys.is_empty(),
            "backup_apply must not bind any key — only reachable via \
             the confirm overlay's CloseAndRun",
        );
        // Negative pin: the legacy single-file receiver is gone. We
        // can't reference `Internal::backup_one` directly (the variant
        // was removed); checking the verb's surface name catches a
        // stray re-registration.
        let one_present = store
            .verbs()
            .iter()
            .any(|v| v.has_name("backup_one"));
        assert!(
            !one_present,
            "backup_one must NOT be registered — single-file backup \
             flows through the unified bulk path",
        );
    }

    /// Dispatch-path pin test: `alt-shift-b` must resolve to the
    /// trigger `Internal::backup`, not to the `backup_apply` receiver.
    /// Registration order matters because `find_key_verb` returns the
    /// first verb whose `keys` list contains the keystroke.
    #[test]
    fn backup_keybind_resolves_to_trigger() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let alt_shift_b = key!(alt - shift - b);
        let verb = first_verb_for_key(&store, alt_shift_b)
            .expect("alt-shift-b must be bound");
        assert_eq!(
            verb.get_internal(),
            Some(Internal::backup),
            "alt-shift-b must resolve to Internal::backup (the \
             trigger), not to backup_apply",
        );
    }

}

#[cfg(test)]
mod staging_bindings_tests {
    //! Pin tests for the staging-related key bindings. Kept separate
    //! from `vim_bindings_tests` because the stage keys (`+`, `=`,
    //! `ctrl-g`, `-`) predate the vim keymap — they were the original
    //! broot bindings and are not part of the bare-letter Command-mode
    //! set that `vim_bindings_tests` is about.
    use {
        super::*,
        crokey::key,
    };

    /// Helper mirrored from `vim_bindings_tests::first_verb_for_key`.
    /// Walks the verb store in registration order returning the first
    /// verb whose `keys` list contains `key` (matches the resolution
    /// order `find_key_verb` uses, minus the per-verb filters which
    /// the staging keys don't carry).
    fn first_verb_for_key(
        store: &VerbStore,
        key: KeyCombination,
    ) -> Option<&Verb> {
        store.verbs().iter().find(|v| v.keys.contains(&key))
    }

    /// Pin test: `+`, `=`, and `ctrl-g` all resolve to `Internal::stage`,
    /// not `Internal::toggle_stage`. All three keys share the
    /// "add-only + advance" fast-stager behaviour in BrowserState. The
    /// `toggle_stage` internal stays registered (callable as
    /// `:toggle_stage` from user conf) but has NO default key binding.
    #[test]
    fn stage_keys_bound_to_stage_not_toggle() {
        let mut conf = Conf::default();
        let store = VerbStore::new(&mut conf).unwrap();
        let keys: &[KeyCombination] =
            &[key!('+'), key!('='), key!(ctrl - g)];
        for key in keys {
            let verb = first_verb_for_key(&store, *key).unwrap_or_else(|| {
                panic!("key {:?} must be bound", key)
            });
            assert_eq!(
                verb.get_internal(),
                Some(Internal::stage),
                "key {:?} must resolve to Internal::stage (the unified \
                 fast-stager), not to Internal::toggle_stage",
                key,
            );
        }
        // `toggle_stage` is still registered but unbound — confirm
        // both halves of that invariant.
        let toggle = store
            .verbs()
            .iter()
            .find(|v| v.is_internal(Internal::toggle_stage))
            .expect("toggle_stage must stay registered for :toggle_stage typed-verb use");
        assert!(
            toggle.keys.is_empty(),
            "toggle_stage must not have any default key binding — \
             the three default keys all bind `stage` now",
        );
    }
}
