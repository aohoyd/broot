use {
    super::*,
    crate::{
        app::*,
        command::{
            Command,
            ScrollCommand,
            TriggerType,
        },
        display::{
            Screen,
            W,
        },
        errors::ProgramError,
        flag::Flag,
        pattern::InputPattern,
        task_sync::Dam,
        tree::TreeOptions,
        verb::*,
    },
    std::path::{
        Path,
        PathBuf,
    },
    termimad::Area,
};

/// an application state dedicated to previewing files.
///
/// It's usually the only state in its panel and is kept when the
/// selection changes (other panels indirectly call `set_selected_path`).
pub struct PreviewState {
    pub preview_area: Area,
    dirty: bool,          // true when background must be cleared
    source_path: PathBuf, // path to the file whose preview is requested
    transform: Option<PreviewTransform>,
    preview: Preview,
    pending_pattern: InputPattern, // a pattern (or not) which has not yet be applied
    filtered_preview: Option<Preview>,
    removed_pattern: InputPattern,
    preferred_mode: Option<PreviewMode>,
    tree_options: TreeOptions,
    mode: Mode,
}

impl PreviewState {
    pub fn new(
        source_path: PathBuf,
        pending_pattern: InputPattern,
        line: LineNumber,
        preferred_mode: Option<PreviewMode>,
        tree_options: TreeOptions,
        con: &AppContext,
    ) -> PreviewState {
        let preview_area = Area::uninitialized(); // will be fixed at drawing time
        let transform = con
            .preview_transformers
            .transform(&source_path, preferred_mode);
        let preview_path = transform
            .as_ref()
            .map(|c| &c.output_path)
            .unwrap_or(&source_path);
        let mut preview = Preview::new(preview_path, preferred_mode, con);
        if line > 0 {
            preview.try_select_line_number(line);
        }
        PreviewState {
            preview_area,
            dirty: true,
            source_path,
            transform,
            preview,
            pending_pattern,
            filtered_preview: None,
            removed_pattern: InputPattern::none(),
            preferred_mode,
            tree_options,
            mode: con.initial_mode(),
        }
    }
    pub fn preview_path(&self) -> &Path {
        self.transform
            .as_ref()
            .map(|c| &c.output_path)
            .unwrap_or(&self.source_path)
    }
    fn vis_preview(&self) -> &Preview {
        self.filtered_preview.as_ref().unwrap_or(&self.preview)
    }
    fn mut_preview(&mut self) -> &mut Preview {
        self.filtered_preview.as_mut().unwrap_or(&mut self.preview)
    }
    fn set_mode(
        &mut self,
        mode: PreviewMode,
        con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        if self.preview.get_mode() == Some(mode) {
            return Ok(CmdResult::Keep);
        }
        Ok(match Preview::with_mode(self.preview_path(), mode, con) {
            Ok(preview) => {
                self.preview = preview;
                self.preferred_mode = Some(mode);
                CmdResult::Keep
            }
            Err(e) => CmdResult::DisplayError(format!("Can't display as {mode:?} : {e:?}")),
        })
    }

    fn no_opt_selection(&self) -> Selection<'_> {
        match self.transform.as_ref() {
            // When there's a transform, we can't assume the line number makes sense
            Some(transform) => Selection {
                path: &transform.output_path,
                stype: SelectionType::File,
                is_exe: false,
                line: 0,
            },
            None => Selection {
                path: &self.source_path,
                stype: SelectionType::File,
                is_exe: false,
                line: self.vis_preview().get_selected_line_number().unwrap_or(0),
            },
        }
    }

    /// do the preview filtering if required and not yet done
    fn do_pending_search(
        &mut self,
        con: &AppContext,
        dam: &mut Dam,
    ) -> Result<(), ProgramError> {
        let old_selection = self
            .filtered_preview
            .as_ref()
            .and_then(|p| p.get_selected_line_number())
            .or_else(|| self.preview.get_selected_line_number());
        let pattern = self.pending_pattern.take();
        self.filtered_preview = time!(
            Info,
            "preview filtering",
            self.preview
                .filtered(self.preview_path(), pattern, dam, con),
        ); // can be None if a cancellation was required
        if let Some(ref mut filtered_preview) = self.filtered_preview {
            if let Some(number) = old_selection {
                filtered_preview.try_select_line_number(number);
            }
        }
        Ok(())
    }
}

impl PanelState for PreviewState {
    fn get_type(&self) -> PanelStateType {
        PanelStateType::Preview
    }

    /// Override the default frame title with `{filename}  •  {info}`,
    /// where `info` is the variant's `info_string` (e.g. `"42 lines"`,
    /// `"1024 bytes"`). When `info` is `None` the title is just the
    /// filename. When the composed title overflows `max_w`, only the
    /// filename gets truncated with `…`; the info clause is preserved.
    fn frame_title(
        &self,
        max_w: u16,
    ) -> String {
        let filename = self
            .source_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "???".to_string());
        let info = self.preview.info_string();
        let max_w = max_w as usize;
        match info {
            Some(info) => {
                let separator = "  •  ";
                // Measure visual width, not char count — the bullet is
                // one cell but multi-byte; `unicode_width` is the source
                // of truth used elsewhere in the frame layer.
                let info_width = unicode_width::UnicodeWidthStr::width(info.as_str())
                    + unicode_width::UnicodeWidthStr::width(separator);
                if info_width >= max_w {
                    // Info alone (with separator) wouldn't fit — fall back
                    // to filename-only truncation.
                    crate::display::frame::truncate_to_width(&filename, max_w)
                } else {
                    let filename_max = max_w - info_width;
                    let truncated_filename =
                        crate::display::frame::truncate_to_width(&filename, filename_max);
                    format!("{truncated_filename}{separator}{info}")
                }
            }
            None => crate::display::frame::truncate_to_width(&filename, max_w),
        }
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
        if self.preview.is_partial() {
            Some("loading")
        } else if self.pending_pattern.is_some() {
            Some("searching")
        } else {
            None
        }
    }

    fn on_pattern(
        &mut self,
        pat: InputPattern,
        _app_state: &AppState,
        _con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        if pat.is_none() {
            if let Some(filtered_preview) = self.filtered_preview.take() {
                let old_selection = filtered_preview.get_selected_line_number();
                if let Some(number) = old_selection {
                    self.preview.try_select_line_number(number);
                }
                self.removed_pattern = filtered_preview.pattern();
            }
        } else if !self.preview.is_filterable() {
            return Ok(CmdResult::error("this preview can't be searched"));
        }
        self.pending_pattern = pat;
        Ok(CmdResult::Keep)
    }

    fn do_pending_task(
        &mut self,
        _app_state: &mut AppState,
        _screen: Screen,
        con: &AppContext,
        dam: &mut Dam,
    ) -> Result<(), ProgramError> {
        if self.preview.is_partial() {
            self.preview.complete_loading(con, dam)?;
        } else if self.pending_pattern.is_some() {
            self.do_pending_search(con, dam)?;
        }
        Ok(())
    }

    fn selected_path(&self) -> Option<&Path> {
        Some(&self.source_path)
    }

    fn set_selected_path(
        &mut self,
        path: PathBuf,
        line: LineNumber,
        con: &AppContext,
    ) {
        let selected_line_number = if line > 0 {
            Some(line)
        } else if self.source_path == path {
            self.filtered_preview
                .as_ref()
                .and_then(|p| p.get_selected_line_number())
                .or_else(|| self.preview.get_selected_line_number())
        } else {
            None
        };
        if let Some(fp) = &self.filtered_preview {
            self.pending_pattern = fp.pattern();
        };
        self.transform = con
            .preview_transformers
            .transform(&path, self.preferred_mode);
        let preview_path = self.transform.as_ref().map_or(&path, |c| &c.output_path);
        self.preview = Preview::new(preview_path, self.preferred_mode, con);
        if let Some(number) = selected_line_number {
            self.preview.try_select_line_number(number);
        }
        self.source_path = path;
    }

    fn selection(&self) -> Option<Selection<'_>> {
        Some(self.no_opt_selection())
    }

    fn tree_options(&self) -> TreeOptions {
        self.tree_options.clone()
    }

    fn with_new_options(
        &mut self,
        _screen: Screen,
        change_options: &dyn Fn(&mut TreeOptions) -> &'static str,
        _in_new_panel: bool, // TODO open tree if true
        _con: &AppContext,
    ) -> CmdResult {
        change_options(&mut self.tree_options);
        CmdResult::Keep
    }

    fn refresh(
        &mut self,
        _screen: Screen,
        con: &AppContext,
    ) -> Command {
        self.dirty = true;
        self.set_selected_path(self.source_path.clone(), 0, con);
        Command::empty()
    }

    fn on_click(
        &mut self,
        _x: u16,
        y: u16,
        _screen: Screen,
        _con: &AppContext,
    ) -> Result<CmdResult, ProgramError> {
        if y >= self.preview_area.top && y < self.preview_area.top + self.preview_area.height {
            let y = y - self.preview_area.top;
            self.mut_preview().try_select_y(y);
        }
        Ok(CmdResult::Keep)
    }

    fn display(
        &mut self,
        w: &mut W,
        disc: &DisplayContext,
    ) -> Result<(), ProgramError> {
        let state_area = &disc.state_area;
        if state_area.height < 2 {
            warn!("area too small for preview");
            return Ok(());
        }
        // The filename + info that used to live on body row 0 now lives in
        // the frame top-border title (see `frame_title` below). The body
        // content therefore fills the full `state_area`.
        let preview_area = state_area.clone();
        if preview_area != self.preview_area {
            self.dirty = true;
            self.preview_area = preview_area;
        }
        if self.dirty {
            disc.panel_skin.styles.default.queue_bg(w)?;
            disc.screen.clear_area_to_right(w, state_area)?;
            self.dirty = false;
        }
        let preview = self.filtered_preview.as_mut().unwrap_or(&mut self.preview);
        if let Err(err) = preview.display(w, disc, &self.preview_area) {
            warn!("error while displaying file: {:?}", &err);
            if preview.get_mode().is_some() {
                // means it's not an error already
                if let ProgramError::Io { source } = err {
                    // we mutate the preview to Preview::IOError
                    self.preview = Preview::IoError(source);
                    return self.display(w, disc);
                }
            }
            return Err(err);
        }
        Ok(())
    }

    fn no_verb_status(
        &self,
        has_previous_state: bool,
        con: &AppContext,
        width: usize, // available width
    ) -> Status {
        let mut ssb =
            con.standard_status
                .builder(PanelStateType::Preview, self.no_opt_selection(), width);
        ssb.has_previous_state = has_previous_state;
        ssb.is_filtered = self.filtered_preview.is_some();
        ssb.has_removed_pattern = self.removed_pattern.is_some();
        ssb.status()
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
        let con = &cc.app.con;
        match internal_exec.internal {
            Internal::back => {
                if self.filtered_preview.is_some() {
                    self.on_pattern(InputPattern::none(), app_state, con)
                } else {
                    Ok(CmdResult::PopState)
                }
            }
            Internal::copy_line => {
                #[cfg(not(feature = "clipboard"))]
                {
                    Ok(CmdResult::error(
                        "Clipboard feature not enabled at compilation",
                    ))
                }
                #[cfg(feature = "clipboard")]
                {
                    Ok(match self.mut_preview().get_selected_line() {
                        Some(line) => match terminal_clipboard::set_string(line) {
                            Ok(()) => CmdResult::Keep,
                            Err(_) => CmdResult::error("Clipboard error while copying path"),
                        },
                        None => CmdResult::error("No selected line in preview"),
                    })
                }
            }
            Internal::line_down => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.mut_preview().move_selection(count, true);
                Ok(CmdResult::Keep)
            }
            Internal::line_up => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.mut_preview().move_selection(-count, true);
                Ok(CmdResult::Keep)
            }
            Internal::line_down_no_cycle => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.mut_preview().move_selection(count, false);
                Ok(CmdResult::Keep)
            }
            Internal::line_up_no_cycle => {
                let count = get_arg(input_invocation, internal_exec, 1);
                self.mut_preview().move_selection(-count, false);
                Ok(CmdResult::Keep)
            }
            Internal::page_down => {
                self.mut_preview().try_scroll(ScrollCommand::Pages(1));
                Ok(CmdResult::Keep)
            }
            Internal::page_up => {
                self.mut_preview().try_scroll(ScrollCommand::Pages(-1));
                Ok(CmdResult::Keep)
            }
            //Internal::restore_pattern => {
            //    debug!("restore_pattern");
            //    self.pending_pattern = self.removed_pattern.take();
            //    Ok(CmdResult::Keep)
            //}
            Internal::panel_left if self.removed_pattern.is_some() => {
                self.pending_pattern = self.removed_pattern.take();
                Ok(CmdResult::Keep)
            }
            Internal::panel_left_no_open if self.removed_pattern.is_some() => {
                self.pending_pattern = self.removed_pattern.take();
                Ok(CmdResult::Keep)
            }
            Internal::panel_right if self.filtered_preview.is_some() => {
                self.on_pattern(InputPattern::none(), app_state, con)
            }
            Internal::panel_right_no_open if self.filtered_preview.is_some() => {
                self.on_pattern(InputPattern::none(), app_state, con)
            }
            Internal::select_first => {
                self.mut_preview().select_first();
                Ok(CmdResult::Keep)
            }
            Internal::select_last => {
                self.mut_preview().select_last();
                Ok(CmdResult::Keep)
            }
            Internal::previous_match => {
                self.mut_preview().previous_match();
                Ok(CmdResult::Keep)
            }
            Internal::next_match => {
                self.mut_preview().next_match();
                Ok(CmdResult::Keep)
            }
            Internal::preview_image => self.set_mode(PreviewMode::Image, con),
            Internal::preview_text => self.set_mode(PreviewMode::Text, con),
            Internal::preview_tty => self.set_mode(PreviewMode::Tty, con),
            Internal::preview_binary => self.set_mode(PreviewMode::Hex, con),
            _ => self.on_internal_generic(
                w,
                invocation_parser,
                internal_exec,
                input_invocation,
                trigger_type,
                app_state,
                cc,
            ),
        }
    }

    fn get_flags(&self) -> Vec<Flag> {
        vec![]
    }

    fn get_starting_input(&self) -> String {
        if let Some(preview) = &self.filtered_preview {
            preview.pattern().raw
        } else {
            self.pending_pattern.raw.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            app::Mode,
            hex::HexView,
            tree::TreeOptions,
        },
        std::path::PathBuf,
    };

    /// Build a `PreviewState` directly for unit testing, bypassing the
    /// regular `new` constructor (which needs an `AppContext`). Only the
    /// fields read by `frame_title` need to be meaningful; the rest are
    /// filled with defaults / inert values.
    fn make_state(
        source_path: PathBuf,
        preview: Preview,
    ) -> PreviewState {
        PreviewState {
            preview_area: Area::uninitialized(),
            dirty: true,
            source_path,
            transform: None,
            preview,
            pending_pattern: InputPattern::none(),
            filtered_preview: None,
            removed_pattern: InputPattern::none(),
            preferred_mode: None,
            tree_options: TreeOptions::default(),
            mode: Mode::Input,
        }
    }

    #[test]
    fn frame_title_filename_only_when_info_none() {
        // ZeroLen's `info_string` returns None — frame title is just the
        // filename.
        let path = PathBuf::from("/tmp/zero.dat");
        let preview = Preview::ZeroLen(ZeroLenFileView::new(path.clone()));
        let state = make_state(path, preview);
        assert_eq!(state.frame_title(80), "zero.dat");
    }

    #[test]
    fn frame_title_filename_plus_info_fits() {
        // HexView reports `"{len} bytes"` — round trip through
        // `frame_title` produces `"{filename}  •  {N} bytes"`.
        let mut path = std::env::temp_dir();
        path.push("broot_preview_frame_title_fits_test.bin");
        std::fs::write(&path, b"1234567890").expect("write tempfile");
        let hv = HexView::new(path.clone()).expect("hex view");
        let preview = Preview::Hex(hv);
        let state = make_state(path.clone(), preview);
        let title = state.frame_title(100);
        let basename = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap();
        assert_eq!(title, format!("{basename}  •  10 bytes"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn frame_title_truncates_filename_only() {
        // Long filename + info; max_w too small for both. The filename
        // must be the only thing that gets `…` truncation; the info
        // clause must be preserved verbatim.
        let mut path = std::env::temp_dir();
        path.push("broot_preview_frame_title_trunc_test_a_very_long_filename_for_truncation_testing.bin");
        std::fs::write(&path, b"12345").expect("write tempfile");
        let hv = HexView::new(path.clone()).expect("hex view");
        let preview = Preview::Hex(hv);
        let state = make_state(path.clone(), preview);
        let title = state.frame_title(40);
        // Width-bound: total title must fit in 40 columns.
        assert!(
            unicode_width::UnicodeWidthStr::width(title.as_str()) <= 40,
            "title width exceeded 40: {:?}",
            title,
        );
        // Separator and info clause must be present verbatim.
        assert!(
            title.contains("  •  5 bytes"),
            "info clause missing or truncated in: {:?}",
            title,
        );
        // The filename portion must be the part that got truncated.
        assert!(
            title.contains('…'),
            "expected truncation indicator in: {:?}",
            title,
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn frame_title_info_overflow_falls_back_to_filename() {
        // `max_w` is smaller than `"  •  {info}"` would need. The
        // implementation falls back to filename-only truncation rather
        // than emitting an empty filename + info.
        let mut path = std::env::temp_dir();
        path.push("broot_preview_frame_title_overflow_test_a_b_c.bin");
        std::fs::write(&path, b"12345").expect("write tempfile");
        let hv = HexView::new(path.clone()).expect("hex view");
        let preview = Preview::Hex(hv);
        let state = make_state(path.clone(), preview);
        // `"  •  5 bytes"` = 12 cols. max_w = 8 forces fallback.
        let title = state.frame_title(8);
        assert!(
            unicode_width::UnicodeWidthStr::width(title.as_str()) <= 8,
            "title width exceeded 8: {:?}",
            title,
        );
        // No bullet separator in fallback form.
        assert!(
            !title.contains('•'),
            "fallback path should not include the separator, got: {:?}",
            title,
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn frame_title_no_filename_uses_placeholder() {
        // A path with no file_name component (e.g. `/`) falls back to
        // the `???` placeholder.
        let path = PathBuf::from("/");
        let preview = Preview::ZeroLen(ZeroLenFileView::new(path.clone()));
        let state = make_state(path, preview);
        assert_eq!(state.frame_title(80), "???");
    }

    #[test]
    fn frame_title_max_w_zero_returns_empty() {
        // Boundary: max_w == 0 → nothing visible. Width must be 0.
        let path = PathBuf::from("/tmp/zero.dat");
        let preview = Preview::ZeroLen(ZeroLenFileView::new(path.clone()));
        let state = make_state(path, preview);
        let title = state.frame_title(0);
        assert_eq!(
            unicode_width::UnicodeWidthStr::width(title.as_str()),
            0,
            "title at max_w=0 must be width-0, got: {:?}",
            title,
        );
    }

    #[test]
    fn frame_title_max_w_one_fits_in_one_column() {
        // Boundary: max_w == 1. The filename "zero.dat" is 8 columns,
        // so it must be truncated to fit in 1 — `truncate_to_width`
        // returns just "…". Pin the exact value so a regression that
        // returns "" instead of "…" is caught.
        let path = PathBuf::from("/tmp/zero.dat");
        let preview = Preview::ZeroLen(ZeroLenFileView::new(path.clone()));
        let state = make_state(path, preview);
        let title = state.frame_title(1);
        assert_eq!(title, "\u{2026}");
    }

    #[test]
    fn frame_title_info_overflow_exact_boundary() {
        // Boundary: `info_width == max_w`. The current implementation
        // uses `>=`, which falls back to filename-only at the exact
        // equality. Pin this so a future change to `>` would be
        // caught.
        //
        // "  •  5 bytes" = 12 cols (2 + 1 + 2 + 7); set max_w = 12.
        let mut path = std::env::temp_dir();
        path.push("broot_preview_frame_title_boundary_test.bin");
        std::fs::write(&path, b"12345").expect("write tempfile");
        let hv = HexView::new(path.clone()).expect("hex view");
        let preview = Preview::Hex(hv);
        let state = make_state(path.clone(), preview);
        let title = state.frame_title(12);
        // At equality, fallback path: no `•` in the title.
        assert!(
            !title.contains('•'),
            "exact-boundary should fall back to filename-only, got: {:?}",
            title,
        );
        let _ = std::fs::remove_file(&path);
    }
}
