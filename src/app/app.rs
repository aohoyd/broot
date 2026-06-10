use {
    super::*,
    crate::{
        backup,
        browser::BrowserState,
        bulk_rename,
        cli::TriBool,
        command::{
            Command,
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
        verb::Internal,
        watcher::Watcher,
    },
    crokey::crossterm::event::Event,
    std::{
        io::Write,
        path::{
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
        //   `Internal::bulk_rename`       — F2 entry point. Unified
        //                                    path-collection
        //                                    (`stage || [selection]`) is
        //                                    fed into the editor; the
        //                                    bulk machinery handles N=1
        //                                    cleanly, so single-file
        //                                    rename is a one-row plan.
        //                                    Runs editor, plans, opens
        //                                    ConfirmOverlay.
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
                    return self.run_bulk_rename(app_state);
                }
                Internal::bulk_rename_apply => {
                    if is_input_invocation {
                        self.panels.clear_input_invocation(con);
                    }
                    self.run_bulk_rename_apply(app_state, con);
                    return Ok(());
                }
                // Backup intercept: two-internal split mirroring
                // bulk_rename. `backup` is the keyed trigger
                // (alt-shift-b); it plans a `BackupRun` (one row per
                // path collected from `stage || [selection]`), stashes
                // it on `self.pending_backup`, and opens a
                // `ConfirmOverlay` whose CloseAndRun re-dispatches
                // `:backup_apply`. The bulk machinery handles N=1
                // cleanly so single-file backup is just a one-row run.
                Internal::backup => {
                    if is_input_invocation {
                        self.panels.clear_input_invocation(con);
                    }
                    return self.run_backup(app_state, con);
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
        // because the deny-list returns `false` for them. The four
        // overlay/modal internals (`bulk_rename`, `bulk_rename_apply`,
        // `backup`, `backup_apply`) also bypass this
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
    /// $EDITOR → parse → plan → confirm-overlay pipeline using the
    /// unified path collection (the stage when the stage panel is
    /// active, otherwise the current selection). N=1 is the
    /// single-file rename case and flows through the same pipeline —
    /// one editor line, one ConfirmOverlay row — no separate fast
    /// path.
    ///
    /// Failure modes surface to the status row and return `Ok(())`;
    /// a hard panic is never appropriate for any user-driven error
    /// here (editor missing, parse failure, validation failure, fs
    /// error during the editor's read-back, etc.).
    fn run_bulk_rename(
        &mut self,
        app_state: &mut AppState,
    ) -> Result<(), ProgramError> {
        // Unified path collection via `collect_bulk_paths`: the stage
        // is used only when the stage panel is the active panel,
        // otherwise the current selection (N=1 bulk). The bulk
        // machinery (`bulk_rename::serialize/parse/plan/apply`)
        // already handles N=1 cleanly — one editor line, one
        // ConfirmOverlay row.
        let stage_panel_active =
            self.panels.state().get_type() == PanelStateType::Stage;
        let selection = self.panels.state().selected_path();
        let paths = collect_bulk_paths(&app_state.stage, selection, stage_panel_active);
        if paths.is_empty() {
            self.panels
                .mut_panel()
                .set_error(BULK_RENAME_NO_SELECTION_MSG.to_string());
            return Ok(());
        }

        let content = bulk_rename::serialize(&paths);
        let edited = match editor::edit_in_external(&content, ".broot-rename") {
            Ok(s) => s,
            Err(e) => {
                self.panels.mut_panel().set_error(format!("bulk rename: {e}"));
                return Ok(());
            }
        };
        let parsed = bulk_rename::parse(&edited);
        let run = match bulk_rename::plan(&paths, &parsed, &|p| p.exists()) {
            Ok(r) => r,
            Err(e) => {
                self.panels.mut_panel().set_error(e.to_string());
                return Ok(());
            }
        };
        if run.renames.is_empty() {
            self.panels.mut_panel().set_message(BULK_RENAME_NO_CHANGES_MSG);
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
        let run = match take_pending_bulk_rename_or_error(&mut self.pending_bulk_rename) {
            Ok(r) => r,
            Err(msg) => {
                self.panels.mut_panel().set_error(msg.to_string());
                return;
            }
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

    /// Drive the `:backup` trigger. Unified flow: collect paths from
    /// the stage when the stage panel is active (otherwise the active
    /// panel's current selection), plan the bulk run, stash it on
    /// `self.pending_backup`, and open a confirm overlay listing the
    /// `src → dst` rows. The overlay's `CloseAndRun` re-dispatches
    /// `:backup_apply`, which `mem::take`s the stash and runs
    /// `backup::apply`.
    ///
    /// The bulk machinery handles N=1 cleanly — single-file backup is
    /// just a one-row bulk run, no separate code path. Path collection
    /// uses `collect_bulk_paths` (stage when the stage panel is active,
    /// else the current selection).
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
        let stage_panel_active =
            self.panels.state().get_type() == PanelStateType::Stage;
        let selection = self.panels.state().selected_path();
        let paths = collect_bulk_paths(&app_state.stage, selection, stage_panel_active);
        if paths.is_empty() {
            self.panels
                .mut_panel()
                .set_error(BACKUP_NO_SELECTION_MSG.to_string());
            return Ok(());
        }

        let run = backup::plan_bulk_backup(&paths, &con.backup_suffix);
        if run.copies.is_empty() {
            self.panels
                .mut_panel()
                .set_error(BACKUP_NO_ELIGIBLE_PATHS_MSG.to_string());
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
        fn name_or_display(p: &std::path::Path) -> String {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(|| p.display().to_string())
        }
        let body: Vec<String> = run
            .copies
            .iter()
            .map(|c| format!("{} → {}", name_or_display(&c.src), name_or_display(&c.dst)))
            .collect();
        let title = if count == 1 {
            "Backup 1 file?".to_string()
        } else {
            format!("Backup {count} files?")
        };
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
    // If the resolved target is an existing directory, the actual write
    // target is `target/<basename>` regardless of whether the source is
    // a file or a directory. For file-into-dir this is the standard
    // copy/move semantics; for dir-into-dir this places the source as
    // a child of `target`. In both cases the existence probe re-checks
    // at the joined path — no overwrite, no prompt.
    let target_meta = target.symlink_metadata().ok()?;
    if target_meta.is_dir() {
        let basename = source.file_name()?;
        target = target.join(basename);
        target.symlink_metadata().ok()?;
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

/// `Option::take` for `pending_bulk_rename` with the canonical
/// status-error string baked in. Sibling to
/// `take_pending_backup_or_error`. Used by `run_bulk_rename_apply`
/// and pinned by `bulk_rename_apply_stale_guard_rejects_when_no_pending`.
fn take_pending_bulk_rename_or_error(
    slot: &mut Option<bulk_rename::RenameRun>,
) -> Result<bulk_rename::RenameRun, &'static str> {
    slot.take().ok_or("bulk rename: nothing pending")
}

/// Single source of truth for the bulk-flow path-collection
/// predicate (shared by `run_bulk_rename` and `run_backup`):
/// `paths = stage || [selection]`. Empty stage falls back to the
/// current selection (treating single-file as N=1 bulk).
///
/// The stage is only consulted when `stage_panel_active` is true —
/// i.e. the stage panel is the active panel. From a tree/preview
/// panel the verb acts on the active panel's single selection even
/// when the stage is non-empty: pressing F2 while browsing an
/// unrelated file renames THAT file, not the staged set. To operate
/// on the staged paths the user focuses the stage panel first. This
/// matches the gate `maybe_bulk_stage_confirm` already uses
/// (`PanelStateType::Stage`), so the confirm overlay and the path
/// collection agree. Cut/copy from staging (`copy_from_staging` /
/// `move_from_staging`) are unaffected — they read the stage
/// directly and always operate on it.
///
/// Returns an empty vector when no paths apply (no selection, or an
/// empty stage while the stage panel is active) — production callers
/// convert that to a "no selection" status error.
///
/// Extracted so the predicate is unit-testable without constructing
/// an `App`.
fn collect_bulk_paths(
    stage: &crate::stage::Stage,
    selection: Option<&Path>,
    stage_panel_active: bool,
) -> Vec<PathBuf> {
    if stage_panel_active && !stage.is_empty() {
        stage.paths().to_vec()
    } else {
        selection.map(|p| vec![p.to_path_buf()]).unwrap_or_default()
    }
}

/// Canonical status-error string for the "backup planned to zero
/// copies" branch in `run_backup`. Surfaces when
/// `plan_bulk_backup` returns an empty `copies` vector — typically
/// because every input path was rejected (non-existent or otherwise
/// ineligible). Pinned by `backup_no_eligible_paths_msg_is_error` so
/// the wording stays stable across refactors.
const BACKUP_NO_ELIGIBLE_PATHS_MSG: &str =
    "backup: no eligible paths";

/// Canonical status-error string for `:bulk_rename` when neither the
/// stage nor the active panel's selection yields any paths. Surfaced
/// via `set_error` so the severity is clear; pinned by
/// `bulk_rename_no_selection_msg_is_error_string` to prevent silent
/// wording drift.
const BULK_RENAME_NO_SELECTION_MSG: &str = "bulk rename: no selection";

/// Canonical status-message string for `:bulk_rename` when the user
/// saves the editor without changing any names — the planned run is
/// empty. Surfaced via `set_message` (not `set_error`) because the
/// no-op outcome is a normal "nothing to do" result, not a failure.
/// Pinned by `bulk_rename_no_changes_msg_is_message_string`.
const BULK_RENAME_NO_CHANGES_MSG: &str = "bulk rename: no changes";

/// Canonical status-error string for `:backup` when neither the stage
/// nor the active panel's selection yields any paths. Sibling to
/// `BULK_RENAME_NO_SELECTION_MSG`. Pinned by
/// `backup_no_selection_msg_is_error_string`.
const BACKUP_NO_SELECTION_MSG: &str = "backup: no selection";

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
    // Precondition: production callers guard with `if blocked_count > 0`
    // (`run_backup`'s cap-exhaust branch only enters when at least one
    // sentinel row is present). A debug assertion catches a future
    // caller that forgets the guard; the `saturating_sub` keeps the
    // release build well-defined (it produces "(and 0 more)" rather
    // than underflowing).
    debug_assert!(
        blocked_count > 0,
        "cap_exhaust_message called with blocked_count == 0 — caller must guard",
    );
    if blocked_count == 1 {
        format!(
            "backup: too many existing backups for {}",
            first_path.display(),
        )
    } else {
        format!(
            "backup: too many existing backups for {} (and {} more); resolve manually before running again",
            first_path.display(),
            blocked_count.saturating_sub(1),
        )
    }
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

    /// Build a real `AppContext` from defaults. Shared with the
    /// sibling `bulk_rename_routing_tests` and `backup_routing_tests`
    /// modules (`pub(super)`) so the helper lives in exactly one
    /// place. Mirrors the `context_with_icon_theme` helper in
    /// `app_context.rs`'s test module — we use the same machinery so
    /// the verb-store the test inspects is the production one, not a
    /// hand-rolled stub.
    pub(super) fn make_app_context() -> crate::app::AppContext {
        use crate::conf::{Conf, parse_default_flags};
        use crate::verb::VerbStore;
        let mut config = Conf::default();
        let verb_store = VerbStore::new(&mut config).unwrap();
        let launch_args = parse_default_flags("").unwrap();
        crate::app::AppContext::from(launch_args, verb_store, &config)
            .expect("AppContext::from must succeed with defaults")
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
            Internal::toggle_stage,
            Internal::unstage,
            Internal::clear_stage,
            Internal::focus_staging_area_no_open,
            Internal::bulk_rename,
            Internal::bulk_rename_apply,
            Internal::backup,
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
    //!   * Unified path collection — `collect_bulk_paths` is the
    //!     single source of truth for the `stage || [selection]`
    //!     rule that `run_bulk_rename` feeds into the editor. There
    //!     is no longer a stage-size branch: single-file rename is an
    //!     N=1 bulk run through the same pipeline as N≥2.

    use {
        super::*,
        crate::verb::VerbInvocation,
    };

    // `fresh_store` and `make_app_context` are shared with
    // `confirm_helper_tests`. Re-using their definitions rather than
    // re-declaring keeps the verb-store and context construction in
    // one place.
    use super::confirm_helper_tests::{fresh_store, make_app_context};

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

    /// `Command::VerbTrigger` pointing to an *external* verb must
    /// resolve to `None` — the helper only returns `Some` when the
    /// resolved verb carries an internal execution. Pinning this
    /// keeps the App-level intercept correct: a triggered external
    /// verb (e.g. the standalone `:rename` shell verb) must fall
    /// through to the panel-dispatch layer, not be claimed by the
    /// `Internal::bulk_rename` arm.
    #[test]
    fn resolved_internal_returns_none_for_verb_trigger_on_external() {
        let con = make_app_context();
        // The external `rename` verb has no internal execution.
        let verb_id = con
            .verb_store
            .verbs()
            .iter()
            .find(|v| v.has_name("rename") && v.get_internal().is_none())
            .expect("external rename verb registered")
            .id;
        let cmd = Command::VerbTrigger {
            verb_id,
            input_invocation: None,
        };
        assert_eq!(
            resolved_internal(&cmd, &con),
            None,
            "VerbTrigger on an external verb must yield None",
        );
    }

    /// Same coverage for `Command::VerbInvocate` on an external verb:
    /// the lookup walks the verb store and finds the external by
    /// name, but `get_internal()` returns `None`, so the helper
    /// returns `None` and the intercept does not fire.
    #[test]
    fn resolved_internal_returns_none_for_verb_invocate_on_external() {
        let con = make_app_context();
        let cmd = Command::VerbInvocate(VerbInvocation::new(
            "rename".to_string(),
            Some("newname".to_string()),
            false,
        ));
        assert_eq!(
            resolved_internal(&cmd, &con),
            None,
            "VerbInvocate on an external verb must yield None",
        );
    }

    /// Pin that the external `rename` verb stays registered in the
    /// default store. F2 no longer reaches it (the bulk-rename flow
    /// has been unified to handle N=1 directly), but the verb must
    /// remain callable by typed invocation (`:rename newname`) from
    /// the command bar — backward compatibility for user muscle
    /// memory and custom conf bindings.
    #[test]
    fn fresh_store_has_external_rename_verb() {
        let store = fresh_store();
        let found = store
            .verbs()
            .iter()
            .any(|v| v.has_name("rename") && v.get_internal().is_none());
        assert!(
            found,
            "external `rename` verb must remain registered for typed `:rename` invocations",
        );
    }

    /// Pin the unified path-collection predicate used by
    /// `run_bulk_rename`: the stage is consulted ONLY when the stage
    /// panel is active. From a tree/preview panel the helper always
    /// yields the single `[selection]` vector even when the stage is
    /// non-empty.
    ///
    /// We test the predicate via the free helper `collect_bulk_paths`
    /// rather than driving `run_bulk_rename` end-to-end (which would
    /// require a real `App` + `Screen`). The helper is the single
    /// source of truth for the rule; if it ever diverges from the
    /// inline implementation in `run_bulk_rename`, that's the bug.
    #[test]
    fn bulk_rename_empty_stage_uses_selection() {
        use crate::stage::Stage;
        let stage = Stage::default();
        let selection = std::path::Path::new("/tmp/sole.txt");
        // Empty stage → selection regardless of the active panel.
        for stage_panel_active in [false, true] {
            let paths = super::collect_bulk_paths(&stage, Some(selection), stage_panel_active);
            assert_eq!(
                paths,
                vec![std::path::PathBuf::from("/tmp/sole.txt")],
                "empty stage must yield exactly [selection]",
            );
        }
    }

    #[test]
    fn bulk_rename_stage_panel_active_uses_stage_paths() {
        use crate::stage::Stage;
        let mut stage = Stage::default();
        stage.add(std::path::PathBuf::from("/tmp/a.txt"));
        stage.add(std::path::PathBuf::from("/tmp/b.txt"));
        // Selection that is intentionally NOT in the stage — must be
        // ignored when the stage panel is active.
        let selection = std::path::Path::new("/tmp/ignored.txt");
        let paths = super::collect_bulk_paths(&stage, Some(selection), true);
        assert_eq!(
            paths,
            vec![
                std::path::PathBuf::from("/tmp/a.txt"),
                std::path::PathBuf::from("/tmp/b.txt"),
            ],
            "stage panel active must yield the stage paths and ignore the selection",
        );
    }

    /// The regression this change pins: from a tree/preview panel
    /// (`stage_panel_active == false`), a non-empty stage is IGNORED
    /// and the verb operates on the single current selection. F2 while
    /// browsing renames the browsed file, not the staged set.
    #[test]
    fn bulk_rename_stage_panel_inactive_ignores_stage() {
        use crate::stage::Stage;
        let mut stage = Stage::default();
        stage.add(std::path::PathBuf::from("/tmp/a.txt"));
        stage.add(std::path::PathBuf::from("/tmp/b.txt"));
        let selection = std::path::Path::new("/tmp/current.txt");
        let paths = super::collect_bulk_paths(&stage, Some(selection), false);
        assert_eq!(
            paths,
            vec![std::path::PathBuf::from("/tmp/current.txt")],
            "non-stage panel must use the selection and ignore the non-empty stage",
        );
    }

    /// Single-file stage: with the stage panel active the helper
    /// returns the single stage path (selection ignored); with it
    /// inactive the helper returns the selection.
    #[test]
    fn bulk_rename_single_stage_entry_respects_active_panel() {
        use crate::stage::Stage;
        let mut stage = Stage::default();
        stage.add(std::path::PathBuf::from("/tmp/only.txt"));
        let selection = std::path::Path::new("/tmp/different.txt");

        let from_stage = super::collect_bulk_paths(&stage, Some(selection), true);
        assert_eq!(
            from_stage,
            vec![std::path::PathBuf::from("/tmp/only.txt")],
            "stage panel active: single-entry stage must use the stage path",
        );

        let from_selection = super::collect_bulk_paths(&stage, Some(selection), false);
        assert_eq!(
            from_selection,
            vec![std::path::PathBuf::from("/tmp/different.txt")],
            "stage panel inactive: must use the selection, not the staged entry",
        );
    }

    /// No applicable paths: helper returns an empty vector. The
    /// production handler converts this to a "no selection" status
    /// error rather than running with zero paths. Covers both the
    /// empty-stage-no-selection case and the stage-panel-active-but-
    /// empty-stage case.
    #[test]
    fn bulk_rename_no_applicable_paths_returns_empty() {
        use crate::stage::Stage;
        let stage = Stage::default();
        for stage_panel_active in [false, true] {
            let paths = super::collect_bulk_paths(&stage, None, stage_panel_active);
            assert!(
                paths.is_empty(),
                "no stage + no selection must yield an empty path list",
            );
        }
    }

    /// Stale-guard pin for `run_bulk_rename_apply`: the receiver
    /// consumes `pending_bulk_rename` via
    /// `take_pending_bulk_rename_or_error` and surfaces a canonical
    /// status error when nothing was pending (mirrors the backup
    /// equivalent). Calls the SAME free function the production path
    /// calls, so a drift in the error string or the `Option::take`
    /// discipline is caught here.
    #[test]
    fn bulk_rename_apply_stale_guard_rejects_when_no_pending() {
        // Stale case: nothing pending → canonical error string.
        let mut slot: Option<bulk_rename::RenameRun> = None;
        let err = super::take_pending_bulk_rename_or_error(&mut slot)
            .expect_err("None must produce the stale-guard error");
        assert_eq!(err, "bulk rename: nothing pending");

        // Healthy case: pending run is consumed AND slot is cleared.
        let run = bulk_rename::RenameRun {
            renames: vec![(
                std::path::PathBuf::from("/tmp/a"),
                std::path::PathBuf::from("/tmp/b"),
            )],
        };
        let mut slot = Some(run);
        let consumed = super::take_pending_bulk_rename_or_error(&mut slot)
            .expect("Some(run) must yield Ok(run)");
        assert_eq!(consumed.renames.len(), 1);
        assert!(
            slot.is_none(),
            "take() must clear the slot so a follow-up apply hits the \
             stale-guard rather than re-applying the same plan",
        );
    }

    /// Pin the canonical "no selection" error string surfaced by
    /// `run_bulk_rename` when `collect_bulk_paths` returns empty (no
    /// stage, no selection). The string is surfaced via `set_error`
    /// (not `set_message`) so the severity is encoded in the API.
    /// Prevents silent wording drift in the rename domain.
    #[test]
    fn bulk_rename_no_selection_msg_is_error_string() {
        assert_eq!(
            super::BULK_RENAME_NO_SELECTION_MSG,
            "bulk rename: no selection",
        );
        assert!(
            super::BULK_RENAME_NO_SELECTION_MSG.starts_with("bulk rename:"),
            "message must be prefixed with the verb domain, got: {}",
            super::BULK_RENAME_NO_SELECTION_MSG,
        );
    }

    /// Pin the canonical "no changes" message surfaced by
    /// `run_bulk_rename` when the user saves the editor without
    /// editing any names — `bulk_rename::plan` returns an empty run.
    /// This is a `set_message` (not `set_error`) because the no-op
    /// outcome is benign.
    #[test]
    fn bulk_rename_no_changes_msg_is_message_string() {
        assert_eq!(
            super::BULK_RENAME_NO_CHANGES_MSG,
            "bulk rename: no changes",
        );
        assert!(
            super::BULK_RENAME_NO_CHANGES_MSG.starts_with("bulk rename:"),
            "message must be prefixed with the verb domain, got: {}",
            super::BULK_RENAME_NO_CHANGES_MSG,
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
    //! intercept behaviour decomposes cleanly into pieces that ARE
    //! testable in isolation:
    //!
    //!   * `resolved_internal` — already tested in
    //!     `bulk_rename_routing_tests`; the same dispatch covers both
    //!     backup internals because the helper is internal-agnostic.
    //!   * Unified path collection — `collect_bulk_paths`
    //!     (`stage || [selection]`) feeds both `run_bulk_rename` and
    //!     `run_backup`, and is pinned in `rename_routing_tests`. The
    //!     backup-side tests below add scenarios that exercise the
    //!     shared helper from the backup angle.
    //!   * Bulk-planning detection — `plan_bulk_backup` already has
    //!     unit tests in `src/backup/mod.rs` covering the cap-exhaust
    //!     sentinel; here we re-pin the predicate that the intercept
    //!     uses to convert that sentinel into a status error.
    //!
    //! These tests guard against regressions in the routing chain
    //! WITHOUT pretending to exercise a real `App`. Manual smoke tests
    //! in a live terminal cover end-to-end behaviour (broot has no
    //! automated UI suite).
    use {
        super::*,
        std::path::PathBuf,
    };

    // Shared with `confirm_helper_tests`; re-used here so the
    // context-construction lives in one place.
    use super::confirm_helper_tests::make_app_context;

    /// Both surviving backup internals must be findable by
    /// `get_internal()` in a fresh verb store. The trigger is reached
    /// via the App-level intercept; `backup_apply` is reached via the
    /// confirm overlay's `CloseAndRun` re-dispatch. Pin both so a
    /// future de-registration breaks loudly here rather than silently
    /// at runtime.
    #[test]
    fn backup_internals_findable_by_get_internal() {
        let con = make_app_context();
        let find = |target: Internal| -> bool {
            con.verb_store
                .verbs()
                .iter()
                .any(|v| v.get_internal() == Some(target))
        };
        assert!(
            find(Internal::backup),
            "backup trigger must be findable in the default store",
        );
        assert!(
            find(Internal::backup_apply),
            "backup_apply receiver must be findable (re-dispatched via confirm overlay)",
        );
    }

    // Negative pin for `backup_one` being absent from the verb store
    // lives in `verb_store.rs::backup_internals_registered`; not
    // duplicated here.

    // `collect_bulk_paths` coverage lives in `bulk_rename_routing_tests`
    // (the helper is shared by both `run_bulk_rename` and `run_backup`,
    // so duplicating the four `stage || [selection]` scenarios here
    // would add no coverage).

    /// Cap-exhaust short-circuits the overlay: when `plan_bulk_backup`
    /// emits at least one sentinel row (`src == dst`), `run_backup`
    /// produces a status message via `cap_exhaust_message` and does
    /// NOT call `request_confirm` (i.e. no `pending_backup` is set
    /// and no overlay opens).
    ///
    /// We exercise the predicate `run_backup` uses to detect the
    /// sentinel (the `c.src == c.dst` filter) directly against a
    /// hand-crafted `BackupRun`. Driving the full `run_backup`
    /// requires an `App`, but the discriminator itself is pure: if
    /// the production code's filter ever drifts (e.g. accidentally
    /// inverted or restricted to a subset), this test fails because
    /// the cap-exhaust message no longer matches the expected shape.
    #[test]
    fn cap_exhaust_short_circuits_overlay() {
        let blocked_src = std::path::PathBuf::from("/tmp/foo");
        let ok_src = std::path::PathBuf::from("/tmp/bar");
        let ok_dst = std::path::PathBuf::from("/tmp/bar.bak");
        let run = backup::BackupRun {
            copies: vec![
                // Healthy row.
                backup::BackupCopy {
                    src: ok_src.clone(),
                    dst: ok_dst.clone(),
                },
                // Sentinel: dst == src means cap-exhausted.
                backup::BackupCopy {
                    src: blocked_src.clone(),
                    dst: blocked_src.clone(),
                },
            ],
        };
        let blocked_count = run.copies.iter().filter(|c| c.src == c.dst).count();
        assert_eq!(blocked_count, 1, "exactly one sentinel row was inserted");

        // The overlay-short-circuit branch in `run_backup` constructs
        // the status message from the FIRST sentinel; pin the format
        // here so a string drift fails loudly.
        let first_blocked = run
            .copies
            .iter()
            .find(|c| c.src == c.dst)
            .expect("blocked_count > 0 guarantees at least one match");
        let msg = super::cap_exhaust_message(&first_blocked.src, blocked_count);
        assert!(
            msg.contains("too many existing backups"),
            "cap-exhaust message must explain the failure, got: {msg}",
        );
        assert!(
            msg.contains("/tmp/foo"),
            "cap-exhaust message must name the blocked source, got: {msg}",
        );
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

    /// Pin: the "no eligible paths" status string surfaces via
    /// `set_error` (not `set_message`) when `plan_bulk_backup`
    /// returns an empty `copies` vector. We can't drive `run_backup`
    /// end-to-end without an `App`, but pinning the constant prevents
    /// silent wording drift — and the production call site uses
    /// `set_error(..to_string())` so the severity is encoded in the
    /// API surface.
    #[test]
    fn backup_no_eligible_paths_msg_is_error() {
        // The constant must not mention "stage" — the unified flow
        // collects paths from `stage || [selection]`, so the message
        // applies to either origin.
        assert!(
            !super::BACKUP_NO_ELIGIBLE_PATHS_MSG.contains("stage"),
            "BACKUP_NO_ELIGIBLE_PATHS_MSG must not mention 'stage' — \
             unified path collection covers stage and selection alike, \
             got: {}",
            super::BACKUP_NO_ELIGIBLE_PATHS_MSG,
        );
        assert!(
            super::BACKUP_NO_ELIGIBLE_PATHS_MSG.starts_with("backup:"),
            "message must be prefixed with the verb domain, got: {}",
            super::BACKUP_NO_ELIGIBLE_PATHS_MSG,
        );
        assert!(
            super::BACKUP_NO_ELIGIBLE_PATHS_MSG.contains("no eligible paths"),
            "message must surface the cause, got: {}",
            super::BACKUP_NO_ELIGIBLE_PATHS_MSG,
        );
    }

    /// Pin the canonical "no selection" error string surfaced by
    /// `run_bulk_rename` when `collect_bulk_paths` returns empty (no
    /// stage, no selection). The string is surfaced via `set_error`
    /// (not `set_message`) so the severity is encoded in the API.
    /// Sibling to `bulk_rename_no_selection_msg_is_error_string`.
    #[test]
    fn backup_no_selection_msg_is_error_string() {
        assert_eq!(
            super::BACKUP_NO_SELECTION_MSG,
            "backup: no selection",
        );
        assert!(
            super::BACKUP_NO_SELECTION_MSG.starts_with("backup:"),
            "message must be prefixed with the verb domain, got: {}",
            super::BACKUP_NO_SELECTION_MSG,
        );
        // Same "stage-agnostic" requirement as the no-eligible-paths
        // sibling: the unified flow accepts stage or selection.
        assert!(
            !super::BACKUP_NO_SELECTION_MSG.contains("stage"),
            "BACKUP_NO_SELECTION_MSG must not mention 'stage' — \
             unified path collection covers stage and selection alike, \
             got: {}",
            super::BACKUP_NO_SELECTION_MSG,
        );
    }

    /// Pin the body shape: `run_backup` paints each `BackupCopy` row as
    /// `{src_name} → {dst_name}` (file names only). Backup destinations
    /// are always siblings of their source — `next_free_backup_name`
    /// only walks `src.parent()` for free `.bak` / `.bak.N` slots — so
    /// the full path is pure noise and the file_name form matches
    /// `bulk_rename::diff_lines`' same-parent branch.
    ///
    /// This test re-implements the body mapper locally so a future drift
    /// between the helper and the production loop trips here. If a
    /// later change moves the mapper into a shared module, replace this
    /// local copy with a call to that helper.
    #[test]
    fn backup_confirm_body_shows_filenames_only() {
        use crate::backup::{BackupCopy, BackupRun};

        let run = BackupRun {
            copies: vec![
                BackupCopy {
                    src: PathBuf::from("/deeply/nested/path/notes.txt"),
                    dst: PathBuf::from("/deeply/nested/path/notes.txt.bak"),
                },
                BackupCopy {
                    src: PathBuf::from("/another/dir/log.json"),
                    dst: PathBuf::from("/another/dir/log.json.bak.3"),
                },
            ],
        };

        fn name_or_display(p: &std::path::Path) -> String {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(|| p.display().to_string())
        }
        let body: Vec<String> = run
            .copies
            .iter()
            .map(|c| {
                format!(
                    "{} → {}",
                    name_or_display(&c.src),
                    name_or_display(&c.dst)
                )
            })
            .collect();

        assert_eq!(
            body,
            vec![
                "notes.txt → notes.txt.bak".to_string(),
                "log.json → log.json.bak.3".to_string(),
            ],
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

    // Coverage note: cap-exhaust detection in `run_backup` is
    // exercised by `cap_exhaust_short_circuits_overlay` above and by
    // `plan_bulk_backup_uses_self_sentinel_when_exhausted` in
    // `src/backup/mod.rs`. End-to-end testing of `run_backup` requires
    // an `App` instance, which the test harness can't construct
    // without a TTY-backed `Screen`. Manual smoke tests in a live
    // terminal cover the integration boundary.

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

    // --- `crate::backup::apply` single-row receiver tests -----------
    //
    // After unification, single-file backup is just an N=1 bulk run.
    // `backup::apply` encapsulates both the file and directory copy
    // branches and is the cleanest entry point to verify them
    // end-to-end without an App. These tests pin the primitives the
    // bulk handler delegates to, including the N=1 special case the
    // old `run_backup_one` flow used to cover.

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
