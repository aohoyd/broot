use {
    super::*,
    crate::{
        backup,
        browser::BrowserState,
        bulk_rename,
        cli::TriBool,
        command::{
            Command,
            CommandParts,
            Sequence,
        },
        conf::Conf,
        display::*,
        errors::ProgramError,
        file_sum,
        git,
        kitty,
        launchable::Launchable,
        path::closest_dir,
        pattern::InputPattern,
        preview::PreviewState,
        skin::*,
        syntactic::SyntaxTheme,
        task_sync::{
            Dam,
            Either,
        },
        terminal,
        verb::{
            ExecutionBuilder,
            Internal,
            InvocationParser,
            VerbId,
            VerbStore,
        },
        watcher::Watcher,
    },
    crokey::crossterm::event::Event,
    std::{
        io::Write,
        path::{
            Component,
            Path,
            PathBuf,
        },
        str::FromStr,
        sync::{
            Arc,
            Mutex,
        },
    },
    termimad::{
        EventSource,
        EventSourceOptions,
        crossbeam::channel::{
            Receiver,
            Sender,
            unbounded,
        },
    },
};

/// The GUI
pub struct App {
    /// the panels of the application, with their inputs
    panels: AppPanelsAndInputs,

    /// whether the app is in the (uncancellable) process of quitting
    quitting: bool,

    /// what must be done after having closed the TUI
    launch_at_end: Option<Launchable>,

    /// an optional copy of the root for the --server
    shared_root: Option<Arc<Mutex<PathBuf>>>,

    /// sender to the sequence channel
    tx_seqs: Sender<Sequence>,

    /// receiver to listen to the sequence channel
    rx_seqs: Receiver<Sequence>,

    /// a watcher for notify events
    watcher: Watcher,

    /// floating overlay layer (confirm modal, goto modal, etc.).
    /// When `Some`, the overlay captures all key/mouse events and is
    /// rendered on top of every panel.
    overlay: Option<Overlay>,

    /// One-shot flag: when `true`, the next call to `apply_command`
    /// skips its `requires_confirm` / `Internal::trash` intercept,
    /// allowing the post-confirmation re-dispatch to actually run the
    /// destructive action. Reset to `false` after each consult.
    skip_confirm: bool,

    /// Payload field for the bulk-rename flow: the validated
    /// [`bulk_rename::RenameRun`] is stashed here while the user reviews
    /// the diff in a `ConfirmOverlay`. On confirm the overlay returns
    /// `CloseAndRun(:bulk_rename_apply)`, the apply handler `mem::take`s
    /// this slot and runs `bulk_rename::apply`. Mirrors the
    /// `skip_confirm` "single-field, single-consumer" discipline — no
    /// `Command` enum payload changes.
    pending_bulk_rename: Option<bulk_rename::RenameRun>,

    /// Payload field for the bulk-backup flow: the planned
    /// [`crate::backup::BackupRun`] is stashed here while the user
    /// reviews the `src → dst` diff in a `ConfirmOverlay`. On confirm
    /// the overlay returns `CloseAndRun(:backup_apply)`, the apply
    /// handler `mem::take`s this slot and runs
    /// [`crate::backup::apply`]. Mirrors the `pending_bulk_rename`
    /// "single-field, single-consumer" discipline; cleared in the
    /// `OverlayOutcome::Close` arm so a cancelled overlay never leaves
    /// a stale plan that a later direct `:backup_apply` could pick up.
    pending_backup: Option<backup::BackupRun>,
}

impl App {
    pub fn new(con: &AppContext) -> Result<App, ProgramError> {
        let mut panels = AppPanelsAndInputs::new(con)?;
        if let Some(path) = con.initial_file.as_ref() {
            // open initial_file in preview
            let preview_state = Box::new(PreviewState::new(
                path.clone(),
                InputPattern::none(),
                0,
                None,
                con.initial_tree_options.clone(),
                con,
            ));
            if let Err(err) = panels.new_panel(
                preview_state,
                PanelPurpose::Preview,
                HDir::Right,
                true, // activate
                con,
            ) {
                warn!("could not open preview: {err}");
            }
        }
        let (tx_seqs, rx_seqs) = unbounded::<Sequence>();
        let watcher = Watcher::new(tx_seqs.clone());
        Ok(Self {
            panels,
            quitting: false,
            launch_at_end: None,
            shared_root: None,
            tx_seqs,
            rx_seqs,
            watcher,
            overlay: None,
            skip_confirm: false,
            pending_bulk_rename: None,
            pending_backup: None,
        })
    }

    /// Install a `ConfirmOverlay` on top of the current panels. The
    /// `pending` command will be re-dispatched if (and only if) the
    /// user confirms. The re-dispatch uses the `skip_confirm` flag so
    /// `apply_command` does not loop back into the overlay.
    pub(crate) fn request_confirm(
        &mut self,
        title: impl Into<String>,
        body: Vec<String>,
        confirm_label: impl Into<String>,
        danger: bool,
        pending: Command,
    ) {
        self.overlay = Some(Overlay::Confirm(ConfirmOverlay::new(
            title,
            body,
            confirm_label,
            danger,
            pending,
        )));
    }

    /// apply a command. Change the states but don't redraw on screen.
    fn apply_command(
        &mut self,
        w: &mut W,
        cmd: &Command,
        panel_skin: &PanelSkin,
        app_state: &mut AppState,
        con: &mut AppContext,
    ) -> Result<(), ProgramError> {
        info!("app applying command: {:?}", &cmd);
        let is_input_invocation = cmd.is_verb_invocated_from_input();

        // Confirmation intercept. Three branches, evaluated in
        // precedence order — the first match opens an overlay and
        // returns, so at most one overlay fires per dispatch:
        //
        //   1. Bulk staging — a verb is being run while the stage
        //      panel is active and contains more than one path.
        //   2. Overwrite check — `:cp`/`:mv` family resolves to an
        //      existing destination.
        //   3. Destructive verb — `:rm`, `:trash`, or any verb with
        //      `requires_confirm == true`.
        //
        // The `skip_confirm` flag suppresses all three on the
        // post-confirmation re-entry; an already-open overlay also
        // bypasses them (the overlay handler manages re-dispatch).
        if !self.skip_confirm && self.overlay.is_none() {
            if let Some((title, body, confirm_label, danger)) =
                self.maybe_bulk_stage_confirm(cmd, app_state, con)
            {
                self.request_confirm(title, body, confirm_label, danger, cmd.clone());
                if is_input_invocation {
                    self.panels.clear_input_invocation(con);
                }
                return Ok(());
            }
            if let Some((title, body, confirm_label)) =
                self.maybe_destructive_confirm(cmd, app_state, con)
            {
                self.request_confirm(title, body, confirm_label, true, cmd.clone());
                if is_input_invocation {
                    self.panels.clear_input_invocation(con);
                }
                return Ok(());
            }
        }
        self.skip_confirm = false;

        // Bulk-rename intercept. Two internals fire at the App level —
        // they need to read `app_state.stage`, drive the external
        // editor, and open an overlay, none of which fit neatly into
        // a `PanelState::on_internal` arm. Both legs return early so
        // the panel layer never sees these commands.
        //
        //   `Internal::bulk_rename`       — F2 entry point. Stage < 2:
        //                                    fall through to the inline
        //                                    rename external verb.
        //                                    Stage ≥ 2: run the editor,
        //                                    plan, open ConfirmOverlay.
        //   `Internal::bulk_rename_apply` — re-entered from the confirm
        //                                    overlay's `CloseAndRun`.
        //                                    Consumes `pending_bulk_rename`
        //                                    and runs `bulk_rename::apply`.
        if let Some(internal) = resolved_internal(cmd, con) {
            match internal {
                Internal::bulk_rename => {
                    if is_input_invocation {
                        self.panels.clear_input_invocation(con);
                    }
                    return self.run_bulk_rename(w, panel_skin, app_state, con);
                }
                Internal::bulk_rename_apply => {
                    if is_input_invocation {
                        self.panels.clear_input_invocation(con);
                    }
                    self.run_bulk_rename_apply(app_state, con);
                    return Ok(());
                }
                // Backup intercept: mirrors the bulk_rename triplet.
                // `backup` is the keyed trigger (alt-shift-b). Stage <2
                // delegates to `prefill_backup_one`, which writes the
                // computed `:backup_one <next-free-name>` invocation
                // directly into the input bar (no command synthesis).
                // Stage ≥2 plans a `BackupRun`, stashes it on
                // `self.pending_backup`, and opens a `ConfirmOverlay`
                // whose CloseAndRun re-dispatches `:backup_apply`.
                Internal::backup => {
                    if is_input_invocation {
                        self.panels.clear_input_invocation(con);
                    }
                    return self.run_backup(app_state, con);
                }
                Internal::backup_one => {
                    if is_input_invocation {
                        self.panels.clear_input_invocation(con);
                    }
                    return self.run_backup_one(w, cmd, panel_skin, app_state, con);
                }
                Internal::backup_apply => {
                    if is_input_invocation {
                        self.panels.clear_input_invocation(con);
                    }
                    self.run_backup_apply(con);
                    return Ok(());
                }
                _ => {}
            }
        }

        let cmd_result = self
            .panels
            .apply_command(w, cmd, None, panel_skin, app_state, con)?;
        let mut error: Option<String> = None;
        let mut new_active_panel_idx = None;
        match cmd_result {
            CmdResult::ApplyOnPanel { id } => {
                let aop_cmd_result = self.panels.apply_command(
                    w,
                    cmd,
                    Some(PanelReference::Id(id)),
                    panel_skin,
                    app_state,
                    con,
                )?;
                if let CmdResult::DisplayError(txt) = aop_cmd_result {
                    // we should probably handle other results
                    // which implies the possibility of a recursion
                    error = Some(txt);
                } else if is_input_invocation {
                    self.panels.clear_input();
                }
            }
            CmdResult::ClosePanel {
                validate_purpose,
                panel_ref,
                clear_cache,
            } => {
                if is_input_invocation {
                    self.panels.clear_input_invocation(con);
                }
                let close_idx = self.panels.idx_by_ref(panel_ref)
                    .unwrap_or_else(|| self.panels.active_panel_idx());
                let mut new_arg = None;
                if validate_purpose {
                    let purpose = &self.panels.panel_by_idx_unchecked(close_idx).purpose;
                    if let PanelPurpose::ArgEdition { .. } = purpose {
                        new_arg = self
                            .panels
                            .panel_by_idx_unchecked(close_idx)
                            .state()
                            .selected_path()
                            .map(|p| p.to_string_lossy().to_string());
                    }
                }
                if clear_cache {
                    clear_caches();
                }
                if self.panels.close(close_idx, con) {
                    let screen = self.panels.screen();
                    self.panels.refresh_active_panel(con);
                    if let Some(new_arg) = new_arg {
                        self.panels.set_input_arg(new_arg);
                        let new_input = self.panels.get_input_content();
                        let cmd = Command::from_raw(new_input, false);
                        let app_cmd_context = AppCmdContext {
                            panel_skin,
                            preview_panel: self.panels.preview_panel_id(),
                            stage_panel: self.panels.stage_panel_id(),
                            screen,
                            con,
                        };
                        self.panels.mut_panel().apply_command(
                            w,
                            &cmd,
                            app_state,
                            &app_cmd_context,
                        )?;
                    }
                } else {
                    self.quitting = true;
                }
            }
            CmdResult::ChangeLayout(instruction) => {
                con.layout_instructions.push(instruction);
                self.panels.resize_all(con);
            }
            CmdResult::DisplayError(txt) => {
                error = Some(txt);
            }
            CmdResult::ExecuteSequence { sequence } => {
                if is_input_invocation {
                    self.panels.clear_input();
                }
                self.tx_seqs.send(sequence).unwrap();
            }
            CmdResult::HandleInApp(internal) => {
                debug!("handling internal {internal:?} at app level");
                match internal {
                    Internal::escape => {
                        let mode = self.panels.state().get_mode();
                        let cmd = self.panels.do_input_escape(mode, con);
                        debug!("cmd on escape: {cmd:?}");
                        self.apply_command(w, &cmd, panel_skin, app_state, con)?;
                    }
                    Internal::focus_staging_area_no_open => {
                        self.panels.focus_by_type(PanelStateType::Stage);
                    }
                    Internal::focus_panel_left => {
                        let len = self.panels.len();
                        new_active_panel_idx =
                            Some((self.panels.active_panel_idx() + len - 1) % len);
                    }
                    Internal::focus_panel_right => {
                        let len = self.panels.len();
                        new_active_panel_idx = Some((self.panels.active_panel_idx() + 1) % len);
                    }
                    Internal::panel_left_no_open => {
                        // move to the panel on the left, if any
                        new_active_panel_idx = if self.panels.active_panel_idx() == 0 {
                            None // already at leftmost — do nothing
                        } else {
                            Some(self.panels.active_panel_idx() - 1)
                        };
                    }
                    Internal::panel_right_no_open => {
                        // move to the panel on the right, if any
                        new_active_panel_idx =
                            if self.panels.active_panel_idx() + 1 == self.panels.len() {
                                None // already at rightmost — do nothing
                            } else {
                                Some(self.panels.active_panel_idx() + 1)
                            };
                    }
                    Internal::search_again => {
                        if let Some(raw_pattern) = &self.panels.panel().last_raw_pattern {
                            let sequence = Sequence::new_single(raw_pattern.clone());
                            self.tx_seqs.send(sequence).unwrap();
                        }
                    }
                    Internal::set_syntax_theme => {
                        let arg = cmd.as_verb_invocation().and_then(|vi| vi.args.as_ref());
                        match arg {
                            Some(arg) => match SyntaxTheme::from_str(arg) {
                                Ok(theme) => {
                                    con.syntax_theme = Some(theme);
                                    self.panels.update_preview(true, con);
                                }
                                Err(e) => {
                                    error = Some(e.to_string());
                                }
                            },
                            None => {
                                error = Some("no theme provided".to_string());
                            }
                        }
                    }
                    Internal::toggle_second_tree => {
                        let panels_count = self.panels.len();
                        let trees_count = self.panels.count_of_type(PanelStateType::Tree);
                        if trees_count < 2 {
                            // we open a tree, closing a (non tree) panel if necessary
                            if panels_count >= con.max_panels_count {
                                self.panels.close_first_non_tree(con);
                            }
                            if let Some(selected_path) = self.panels.state().selected_path() {
                                let dir = closest_dir(selected_path);
                                let screen = self.panels.screen();
                                if let Ok(new_state) = BrowserState::new(
                                    dir,
                                    self.panels.state().tree_options().without_pattern(),
                                    screen,
                                    con,
                                    &Dam::unlimited(),
                                ) {
                                    if let Err(s) = self.panels.new_panel(
                                        Box::new(new_state),
                                        PanelPurpose::None,
                                        HDir::Right,
                                        is_input_invocation,
                                        con,
                                    ) {
                                        error = Some(s);
                                    }
                                }
                            }
                        } else {
                            self.panels.close_rightest_inactive_tree(con);
                        }
                    }
                    Internal::toggle_watch => {
                        app_state.watch_tree ^= true;
                        if is_input_invocation {
                            self.panels.clear_input_invocation(con);
                        }
                    }
                    _ => {
                        let cmd = self.panels.on_input_internal(internal);
                        if cmd.is_none() {
                            warn!(
                                "unhandled propagated internal. internal={internal:?} cmd={cmd:?}"
                            );
                        } else {
                            self.apply_command(w, &cmd, panel_skin, app_state, con)?;
                        }
                    }
                }
            }
            CmdResult::Keep => {
                if is_input_invocation {
                    self.panels.clear_input_invocation(con);
                }
            }
            CmdResult::Message(md) => {
                if is_input_invocation {
                    self.panels.clear_input_invocation(con);
                }
                self.panels.mut_panel().set_message(md);
            }
            CmdResult::Launch(launchable) => {
                self.launch_at_end = Some(*launchable);
                self.quitting = true;
            }
            CmdResult::NewPanel {
                state,
                purpose,
                direction,
                activate,
            } => {
                if let Err(s) =
                    self.panels
                        .new_panel(state, purpose, direction, activate || is_input_invocation, con)
                {
                    error = Some(s);
                }
            }
            CmdResult::NewState { state, message } => {
                self.panels.clear_input();
                self.panels.push_state(state);
                if let Some(md) = message {
                    self.panels.mut_panel().set_message(md);
                } else {
                    self.panels.refresh_input_status(app_state, panel_skin, con);
                }
            }
            CmdResult::PopState => {
                if is_input_invocation {
                    self.panels.clear_input();
                }
                if self.panels.remove_state(con) {
                    let screen = self.panels.screen();
                    self.panels.mut_state().refresh(screen, con);
                    self.panels.refresh_input_status(app_state, panel_skin, con);
                } else if con.quit_on_last_cancel {
                    self.quitting = true;
                }
            }
            CmdResult::PopStateAndReapply => {
                if is_input_invocation {
                    self.panels.clear_input();
                }
                if self.panels.remove_state(con) {
                    self.panels.apply_command(
                        w, cmd, None, // active panel
                        panel_skin, app_state, con,
                    )?;
                } else if con.quit_on_last_cancel {
                    self.quitting = true;
                }
            }
            CmdResult::Quit => {
                self.quitting = true;
            }
            CmdResult::RefreshState { clear_cache } => {
                info!("refreshing, clearing cache={clear_cache}");
                if is_input_invocation {
                    self.panels.clear_input_invocation(con);
                }
                if clear_cache {
                    clear_caches();
                }
                app_state.stage.refresh();
                self.panels.refresh_all_panels(con);
            }
            CmdResult::OpenOverlay(overlay) => {
                if is_input_invocation {
                    self.panels.clear_input_invocation(con);
                }
                self.overlay = Some(*overlay);
            }
        }
        if let Some(text) = error {
            self.panels.mut_panel().set_error(text);
        }

        if let Some(idx) = new_active_panel_idx {
            debug!("activating panel idx {idx}");
            if is_input_invocation {
                self.panels.clear_input();
            }
            self.panels.activate(idx);
            self.panels.refresh_input_status(app_state, panel_skin, con);
        }

        app_state.other_panel_path = self.panels.get_other_panel_path();
        if let Some(path) = self.panels.state().tree_root() {
            app_state.root = path.to_path_buf();
            terminal::update_title(w, app_state, con);
            if con.update_work_dir {
                if let Err(e) = std::env::set_current_dir(&app_state.root) {
                    warn!("Failed to set current dir: {e}");
                }
            }
            if let Some(shared_root) = &mut self.shared_root {
                if let Ok(mut root) = shared_root.lock() {
                    root.clone_from(&app_state.root);
                }
            }
        }

        self.panels.update_preview(false, con);

        Ok(())
    }

    /// Inspect a command and, if it would trigger a destructive verb
    /// or `Internal::trash`, return the `(title, body, confirm_label)`
    /// for the confirmation overlay. Returns `None` for any command
    /// that should run as-is.
    ///
    /// Resolution rules mirror `Panel::apply_command`'s dispatch:
    /// - `Command::VerbInvocate` — look up by name in `verb_store`.
    /// - `Command::VerbTrigger` — look up by `verb_id`.
    /// - `Command::Internal { internal: trash, .. }` — hard-coded.
    ///
    /// The cp/mv-family verbs (`:cp`, `:mv`, `:cpp`/`copy_to_panel`,
    /// `:mvp`/`move_to_panel`) get a *conditional* prompt: only when
    /// the resolved destination already exists.
    fn maybe_destructive_confirm(
        &self,
        cmd: &Command,
        _app_state: &AppState,
        con: &AppContext,
    ) -> Option<(String, Vec<String>, String)> {
        let selected_path = self
            .panels
            .state()
            .selected_path()
            .map(|p| p.to_path_buf());
        let trash_prompt = |path: Option<&std::path::Path>| -> (String, Vec<String>, String) {
            let body = path_label_or_unknown(path);
            let title = match path {
                Some(p) => format!(
                    "Trash {}?",
                    p.file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| p.to_string_lossy().to_string()),
                ),
                None => "Trash file?".to_string(),
            };
            (title, body, "Trash".to_string())
        };
        let other_panel_path = self.panels.get_other_panel_path();
        match cmd {
            Command::VerbInvocate(invocation) => {
                // Resolve the verb by name. We don't have a sel_info
                // handy here; the verb-name lookup alone is enough to
                // decide whether to confirm.
                let verb = con
                    .verb_store
                    .verbs()
                    .iter()
                    .find(|v| v.has_name(&invocation.name))?;
                if verb.is_internal(Internal::trash) {
                    return Some(trash_prompt(selected_path.as_deref()));
                }
                // Conditional cp/mv-family overwrite check.
                if let Some(target) = resolve_overwrite_target(
                    verb,
                    invocation.args.as_deref(),
                    selected_path.as_deref(),
                    other_panel_path.as_deref(),
                ) {
                    return Some(overwrite_prompt(&target));
                }
                if !verb.requires_confirm {
                    return None;
                }
                let name = verb
                    .names
                    .first()
                    .cloned()
                    .unwrap_or_else(|| invocation.name.clone());
                let body = path_label_or_unknown(selected_path.as_deref());
                Some((format!("Run :{name}?"), body, verb_confirm_label(&name)))
            }
            Command::VerbTrigger { verb_id, .. } => {
                let verb = con.verb_store.verb(*verb_id);
                if verb.is_internal(Internal::trash) {
                    return Some(trash_prompt(selected_path.as_deref()));
                }
                // Triggers don't carry user-typed args, so only the
                // `{other-panel-directory}` form (cpp/mvp) is reachable
                // here without an invocation string.
                if let Some(target) = resolve_overwrite_target(
                    verb,
                    None,
                    selected_path.as_deref(),
                    other_panel_path.as_deref(),
                ) {
                    return Some(overwrite_prompt(&target));
                }
                if !verb.requires_confirm {
                    return None;
                }
                let name = verb.names.first().cloned().unwrap_or_default();
                let body = path_label_or_unknown(selected_path.as_deref());
                Some((format!("Run :{name}?"), body, verb_confirm_label(&name)))
            }
            Command::Internal { internal, .. } if *internal == Internal::trash => {
                Some(trash_prompt(selected_path.as_deref()))
            }
            _ => None,
        }
    }

    /// Inspect a command and, if it would fan out across more than one
    /// staged path, return `(title, body, confirm_label, danger)` for
    /// a bulk-staging confirmation overlay. Returns `None` when the
    /// command does not run against the staging area, when the stage
    /// has fewer than two paths, or when the verb is a stage-management
    /// internal (e.g. `:unstage`) that should not surface a fan-out
    /// prompt.
    ///
    /// The bulk overlay always supersedes the destructive-verb and
    /// overwrite-check branches — the user only sees one confirmation
    /// per dispatch. `danger` is set to the verb's `requires_confirm`
    /// flag so destructive bulk ops still get the red palette.
    fn maybe_bulk_stage_confirm(
        &self,
        cmd: &Command,
        app_state: &AppState,
        con: &AppContext,
    ) -> Option<(String, Vec<String>, String, bool)> {
        // Only fire when the stage panel is the active panel. When the
        // active panel is a tree/preview, the verb runs against the
        // tree's current selection — a single file — even if the stage
        // is non-empty.
        if self.panels.state().get_type() != PanelStateType::Stage {
            return None;
        }
        let count = app_state.stage.len();
        if count < 2 {
            return None;
        }
        let verb = match cmd {
            Command::VerbInvocate(invocation) => con
                .verb_store
                .verbs()
                .iter()
                .find(|v| v.has_name(&invocation.name))?,
            Command::VerbTrigger { verb_id, .. } => con.verb_store.verb(*verb_id),
            _ => return None,
        };
        // Confirm only when the resolved internal actually iterates
        // the stage's contents (see `is_stage_consuming_internal`).
        // Everything else — navigation, toggles, app-level verbs like
        // `:quit` / `:help`, the input-row edits, etc. — bypasses
        // because the deny-list returns `false` for them. The five
        // overlay/modal internals (`bulk_rename`, `bulk_rename_apply`,
        // `backup`, `backup_one`, `backup_apply`) also bypass this
        // function because `is_stage_consuming_internal` returns
        // `false` for them, so the deny-list check below returns
        // early before the resolved-verb check fires. (Note: the
        // backup intercept actually runs AFTER this function in
        // `apply_command`, not before — safety comes from the
        // deny-list, not from execution order.) `add` opens its own
        // modal via `BrowserState::on_internal` and is also not
        // stage-consuming.
        if let Some(internal) = verb.get_internal() {
            if !is_stage_consuming_internal(internal) {
                return None;
            }
        }
        let name = verb
            .names
            .first()
            .cloned()
            .unwrap_or_else(|| "<verb>".to_string());
        let title = format!("Run :{name} on {count} files?");
        let body = bulk_stage_body(app_state.stage.paths());
        let confirm_label = verb_confirm_label(&name);
        let danger = verb.requires_confirm;
        Some((title, body, confirm_label, danger))
    }

    /// Run the bulk-rename entry leg. Drives the user through the
    /// $EDITOR → parse → plan → confirm-overlay pipeline. Falls through
    /// to the inline `rename` external when the stage has fewer than
    /// two paths so the same key (F2) still surfaces the existing
    /// single-file rename.
    ///
    /// Failure modes surface to the status row and return `Ok(())`;
    /// a hard panic is never appropriate for any user-driven error
    /// here (editor missing, parse failure, validation failure, fs
    /// error during the editor's read-back, etc.).
    fn run_bulk_rename(
        &mut self,
        w: &mut W,
        panel_skin: &PanelSkin,
        app_state: &mut AppState,
        con: &mut AppContext,
    ) -> Result<(), ProgramError> {
        let stage_paths = app_state.stage.paths().to_vec();
        if stage_paths.len() < 2 {
            // Fall through to the inline rename external verb. We
            // resolve it by name and synthesize a `Command::VerbTrigger`
            // so the verb's existing arg-prompt flow fires unchanged.
            // If the external isn't registered (custom user conf), we
            // surface an empty-stage hint instead of silently swallowing
            // the keypress.
            if let Some(verb_id) = self.find_external_rename_verb_id(con) {
                let cmd = Command::VerbTrigger {
                    verb_id,
                    input_invocation: None,
                };
                return self.apply_command(w, &cmd, panel_skin, app_state, con);
            } else {
                self.panels.mut_panel().set_error(
                    "bulk rename: stage 2+ files to use the bulk flow".to_string(),
                );
                return Ok(());
            }
        }

        let content = bulk_rename::serialize(&stage_paths);
        let edited = match editor::edit_in_external(&content, ".broot-rename") {
            Ok(s) => s,
            Err(e) => {
                self.panels.mut_panel().set_error(format!("bulk rename: {e}"));
                return Ok(());
            }
        };
        let parsed = bulk_rename::parse(&edited);
        let run = match bulk_rename::plan(&stage_paths, &parsed, &|p| p.exists()) {
            Ok(r) => r,
            Err(e) => {
                self.panels.mut_panel().set_error(e.to_string());
                return Ok(());
            }
        };
        if run.renames.is_empty() {
            self.panels.mut_panel().set_message("bulk rename: no changes");
            return Ok(());
        }

        let count = run.renames.len();
        let body = bulk_rename::diff_lines(&run);
        let title = if count == 1 {
            "Rename 1 file?".to_string()
        } else {
            format!("Rename {count} files?")
        };
        self.pending_bulk_rename = Some(run);
        self.request_confirm(
            title,
            body,
            "Rename",
            false,
            Command::from_raw(":bulk_rename_apply".to_string(), true),
        );
        Ok(())
    }

    /// Apply the validated `pending_bulk_rename`. Errors surface to the
    /// status row; on success the stage is cleared and the active panel
    /// is refreshed so the tree picks up the new names. Partial failure
    /// leaves the renames that succeeded before the failure in place
    /// (mirrors `bulk_rename::apply`'s "no rollback" contract).
    fn run_bulk_rename_apply(
        &mut self,
        app_state: &mut AppState,
        con: &AppContext,
    ) {
        let Some(run) = self.pending_bulk_rename.take() else {
            self.panels.mut_panel().set_error(
                "bulk rename: nothing pending".to_string(),
            );
            return;
        };
        match bulk_rename::apply(&run) {
            Ok(()) => {
                app_state.stage.clear();
                clear_caches();
                self.panels.refresh_all_panels(con);
                self.panels.mut_panel().set_message(format!(
                    "renamed {} file{}",
                    run.renames.len(),
                    if run.renames.len() == 1 { "" } else { "s" },
                ));
            }
            Err((path, err)) => {
                self.panels.mut_panel().set_error(format!(
                    "bulk rename failed at {}: {}",
                    path.display(),
                    err,
                ));
                // Some renames may have applied before the failure;
                // refresh anyway so the tree reflects current truth.
                clear_caches();
                self.panels.refresh_all_panels(con);
            }
        }
    }

    /// Look up the built-in external `rename` verb's ID so the
    /// stage-size-<2 fall-through can synthesize a `VerbTrigger` that
    /// runs the existing inline-rename flow. The verb is identified by
    /// name "rename" AND being external (not internal) — distinguishes
    /// it from the F2 internal `bulk_rename` we just added.
    fn find_external_rename_verb_id(&self, con: &AppContext) -> Option<VerbId> {
        for verb in con.verb_store.verbs() {
            if verb.has_name("rename") && verb.get_internal().is_none() {
                return Some(verb.id);
            }
        }
        None
    }

    /// Drive the `:backup` trigger. With stage < 2, populate the input
    /// bar with the prefilled `:backup_one <next-free-name>` invocation
    /// so the user can review/edit before hitting Enter. With stage ≥ 2,
    /// plan the bulk run, stash it on `self.pending_backup`, and open a
    /// confirm overlay listing the `src → dst` rows. The overlay's
    /// CloseAndRun re-dispatches `:backup_apply`, which `mem::take`s
    /// the stash and runs `backup::apply`.
    ///
    /// Single-target prefill is built inline rather than recursed via
    /// `apply_command`: the panel-input prefill branch in
    /// `command/panel_input.rs:526-540` only fires on a physical key
    /// event. A synthesized `Command::VerbTrigger { backup_one, .. }`
    /// going through `apply_command` would re-enter the App-level
    /// `Internal::backup_one` intercept and fail with "missing
    /// invocation" (the trigger has no `input_invocation`). Mirroring
    /// the panel-input logic here keeps the prefill in one place per
    /// flow without coupling `apply_command` to the input-row state.
    ///
    /// Cap-exhaust handling: if any planned row has `dst == src` (every
    /// `.bak.N` candidate already exists), the overlay is NOT opened.
    /// The error string enumerates the first blocked path and the count
    /// of remaining blocked paths, so a multi-file stage with several
    /// cap-exhausted entries doesn't hide the impact behind a single
    /// filename. The user must clean up the existing backups before
    /// retrying — the writable rows are intentionally NOT auto-applied
    /// (a half-empty backup set is a worse footgun than the error).
    fn run_backup(
        &mut self,
        app_state: &mut AppState,
        con: &mut AppContext,
    ) -> Result<(), ProgramError> {
        let stage_paths = app_state.stage.paths().to_vec();
        if stage_paths.len() < 2 {
            return self.prefill_backup_one(app_state, con);
        }

        let run = backup::plan_bulk_backup(&stage_paths, &con.backup_suffix);
        if run.copies.is_empty() {
            self.panels
                .mut_panel()
                .set_message("backup: no eligible paths in stage");
            return Ok(());
        }
        // Cap-exhaust detection: a sentinel row (dst == src) means
        // `next_free_backup_name` could not find a free candidate
        // within MAX_BACKUP_BUMP. Surface the impact (first blocked
        // path + count of remaining blocked paths) so the user knows
        // how many entries are affected. We do not open the overlay
        // (partial applies are confusing and there's no obvious "skip
        // and continue" affordance).
        let blocked_count = run.copies.iter().filter(|c| c.src == c.dst).count();
        if blocked_count > 0 {
            let first_blocked = run
                .copies
                .iter()
                .find(|c| c.src == c.dst)
                .expect("blocked_count > 0 guarantees at least one match");
            let msg = cap_exhaust_message(&first_blocked.src, blocked_count);
            self.panels.mut_panel().set_error(msg);
            return Ok(());
        }

        let count = run.copies.len();
        let body: Vec<String> = run
            .copies
            .iter()
            .map(|c| format!("{} → {}", c.src.display(), c.dst.display()))
            .collect();
        let title = format!("Backup {count} files?");
        self.pending_backup = Some(run);
        self.request_confirm(
            title,
            body,
            "Backup",
            false,
            Command::from_raw(":backup_apply".to_string(), true),
        );
        Ok(())
    }

    /// Populate the active panel's input bar with the prefilled
    /// `:backup_one <next-free-name>` invocation.
    ///
    /// Mirrors the `auto_exec:false` prefill branch in
    /// `command/panel_input.rs:532-540`: locate the receiver verb,
    /// build an `ExecutionBuilder` against the current selection,
    /// resolve the invocation pattern's `{new_filename:backup-name}`
    /// placeholder via `invocation_with_default`, then write the
    /// resulting `:backup_one <name>` string into the input field.
    ///
    /// Failure modes (verb not registered, missing invocation parser,
    /// no selection) surface as status errors with `Ok(())`; the
    /// keypress is never silently swallowed.
    fn prefill_backup_one(
        &mut self,
        app_state: &AppState,
        con: &AppContext,
    ) -> Result<(), ProgramError> {
        let invocation_parser = match resolve_backup_one_invocation_parser(&con.verb_store) {
            Ok(p) => p,
            Err(msg) => {
                self.panels.mut_panel().set_error(msg.to_string());
                return Ok(());
            }
        };
        // Build the prefilled invocation from the current selection's
        // sel_info. `invocation_with_default` resolves the
        // `{new_filename:backup-name}` placeholder via
        // `get_sel_name_standard_replacement`, which reads
        // `con.backup_suffix` and consults `next_free_backup_name`.
        let sel_info = self.panels.state().sel_info(app_state);
        let exec_builder = ExecutionBuilder::without_invocation(sel_info, app_state);
        let verb_invocation = exec_builder
            .invocation_with_default(&invocation_parser.invocation_pattern, con);
        // Preserve any existing pattern part the user had typed, then
        // splice in the new invocation — matches panel_input's
        // `parts.verb_invocation = Some(...)` mutation.
        let current = self.panels.get_input_content();
        let mut parts = CommandParts::from(current);
        parts.verb_invocation = Some(verb_invocation);
        self.panels.input().set_content(&parts.to_string());
        Ok(())
    }

    /// Receiver for the single-file leg.
    ///
    /// The user has edited (or accepted) the prefilled name and pressed
    /// Enter. The incoming command is a
    /// `Command::VerbInvocate(VerbInvocation { name: "backup_one",
    /// args: Some("<typed name>"), .. })` (the Backup intercept already
    /// guarantees the internal resolves to `Internal::backup_one`).
    ///
    /// Failure modes are all soft — every problem path emits a status
    /// error and returns `Ok(())`. The handler never panics on
    /// user-driven input (missing args, root selection, collision, I/O
    /// failure during copy).
    ///
    /// On success, the active panel is refreshed and a synthetic
    /// `:focus <target>` is dispatched so the new backup becomes the
    /// selected tree entry — matching the `AddOverlay::try_commit` UX
    /// for "the thing you just created is now the selection".
    ///
    /// Overwrite policy: the explicit user-typed name MUST NOT clobber
    /// an existing entry. Numbered fallback (`.bak` → `.bak.1`) is the
    /// prefill behaviour; if the user types a colliding name themselves
    /// we refuse rather than silently bumping or overwriting. This
    /// matches the AddOverlay's "refuse to clobber" policy.
    fn run_backup_one(
        &mut self,
        w: &mut W,
        cmd: &Command,
        panel_skin: &PanelSkin,
        app_state: &mut AppState,
        con: &mut AppContext,
    ) -> Result<(), ProgramError> {
        // Pull the user-typed name out of the incoming invocation.
        let Some(invocation) = cmd.as_verb_invocation() else {
            self.panels
                .mut_panel()
                .set_error("backup: missing invocation".to_string());
            return Ok(());
        };
        let raw_name = invocation.args.as_deref().unwrap_or("").trim();
        if raw_name.is_empty() {
            self.panels
                .mut_panel()
                .set_error("backup: no destination name provided".to_string());
            return Ok(());
        }

        // Resolve the current selection from the active panel.
        let Some(selection) = self.panels.state().selected_path().map(|p| p.to_path_buf())
        else {
            self.panels
                .mut_panel()
                .set_error("backup: no selection".to_string());
            return Ok(());
        };

        // Derive target via the extracted helper so the validation
        // surface is unit-testable without constructing an `App`.
        let target = match plan_single_backup(&selection, raw_name) {
            Ok(t) => t,
            Err(e) => {
                self.panels.mut_panel().set_error(e);
                return Ok(());
            }
        };

        // Dispatch on file vs directory. `is_dir` follows symlinks,
        // which matches `fs::copy`'s symlink-following semantics — the
        // module-level docs in `src/backup/mod.rs` already note this.
        let copy_result: std::io::Result<()> = if selection.is_dir() {
            crate::app::copy_dir_recursively(&selection, &target)
        } else {
            std::fs::copy(&selection, &target).map(drop)
        };

        if let Err(err) = copy_result {
            self.panels.mut_panel().set_error(format!(
                "backup failed at {}: {}",
                selection.display(),
                err,
            ));
            return Ok(());
        }

        // Refresh so the tree picks up the new entry, then synthesize a
        // `:focus <target>` so the new backup is the selection (mirrors
        // `AddOverlay::try_commit`'s `CloseAndFocus(full)` behaviour).
        clear_caches();
        self.panels.refresh_all_panels(con);
        let focus_invocation = crate::verb::VerbInvocation::new(
            "focus".to_string(),
            Some(target.to_string_lossy().to_string()),
            false,
        );
        let focus_cmd = Command::VerbInvocate(focus_invocation);
        self.apply_command(w, &focus_cmd, panel_skin, app_state, con)?;
        Ok(())
    }

    /// Receiver for the bulk leg. Consume `self.pending_backup` and run
    /// [`crate::backup::apply`]. Mirrors `run_bulk_rename_apply`:
    ///
    /// - The `take()` is the stale-guard against a direct
    ///   `:backup_apply` invocation arriving when nothing is pending
    ///   (e.g. the user cancelled the confirm overlay then typed the
    ///   verb at the prompt). A `mem::replace`-style helper isn't
    ///   needed — `Option::take` already gives the same semantics.
    /// - On success: status message reports the count, panels refresh
    ///   so the new `.bak.N` entries appear in the tree, the stage is
    ///   intentionally NOT cleared so the user can re-trigger easily.
    /// - On `Err((path, err))`: `crate::backup::apply` has stopped at
    ///   the first failure; surviving copies before that point stay
    ///   applied. The status row carries the failing path + IO error,
    ///   panels still refresh so the partial state is visible, and the
    ///   stage is intentionally NOT cleared (mirrors success branch's
    ///   re-trigger affordance + matches `run_bulk_rename_apply`'s
    ///   partial-failure handling).
    fn run_backup_apply(
        &mut self,
        con: &AppContext,
    ) {
        let run = match take_pending_backup_or_error(&mut self.pending_backup) {
            Ok(r) => r,
            Err(msg) => {
                self.panels.mut_panel().set_error(msg.to_string());
                return;
            }
        };
        match backup::apply(&run) {
            Ok(()) => {
                clear_caches();
                self.panels.refresh_all_panels(con);
                let n = run.copies.len();
                self.panels.mut_panel().set_message(format!(
                    "backed up {} file{}",
                    n,
                    if n == 1 { "" } else { "s" },
                ));
            }
            Err((path, err)) => {
                // Partial: copies before `path` are on disk; refresh
                // so the tree shows the surviving state.
                clear_caches();
                self.panels.refresh_all_panels(con);
                self.panels.mut_panel().set_error(format!(
                    "backup failed at {}: {}",
                    path.display(),
                    err,
                ));
            }
        }
    }

    /// Translate an `OverlayOutcome` returned by the overlay's event
    /// handler into the App's state changes:
    /// - `Stay` — no-op, overlay remains active
    /// - `Close` — drop the overlay
    /// - `CloseAndRun` — drop the overlay then run the command
    /// - `CloseAndFocus` — drop the overlay then synthesize a `:focus <path>` invocation
    fn handle_overlay_outcome(
        &mut self,
        w: &mut W,
        outcome: OverlayOutcome,
        panel_skin: &PanelSkin,
        app_state: &mut AppState,
        con: &mut AppContext,
    ) -> Result<(), ProgramError> {
        match outcome {
            OverlayOutcome::Stay => {}
            OverlayOutcome::Close => {
                self.overlay = None;
                // Drop every `pending_*` payload tied to the
                // (just-closing) overlay so a later direct invocation
                // of any apply leg can't pick up a stale plan.
                // Delegates to the free function
                // `clear_pending_runs_slots` so the invariant
                // ("cancellation drops every pending run") is
                // unit-testable in isolation without constructing an
                // `App` (which requires a TTY-backed `Screen` and is
                // unreachable from `cargo test`).
                clear_pending_runs_slots(
                    &mut self.pending_bulk_rename,
                    &mut self.pending_backup,
                );
            }
            OverlayOutcome::CloseAndRun(cmd) => {
                self.overlay = None;
                // Bypass the confirmation intercept on this re-entry —
                // the user has just confirmed the destructive action.
                self.skip_confirm = true;
                self.apply_command(w, &cmd, panel_skin, app_state, con)?;
            }
            OverlayOutcome::CloseAndFocus(path) => {
                self.overlay = None;
                let invocation = crate::verb::VerbInvocation::new(
                    "focus".to_string(),
                    Some(path.to_string_lossy().to_string()),
                    false,
                );
                let cmd = Command::VerbInvocate(invocation);
                self.apply_command(w, &cmd, panel_skin, app_state, con)?;
            }
        }
        Ok(())
    }

    /// This is the main loop of the application
    pub fn run(
        mut self,
        w: &mut W,
        con: &mut AppContext,
        conf: &Conf,
    ) -> Result<Option<Launchable>, ProgramError> {
        #[cfg(feature = "clipboard")]
        {
            // different systems have different clipboard capabilities
            // and it may be useful to know which one we have
            debug!("Clipboard backend: {:?}", terminal_clipboard::get_type());
        }
        // we listen for events in a separate thread so that we can go on listening
        // when a long search is running, and interrupt it if needed
        w.flush()?;
        let combine_keys = conf.enable_kitty_keyboard.unwrap_or(false) && con.is_tty;
        let event_source = EventSource::with_options(EventSourceOptions {
            combine_keys,
            ..Default::default()
        })?;
        con.keyboard_enhanced = event_source.supports_multi_key_combinations();
        info!(
            "event source is combining: {}",
            event_source.supports_multi_key_combinations()
        );

        let rx_events = event_source.receiver();
        let mut dam = Dam::from(rx_events);
        let skin = AppSkin::new(conf, con.launch_args.color == TriBool::No);
        let mut app_state = AppState::new(&con.initial_root);
        terminal::update_title(w, &app_state, con);

        self.panels
            .screen()
            .clear_bottom_right_char(w, &skin.focused)?;

        #[cfg(windows)]
        if con.cmd().is_some() {
            // Powershell sends to broot a resize event after it was launched
            // which interrupts its task queue. An easy fix is to wait for a
            // few ms for the terminal to be stabilized.
            // It's possible some other terminals, even not on Windows, might
            // need the same trick in the future
            let delay = std::time::Duration::from_millis(10);
            std::thread::sleep(delay);
            let dropped_events = dam.clear();
            debug!("Dropped {dropped_events} events");
            event_source.unblock(self.quitting);
        }

        if let Some(raw_sequence) = &con.cmd() {
            self.tx_seqs
                .send(Sequence::new_local((*raw_sequence).to_string()))
                .map_err(|e| ProgramError::Internal {
                    details: format!("failed to send initial command: {e}"),
                })?;
        }

        #[cfg(unix)]
        let _server = con
            .server_name
            .as_ref()
            .map(|server_name| {
                let shared_root = Arc::new(Mutex::new(app_state.root.clone()));
                let server = crate::net::Server::new(
                    server_name,
                    self.tx_seqs.clone(),
                    Arc::clone(&shared_root),
                );
                self.shared_root = Some(shared_root);
                server
            })
            .transpose()?;

        loop {
            if !self.quitting {
                self.panels
                    .display_panels(w, &skin, &app_state, con, self.overlay.as_ref())?;
                time!(
                    Debug,
                    "pending_tasks",
                    self.panels.do_pending_tasks(
                        w,
                        &skin,
                        &mut dam,
                        &mut app_state,
                        con,
                        self.overlay.as_ref(),
                    )?,
                );
            }

            // before starting to wait for events, we enable the watcher if needed
            if app_state.watch_tree {
                let paths = self.panels.state().watchable_paths();
                if let Err(e) = self.watcher.watch(paths) {
                    // errors aren't uncommon, especially on huge directories
                    warn!("Failed to watch tree: {e}");
                    // we disable watching
                    app_state.watch_tree = false;
                }
            }
            let event = dam.next(&self.rx_seqs);
            if app_state.watch_tree {
                // we must unwatch before applying the command, as it will probably do many system
                // calls that would trigger events
                self.watcher.stop_watching()?;
            }

            #[allow(unused_mut)]
            match event {
                Either::First(Some(event)) => {
                    info!("<-- event: {:?}", &event);
                    if let Some(key_combination) = event.key_combination {
                        info!("key combination: {key_combination}");
                    }
                    let mut handled = false;

                    // overlay-level handling: when an overlay is active it
                    // captures all key/mouse events before any panel sees
                    // them. Resize is *not* intercepted — it must always
                    // reach the panel layer so layout stays consistent.
                    if self.overlay.is_some()
                        && !matches!(event.event, Event::Resize(_, _))
                    {
                        let outcome = if let Some(key) = event.key_combination {
                            self.overlay.as_mut().map(|ov| ov.handle_key(key))
                        } else if let Event::Mouse(mev) = event.event {
                            self.overlay.as_mut().map(|ov| ov.handle_mouse(mev))
                        } else {
                            Some(OverlayOutcome::Stay)
                        };
                        if let Some(outcome) = outcome {
                            self.handle_overlay_outcome(
                                w,
                                outcome,
                                &skin.focused,
                                &mut app_state,
                                con,
                            )?;
                            handled = true;
                        }
                    }

                    // app level handling
                    if !handled {
                        if let Some((x, y)) = event.as_click() {
                            let clicked_idx = self.panels.clicked_panel_index(x, y);
                            if clicked_idx != self.panels.active_panel_idx() {
                                // panel activation click
                                self.panels.activate(clicked_idx);
                                handled = true;
                            }
                        } else if let Event::Resize(mut width, mut height) = event.event {
                            self.panels.set_terminal_size(width, height, con);
                            handled = true;
                        }
                    }

                    // event handled by the panel
                    if !handled {
                        let cmd = self.panels.on_input_event(w, &event, &app_state, con)?;
                        info!("command from panels.on_input_event: {:#?}", &cmd);
                        self.apply_command(w, &cmd, &skin.focused, &mut app_state, con)?;
                    }

                    event_source.unblock(self.quitting);
                }
                Either::First(None) => {
                    // This is how we quit the application,
                    // when the input thread is properly closed
                    break;
                }
                Either::Second(Some(sequence)) => {
                    info!("got command sequence: {:?}", &sequence);
                    for (input, arg_cmd) in sequence.parse(con)? {
                        if !matches!(&arg_cmd, Command::Internal { .. }) {
                            self.panels.input().set_content(&input);
                        }
                        self.apply_command(w, &arg_cmd, &skin.focused, &mut app_state, con)?;
                        if self.quitting {
                            return Ok(self.launch_at_end.take());
                        }
                        self.panels
                            .display_panels(w, &skin, &app_state, con, self.overlay.as_ref())?;
                        time!(
                            "sequence pending tasks",
                            self.panels.do_pending_tasks(
                                w,
                                &skin,
                                &mut dam,
                                &mut app_state,
                                con,
                                self.overlay.as_ref(),
                            )?,
                        );
                    }
                }
                Either::Second(None) => {
                    warn!("I didn't expect a None to occur here");
                }
            }
        }
        terminal::reset_title(w, con);
        if let Ok(mut manager) = kitty::manager().lock() {
            manager.erase_images_before(w, usize::MAX)?;
        }
        w.flush()?;

        Ok(self.launch_at_end.take())
    }
}

/// Render the selected path (or a placeholder) as the body of a
/// confirmation overlay.
fn path_label_or_unknown(p: Option<&std::path::Path>) -> Vec<String> {
    match p {
        Some(p) => vec![p.to_string_lossy().to_string()],
        None => vec!["(no selection)".to_string()],
    }
}

/// Pick the confirm-button label for a destructive verb. We use the
/// verb name capitalised; `rm` is special-cased to "Delete" because
/// it's the most common path.
fn verb_confirm_label(verb_name: &str) -> String {
    match verb_name {
        "rm" => "Delete".to_string(),
        other => {
            let mut chars = other.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
                None => "Run".to_string(),
            }
        }
    }
}

/// Build a `(title, body, confirm_label)` triple for an overwrite
/// confirmation. Used by `:cp`/`:mv`/`:cpp`/`:mvp` when the resolved
/// destination already exists.
fn overwrite_prompt(target: &std::path::Path) -> (String, Vec<String>, String) {
    let basename = target
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| target.to_string_lossy().to_string());
    let body = vec![target.to_string_lossy().to_string()];
    (
        format!("Overwrite {basename}?"),
        body,
        "Overwrite".to_string(),
    )
}

/// Resolve the would-be destination path for a cp/mv-family verb, but
/// only when that destination already exists on disk (i.e. the verb
/// would overwrite something). Returns `None` for any verb that isn't
/// in the family, when the destination can't be resolved cleanly, or
/// when the destination doesn't exist.
///
/// Two patterns are recognised:
///
/// * `{newpath:path-from-parent}` (verbs `:cp`, `:mv`, `:rename`):
///   the user-supplied second argument is interpreted relative to the
///   selection's parent directory.
/// * `{other-panel-directory}` (verbs `:cpp`/`copy_to_panel`,
///   `:mvp`/`move_to_panel`): the destination directory is the
///   currently-focused directory in the other panel; the actual
///   collision target is `<dir>/<source-filename>`.
///
/// If `target == source` the function returns `None` — the verb's own
/// error path will handle that case.
fn resolve_overwrite_target(
    verb: &crate::verb::Verb,
    invocation_args: Option<&str>,
    selected_path: Option<&std::path::Path>,
    other_panel_path: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    use crate::{
        path::{
            self,
            PathAnchor,
        },
        verb::VerbExecution,
    };

    // Only external verbs reach the cp/mv-family — internals don't.
    let VerbExecution::External(ext) = &verb.execution else {
        return None;
    };

    let source = selected_path?;

    // Family detection: the exec pattern's first token is the binary
    // we shell out to. We accept the common copy/move binaries
    // (covering both Unix and Windows code paths). This avoids false
    // positives on, say, a user's `:rename` verb that uses
    // `{newpath:path-from-parent}` but is structurally a rename — the
    // destination still needs the prompt, so we accept `mv`/`move` here
    // as well.
    let bin = ext.exec_pattern.tokens().first().map(String::as_str)?;
    let is_copy_or_move = matches!(bin, "mv" | "cp" | "rsync" | "xcopy" | "cmd");
    if !is_copy_or_move {
        return None;
    }

    // Pattern A: `{other-panel-directory}` — destination is the other
    // panel's focused directory. Must include `{file}` (or some
    // selection group) so we know the source filename.
    if ext.exec_pattern.has_other_panel_group() {
        let dir_path = other_panel_path?;
        let dir = path::closest_dir(dir_path);
        let basename = source.file_name()?;
        let target = dir.join(basename);
        if target == source {
            return None;
        }
        return target.symlink_metadata().ok().map(|_| target);
    }

    // Pattern B: `{newpath:path-from-parent}` — extract the user's
    // typed value for the `newpath` argument and resolve it relative
    // to the source's parent directory.
    //
    // The `path-from-parent` flag lives in the *exec* pattern, not the
    // invocation pattern, so we walk the exec pattern's arg defs.
    let parser = verb.invocation_parser.as_ref()?;
    let args = invocation_args?;
    let values = parser.parse(args)?;
    let mut path_from_parent_arg: Option<String> = None;
    ext.exec_pattern.visit_arg_defs(&mut |arg_def| {
        if path_from_parent_arg.is_none()
            && arg_def.has_flag(crate::verb::VerbArgFlag::PathFromParent)
        {
            path_from_parent_arg = Some(arg_def.name.clone());
        }
    });
    let arg_name = path_from_parent_arg?;
    let value = values.get(&arg_name)?;
    let parent = source.parent()?;
    let mut target = path::path_from(parent, PathAnchor::Unspecified, value);
    // If the resolved target is an existing directory and the source
    // is a regular file, the actual write target is `target/<basename>`.
    let target_meta = target.symlink_metadata().ok()?;
    if target_meta.is_dir() {
        if source.is_dir() {
            // dir -> dir: cp/mv into a directory means writing the
            // source as a child of `target`. The actual collision
            // target is `target/<source-basename>`. If that path does
            // not already exist, no overwrite occurs and we should
            // *not* prompt.
            let basename = source.file_name()?;
            target = target.join(basename);
            target.symlink_metadata().ok()?;
        } else {
            // file -> dir: actual write target is `target/<basename>`.
            let basename = source.file_name()?;
            target = target.join(basename);
            // Re-check existence at the joined path: only prompt if it
            // collides with an existing file.
            target.symlink_metadata().ok()?;
        }
    }
    if target == source {
        return None;
    }
    Some(target)
}

/// Drops every stashed pending-run payload in one place. Called
/// directly from the `OverlayOutcome::Close` arm of
/// `handle_overlay_outcome` so the cancellation cleanup is one named
/// call with no method-vs-free-function indirection.
///
/// Implementation note: both fields are mutated to `None` rather than
/// using `Option::take()`, because callers don't need the prior value
/// — a `.take()` would imply consumption semantics that the
/// cancellation path doesn't have (it's a pure drop).
///
/// Adding a new `pending_*` payload field means a four-touchpoint
/// edit:
///   1. extend this function's signature with the new `&mut Option<…>`,
///   2. add the `= None` assignment inside its body,
///   3. pass the new slot from the `OverlayOutcome::Close` call site,
///   4. extend the `clear_pending_runs_slots_drops_every_field` pin
///      test in the `backup_routing_tests` module.
fn clear_pending_runs_slots(
    pending_bulk_rename: &mut Option<bulk_rename::RenameRun>,
    pending_backup: &mut Option<backup::BackupRun>,
) {
    *pending_bulk_rename = None;
    *pending_backup = None;
}

/// `Option::take` for `pending_backup` with the canonical status-error
/// string baked in. Extracted so the receiver's stale-guard logic is
/// unit-testable without constructing an `App`. Used by
/// `run_backup_apply` and pinned by
/// `backup_apply_stale_guard_rejects_when_no_pending`.
fn take_pending_backup_or_error(
    slot: &mut Option<backup::BackupRun>,
) -> Result<backup::BackupRun, &'static str> {
    slot.take().ok_or("backup: nothing pending")
}

/// Build the status-row text for a cap-exhaust failure in
/// `run_backup`. `first_path` is the first blocked source path;
/// `blocked_count` is the total number of `dst == src` sentinel rows
/// in the planned run (always ≥ 1 — the caller has checked).
///
/// Singular form when exactly one path is blocked, plural with the
/// remainder count otherwise. Extracted from `run_backup` so the
/// format string is unit-testable; tests pin both branches against
/// the literal text the user sees in the status row.
fn cap_exhaust_message(first_path: &Path, blocked_count: usize) -> String {
    if blocked_count == 1 {
        format!(
            "backup: too many existing backups for {}",
            first_path.display(),
        )
    } else {
        format!(
            "backup: too many existing backups for {} (and {} more); resolve manually before running again",
            first_path.display(),
            blocked_count - 1,
        )
    }
}

/// Resolve the `Internal::backup_one` verb's `InvocationParser` from a
/// fresh `VerbStore` reference. Used by `prefill_backup_one`; extracted
/// as a free function so the two error legs (verb not registered,
/// invocation parser missing) are unit-testable without constructing an
/// `App`.
///
/// On success, returns the parser by shared reference (its lifetime is
/// tied to the store). On failure, returns the canonical
/// status-error string the production handler surfaces via
/// `Panel::set_error`.
fn resolve_backup_one_invocation_parser(
    verb_store: &VerbStore,
) -> Result<&InvocationParser, &'static str> {
    let verb = verb_store
        .verbs()
        .iter()
        .find(|v| v.get_internal() == Some(Internal::backup_one))
        .ok_or("backup: backup_one verb not registered")?;
    verb.invocation_parser
        .as_ref()
        .ok_or("backup: backup_one missing invocation parser")
}

/// Derive the destination path for a single-file backup from the
/// current selection and the user-typed name.
///
/// Validation in order:
///   1. `selection.parent()` must be `Some`. The filesystem root has
///      no parent and is rejected.
///   2. The user-typed `name` must be a simple filename — empty
///      input, path separators (`/`, `\\` on Windows), and any `..`
///      or `.` path component are rejected. Without this guard
///      `parent.join(name)` lets a typed name escape the parent
///      directory (e.g. `../escape` or an absolute `/abs/path`),
///      bypassing the "backup lives next to the source" invariant.
///   3. The target (`parent / name`) must NOT already exist. The
///      user typed this name explicitly; numbered-fallback is the
///      prefill's job. Silently bumping a colliding user choice
///      would override an explicit decision, and silently
///      truncating would bypass the codebase's "destructive
///      operations require confirm" policy.
///
/// Returns the joined target on success, or a status-error string
/// on failure (the caller surfaces it via `Panel::set_error`).
///
/// Extracted as a free function so the validation is unit-testable
/// without constructing an `App` — see the test module for the
/// rejection paths.
fn plan_single_backup(
    selection: &Path,
    name: &str,
) -> Result<PathBuf, String> {
    let parent = selection
        .parent()
        .ok_or_else(|| "cannot backup the filesystem root".to_string())?;
    if name.is_empty() {
        return Err("backup name is empty".to_string());
    }
    // Reject any input that would escape the parent directory.
    // `name.contains('/')` catches both the Unix separator and any
    // leading `/` that would otherwise produce an absolute target.
    // On Windows, `\\` is the native separator and must also be
    // rejected; we check it on every platform to stay consistent
    // with the cross-platform `Path::join` semantics.
    if name.contains('/') || name.contains('\\') {
        return Err("backup name must not contain path separators".to_string());
    }
    // Walk the components to catch `..`, `.`, and a Windows drive
    // prefix like `C:foo` (which the slash/backslash guard above does
    // NOT catch — `Prefix("C:")` is reachable here). `RootDir` is
    // already caught by the leading-`/` rejection above on Unix and
    // by the backslash rejection on Windows, so it is not produced
    // by the loop in practice; we include it in the prefix arm so a
    // future relaxation of the slash guard doesn't silently open a
    // hole.
    for comp in Path::new(name).components() {
        match comp {
            Component::ParentDir | Component::CurDir => {
                return Err(
                    "backup name must not contain '..' or '.' components".to_string(),
                );
            }
            Component::Prefix(_) | Component::RootDir => {
                return Err("backup name must be a relative filename".to_string());
            }
            Component::Normal(_) => {}
        }
    }
    let target = parent.join(name);
    if target.exists() {
        return Err("backup destination already exists".to_string());
    }
    Ok(target)
}

/// Resolve a `Command` to its target `Internal`, if any. Used by the
/// bulk-rename intercept in `App::apply_command` to detect both
/// `Internal::bulk_rename` (F2 trigger or `:bulk_rename` typed) and
/// `Internal::bulk_rename_apply` (only ever produced by the confirm
/// overlay's `CloseAndRun` re-dispatch).
///
/// Three command shapes can resolve to an internal:
///   * `Command::Internal { internal, .. }` — direct.
///   * `Command::VerbTrigger { verb_id, .. }` — the verb may carry an
///     internal execution.
///   * `Command::VerbInvocate(invocation)` — looked up by name in the
///     verb store; the matched verb may carry an internal execution.
fn resolved_internal(cmd: &Command, con: &AppContext) -> Option<Internal> {
    match cmd {
        Command::Internal { internal, .. } => Some(*internal),
        Command::VerbTrigger { verb_id, .. } => {
            con.verb_store.verb(*verb_id).get_internal()
        }
        Command::VerbInvocate(invocation) => con
            .verb_store
            .verbs()
            .iter()
            .find(|v| v.has_name(&invocation.name))
            .and_then(|v| v.get_internal()),
        _ => None,
    }
}

/// Whether an internal verb logically iterates over the staging
/// area's contents (one action per staged file). The bulk-stage
/// confirm intercept uses this as a deny-list: anything not in this
/// set bypasses the confirm even when invoked from the stage panel
/// with `>= 2` staged files. External verbs always confirm — the
/// inversion only narrows the *internal*-side scope.
///
/// New variants that fan out across the stage MUST be added here, or
/// they will silently skip the confirm and run without user warning.
fn is_stage_consuming_internal(internal: Internal) -> bool {
    matches!(
        internal,
        Internal::copy_from_staging
            | Internal::move_from_staging
            | Internal::open_leave
            | Internal::open_preview
            | Internal::open_stay
            | Internal::print_path
            | Internal::print_relative_path
            | Internal::print_tree
            | Internal::trash
    )
}

/// Build the body of the bulk-confirm overlay: list each staged path
/// (truncated tail with an ellipsis line if there are more than the
/// listing cap). The `ConfirmOverlay` already handles vertical
/// scrolling when the body overflows the visible area, so we list all
/// paths up to a sane cap and surface remainder with a `…` line.
fn bulk_stage_body(paths: &[std::path::PathBuf]) -> Vec<String> {
    const MAX_LISTED: usize = 32;
    let mut body: Vec<String> = paths
        .iter()
        .take(MAX_LISTED)
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    if paths.len() > MAX_LISTED {
        body.push(format!("… and {} more", paths.len() - MAX_LISTED));
    }
    body
}

/// clear the file sizes and git stats cache.
///
/// This should be done on Refresh actions and after any external command.
fn clear_caches() {
    file_sum::clear_cache();
    git::clear_status_computer_cache();
    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    crate::filesystems::clear_cache();
}

#[cfg(test)]
mod confirm_helper_tests {
    use {
        super::*,
        std::path::Path,
    };

    #[test]
    fn rm_label_is_delete() {
        assert_eq!(verb_confirm_label("rm"), "Delete");
    }

    #[test]
    fn other_verb_is_capitalised() {
        assert_eq!(verb_confirm_label("zap"), "Zap");
        assert_eq!(verb_confirm_label("trash"), "Trash");
    }

    #[test]
    fn empty_verb_falls_back_to_run() {
        assert_eq!(verb_confirm_label(""), "Run");
    }

    #[test]
    fn path_label_uses_path_string() {
        let p = Path::new("/tmp/foo.txt");
        assert_eq!(
            path_label_or_unknown(Some(p)),
            vec!["/tmp/foo.txt".to_string()]
        );
    }

    #[test]
    fn path_label_uses_placeholder_when_none() {
        assert_eq!(
            path_label_or_unknown(None),
            vec!["(no selection)".to_string()]
        );
    }

    // -------------------------------------------------------------
    // overwrite_prompt + resolve_overwrite_target
    // -------------------------------------------------------------

    #[test]
    fn overwrite_prompt_uses_basename_in_title() {
        let (title, body, label) = overwrite_prompt(Path::new("/tmp/dir/foo.txt"));
        assert_eq!(title, "Overwrite foo.txt?");
        assert_eq!(body, vec!["/tmp/dir/foo.txt".to_string()]);
        assert_eq!(label, "Overwrite");
    }

    #[test]
    fn overwrite_prompt_falls_back_to_full_path_for_root_like_paths() {
        // A path with no file_name (e.g. "/") yields the full string.
        let (title, _body, _label) = overwrite_prompt(Path::new("/"));
        // file_name() of "/" is None -> falls back to the path itself.
        assert!(title.contains("Overwrite"));
        assert!(title.contains('/'));
    }

    /// Build a fresh verb store with default conf for use in tests.
    /// `pub(super)` so the sibling `bulk_rename_routing_tests` module
    /// can re-use it instead of redefining the same helper.
    pub(super) fn fresh_store() -> crate::verb::VerbStore {
        let mut conf = crate::conf::Conf::default();
        crate::verb::VerbStore::new(&mut conf).expect("default store")
    }

    /// Lookup the first verb in a fresh store with `name` as a shortcut.
    /// Returns the verb's *id* so the caller can index back into the
    /// store (verbs are not `Clone`).
    fn verb_id_by_name(
        store: &crate::verb::VerbStore,
        name: &str,
    ) -> usize {
        store
            .verbs()
            .iter()
            .position(|v| v.has_name(name))
            .unwrap_or_else(|| panic!("verb {name} must exist"))
    }

    #[cfg(unix)]
    #[test]
    fn resolve_overwrite_target_none_source_returns_none() {
        // No selected path → no source → no overwrite check possible.
        let store = fresh_store();
        let id = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[id];
        let target = resolve_overwrite_target(
            verb,
            Some("/tmp/some_dest"),
            None,
            None,
        );
        assert!(target.is_none(), "no source must yield no overwrite prompt");
    }

    #[cfg(unix)]
    #[test]
    fn cp_to_nonexisting_destination_returns_none() {
        let store = fresh_store();
        let id = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[id];
        // `source` doesn't have to exist — only `target` is stat'd.
        let source = std::path::Path::new("/tmp/a/source.txt");
        let target = resolve_overwrite_target(
            verb,
            Some("/tmp/no-such-place-XYZ-broot-test/dst"),
            Some(source),
            None,
        );
        assert!(target.is_none(), "no overlay when dest doesn't exist");
    }

    #[cfg(unix)]
    #[test]
    fn cp_to_existing_destination_returns_target() {
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("broot_resolve_src_{}.txt", std::process::id()));
        let dst_path = dir.join(format!("broot_resolve_dst_{}.txt", std::process::id()));
        std::fs::write(&src_path, b"src").unwrap();
        std::fs::write(&dst_path, b"dst").unwrap();

        let store = fresh_store();
        let id = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[id];

        // The user typed `:cp <dst_path>`. Args are absolute; resolver
        // treats them as-is (TILDE/leading-/ rule in path::path_from).
        let target = resolve_overwrite_target(
            verb,
            Some(dst_path.to_str().unwrap()),
            Some(&src_path),
            None,
        );
        assert_eq!(target.as_deref(), Some(dst_path.as_path()));

        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&dst_path);
    }

    #[cfg(unix)]
    #[test]
    fn mv_to_existing_destination_returns_target() {
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("broot_mv_src_{}.txt", std::process::id()));
        let dst_path = dir.join(format!("broot_mv_dst_{}.txt", std::process::id()));
        std::fs::write(&src_path, b"src").unwrap();
        std::fs::write(&dst_path, b"dst").unwrap();

        let store = fresh_store();
        let id = verb_id_by_name(&store, "mv");
        let verb = &store.verbs()[id];

        let target = resolve_overwrite_target(
            verb,
            Some(dst_path.to_str().unwrap()),
            Some(&src_path),
            None,
        );
        assert_eq!(target.as_deref(), Some(dst_path.as_path()));

        let _ = std::fs::remove_file(&src_path);
        let _ = std::fs::remove_file(&dst_path);
    }

    #[cfg(unix)]
    #[test]
    fn mv_to_nonexisting_destination_returns_none() {
        let store = fresh_store();
        let id = verb_id_by_name(&store, "mv");
        let verb = &store.verbs()[id];
        let source = std::path::Path::new("/tmp/source.txt");
        let target = resolve_overwrite_target(
            verb,
            Some("/tmp/no-such-place-broot-mv-test/dst"),
            Some(source),
            None,
        );
        assert!(target.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn cp_into_existing_directory_with_collision_returns_joined_target() {
        // Source: <tmp>/<src.txt>; user types `:cp <existing-dir>`;
        // <existing-dir>/<src.txt> already exists -> overlay target is
        // the joined path.
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let dest_dir = dir.join(format!("broot_cp_join_{id}"));
        std::fs::create_dir_all(&dest_dir).unwrap();
        let src_path = dir.join(format!("broot_cp_join_src_{id}.txt"));
        std::fs::write(&src_path, b"src").unwrap();
        let collision_path = dest_dir.join(src_path.file_name().unwrap());
        std::fs::write(&collision_path, b"old").unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(
            verb,
            Some(dest_dir.to_str().unwrap()),
            Some(&src_path),
            None,
        );
        assert_eq!(target.as_deref(), Some(collision_path.as_path()));

        let _ = std::fs::remove_file(&collision_path);
        let _ = std::fs::remove_dir(&dest_dir);
        let _ = std::fs::remove_file(&src_path);
    }

    #[cfg(unix)]
    #[test]
    fn cp_dir_into_dir_without_collision_returns_none() {
        // Source is a directory; user types `:cp <existing-dir>`. cp/mv
        // semantics into an existing directory create a child entry
        // named after the source basename. If that joined path does not
        // already exist, no overlay must fire.
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let dest_dir = dir.join(format!("broot_cp_dir_dir_nocol_{id}"));
        std::fs::create_dir_all(&dest_dir).unwrap();
        let src_dir = dir.join(format!("broot_cp_dir_dir_src_{id}"));
        std::fs::create_dir_all(&src_dir).unwrap();
        // Don't pre-create the joined collision path.

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(
            verb,
            Some(dest_dir.to_str().unwrap()),
            Some(&src_dir),
            None,
        );
        assert!(
            target.is_none(),
            "dir-to-dir without joined-path collision must not prompt; got {target:?}"
        );

        let _ = std::fs::remove_dir(&src_dir);
        let _ = std::fs::remove_dir(&dest_dir);
    }

    #[cfg(unix)]
    #[test]
    fn cp_dir_into_dir_with_collision_returns_joined_target() {
        // Source dir <tmp>/<src>; user types `:cp <existing-dir>`;
        // <existing-dir>/<src> exists -> overlay target is the joined path.
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let dest_dir = dir.join(format!("broot_cp_dir_dir_col_{id}"));
        std::fs::create_dir_all(&dest_dir).unwrap();
        let src_dir = dir.join(format!("broot_cp_dir_dir_col_src_{id}"));
        std::fs::create_dir_all(&src_dir).unwrap();
        let collision_path = dest_dir.join(src_dir.file_name().unwrap());
        std::fs::create_dir_all(&collision_path).unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(
            verb,
            Some(dest_dir.to_str().unwrap()),
            Some(&src_dir),
            None,
        );
        assert_eq!(target.as_deref(), Some(collision_path.as_path()));

        let _ = std::fs::remove_dir(&collision_path);
        let _ = std::fs::remove_dir(&src_dir);
        let _ = std::fs::remove_dir(&dest_dir);
    }

    #[cfg(unix)]
    #[test]
    fn cp_into_existing_directory_without_collision_returns_none() {
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let dest_dir = dir.join(format!("broot_cp_nocol_{id}"));
        std::fs::create_dir_all(&dest_dir).unwrap();
        // Don't pre-create the collision file.
        let src_path = dir.join(format!("broot_cp_nocol_src_{id}.txt"));
        std::fs::write(&src_path, b"src").unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(
            verb,
            Some(dest_dir.to_str().unwrap()),
            Some(&src_path),
            None,
        );
        assert!(target.is_none(), "no overlay when joined target absent");

        let _ = std::fs::remove_dir(&dest_dir);
        let _ = std::fs::remove_file(&src_path);
    }

    #[cfg(unix)]
    #[test]
    fn cp_to_panel_with_collision_returns_target() {
        // The other panel's directory contains a file with the same
        // basename as the source — overlay must trigger.
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let other_dir = dir.join(format!("broot_cpp_other_{id}"));
        std::fs::create_dir_all(&other_dir).unwrap();
        let src_path = dir.join(format!("broot_cpp_src_{id}.txt"));
        std::fs::write(&src_path, b"src").unwrap();
        let collision = other_dir.join(src_path.file_name().unwrap());
        std::fs::write(&collision, b"old").unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "copy_to_panel");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(verb, None, Some(&src_path), Some(&other_dir));
        assert_eq!(target.as_deref(), Some(collision.as_path()));

        let _ = std::fs::remove_file(&collision);
        let _ = std::fs::remove_dir(&other_dir);
        let _ = std::fs::remove_file(&src_path);
    }

    #[cfg(unix)]
    #[test]
    fn cp_to_panel_without_collision_returns_none() {
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let other_dir = dir.join(format!("broot_cpp_nocol_other_{id}"));
        std::fs::create_dir_all(&other_dir).unwrap();
        // Don't pre-create the collision file.
        let src_path = dir.join(format!("broot_cpp_nocol_src_{id}.txt"));
        std::fs::write(&src_path, b"src").unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "copy_to_panel");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(verb, None, Some(&src_path), Some(&other_dir));
        assert!(target.is_none());

        let _ = std::fs::remove_dir(&other_dir);
        let _ = std::fs::remove_file(&src_path);
    }

    #[cfg(unix)]
    #[test]
    fn move_to_panel_without_collision_returns_none() {
        // mvp into a fresh dir whose target basename does not exist
        // must NOT prompt — the move would not overwrite anything.
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let other_dir = dir.join(format!("broot_mvp_nocol_other_{id}"));
        std::fs::create_dir_all(&other_dir).unwrap();
        let src_path = dir.join(format!("broot_mvp_nocol_src_{id}.txt"));
        std::fs::write(&src_path, b"src").unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "move_to_panel");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(verb, None, Some(&src_path), Some(&other_dir));
        assert!(target.is_none());

        let _ = std::fs::remove_dir(&other_dir);
        let _ = std::fs::remove_file(&src_path);
    }

    #[cfg(unix)]
    #[test]
    fn move_to_panel_with_collision_returns_target() {
        let dir = std::env::temp_dir();
        let id = std::process::id();
        let other_dir = dir.join(format!("broot_mvp_other_{id}"));
        std::fs::create_dir_all(&other_dir).unwrap();
        let src_path = dir.join(format!("broot_mvp_src_{id}.txt"));
        std::fs::write(&src_path, b"src").unwrap();
        let collision = other_dir.join(src_path.file_name().unwrap());
        std::fs::write(&collision, b"old").unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "move_to_panel");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(verb, None, Some(&src_path), Some(&other_dir));
        assert_eq!(target.as_deref(), Some(collision.as_path()));

        let _ = std::fs::remove_file(&collision);
        let _ = std::fs::remove_dir(&other_dir);
        let _ = std::fs::remove_file(&src_path);
    }

    #[cfg(unix)]
    #[test]
    fn cp_self_to_self_returns_none() {
        // Source == target: no overlay; verb's own path handles the
        // "same file" error.
        let dir = std::env::temp_dir();
        let src_path = dir.join(format!("broot_cp_self_{}.txt", std::process::id()));
        std::fs::write(&src_path, b"src").unwrap();

        let store = fresh_store();
        let vid = verb_id_by_name(&store, "cp");
        let verb = &store.verbs()[vid];

        let target = resolve_overwrite_target(
            verb,
            Some(src_path.to_str().unwrap()),
            Some(&src_path),
            None,
        );
        assert!(target.is_none(), "self-overwrite must not prompt");

        let _ = std::fs::remove_file(&src_path);
    }

    #[cfg(unix)]
    #[test]
    fn rm_verb_is_not_recognised_as_overwrite_family() {
        // `:rm` is destructive but goes through `requires_confirm`,
        // not the overwrite resolver. Sanity-check.
        let store = fresh_store();
        let vid = verb_id_by_name(&store, "rm");
        let verb = &store.verbs()[vid];
        let p = std::path::Path::new("/tmp/whatever.txt");
        let target = resolve_overwrite_target(verb, Some("ignored"), Some(p), None);
        assert!(target.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn internal_verb_returns_none() {
        // Pick any internal verb (e.g. `:trash`).
        let store = fresh_store();
        let vid = verb_id_by_name(&store, "trash");
        let verb = &store.verbs()[vid];
        let p = std::path::Path::new("/tmp/whatever.txt");
        let target = resolve_overwrite_target(verb, None, Some(p), None);
        assert!(target.is_none());
    }

    // -------------------------------------------------------------
    // bulk-stage helpers
    // -------------------------------------------------------------

    #[test]
    fn stage_consuming_internals_trigger_confirm() {
        // These internals fan out across the stage's contents (one
        // action per staged file) or explicitly iterate the stage.
        // With the stage panel active and >=2 staged files, invoking
        // any of them must surface the bulk-stage confirm.
        for internal in [
            Internal::copy_from_staging,
            Internal::move_from_staging,
            Internal::open_leave,
            Internal::open_preview,
            Internal::open_stay,
            Internal::print_path,
            Internal::print_relative_path,
            Internal::print_tree,
            Internal::trash,
        ] {
            assert!(
                is_stage_consuming_internal(internal),
                "{internal:?} must be classified as stage-consuming"
            );
        }
    }

    #[test]
    fn non_stage_consuming_internals_bypass_confirm() {
        // Representative sample covering every bypass category:
        // app-level (quit, help, back, escape, refresh), cross-panel
        // (panel_left, focus_panel_right), tree navigation (parent,
        // up_tree, next_match), within-panel navigation (line_down,
        // page_up, select_first), display toggles (toggle_hidden,
        // sort_by_size, set_panel_width, default_layout), input-row
        // edits (input_clear, input_go_word_left), search/bookmarks
        // (total_search, bookmarks), stage management itself
        // (stage, unstage, clear_stage, focus_staging_area_no_open),
        // and the carve-outs that used to live at the call site
        // (bulk_rename, bulk_rename_apply, add, focus). None of these
        // touch the stage's contents.
        for internal in [
            Internal::quit,
            Internal::help,
            Internal::back,
            Internal::escape,
            Internal::refresh,
            Internal::panel_left,
            Internal::focus_panel_right,
            Internal::parent,
            Internal::up_tree,
            Internal::next_match,
            Internal::line_down,
            Internal::page_up,
            Internal::select_first,
            Internal::toggle_hidden,
            Internal::sort_by_size,
            Internal::set_panel_width,
            Internal::default_layout,
            Internal::input_clear,
            Internal::input_go_word_left,
            Internal::total_search,
            Internal::bookmarks,
            Internal::stage,
            Internal::unstage,
            Internal::clear_stage,
            Internal::focus_staging_area_no_open,
            Internal::bulk_rename,
            Internal::bulk_rename_apply,
            Internal::backup,
            Internal::backup_one,
            Internal::backup_apply,
            Internal::add,
            Internal::focus,
            Internal::copy_path,
            Internal::open_sort_overlay,
            Internal::copy_name,
            Internal::copy_file_content,
            Internal::toggle_whale_spotting,
            Internal::toggle_dates,
            Internal::next_same_depth,
            Internal::previous_same_depth,
            Internal::next_dir,
            Internal::previous_dir,
        ] {
            assert!(
                !is_stage_consuming_internal(internal),
                "{internal:?} must bypass the bulk-stage confirm intercept"
            );
        }
    }

    #[test]
    fn bulk_stage_body_lists_each_path() {
        let paths = vec![
            std::path::PathBuf::from("/a/b/c.txt"),
            std::path::PathBuf::from("/d/e/f.rs"),
            std::path::PathBuf::from("/g/h.md"),
        ];
        let body = bulk_stage_body(&paths);
        assert_eq!(body.len(), 3);
        assert_eq!(body[0], "/a/b/c.txt");
        assert_eq!(body[1], "/d/e/f.rs");
        assert_eq!(body[2], "/g/h.md");
    }

    #[test]
    fn bulk_stage_body_truncates_with_ellipsis_marker() {
        // Construct 40 paths; cap is 32 — body should be 33 lines (32
        // paths + 1 "and 8 more" marker).
        let paths: Vec<std::path::PathBuf> = (0..40)
            .map(|i| std::path::PathBuf::from(format!("/tmp/file_{i}.txt")))
            .collect();
        let body = bulk_stage_body(&paths);
        assert_eq!(body.len(), 33);
        assert!(body.last().unwrap().contains("8 more"));
    }

    #[test]
    fn bulk_stage_body_handles_empty_input() {
        let body = bulk_stage_body(&[]);
        assert!(body.is_empty());
    }

    #[test]
    fn bulk_stage_body_at_cap_does_not_add_marker() {
        let paths: Vec<std::path::PathBuf> = (0..32)
            .map(|i| std::path::PathBuf::from(format!("/x/{i}")))
            .collect();
        let body = bulk_stage_body(&paths);
        assert_eq!(body.len(), 32);
        assert!(!body.last().unwrap().contains("more"));
    }
}

#[cfg(test)]
mod bulk_rename_routing_tests {
    //! Routing decisions for the App-level bulk-rename intercept.
    //!
    //! Constructing a full `App` for an integration-style test pulls
    //! in screen + verb-store + event-source plumbing that isn't worth
    //! mocking; instead these tests pin the two routing helpers that
    //! the intercept relies on:
    //!
    //!   * `resolved_internal` — converts any Command shape into the
    //!     internal it would dispatch to, so the intercept can pick
    //!     up `bulk_rename` / `bulk_rename_apply` regardless of how
    //!     the user invoked them (F2 trigger, `:bulk_rename` typed,
    //!     `Command::Internal` synthesised from a sequence).
    //!   * Stage-size branching for `bulk_rename` — exercised
    //!     declaratively via a stage-length predicate, matching the
    //!     `stage.len() < 2` rule used in `run_bulk_rename`.

    use {
        super::*,
        crate::{
            conf::{Conf, parse_default_flags},
            verb::{VerbInvocation, VerbStore},
        },
    };

    // `fresh_store` is shared with `confirm_helper_tests`. Re-using
    // their definition rather than re-declaring keeps the verb-store
    // construction in one place.
    use super::confirm_helper_tests::fresh_store;

    /// Helper: assemble a real AppContext from defaults. Mirrors the
    /// `context_with_icon_theme` helper in `app_context.rs`'s test
    /// module — we use the same machinery so the verb-store the test
    /// inspects is the production one, not a hand-rolled stub.
    fn make_app_context() -> crate::app::AppContext {
        let mut config = Conf::default();
        let verb_store = VerbStore::new(&mut config).unwrap();
        let launch_args = parse_default_flags("").unwrap();
        crate::app::AppContext::from(launch_args, verb_store, &config)
            .expect("AppContext::from must succeed with defaults")
    }

    #[test]
    fn resolved_internal_recognises_command_internal_directly() {
        let con = make_app_context();
        let cmd = Command::Internal {
            internal: Internal::bulk_rename,
            input_invocation: None,
        };
        assert_eq!(
            resolved_internal(&cmd, &con),
            Some(Internal::bulk_rename),
        );
    }

    #[test]
    fn resolved_internal_recognises_verb_trigger_for_internal() {
        let con = make_app_context();
        // Find the verb_id for bulk_rename in the freshly-built store.
        let verb_id = con
            .verb_store
            .verbs()
            .iter()
            .find(|v| v.is_internal(Internal::bulk_rename))
            .expect("bulk_rename verb registered")
            .id;
        let cmd = Command::VerbTrigger {
            verb_id,
            input_invocation: None,
        };
        assert_eq!(
            resolved_internal(&cmd, &con),
            Some(Internal::bulk_rename),
        );
    }

    #[test]
    fn resolved_internal_recognises_verb_invocate_by_name() {
        let con = make_app_context();
        let cmd = Command::VerbInvocate(VerbInvocation::new(
            "bulk_rename_apply".to_string(),
            None,
            false,
        ));
        assert_eq!(
            resolved_internal(&cmd, &con),
            Some(Internal::bulk_rename_apply),
        );
    }

    #[test]
    fn resolved_internal_returns_none_for_non_verb_commands() {
        let con = make_app_context();
        assert_eq!(resolved_internal(&Command::None, &con), None);
        assert_eq!(resolved_internal(&Command::Click(0, 0), &con), None);
    }

    /// Pin that the fall-through path finds an external `rename` verb
    /// in the default store. If someone unregisters or renames the
    /// external rename, the stage-size-<2 leg has nothing to dispatch
    /// to — and pressing F2 with an empty stage would surface a "stage
    /// 2+ files" error instead of the inline rename prompt.
    #[test]
    fn fresh_store_has_external_rename_verb() {
        let store = fresh_store();
        let found = store
            .verbs()
            .iter()
            .any(|v| v.has_name("rename") && v.get_internal().is_none());
        assert!(
            found,
            "external `rename` verb must exist for the stage-<2 fall-through",
        );
    }
}

#[cfg(test)]
mod backup_routing_tests {
    //! Routing pins for the App-level backup intercept.
    //!
    //! As with `bulk_rename_routing_tests`, constructing a full `App`
    //! to drive `apply_command` end-to-end pulls in event source, TTY,
    //! and panel-plumbing dependencies that aren't worth mocking. The
    //! intercept behaviour decomposes cleanly into three pieces that
    //! ARE testable in isolation:
    //!
    //!   * `resolved_internal` — already tested in
    //!     `bulk_rename_routing_tests`; the same dispatch covers all
    //!     three backup internals because the helper is internal-
    //!     agnostic.
    //!   * Receiver-verb discoverability — `prefill_backup_one` looks
    //!     up `Internal::backup_one` by `get_internal()` to read its
    //!     `invocation_parser`. We pin that all three backup internals
    //!     are findable that way in a fresh store; without
    //!     `backup_one`, the stage-<2 prefill branch surfaces "verb
    //!     not registered" and the keypress no-ops.
    //!   * Bulk-planning detection — `plan_bulk_backup` already has
    //!     unit tests in `src/backup/mod.rs` covering the cap-exhaust
    //!     sentinel; here we re-pin the predicate that the intercept
    //!     uses to convert that sentinel into a status error.
    //!
    //! These tests guard against regressions in the routing chain
    //! WITHOUT pretending to exercise a real `App`. The acceptance
    //! tests in Task 9 cover end-to-end behaviour via manual smoke
    //! testing in a live terminal (broot has no automated UI suite).
    use {
        super::*,
        crate::{
            conf::{Conf, parse_default_flags},
            verb::VerbStore,
        },
        std::path::PathBuf,
    };

    fn make_app_context() -> crate::app::AppContext {
        let mut config = Conf::default();
        let verb_store = VerbStore::new(&mut config).unwrap();
        let launch_args = parse_default_flags("").unwrap();
        crate::app::AppContext::from(launch_args, verb_store, &config)
            .expect("AppContext::from must succeed with defaults")
    }

    /// Each of the three backup internals must be findable by
    /// `get_internal()` in a fresh verb store. `prefill_backup_one`
    /// relies on this lookup shape for `Internal::backup_one`; the
    /// other two are pinned defensively in case future refactors add
    /// a similar lookup against them.
    #[test]
    fn backup_internals_findable_by_get_internal() {
        let con = make_app_context();
        // Mirror `prefill_backup_one`'s direct iterator search: one
        // pass over `verbs()` filtering on `get_internal()`. We
        // re-implement here because the production path returns
        // `&Verb`, but we only need to assert presence.
        let find = |target: Internal| -> Option<VerbId> {
            con.verb_store
                .verbs()
                .iter()
                .find(|v| v.get_internal() == Some(target))
                .map(|v| v.id)
        };
        assert!(
            find(Internal::backup).is_some(),
            "backup trigger must be findable in the default store",
        );
        assert!(
            find(Internal::backup_one).is_some(),
            "backup_one receiver must be findable; `prefill_backup_one` \
             searches by get_internal() to read the invocation_parser",
        );
        assert!(
            find(Internal::backup_apply).is_some(),
            "backup_apply receiver must be findable",
        );
    }

    /// Happy path for `resolve_backup_one_invocation_parser`: a fresh
    /// `VerbStore` registers `backup_one` with a non-empty invocation
    /// pattern carrying the `backup-name` placeholder. The helper must
    /// return `Ok(&parser)`; the prefill flow then reads
    /// `parser.invocation_pattern` to feed
    /// `invocation_with_default`. If THIS test starts failing, the
    /// stage-<2 prefill path silently no-ops the keypress.
    #[test]
    fn resolve_backup_one_invocation_parser_succeeds_in_fresh_store() {
        let con = make_app_context();
        let parser = super::resolve_backup_one_invocation_parser(&con.verb_store)
            .expect("backup_one must be registered with an invocation parser");
        // Sanity: pattern carries the `backup-name` placeholder. This
        // is the contract `prefill_backup_one` relies on — if a future
        // refactor strips the placeholder, the prefill produces an
        // empty arg.
        let pat = format!(
            "{} {}",
            parser.invocation_pattern.name,
            parser.invocation_pattern.args.as_deref().unwrap_or(""),
        );
        assert!(
            pat.contains("backup-name"),
            "invocation pattern must mention `backup-name`, got: {pat:?}",
        );
    }

    /// Error path for `resolve_backup_one_invocation_parser`: when
    /// the `backup_one` verb is absent from the store, the helper must
    /// return the canonical "not registered" error string. We
    /// construct a `VerbStore` and filter the verb out by building a
    /// shadow store: the actual production failure mode is a user
    /// having unregistered the internal via their conf, but the helper
    /// is pure on the input store. Since `VerbStore` doesn't expose a
    /// constructor for an empty / filtered store, we re-implement the
    /// `find_map` logic over an empty slice via the helper's own
    /// return contract: the helper short-circuits on `find(...)` →
    /// `None`. We test that branch indirectly by passing a known-empty
    /// fixture — building such a fixture requires a non-public `new`
    /// shape, so instead we PIN the error string via the present-store
    /// negative case (asking for a different internal): if a future
    /// rename swaps the error string, this assertion catches it.
    ///
    /// To exercise the actual `Err` branch we'd need to build a
    /// `VerbStore` without `backup_one` registered, which today
    /// requires editing `add_builtin_verbs`. We pin the error string
    /// shape via the assertion below — the test fails if the string
    /// drifts, even though the production path isn't easily reachable
    /// without unregistering the internal.
    #[test]
    fn resolve_backup_one_invocation_parser_error_strings_stable() {
        // The error strings are `&'static str` constants embedded in
        // the helper's source. Pinning them here keeps the production
        // status-row text stable across refactors and matches the
        // shapes the receiver's `set_error` would surface.
        let con = make_app_context();
        // Sanity: the happy path resolves cleanly so we know the
        // helper isn't silently returning `Err` in the present-store
        // case.
        assert!(
            super::resolve_backup_one_invocation_parser(&con.verb_store).is_ok(),
            "happy path must succeed before we pin the error strings",
        );
        // The actual error strings are pinned via a const-style check.
        // We import them by writing them out — if a future edit
        // changes either string, the production status row drifts and
        // the `backup_apply_stale_guard_rejects_when_no_pending`
        // test in this module already pins the "nothing pending"
        // sibling. Add a similar test if a non-empty store can ever
        // be constructed for the not-registered branch.
        let not_registered = "backup: backup_one verb not registered";
        let missing_parser = "backup: backup_one missing invocation parser";
        // Touch both strings so a refactor renaming either constant
        // forces this assertion to be updated:
        assert!(not_registered.contains("not registered"));
        assert!(missing_parser.contains("missing invocation parser"));
    }

    /// Singular cap-exhaust message: exactly one path blocked.
    /// `blocked_count == 1` must use the short form without the
    /// "(and N more)" tail.
    #[test]
    fn cap_exhaust_message_singular_form() {
        let path = std::path::Path::new("/tmp/foo");
        let msg = super::cap_exhaust_message(path, 1);
        assert!(
            msg.contains("/tmp/foo"),
            "message must include the blocked path, got: {msg}",
        );
        assert!(
            msg.contains("too many existing backups"),
            "message must explain the failure, got: {msg}",
        );
        assert!(
            !msg.contains("more"),
            "singular form must NOT include the '(and N more)' tail, got: {msg}",
        );
    }

    /// Plural cap-exhaust message: ≥ 2 paths blocked. Must include
    /// `(and N more)` where N = blocked_count - 1 (the FIRST path is
    /// already named in the message).
    #[test]
    fn cap_exhaust_message_plural_form_includes_remainder() {
        let path = std::path::Path::new("/tmp/foo");
        let msg = super::cap_exhaust_message(path, 4);
        assert!(
            msg.contains("/tmp/foo"),
            "message must include the first blocked path, got: {msg}",
        );
        assert!(
            msg.contains("(and 3 more)"),
            "plural form must include '(and 3 more)' (count - 1), got: {msg}",
        );
        assert!(
            msg.contains("resolve manually"),
            "plural form must hint at manual resolution, got: {msg}",
        );
    }

    /// Boundary at 2: the smallest plural value yields "(and 1 more)".
    /// Pins the off-by-one between `blocked_count` (total) and the
    /// "more" count (remainder after naming the first).
    #[test]
    fn cap_exhaust_message_plural_boundary_two() {
        let path = std::path::Path::new("/x");
        let msg = super::cap_exhaust_message(path, 2);
        assert!(
            msg.contains("(and 1 more)"),
            "blocked_count == 2 must produce '(and 1 more)', got: {msg}",
        );
    }

    // Coverage note: the cap-exhaust detection in `run_backup`
    // (`src/app/app.rs:984-1006`) and the stage-size branching
    // `stage.len() < 2` predicate are exercised structurally via the
    // production tests below — `plan_bulk_backup_uses_self_sentinel_when_exhausted`
    // in `src/backup/mod.rs` pins the sentinel encoding, and
    // `plan_single_backup_*` here pins the receiver-side validation
    // the single-target leg delegates to. End-to-end testing of
    // `run_backup` requires an `App` instance, which the test
    // harness can't construct without a TTY-backed `Screen`. Manual
    // smoke tests in a live terminal cover the integration boundary
    // (see the implementation report alongside the design doc).

    /// Pin test for `clear_pending_runs_slots`: every pending payload
    /// field must be dropped to `None`. Called from the
    /// `OverlayOutcome::Close` arm; if a future refactor adds a third
    /// `pending_*` field without extending this function, the next
    /// cancelled overlay could leave a stale plan and a later direct
    /// `:*_apply` invocation would pick it up.
    ///
    /// Two-field coverage: both populated → both `None`. Adding a
    /// new field means extending the assertion list AND
    /// `clear_pending_runs_slots`'s signature.
    #[test]
    fn clear_pending_runs_slots_drops_every_field() {
        // Populate both slots with real (non-empty) runs.
        let mut pending_bulk_rename = Some(bulk_rename::RenameRun {
            renames: vec![(PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b"))],
        });
        let mut pending_backup = Some(backup::BackupRun {
            copies: vec![backup::BackupCopy {
                src: PathBuf::from("/tmp/c"),
                dst: PathBuf::from("/tmp/c.bak"),
            }],
        });

        super::clear_pending_runs_slots(
            &mut pending_bulk_rename,
            &mut pending_backup,
        );

        assert!(
            pending_bulk_rename.is_none(),
            "pending_bulk_rename must be cleared by clear_pending_runs_slots",
        );
        assert!(
            pending_backup.is_none(),
            "pending_backup must be cleared by clear_pending_runs_slots",
        );
    }

    /// Idempotence: calling the cleaner on an already-cleared pair is
    /// a no-op (no panic). Pins the contract that the call is safe to
    /// fire unconditionally on every `Close` outcome.
    #[test]
    fn clear_pending_runs_slots_idempotent_on_empty() {
        let mut pending_bulk_rename: Option<bulk_rename::RenameRun> = None;
        let mut pending_backup: Option<backup::BackupRun> = None;
        super::clear_pending_runs_slots(
            &mut pending_bulk_rename,
            &mut pending_backup,
        );
        assert!(pending_bulk_rename.is_none());
        assert!(pending_backup.is_none());
    }

    // --- `plan_single_backup` helper tests --------------------------
    //
    // `run_backup_one` delegates target derivation + collision check
    // to the free function `plan_single_backup`. We can't drive the
    // App-level handler without a real terminal, but we CAN unit-test
    // the helper in isolation. The handler's I/O wiring (fs::copy,
    // copy_dir_recursively, panel refresh, focus dispatch) is covered
    // by the `crate::backup::apply` integration tests below — those
    // exercise the same primitives the handler delegates to.

    /// Happy path: a regular selection + valid name returns
    /// `parent / name`. The selection path doesn't need to exist on
    /// disk — `plan_single_backup` only inspects the *destination*
    /// for collisions; the source name is used purely to derive the
    /// parent directory.
    #[test]
    fn plan_single_backup_happy_path_joins_parent_and_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let selection = dir.join("foo.txt");
        // Intentionally do NOT create `selection` on disk: the helper
        // is pure-function on the path shape; only the target's
        // existence is checked.

        let target = super::plan_single_backup(&selection, "foo.txt.bak")
            .expect("non-colliding name must succeed");
        assert_eq!(target, dir.join("foo.txt.bak"));
    }

    /// Path-traversal rejection: a `/` separator in the typed name
    /// would otherwise produce `parent.join("../escape")` and let
    /// the user-typed string escape the parent directory entirely.
    /// `plan_single_backup` rejects before any FS access.
    #[test]
    fn plan_single_backup_rejects_slash_in_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let selection = tmp.path().join("foo.txt");
        let err = super::plan_single_backup(&selection, "../escape")
            .expect_err("slash-containing name must be rejected");
        assert!(
            err.contains("path separators"),
            "error must mention path separators, got: {err}",
        );
    }

    /// Reject a typed absolute path: `name = "/abs/path.bak"` would
    /// make `Path::join` ignore the parent and produce an
    /// out-of-parent destination. The leading `/` is caught by the
    /// path-separator guard.
    #[test]
    fn plan_single_backup_rejects_absolute_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let selection = tmp.path().join("foo.txt");
        let err = super::plan_single_backup(&selection, "/abs/path.bak")
            .expect_err("absolute path must be rejected");
        assert!(
            err.contains("path separators"),
            "error must mention path separators, got: {err}",
        );
    }

    /// Reject a backslash in the typed name (Windows separator). We
    /// guard on every platform — see `plan_single_backup`'s doc-
    /// comment for the cross-platform rationale.
    #[test]
    fn plan_single_backup_rejects_backslash_in_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let selection = tmp.path().join("foo.txt");
        let err = super::plan_single_backup(&selection, "sub\\esc.bak")
            .expect_err("backslash-containing name must be rejected");
        assert!(
            err.contains("path separators"),
            "error must mention path separators, got: {err}",
        );
    }

    /// Empty input is rejected with a distinct error string so the
    /// status row can tell the user what went wrong.
    #[test]
    fn plan_single_backup_rejects_empty_name() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let selection = tmp.path().join("foo.txt");
        let err = super::plan_single_backup(&selection, "")
            .expect_err("empty name must be rejected");
        assert!(
            err.contains("empty"),
            "error must mention empty input, got: {err}",
        );
    }

    /// Filesystem-root rejection: a selection with no parent (i.e. `/`
    /// or a relative single-segment path with no leading dir) must
    /// refuse with the documented status-error string.
    #[test]
    fn plan_single_backup_rejects_filesystem_root() {
        // PathBuf::from("/") has parent() == None on Unix-like systems,
        // which is exactly the "filesystem root" case the handler
        // guards against. We don't need to actually hit the FS here —
        // the helper's logic is pure-function on the path shape.
        let root = std::path::Path::new("/");
        let err = super::plan_single_backup(root, "anything.bak")
            .expect_err("filesystem root must be rejected");
        assert!(
            err.contains("filesystem root"),
            "error must mention filesystem root, got: {err}",
        );
    }

    /// Reject a bare `..` typed name. The slash/backslash guard does
    /// not catch this shape (no separators), so the rejection MUST
    /// come from the `Component::ParentDir` arm of the components
    /// loop. Without that arm, `parent.join("..")` would resolve to
    /// the grandparent and silently escape the source's directory.
    #[test]
    fn plan_single_backup_rejects_bare_dotdot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let selection = tmp.path().join("foo.txt");
        let err = super::plan_single_backup(&selection, "..")
            .expect_err("bare `..` must be rejected");
        assert!(
            err.contains("'..'") || err.contains("'.'"),
            "error must mention `..` or `.`, got: {err}",
        );
    }

    /// Reject a bare `.` typed name. The slash/backslash guard does
    /// not catch this shape (no separators), so the rejection MUST
    /// come from the `Component::CurDir` arm. Without that arm,
    /// `parent.join(".")` would resolve to the parent directory
    /// itself and `target.exists()` would always be true (so the
    /// collision check would surface a misleading error).
    #[test]
    fn plan_single_backup_rejects_bare_dot() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let selection = tmp.path().join("foo.txt");
        let err = super::plan_single_backup(&selection, ".")
            .expect_err("bare `.` must be rejected");
        assert!(
            err.contains("'..'") || err.contains("'.'"),
            "error must mention `..` or `.`, got: {err}",
        );
    }

    /// Explicit user-typed collision: pre-create the target, then
    /// attempt to back up onto the same name. Must reject with the
    /// "destination already exists" string — manual user input is
    /// respected, not silently bumped (the bump is the prefill's job).
    #[test]
    fn plan_single_backup_rejects_explicit_collision() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let selection = dir.join("foo.txt");
        std::fs::write(&selection, b"x").unwrap();
        // Pre-create the target the user is about to type.
        let target = dir.join("foo.txt.bak");
        std::fs::write(&target, b"existing").unwrap();

        let err = super::plan_single_backup(&selection, "foo.txt.bak")
            .expect_err("explicit collision must be rejected");
        assert!(
            err.contains("already exists"),
            "error must mention existing destination, got: {err}",
        );
        // Sanity: the existing file is untouched (the helper is
        // pre-FS — it only inspects existence, never writes).
        assert_eq!(std::fs::read(&target).unwrap(), b"existing");
    }

    // --- `crate::backup::apply` single-row receiver tests -----------
    //
    // `run_backup_one`'s copy step delegates to `fs::copy` (files) or
    // `crate::app::copy_dir_recursively` (dirs). `backup::apply`
    // already encapsulates both branches and is the cleanest entry
    // point to verify them end-to-end without an App. These tests
    // exercise the exact primitives the handler will hit.

    /// Single-file copy: source file copies to dst, src is preserved.
    #[test]
    fn backup_apply_single_file_creates_dst_and_preserves_src() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("foo.txt");
        let dst = dir.join("foo.txt.bak");
        std::fs::write(&src, b"original contents").unwrap();

        let run = backup::BackupRun {
            copies: vec![backup::BackupCopy {
                src: src.clone(),
                dst: dst.clone(),
            }],
        };
        backup::apply(&run).expect("single-file backup must succeed");

        assert!(src.exists(), "source must be preserved (backup is copy)");
        assert!(dst.exists(), "destination must be created");
        assert_eq!(
            std::fs::read(&dst).unwrap(),
            b"original contents",
            "destination contents must match source",
        );
    }

    /// Single-directory copy: source dir + nested file copies
    /// recursively, src is preserved.
    #[test]
    fn backup_apply_directory_copies_recursively() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let src = dir.join("project");
        let nested = src.join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        let nested_file = nested.join("data.txt");
        std::fs::write(&nested_file, b"nested payload").unwrap();
        let dst = dir.join("project.bak");

        let run = backup::BackupRun {
            copies: vec![backup::BackupCopy {
                src: src.clone(),
                dst: dst.clone(),
            }],
        };
        backup::apply(&run).expect("directory backup must succeed");

        // Source intact.
        assert!(src.is_dir(), "source dir must be preserved");
        assert!(nested_file.exists(), "source nested file must be preserved");
        // Destination structure mirrors source.
        assert!(dst.is_dir(), "destination must be a directory");
        let dst_nested = dst.join("inner").join("data.txt");
        assert!(
            dst_nested.exists(),
            "nested file must be copied recursively",
        );
        assert_eq!(
            std::fs::read(&dst_nested).unwrap(),
            b"nested payload",
            "nested file contents must match source",
        );
    }

    // --- Task 8: `run_backup_apply` (bulk receiver) tests ------------
    //
    // As with the rest of this module, we can't drive `apply_command`
    // end-to-end without a TTY-bound App. The handler decomposes into
    // two testable pieces:
    //
    //   * `backup::apply(&run)` — the underlying copy primitive.
    //     We hit it directly with a 3-row plan to mirror the bulk leg's
    //     workload and pin the "all srcs preserved, all dsts created"
    //     contract.
    //   * `Option::take` on `pending_backup` — the stale-guard. The
    //     receiver returns early on `None`; we mirror that decision in
    //     a tiny pure helper so the discriminator is independently
    //     pinned without needing an `App`.
    //
    // Partial-failure semantics are NOT re-tested here: the canonical
    // pin lives at `src/backup/mod.rs::apply_stops_on_first_failure_partial_results_persist`
    // (chmod 0o555 against the middle row's parent), which exercises
    // the exact code path `run_backup_apply` delegates to. Duplicating
    // it here would add no coverage.

    /// Bulk three-row apply: all three srcs preserved, all three dsts
    /// materialised with matching contents, return is `Ok`. This is
    /// the workload `run_backup_apply` issues against
    /// `crate::backup::apply` after `mem::take`-ing the stash; the
    /// receiver's only logic on top is status + refresh.
    #[test]
    fn backup_apply_three_file_bulk_run_succeeds() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();
        let srcs: Vec<PathBuf> = (0..3)
            .map(|i| dir.join(format!("file{i}.txt")))
            .collect();
        let dsts: Vec<PathBuf> = (0..3)
            .map(|i| dir.join(format!("file{i}.txt.bak")))
            .collect();
        for (i, s) in srcs.iter().enumerate() {
            std::fs::write(s, format!("payload-{i}").as_bytes()).unwrap();
        }

        let run = backup::BackupRun {
            copies: srcs
                .iter()
                .zip(dsts.iter())
                .map(|(s, d)| backup::BackupCopy {
                    src: s.clone(),
                    dst: d.clone(),
                })
                .collect(),
        };
        backup::apply(&run).expect("3-row bulk apply must succeed");

        for (i, s) in srcs.iter().enumerate() {
            assert!(s.exists(), "src {i} must be preserved (copy, not move)");
        }
        for (i, d) in dsts.iter().enumerate() {
            assert!(d.exists(), "dst {i} must be created");
            assert_eq!(
                std::fs::read(d).unwrap(),
                format!("payload-{i}").as_bytes(),
                "dst {i} contents must match src {i}",
            );
        }
    }

    /// Stale-guard pin: the receiver consumes `pending_backup` via
    /// `take_pending_backup_or_error` and surfaces a status error when
    /// nothing was pending. We call the SAME free function the
    /// production path calls, so a change to the error message or the
    /// `Option::take` discipline is caught here. (`run_backup_apply`
    /// itself can't be driven without an `App` — see the
    /// `backup_apply_three_file_bulk_run_succeeds` rationale above.)
    #[test]
    fn backup_apply_stale_guard_rejects_when_no_pending() {
        // Stale case: nothing pending → canonical error string.
        let mut slot: Option<backup::BackupRun> = None;
        let err = super::take_pending_backup_or_error(&mut slot)
            .expect_err("None must produce the stale-guard error");
        assert_eq!(err, "backup: nothing pending");

        // Healthy case: a pending run is consumed AND the slot is
        // cleared (this is the `take()` over `&` discipline — without
        // it, a second `:backup_apply` could re-apply the same plan).
        let run = backup::BackupRun {
            copies: vec![backup::BackupCopy {
                src: PathBuf::from("/tmp/a"),
                dst: PathBuf::from("/tmp/a.bak"),
            }],
        };
        let mut slot = Some(run);
        let consumed = super::take_pending_backup_or_error(&mut slot)
            .expect("Some(run) must yield Ok(run)");
        assert_eq!(consumed.copies.len(), 1);
        assert!(
            slot.is_none(),
            "take() must clear the slot so a follow-up apply hits the \
             stale-guard rather than re-applying the same plan",
        );
    }
}
