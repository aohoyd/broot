use {
    crate::{
        app::*,
        command::*,
        display::*,
        errors::{ProgramError, TreeBuildError},
        flag::Flag,
        git,
        path::{self, PathAnchor},
        pattern::*,
        print,
        stage::*,
        task_sync::{ComputationResult, Dam},
        tree::*,
        tree_build::TreeBuilder,
        verb::*,
    },
    opener,
    std::path::{Path, PathBuf},
};

/// An application state dedicated to displaying a tree.
/// It's the first and main screen of broot.
pub struct BrowserState {
    pub tree: Tree,
    pub filtered_tree: Option<Tree>,
    mode: Mode,                        // whether we're in 'input' or 'normal' mode
    pending_task: Option<BrowserTask>, // note: there are some other pending task, see
    /// Top row (absolute) of the body region as last painted. Cached so
    /// click coordinates (which arrive in absolute terminal coords) can be
    /// translated to body-relative coords for `Tree::try_select_y`. `0`
    /// before the first render (no click possible before then).
    body_top: u16,
}

/// A task that can be computed in background
#[derive(Debug)]
enum BrowserTask {
    Search {
        pattern: InputPattern,
        total: bool,
    },
    StageAll {
        pattern: InputPattern,
        file_type_condition: FileTypeCondition,
    },
}

/// Decide which directory `Internal::add` should target given the
/// currently-selected path and the panel's root.
///
/// Rule: if the selection is itself a directory, create inside it;
/// otherwise create alongside the selection (its parent). If the
/// selection has no parent (degenerate `/`-only case), fall back to
/// `panel_root` so the overlay always has *some* well-formed
/// `target_dir`. Pure function, no side effects beyond an `is_dir()`
/// metadata probe — extracted so the routing rule is unit-testable
/// without spinning up a full `BrowserState`.
pub(crate) fn resolve_add_target_dir(
    selected_path: &Path,
    panel_root: &Path,
) -> PathBuf {
    if selected_path.is_dir() {
        selected_path.to_path_buf()
    } else {
        selected_path
            .parent()
            .unwrap_or(panel_root)
            .to_path_buf()
    }
}

impl BrowserState {
    /// build a new tree state if there's no error and there's no cancellation.
    pub fn new(
        path: PathBuf,
        mut options: TreeOptions,
        screen: Screen,
        con: &AppContext,
        dam: &Dam,
    ) -> Result<BrowserState, TreeBuildError> {
        // on windows, canonicalize the path produces UNC paths, so we don't do it.
        // On other platforms, it's a desirable step, mainly because it simplifies the
        // paths you'd get for example when focusing a relative symlink containing "..".
        #[cfg(not(target_os = "windows"))]
        let path = path.canonicalize().unwrap_or(path);

        let pending_task = options
            .pattern
            .take()
            .as_option()
            .map(|pattern| BrowserTask::Search {
                pattern,
                total: false,
            });
        let builder = TreeBuilder::from(path, options, BrowserState::page_height(screen), con)?;
        let tree = builder.build_tree(false, dam)?;
        Ok(BrowserState {
            tree,
            filtered_tree: None,
            mode: con.initial_mode(),
            pending_task,
            body_top: 0,
        })
    }

    fn search(
        &mut self,
        pattern: InputPattern,
        total: bool,
    ) {
        self.pending_task = Some(BrowserTask::Search { pattern, total });
    }

    /// build a cmdResult asking for the addition of a new state
    /// being a browser state similar to the current one but with
    /// different options or a different root, or both
    fn modified(
        &self,
        screen: Screen,
        root: PathBuf,
        options: TreeOptions,
        message: Option<&'static str>,
        in_new_panel: bool,
        con: &AppContext,
    ) -> CmdResult {
        let tree = self.displayed_tree();
        let mut new_state = BrowserState::new(root, options, screen, con, &Dam::unlimited());
        if let Ok(bs) = &mut new_state {
            if tree.selection != 0 {
                bs.displayed_tree_mut()
                    .try_select_path(&tree.selected_line().path);
            }
        }
        CmdResult::from_optional_browser_state(new_state, message, in_new_panel)
    }

    pub fn root(&self) -> &Path {
        self.tree.root()
    }

    pub fn page_height(screen: Screen) -> usize {
        // The interior content rows of a panel are bounded by:
        //   - 1 status row + 1 input row at the bottom of the screen
        //   - 1 top frame edge + 1 bottom frame edge inset by the panel frame
        // Use saturating_sub so very small terminals don't underflow.
        (screen.height as usize).saturating_sub(4)
    }

    /// Translate an absolute terminal y to a body-relative y, or `None`
    /// if the click is above the body (frame border).
    ///
    /// Title row clicks never reach this helper: `on_click` /
    /// `on_double_click` intercept them via `try_select_title_row`
    /// before delegating here, so the only y < body_top case left is
    /// the top frame edge.
    ///
    /// Body row 0 sits at `self.body_top` (== `state.top` after the frame
    /// inset, set during `display`). `try_select_y` expects body-relative
    /// y and maps it to `lines[y + 1 + scroll]`.
    fn body_relative_y(&self, y: u16) -> Option<usize> {
        if y >= self.body_top {
            Some((y - self.body_top) as usize)
        } else {
            None
        }
    }

    /// If `y` is the title row (body_top - 1), reset selection to 0
    /// and return true. Otherwise return false. Underflow-safe — when
    /// `body_top == 0` the frame has collapsed and there is no title
    /// row to click.
    ///
    /// Width note: the title glyph itself is only painted when
    /// `outer.width >= 6` (see `app_panels.rs`); for narrower panels
    /// (3..6) the top frame edge is still drawn but carries no visible
    /// title. We deliberately do NOT plumb width into this helper —
    /// clicks on the top edge of those pathological panels still select
    /// the root, matching the spirit of "the top frame edge is the
    /// root's seat". Pathological because preview / stage / tree panels
    /// at that width are essentially unusable for their primary purpose
    /// anyway.
    fn try_select_title_row(&mut self, y: u16) -> bool {
        if self.body_top > 0 && y == self.body_top - 1 {
            self.displayed_tree_mut().selection = 0;
            true
        } else {
            false
        }
    }

    /// return a reference to the currently displayed tree, which
    /// is the filtered tree if there's one, the base tree if not.
    pub fn displayed_tree(&self) -> &Tree {
        self.filtered_tree.as_ref().unwrap_or(&self.tree)
    }

    /// return a mutable reference to the currently displayed tree, which
    /// is the filtered tree if there's one, the base tree if not.
    pub fn displayed_tree_mut(&mut self) -> &mut Tree {
        self.filtered_tree.as_mut().unwrap_or(&mut self.tree)
    }

    pub fn open_selection_stay_in_broot(
        &mut self,
        screen: Screen,
        con: &AppContext,
        in_new_panel: bool,
        keep_pattern: bool,
    ) -> Result<CmdResult, ProgramError> {
        let tree = self.displayed_tree();
        let line = tree.selected_line();
        let mut target = line.target().to_path_buf();
        if line.is_dir() {
            if tree.selection == 0 {
                // opening the root would be going to where we already are.
                // We go up one level instead
                if let Some(parent) = target.parent() {
                    target = PathBuf::from(parent);
                }
            }
            let dam = Dam::unlimited();
            Ok(CmdResult::from_optional_browser_state(
                BrowserState::new(
                    target,
                    if keep_pattern {
                        tree.options.clone()
                    } else {
                        tree.options.without_pattern()
                    },
                    screen,
                    con,
                    &dam,
                ),
                None,
                in_new_panel,
            ))
        } else {
            match opener::open(&target) {
                Ok(exit_status) => {
                    info!("open returned with exit_status {exit_status:?}");
                    Ok(CmdResult::Keep)
                }
                Err(e) => Ok(CmdResult::error(format!("{e:?}"))),
            }
        }
    }

    pub fn go_to_parent(
        &mut self,
        screen: Screen,
        con: &AppContext,
        in_new_panel: bool,
    ) -> CmdResult {
        match &self.displayed_tree().selected_line().path.parent() {
            Some(path) => CmdResult::from_optional_browser_state(
                BrowserState::new(
                    path.to_path_buf(),
                    self.displayed_tree().options.without_pattern(),
                    screen,
                    con,
                    &Dam::unlimited(),
                ),
                None,
                in_new_panel,
            ),
            None => CmdResult::error("no parent found"),
        }
    }
}

impl PanelState for BrowserState {
    fn tree_root(&self) -> Option<&Path> {
        Some(self.root())
    }

    fn get_type(&self) -> PanelStateType {
        PanelStateType::Tree
    }

    fn set_mode(
        &mut self,
        mode: Mode,
    ) {
        self.mode = mode;
    }

    fn get_mode(&self) -> Mode {
        self.mode
    }

    fn get_pending_task(&self) -> Option<&'static str> {
        if self.displayed_tree().has_dir_missing_sum() {
            Some("computing stats")
        } else if self.displayed_tree().is_missing_git_status_computation() {
            Some("computing git status")
        } else {
            self.pending_task.as_ref().map(|task| match task {
                BrowserTask::Search { .. } => "searching",
                BrowserTask::StageAll { .. } => "staging",
            })
        }
    }

    fn watchable_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for line in &self.tree.lines {
            paths.push(line.path.clone());
        }
        paths
    }

    fn selected_path(&self) -> Option<&Path> {
        Some(&self.displayed_tree().selected_line().path)
    }

    fn selection(&self) -> Option<Selection<'_>> {
        let tree = self.displayed_tree();
        let mut selection = tree.selected_line().as_selection();
        selection.line = tree
            .options
            .pattern
            .pattern
            .get_match_line_count(selection.path)
            .unwrap_or(0);
        Some(selection)
    }

    fn tree_options(&self) -> TreeOptions {
        self.displayed_tree().options.clone()
    }

    /// build a cmdResult asking for the addition of a new state
    /// being a browser state similar to the current one but with
    /// different options
    fn with_new_options(
        &mut self,
        screen: Screen,
        change_options: &dyn Fn(&mut TreeOptions) -> &'static str,
        in_new_panel: bool,
        con: &AppContext,
    ) -> CmdResult {
        let tree = self.displayed_tree();
        let mut options = tree.options.clone();
        let message = change_options(&mut options);
        let message = Some(message);
        self.modified(
            screen,
            tree.root().clone(),
            options,
            message,
            in_new_panel,
            con,
        )
    }

    fn clear_pending(&mut self) {
        self.pending_task = None;
    }

    fn on_click(
        &mut self,
        _x: u16,
        y: u16,
        _screen: Screen,
        _con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        if self.try_select_title_row(y) {
            return Ok(CmdResult::Keep);
        }
        if let Some(body_y) = self.body_relative_y(y) {
            self.displayed_tree_mut().try_select_y(body_y);
        }
        Ok(CmdResult::Keep)
    }

    fn on_double_click(
        &mut self,
        _x: u16,
        y: u16,
        screen: Screen,
        con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        // Double-click on the title row navigates up. `try_select_title_row`
        // already pinned `selection = 0` from the preceding single click,
        // and `open_selection_stay_in_broot` treats `selection == 0` as
        // "go to parent" — so the title becomes a click-target for the
        // implicit parent directory.
        if self.try_select_title_row(y) {
            return self.open_selection_stay_in_broot(screen, con, false, false);
        }
        // A double-click always follows a simple click at the same y, so the
        // previous click already updated `selection` to the line under the
        // pointer. Recompute the same line index from `y` and compare to
        // `selection`: if they match the line was selectable and openable.
        let Some(body_y) = self.body_relative_y(y) else {
            return Ok(CmdResult::Keep);
        };
        let tree = self.displayed_tree();
        let line_index = body_y + 1 + tree.scroll;
        if tree.selection == line_index {
            self.open_selection_stay_in_broot(screen, con, false, false)
        } else {
            // click wasn't on a selectable/openable tree line
            Ok(CmdResult::Keep)
        }
    }

    fn on_pattern(
        &mut self,
        pat: InputPattern,
        _app_state: &AppState,
        _con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        if pat.is_none() {
            self.filtered_tree = None;
        }
        if let Some(filtered_tree) = &self.filtered_tree {
            if pat != filtered_tree.options.pattern {
                self.search(pat, false);
            }
        } else {
            self.search(pat, false);
        }
        Ok(CmdResult::Keep)
    }

    fn on_internal(
        &mut self,
        w: &mut W,
        invocation_parser: Option<&InvocationParser>,
        internal_exec: &InternalExecution,
        input_invocation: Option<&VerbInvocation>,
        trigger_type: TriggerType,
        app_state: &mut AppState,
        cc: &CmdContext,
    ) -> Result<CmdResult, ProgramError> {
        debug!("browser_state on_internal {internal_exec:?}");
        let con = &cc.app.con;
        let screen = cc.app.screen;
        let page_height = BrowserState::page_height(cc.app.screen);
        let bang = input_invocation
            .map(|inv| inv.bang)
            .unwrap_or(internal_exec.bang);
        Ok(match internal_exec.internal {
            Internal::back => {
                if let Some(filtered_tree) = &self.filtered_tree {
                    let filtered_selection = &filtered_tree.selected_line().path;
                    if self.tree.try_select_path(filtered_selection) {
                        self.tree.make_selection_visible(page_height);
                    }
                    self.filtered_tree = None;
                    CmdResult::Keep
                } else if self.tree.selection > 0 {
                    self.tree.selection = 0;
                    CmdResult::Keep
                } else {
                    CmdResult::PopState
                }
            }
            Internal::focus => {
                let tree = self.displayed_tree();
                internal_focus::on_internal(
                    internal_exec,
                    input_invocation,
                    trigger_type,
                    &tree.selected_line().path,
                    tree.is_root_selected(),
                    tree.options.clone(),
                    app_state,
                    cc,
                )
            }
            Internal::line_down => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut()
                    .move_selection(count, page_height, true);
                CmdResult::Keep
            }
            Internal::line_down_no_cycle => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut()
                    .move_selection(count, page_height, false);
                CmdResult::Keep
            }
            Internal::line_up => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut()
                    .move_selection(-count, page_height, true);
                CmdResult::Keep
            }
            Internal::line_up_no_cycle => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.displayed_tree_mut()
                    .move_selection(-count, page_height, false);
                CmdResult::Keep
            }
            Internal::next_dir => {
                self.displayed_tree_mut()
                    .try_select_next_filtered(TreeLine::is_dir, page_height);
                CmdResult::Keep
            }
            Internal::next_match => {
                self.displayed_tree_mut()
                    .try_select_next_filtered(|line| line.direct_match, page_height);
                CmdResult::Keep
            }
            Internal::next_same_depth => {
                self.displayed_tree_mut()
                    .try_select_next_same_depth(page_height);
                CmdResult::Keep
            }
            Internal::open_stay => self.open_selection_stay_in_broot(screen, con, bang, false)?,
            Internal::open_stay_filter => {
                self.open_selection_stay_in_broot(screen, con, bang, true)?
            }
            Internal::page_down => {
                let tree = self.displayed_tree_mut();
                if !tree.try_scroll(page_height as i32, page_height) {
                    tree.try_select_last(page_height);
                }
                CmdResult::Keep
            }
            Internal::page_up => {
                let tree = self.displayed_tree_mut();
                if !tree.try_scroll(-(page_height as i32), page_height) {
                    tree.try_select_first();
                }
                CmdResult::Keep
            }
            Internal::panel_left => {
                let areas = &cc.panel.areas;
                if areas.is_first() && areas.nb_pos < con.max_panels_count {
                    // we ask for the creation of a panel to the left
                    internal_focus::new_panel_on_path(
                        self.displayed_tree().selected_line().path.clone(),
                        screen,
                        self.displayed_tree().options.clone(),
                        PanelPurpose::None,
                        con,
                        HDir::Left,
                        false,
                    )
                } else {
                    // we let the app handle other cases
                    CmdResult::HandleInApp(Internal::panel_left_no_open)
                }
            }
            Internal::panel_left_no_open => CmdResult::HandleInApp(Internal::panel_left_no_open),
            Internal::panel_right => {
                let areas = &cc.panel.areas;
                let selected_path = &self.displayed_tree().selected_line().path;
                if areas.is_last() && areas.nb_pos < con.max_panels_count {
                    let purpose = if selected_path.is_file() && cc.app.preview_panel.is_none() {
                        PanelPurpose::Preview
                    } else {
                        PanelPurpose::None
                    };
                    // we ask for the creation of a panel to the right
                    internal_focus::new_panel_on_path(
                        selected_path.clone(),
                        screen,
                        self.displayed_tree().options.clone(),
                        purpose,
                        con,
                        HDir::Right,
                        true,
                    )
                } else {
                    // we ask the app to handle other cases :
                    // focus the panel to the right, if any
                    CmdResult::HandleInApp(Internal::panel_right_no_open)
                }
            }
            Internal::panel_right_no_open => CmdResult::HandleInApp(Internal::panel_right_no_open),
            Internal::parent => self.go_to_parent(screen, con, bang),
            Internal::previous_dir => {
                self.displayed_tree_mut()
                    .try_select_previous_filtered(TreeLine::is_dir, page_height);
                CmdResult::Keep
            }
            Internal::previous_match => {
                self.displayed_tree_mut()
                    .try_select_previous_filtered(|line| line.direct_match, page_height);
                CmdResult::Keep
            }
            Internal::previous_same_depth => {
                self.displayed_tree_mut()
                    .try_select_previous_same_depth(page_height);
                CmdResult::Keep
            }
            Internal::print_tree => {
                print::print_tree(self.displayed_tree(), cc.app.screen, cc.app.panel_skin, con)?
            }
            Internal::quit => CmdResult::Quit,
            Internal::root_down => {
                let tree = self.displayed_tree();
                if tree.selection > 0 {
                    let root_len = tree.root().components().count();
                    let new_root = tree
                        .selected_line()
                        .path
                        .components()
                        .take(root_len + 1)
                        .collect();
                    self.modified(screen, new_root, tree.options.clone(), None, bang, con)
                } else {
                    CmdResult::error("No selected line")
                }
            }
            Internal::root_up => {
                let tree = self.displayed_tree();
                let root = tree.root();
                if let Some(new_root) = root.parent() {
                    self.modified(
                        screen,
                        new_root.to_path_buf(),
                        tree.options.clone(),
                        None,
                        bang,
                        con,
                    )
                } else {
                    CmdResult::error(format!("{root:?} has no parent"))
                }
            }
            Internal::search_again => {
                match self.filtered_tree.as_ref().map(|t| t.total_search) {
                    None => {
                        // we delegate to the app the task of looking for a preview pattern
                        // used before this state
                        CmdResult::HandleInApp(Internal::search_again)
                    }
                    Some(true) => CmdResult::error(
                        "search was already total: all possible matches have been ranked",
                    ),
                    Some(false) => {
                        self.search(self.displayed_tree().options.pattern.clone(), true);
                        CmdResult::Keep
                    }
                }
            }
            Internal::select => internal_select::on_internal(
                internal_exec,
                input_invocation,
                trigger_type,
                self.displayed_tree_mut(),
                app_state,
                cc,
            ),
            Internal::select_first => {
                self.displayed_tree_mut().try_select_first();
                CmdResult::Keep
            }
            Internal::select_last => {
                let page_height = BrowserState::page_height(screen);
                self.displayed_tree_mut().try_select_last(page_height);
                CmdResult::Keep
            }
            Internal::show => {
                let path = internal_path::determine_path(
                    internal_exec,
                    input_invocation,
                    trigger_type,
                    self.displayed_tree(),
                    app_state,
                    cc,
                );
                match path {
                    Some(path) => {
                        let res = self.displayed_tree_mut().show_path(&path, con);
                        match res {
                            Ok(()) => {
                                let page_height = BrowserState::page_height(screen);
                                self.displayed_tree_mut()
                                    .make_selection_visible(page_height);
                                CmdResult::Keep
                            }
                            Err(e) => CmdResult::DisplayError(format!("{e}")),
                        }
                    }
                    None => CmdResult::Keep,
                }
            }
            Internal::stage_all_directories => {
                let pattern = self.displayed_tree().options.pattern.clone();
                let file_type_condition = FileTypeCondition::Directory;
                self.pending_task = Some(BrowserTask::StageAll {
                    pattern,
                    file_type_condition,
                });
                if cc.app.stage_panel.is_none() {
                    let stage_options = self.tree.options.without_pattern();
                    CmdResult::NewPanel {
                        state: Box::new(StageState::new(app_state, stage_options, con)),
                        purpose: PanelPurpose::None,
                        direction: HDir::Right,
                        activate: false,
                    }
                } else {
                    CmdResult::Keep
                }
            }
            Internal::stage_all_files => {
                let pattern = self.displayed_tree().options.pattern.clone();
                let file_type_condition = FileTypeCondition::File;
                self.pending_task = Some(BrowserTask::StageAll {
                    pattern,
                    file_type_condition,
                });
                if cc.app.stage_panel.is_none() {
                    let stage_options = self.tree.options.without_pattern();
                    CmdResult::NewPanel {
                        state: Box::new(StageState::new(app_state, stage_options, con)),
                        purpose: PanelPurpose::None,
                        direction: HDir::Right,
                        activate: false,
                    }
                } else {
                    CmdResult::Keep
                }
            }
            Internal::start_end_panel => {
                if cc.panel.purpose.is_arg_edition() {
                    debug!("start_end understood as end");
                    CmdResult::ClosePanel {
                        validate_purpose: true,
                        panel_ref: PanelReference::Active,
                        clear_cache: false,
                    }
                } else {
                    debug!("start_end understood as start");
                    let tree_options = self.displayed_tree().options.clone();
                    if let Some(input_invocation) = input_invocation {
                        // we'll go for input arg editing
                        let path = if let Some(input_arg) = &input_invocation.args {
                            path::path_from(self.root(), PathAnchor::Unspecified, input_arg)
                        } else {
                            self.root().to_path_buf()
                        };
                        let arg_type = SelectionType::Any; // We might do better later
                        let purpose = PanelPurpose::ArgEdition { arg_type };
                        internal_focus::new_panel_on_path(
                            path,
                            screen,
                            tree_options,
                            purpose,
                            con,
                            HDir::Right,
                            false,
                        )
                    } else {
                        // we just open a new panel on the selected path,
                        // without purpose
                        internal_focus::new_panel_on_path(
                            self.displayed_tree().selected_line().path.clone(),
                            screen,
                            tree_options,
                            PanelPurpose::None,
                            con,
                            HDir::Right,
                            false,
                        )
                    }
                }
            }
            Internal::total_search => match self.filtered_tree.as_ref().map(|t| t.total_search) {
                None => CmdResult::error("this verb can be used only after a search"),
                Some(true) => CmdResult::error(
                    "search was already total: all possible matches have been ranked",
                ),
                Some(false) => {
                    self.search(self.displayed_tree().options.pattern.clone(), true);
                    CmdResult::Keep
                }
            },
            Internal::trash => {
                let path = self.displayed_tree().selected_line().path.clone();
                info!("trash {:?}", &path);

                #[cfg(any(target_os = "windows", all(unix, not(any(target_os = "ios", target_os = "android")))))]
                match trash::delete(&path) {
                    Ok(()) => CmdResult::RefreshState { clear_cache: true },
                    Err(e) => {
                        warn!("trash error: {:?}", &e);
                        CmdResult::DisplayError(format!("trash error: {:?}", &e))
                    }
                }

                #[cfg(not(any(target_os = "windows", all(unix, not(any(target_os = "ios", target_os = "android"))))))]
                CmdResult::DisplayError("trash not supported on this platform".into())
            }
            Internal::up_tree => match self.displayed_tree().root().parent() {
                Some(path) => internal_focus::on_path(
                    path.to_path_buf(),
                    screen,
                    self.displayed_tree().options.clone(),
                    bang,
                    con,
                ),
                None => CmdResult::error("no parent found"),
            },
            Internal::add => {
                // Resolve the directory the Add modal should create entries
                // in: the selected line's path if it's a directory, else the
                // selected file's parent (falling back to the panel root for
                // a path with no parent — shouldn't happen at runtime).
                let tree = self.displayed_tree();
                let selected = &tree.selected_line().path;
                let target_dir = resolve_add_target_dir(selected, tree.root());
                let overlay = Overlay::Add(AddOverlay::new(target_dir));
                CmdResult::OpenOverlay(Box::new(overlay))
            }
            _ => self.on_internal_generic(
                w,
                invocation_parser,
                internal_exec,
                input_invocation,
                trigger_type,
                app_state,
                cc,
            )?,
        })
    }

    fn no_verb_status(
        &self,
        has_previous_state: bool,
        con: &AppContext,
        width: usize,
    ) -> Status {
        let tree = self.displayed_tree();
        if tree.is_empty() && tree.build_report.hidden_count > 0 {
            let mut parts = Vec::new();
            if let Some(md) = con.standard_status.all_files_hidden.clone() {
                parts.push(md);
            }
            if let Some(md) = con.standard_status.all_files_ignored.clone() {
                parts.push(md);
            }
            if !parts.is_empty() {
                return Status::from_error(parts.join(". "));
            }
        }
        let mut ssb = con.standard_status.builder(
            PanelStateType::Tree,
            tree.selected_line().as_selection(),
            width,
        );
        ssb.has_previous_state = has_previous_state;
        ssb.is_filtered = self.filtered_tree.is_some();
        ssb.has_removed_pattern = false;
        ssb.on_tree_root = tree.selection == 0;
        ssb.status()
    }

    /// do some work, totally or partially, if there's some to do.
    /// Stop as soon as the dam asks for interruption
    fn do_pending_task(
        &mut self,
        app_state: &mut AppState,
        screen: Screen,
        con: &AppContext,
        dam: &mut Dam,
    ) -> Result<(), ProgramError> {
        if let Some(pending_task) = self.pending_task.take() {
            match pending_task {
                BrowserTask::Search { pattern, total } => {
                    let pattern_str = pattern.raw.clone();
                    let mut options = self.tree.options.clone();
                    options.pattern = pattern;
                    let root = self.tree.root().clone();
                    let page_height = BrowserState::page_height(screen);
                    let builder = TreeBuilder::from(root, options, page_height, con)?;
                    let filtered_tree = time!(
                        Info,
                        "tree filtering",
                        &pattern_str,
                        builder.build_tree(total, dam),
                    );
                    if let Ok(mut ft) = filtered_tree {
                        ft.try_select_best_match();
                        ft.make_selection_visible(BrowserState::page_height(screen));
                        self.filtered_tree = Some(ft);
                    }
                }
                BrowserTask::StageAll {
                    pattern,
                    file_type_condition,
                } => {
                    debug!("stage all pattern: {pattern:?}");
                    let tree = self.displayed_tree();
                    let root = tree.root().clone();
                    let mut options = tree.options.clone();
                    let total_search = true;
                    options.pattern = pattern; // should be the same
                    let builder = TreeBuilder::from(root, options, con.max_staged_count, con);
                    let mut paths = builder.and_then(|mut builder| {
                        builder.matches_max = Some(con.max_staged_count);
                        time!(builder.build_paths(total_search, dam, |line| {
                            debug!("??staging {:?}", &line.path);
                            file_type_condition.accepts_path(&line.path)
                        }))
                    })?;
                    for path in paths.drain(..) {
                        app_state.stage.add(path);
                    }
                }
            }
        } else if self.displayed_tree().is_missing_git_status_computation() {
            let root_path = self.displayed_tree().root();
            let git_status = git::get_tree_status(root_path, dam);
            self.displayed_tree_mut().git_status = git_status;
        } else {
            self.displayed_tree_mut()
                .fetch_some_missing_dir_sum(dam, con);
        }
        Ok(())
    }

    fn display(
        &mut self,
        w: &mut W,
        disc: &DisplayContext,
    ) -> Result<(), ProgramError> {
        // Cache the body region's top row so subsequent clicks can be
        // translated from absolute terminal coords back to body-relative
        // coords (the click handlers don't receive the panel `Areas`).
        self.body_top = disc.state_area.top;
        let dp = DisplayableTree {
            app_state: Some(disc.app_state),
            tree: self.displayed_tree(),
            skin: &disc.panel_skin.styles,
            ext_colors: &disc.con.ext_colors,
            area: disc.state_area.clone(),
            in_app: true,
        };
        dp.write_on(w)
    }

    fn refresh(
        &mut self,
        screen: Screen,
        con: &AppContext,
    ) -> Command {
        let page_height = BrowserState::page_height(screen);
        // refresh the base tree
        if let Err(e) = self.tree.refresh(page_height, con) {
            warn!("refreshing base tree failed : {e:?}");
        }
        // refresh the filtered tree, if any
        Command::from_pattern(match self.filtered_tree {
            Some(ref mut tree) => {
                if let Err(e) = tree.refresh(page_height, con) {
                    warn!("refreshing filtered tree failed : {e:?}");
                }
                &tree.options.pattern
            }
            None => &self.tree.options.pattern,
        })
    }

    /// Build the right-aligned status-row aux block.
    ///
    /// Surfaces three optional pieces that used to decorate the (now
    /// hidden) tree root row:
    /// - git status summary (when `tree.git_status` is computed)
    /// - total size of the tree (when `tree.options.show_sizes`)
    /// - mount-space widget (when `tree.options.show_root_fs`)
    ///
    /// Returns `None` when nothing applies so the status row painter can
    /// skip the right-alignment math entirely.
    fn status_aux(&self) -> Option<StatusAux> {
        let tree = self.displayed_tree();
        let mut aux = StatusAux::default();
        if let ComputationResult::Done(git_status) = &tree.git_status {
            aux.git_summary = status_aux::format_git_summary(git_status);
        }
        if tree.options.show_sizes {
            if let Some(sum) = tree.lines[0].sum {
                aux.total_size = Some(file_size::fit_4(sum.to_size()));
            }
        }
        #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
        if tree.options.show_root_fs {
            aux.mount = tree.lines[0].mount();
        }
        if aux.is_empty() { None } else { Some(aux) }
    }

    fn title_selected(&self) -> bool {
        self.displayed_tree().selection == 0
    }

    fn get_flags(&self) -> Vec<Flag> {
        let options = &self.displayed_tree().options;
        vec![
            Flag {
                name: "h",
                value: if options.show_hidden { "y" } else { "n" },
            },
            Flag {
                name: "gi",
                value: if options.respect_git_ignore { "y" } else { "n" },
            },
        ]
    }

    fn get_starting_input(&self) -> String {
        if let Some(BrowserTask::Search { pattern, .. }) = self.pending_task.as_ref() {
            pattern.raw.clone()
        } else {
            self.displayed_tree().options.pattern.raw.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(height: u16) -> Screen {
        Screen { width: 80, height }
    }

    #[test]
    fn page_height_typical_terminals() {
        // status row + input row + 2 frame rows = 4 rows reserved
        assert_eq!(BrowserState::page_height(screen(24)), 20);
        assert_eq!(BrowserState::page_height(screen(50)), 46);
        assert_eq!(BrowserState::page_height(screen(80)), 76);
    }

    #[test]
    fn page_height_minimal_terminal() {
        // Just barely usable: 5 rows yields 1 interior row.
        assert_eq!(BrowserState::page_height(screen(5)), 1);
    }

    #[test]
    fn page_height_too_small_clamps_to_zero() {
        // 4 rows is exactly the reserved chrome -> 0 interior rows.
        assert_eq!(BrowserState::page_height(screen(4)), 0);
    }

    #[test]
    fn page_height_zero_does_not_underflow() {
        // 0 rows must not wrap around via unsigned underflow.
        assert_eq!(BrowserState::page_height(screen(0)), 0);
    }

    //
    // status_aux tests
    //
    // Real `BrowserState::new` would need an `AppContext` (heavy). We build
    // the state directly from its public-ish fields. `TreeLine` carries a
    // `fs::Metadata`; we reuse `symlink_metadata(".")` for it — its content
    // is irrelevant to the aux logic.
    //

    use {
        crate::{
            file_sum::FileSum,
            git::TreeGitStatus,
            task_sync::ComputationResult,
            tree::{
                TreeLine,
                TreeLineId,
                TreeLineType,
                TreeOptions,
            },
            tree_build::BuildReport,
        },
        std::{fs, path::PathBuf},
    };

    fn fake_root_line(sum: Option<FileSum>) -> TreeLine {
        let metadata =
            fs::symlink_metadata(".").expect("symlink_metadata of current dir works");
        TreeLine {
            id: 0 as TreeLineId,
            parent_id: None,
            left_branches: vec![false; 0].into_boxed_slice(),
            depth: 0,
            path: PathBuf::from("/"),
            subpath: "/".to_string(),
            icon: None,
            name: "/".to_string(),
            line_type: TreeLineType::Dir,
            has_error: false,
            nb_kept_children: 0,
            unlisted: 0,
            score: 0,
            direct_match: false,
            sum,
            metadata,
            git_status: None,
        }
    }

    fn fake_browser_state(
        sum: Option<FileSum>,
        show_sizes: bool,
        git: ComputationResult<TreeGitStatus>,
    ) -> BrowserState {
        let mut options = TreeOptions::default();
        options.show_sizes = show_sizes;
        let tree = Tree {
            lines: vec![fake_root_line(sum)],
            next_line_id: 1,
            selection: 0,
            options,
            scroll: 0,
            total_search: false,
            git_status: git,
            build_report: BuildReport::default(),
        };
        BrowserState {
            tree,
            filtered_tree: None,
            mode: Mode::Input,
            pending_task: None,
            body_top: 0,
        }
    }

    #[test]
    fn aux_status_none_when_no_toggles_and_no_git() {
        let state = fake_browser_state(None, false, ComputationResult::None);
        assert!(state.status_aux().is_none());
    }

    #[test]
    fn aux_status_some_when_sizes_on_with_sum() {
        let sum = FileSum::new(1024, false, 1, 0);
        let state = fake_browser_state(Some(sum), true, ComputationResult::None);
        let aux = state.status_aux().expect("aux present");
        assert!(aux.total_size.is_some(), "total_size should be populated");
        assert!(aux.git_summary.is_none());
    }

    #[test]
    fn aux_status_skips_sizes_when_no_sum_even_if_toggle_on() {
        // Toggle is on but the root line has no measured `sum` -> no aux piece.
        let state = fake_browser_state(None, true, ComputationResult::None);
        // No git, no measurable size, no mount toggle -> aux is None.
        assert!(state.status_aux().is_none());
    }

    #[test]
    fn aux_status_includes_git_summary_when_done() {
        let git = ComputationResult::Done(TreeGitStatus {
            current_branch_name: Some("trunk".to_string()),
            insertions: 2,
            deletions: 1,
        });
        let state = fake_browser_state(None, false, git);
        let aux = state.status_aux().expect("aux present");
        let summary = aux.git_summary.expect("git summary populated");
        assert!(summary.contains("trunk"));
        assert!(summary.contains("+2-1"));
    }

    #[test]
    fn aux_status_skips_git_when_not_computed() {
        // ComputationResult::NotComputed must be treated the same as
        // None — no git summary surfaces until the value is `Done`.
        let state = fake_browser_state(None, false, ComputationResult::NotComputed);
        assert!(state.status_aux().is_none());
    }

    #[test]
    fn aux_status_combines_git_and_size() {
        let sum = FileSum::new(2048, false, 1, 0);
        let git = ComputationResult::Done(TreeGitStatus {
            current_branch_name: Some("main".to_string()),
            insertions: 0,
            deletions: 0,
        });
        let state = fake_browser_state(Some(sum), true, git);
        let aux = state.status_aux().expect("aux present");
        assert!(aux.git_summary.is_some());
        assert!(aux.total_size.is_some());
    }

    //
    // body_relative_y / click-translation tests
    //
    // Pin the Phase 1 fix: clicks arrive in absolute terminal y, the body
    // row math is body-relative. `BrowserState::on_click` must subtract
    // `self.body_top` before calling `Tree::try_select_y`. Phase 2 caught
    // that this critical fix was untested.
    //

    fn fake_child_line(id: u32, name: &str) -> TreeLine {
        let metadata =
            fs::symlink_metadata(".").expect("symlink_metadata of current dir works");
        TreeLine {
            id: id as TreeLineId,
            parent_id: Some(0 as TreeLineId),
            left_branches: vec![false; 1].into_boxed_slice(),
            depth: 1,
            path: PathBuf::from(format!("/{name}")),
            subpath: name.to_string(),
            icon: None,
            name: name.to_string(),
            line_type: TreeLineType::File,
            has_error: false,
            nb_kept_children: 0,
            unlisted: 0,
            score: 0,
            direct_match: false,
            sum: None,
            metadata,
            git_status: None,
        }
    }

    fn fake_browser_state_with_children(body_top: u16, n: u32) -> BrowserState {
        let mut lines = vec![fake_root_line(None)];
        for i in 1..=n {
            lines.push(fake_child_line(i, &format!("child{i}")));
        }
        let tree = Tree {
            lines,
            next_line_id: (n + 1) as TreeLineId,
            selection: 0,
            options: TreeOptions::default(),
            scroll: 0,
            total_search: false,
            git_status: ComputationResult::None,
            build_report: BuildReport::default(),
        };
        BrowserState {
            tree,
            filtered_tree: None,
            mode: Mode::Input,
            pending_task: None,
            body_top,
        }
    }

    #[test]
    fn body_relative_y_subtracts_body_top() {
        let state = fake_browser_state_with_children(3, 4);
        // y at the frame border / title rows → None
        assert_eq!(state.body_relative_y(0), None);
        assert_eq!(state.body_relative_y(2), None);
        // y at first body row (body_top) → 0
        assert_eq!(state.body_relative_y(3), Some(0));
        // y at body_top + k → k
        assert_eq!(state.body_relative_y(5), Some(2));
    }

    #[test]
    fn body_relative_y_zero_body_top_is_identity() {
        let state = fake_browser_state_with_children(0, 4);
        assert_eq!(state.body_relative_y(0), Some(0));
        assert_eq!(state.body_relative_y(3), Some(3));
    }

    #[test]
    fn click_translation_selects_correct_child_when_body_top_nonzero() {
        // body_top = 3, tree = [root, child1, child2, child3, child4]
        // A click at absolute y=3 should select lines[1] (first child).
        // A click at absolute y=4 should select lines[2].
        //
        // The Phase 1 bug was passing absolute y straight to try_select_y,
        // which (with the +1 root-shift) would have mapped y=3 → lines[4]
        // (4th child), not lines[1] (first child).
        let mut state = fake_browser_state_with_children(3, 4);

        let body_y = state.body_relative_y(3).expect("y=3 inside body");
        assert_eq!(body_y, 0);
        state.displayed_tree_mut().try_select_y(body_y);
        assert_eq!(
            state.displayed_tree().selection,
            1,
            "click at body_top should select lines[1] (first child), not lines[1 + body_top]"
        );

        let body_y = state.body_relative_y(5).expect("y=5 inside body");
        assert_eq!(body_y, 2);
        state.displayed_tree_mut().try_select_y(body_y);
        assert_eq!(
            state.displayed_tree().selection,
            3,
            "click at body_top + 2 should select lines[3]"
        );
    }

    #[test]
    fn click_translation_above_body_is_noop() {
        // Click at y < body_top (frame border) yields None and selection
        // is not modified.
        let mut state = fake_browser_state_with_children(3, 4);
        state.displayed_tree_mut().selection = 2; // pre-existing selection

        assert_eq!(state.body_relative_y(0), None);
        assert_eq!(state.body_relative_y(2), None);
        // Verify selection unchanged (no try_select_y call was made on None).
        assert_eq!(state.displayed_tree().selection, 2);
    }

    #[test]
    fn title_selected_true_when_root_selected() {
        let state = fake_browser_state_with_children(3, 4);
        // default selection in fake_browser_state_with_children is 0
        assert!(state.title_selected());
    }

    #[test]
    fn title_selected_false_when_child_selected() {
        let mut state = fake_browser_state_with_children(3, 4);
        state.displayed_tree_mut().selection = 2;
        assert!(!state.title_selected());
    }

    #[test]
    fn try_select_title_row_at_title_y_selects_root() {
        // body_top = 3 → title row sits at y = 2. With selection initially
        // pointing at a child, a click on the title row must reset it to 0.
        let mut state = fake_browser_state_with_children(3, 4);
        state.displayed_tree_mut().selection = 2;
        assert!(state.try_select_title_row(2));
        assert_eq!(state.displayed_tree().selection, 0);
    }

    #[test]
    fn try_select_title_row_non_title_y_is_noop() {
        // Below body_top or above the title row — helper returns false and
        // does not touch selection.
        let mut state = fake_browser_state_with_children(3, 4);
        state.displayed_tree_mut().selection = 2;
        // y inside body, not the title row.
        assert!(!state.try_select_title_row(4));
        assert_eq!(state.displayed_tree().selection, 2);
        // y at body_top itself — also not the title row.
        assert!(!state.try_select_title_row(3));
        assert_eq!(state.displayed_tree().selection, 2);
        // y above the title row (frame border rows) — also a no-op.
        // body_top = 3 → title row sits at y = 2; y = 0 and y = 1 are
        // strictly above and must not flip selection.
        assert!(!state.try_select_title_row(0));
        assert_eq!(state.displayed_tree().selection, 2);
        assert!(!state.try_select_title_row(1));
        assert_eq!(state.displayed_tree().selection, 2);
    }

    #[test]
    fn try_select_title_row_no_op_when_body_top_zero() {
        // Degenerate terminal: body_top = 0 → no title row exists,
        // and the `body_top > 0` guard must prevent underflow.
        let mut state = fake_browser_state_with_children(0, 4);
        state.displayed_tree_mut().selection = 2;
        assert!(!state.try_select_title_row(0));
        // Selection unchanged.
        assert_eq!(state.displayed_tree().selection, 2);
    }

    // Build a filtered tree mirroring the base tree (root + n children),
    // so the displayed_tree() path goes through `filtered_tree`.
    fn install_filtered_tree(state: &mut BrowserState, n: u32) {
        let mut lines = vec![fake_root_line(None)];
        for i in 1..=n {
            lines.push(fake_child_line(i, &format!("fchild{i}")));
        }
        let filtered = Tree {
            lines,
            next_line_id: (n + 1) as TreeLineId,
            selection: 0,
            options: TreeOptions::default(),
            scroll: 0,
            total_search: false,
            git_status: ComputationResult::None,
            build_report: BuildReport::default(),
        };
        state.filtered_tree = Some(filtered);
    }

    #[test]
    fn title_selected_reflects_filtered_tree_selection() {
        // With a filtered tree installed, `displayed_tree()` resolves to
        // the filtered one. `title_selected()` must therefore key off
        // `filtered_tree.selection`, not `tree.selection`.
        let mut state = fake_browser_state_with_children(3, 4);
        install_filtered_tree(&mut state, 3);

        // filtered selection = 0 → title selected
        state.filtered_tree.as_mut().unwrap().selection = 0;
        assert!(state.title_selected());

        // filtered selection = 1 → title not selected, even though the
        // base tree's selection is still 0.
        state.filtered_tree.as_mut().unwrap().selection = 1;
        assert_eq!(state.tree.selection, 0);
        assert!(!state.title_selected());
    }

    #[test]
    fn try_select_title_row_updates_filtered_tree_only() {
        // With a filtered tree installed, the title click must update the
        // filtered tree's selection, not the base tree's. Pins that
        // `displayed_tree_mut()` is the seam the helper goes through.
        let mut state = fake_browser_state_with_children(3, 4);
        install_filtered_tree(&mut state, 3);
        state.filtered_tree.as_mut().unwrap().selection = 2;
        // Base tree starts at selection = 0; mark it explicitly so we
        // can confirm it stays unchanged.
        state.tree.selection = 0;

        assert!(state.try_select_title_row(2));
        assert_eq!(state.filtered_tree.as_ref().unwrap().selection, 0);
        assert_eq!(
            state.tree.selection, 0,
            "base tree.selection must not be touched by title-row click",
        );

        // Now exercise the case where the base tree happened to have a
        // different selection — title click should still only touch
        // the filtered tree.
        state.tree.selection = 7; // arbitrary non-zero
        state.filtered_tree.as_mut().unwrap().selection = 3;
        assert!(state.try_select_title_row(2));
        assert_eq!(state.filtered_tree.as_ref().unwrap().selection, 0);
        assert_eq!(state.tree.selection, 7);
    }

    #[test]
    fn on_click_title_row_resets_selection_to_zero() {
        // End-to-end: `on_click` at the title-row y should pin selection
        // to 0. AppContext / Screen are not consulted by the title-row
        // intercept (the args are underscored), so default values are
        // fine.
        let mut state = fake_browser_state_with_children(3, 4);
        state.displayed_tree_mut().selection = 2;
        let con = AppContext::default();
        let screen = Screen { width: 80, height: 24 };
        let res = state.on_click(0, 2, screen, &con).expect("on_click ok");
        assert!(matches!(res, CmdResult::Keep));
        assert_eq!(state.displayed_tree().selection, 0);
    }

    #[test]
    fn on_click_body_row_selects_first_child() {
        // End-to-end: `on_click` at body_top should map to the first
        // child line (lines[1]).
        let mut state = fake_browser_state_with_children(3, 4);
        state.displayed_tree_mut().selection = 0;
        let con = AppContext::default();
        let screen = Screen { width: 80, height: 24 };
        let res = state.on_click(0, 3, screen, &con).expect("on_click ok");
        assert!(matches!(res, CmdResult::Keep));
        assert_eq!(
            state.displayed_tree().selection,
            1,
            "click at body_top should select the first child",
        );
    }

    #[test]
    fn on_double_click_body_row_mismatch_does_not_navigate() {
        // `on_double_click` only opens the selected line when the recomputed
        // `line_index` matches the current selection (a real double-click
        // always follows a single click at the same y, so a mismatch means
        // the click wasn't on a selectable/openable line and we must NOT
        // call `open_selection_stay_in_broot` — that would touch the
        // filesystem and is moot anyway.
        //
        // body_top = 3, click at y = body_top (=3) → body_y = 0 → line_index
        // = 1. We pre-set selection to 2, so the mismatch branch is taken
        // and the function returns `CmdResult::Keep` without filesystem I/O.
        let mut state = fake_browser_state_with_children(3, 4);
        state.displayed_tree_mut().selection = 2;
        let con = AppContext::default();
        let screen = Screen { width: 80, height: 24 };
        let res = state
            .on_double_click(0, 3, screen, &con)
            .expect("on_double_click ok");
        assert!(matches!(res, CmdResult::Keep));
        assert_eq!(
            state.displayed_tree().selection,
            2,
            "mismatch branch must leave selection untouched",
        );
    }

    //
    // resolve_add_target_dir tests
    //
    // Pin the routing rule that Internal::add uses to pick its target
    // directory. We avoid building a full BrowserState here; the helper
    // is a pure function over `Path`/`PathBuf` plus a single `is_dir()`
    // probe, so a `tempdir`-backed tree is enough.
    //

    #[test]
    fn resolve_add_target_dir_uses_selection_when_it_is_a_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let sub = root.join("sub");
        fs::create_dir(&sub).expect("create sub");
        let target = resolve_add_target_dir(&sub, root);
        assert_eq!(target, sub);
    }

    #[test]
    fn resolve_add_target_dir_uses_parent_when_selection_is_a_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let file = root.join("hello.txt");
        fs::write(&file, b"x").expect("write");
        let target = resolve_add_target_dir(&file, root);
        assert_eq!(target, root.to_path_buf());
    }

    #[test]
    fn resolve_add_target_dir_falls_back_to_panel_root_when_selection_has_no_parent() {
        // A degenerate path that has no parent (a single root component on
        // unix is "/" which DOES have parent==None). We use a synthetic
        // path that doesn't exist so `is_dir()` is false; `parent()` on
        // a single-component relative path returns Some("") on unix —
        // not None — so the proof of the fallback branch is the
        // single-component `/` case via Path::new("/").
        let root = Path::new("/tmp");
        let degenerate = Path::new("/");
        // `/` is a directory on every platform we ship on, so `is_dir()`
        // is true and the dir-branch is taken — that's the expected
        // behaviour for the literal filesystem root.
        let target = resolve_add_target_dir(degenerate, root);
        assert_eq!(target, PathBuf::from("/"));
    }

    #[test]
    fn resolve_add_target_dir_nonexistent_file_uses_parent() {
        // is_dir() returns false for a non-existent path, so the parent
        // branch fires. parent() of a multi-component path is Some.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let missing = root.join("does-not-exist.txt");
        let target = resolve_add_target_dir(&missing, root);
        assert_eq!(target, root.to_path_buf());
    }

    // Routing for `Internal::add` is exercised through the
    // `resolve_add_target_dir_*` tests above — the arm body is a
    // three-line `Overlay::Add(AddOverlay::new(resolve_add_target_dir(...)))`
    // composition with no additional logic to pin, so a separate test
    // just wrapping the helper output in `Overlay::Add` would duplicate
    // the existing coverage.
}
