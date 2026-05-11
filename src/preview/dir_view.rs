use {
    crate::{
        app::{
            AppContext,
            DisplayContext,
        },
        command::ScrollCommand,
        display::{
            DisplayableTree,
            W,
        },
        errors::ProgramError,
        pattern::InputPattern,
        task_sync::Dam,
        tree::{
            Tree,
            TreeOptions,
        },
        tree_build::TreeBuilder,
    },
    std::{
        io,
        path::PathBuf,
    },
    termimad::Area,
};

pub struct DirView {
    pub tree: Tree,
    page_height: Option<usize>,
}
impl DirView {
    pub fn new(
        dir: PathBuf,
        pattern: InputPattern,
        dam: &Dam,
        con: &AppContext,
    ) -> Result<Self, io::Error> {
        let options = TreeOptions {
            show_hidden: true,
            respect_git_ignore: false,
            pattern,
            ..Default::default()
        };
        let mut builder = TreeBuilder::from(dir, options, 100, con).map_err(io::Error::other)?;
        builder.deep = false;
        let tree = builder
            .build_tree(
                false, // on refresh we always do a non total search
                dam,
            )
            .map_err(io::Error::other)?;
        Ok(Self {
            tree,
            page_height: None,
        })
    }
    pub fn display(
        &mut self,
        w: &mut W,
        disc: &DisplayContext,
        area: &Area,
    ) -> Result<(), ProgramError> {
        let page_height = area.height as usize;
        if Some(page_height) != self.page_height {
            self.page_height = Some(page_height);
        }
        let dp = DisplayableTree {
            app_state: None,
            tree: &self.tree,
            skin: &disc.panel_skin.styles,
            ext_colors: &disc.con.ext_colors,
            area: area.clone(),
            in_app: true,
        };
        dp.write_on(w)?;
        Ok(())
    }
    /// Returns `"{N} entries"` where `N` is the number of children
    /// (`tree.lines.len() - 1`). The root row is excluded from the count
    /// because the body no longer paints it (the root is named in the
    /// frame title), so "entries" matches the rows the user actually sees.
    pub fn info_string(&self) -> Option<String> {
        Some(format!(
            "{} entries",
            self.tree.lines.len().saturating_sub(1),
        ))
    }
    pub fn try_scroll(
        &mut self,
        cmd: ScrollCommand,
    ) -> bool {
        let Some(page_height) = self.page_height else {
            return false;
        };
        let dy = cmd.to_lines(page_height);
        self.tree.try_scroll(dy, page_height)
    }
    pub fn try_select_y(
        &mut self,
        y: u16,
    ) -> bool {
        self.tree.try_select_y(y as usize)
    }
    pub fn move_selection(
        &mut self,
        dy: i32,
        cycle: bool,
    ) {
        if let Some(page_height) = self.page_height {
            self.tree.move_selection(dy, page_height, cycle);
        }
    }
    pub fn select_first(&mut self) {
        self.tree.try_select_first();
    }
    pub fn select_last(&mut self) {
        if let Some(page_height) = self.page_height {
            self.tree.try_select_last(page_height);
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::{
            task_sync::ComputationResult,
            tree::{TreeLine, TreeLineId, TreeLineType, TreeOptions},
            tree_build::BuildReport,
        },
        std::fs,
    };

    /// Build a `DirView` directly for unit testing, bypassing
    /// `DirView::new` (which needs an `AppContext` and a real
    /// directory). The tree has only the root line + the requested
    /// children; metadata is shared from `symlink_metadata(".")`.
    fn fake_dir_view(n_children: usize) -> DirView {
        let metadata =
            fs::symlink_metadata(".").expect("symlink_metadata of current dir works");
        let mut lines = Vec::with_capacity(n_children + 1);
        let mk = |id: TreeLineId, name: &str, line_type: TreeLineType| TreeLine {
            id,
            parent_id: if id == 0 { None } else { Some(0) },
            left_branches: vec![false; 0].into_boxed_slice(),
            depth: if id == 0 { 0 } else { 1 },
            path: PathBuf::from(name),
            subpath: name.to_string(),
            icon: None,
            name: name.to_string(),
            line_type,
            has_error: false,
            nb_kept_children: 0,
            unlisted: 0,
            score: 0,
            direct_match: false,
            sum: None,
            metadata: metadata.clone(),
            git_status: None,
        };
        lines.push(mk(0, "/", TreeLineType::Dir));
        for i in 1..=n_children {
            lines.push(mk(i, &format!("c{i}"), TreeLineType::File));
        }
        let tree = Tree {
            lines,
            next_line_id: n_children + 1,
            selection: 0,
            options: TreeOptions::default(),
            scroll: 0,
            total_search: false,
            git_status: ComputationResult::None,
            build_report: BuildReport::default(),
        };
        DirView {
            tree,
            page_height: None,
        }
    }

    #[test]
    fn info_string_counts_children_not_root() {
        // 3 children + 1 root = 4 lines → reported as "3 entries"
        // (root is in the frame title, not the body, so it isn't
        // counted as an "entry" in the title's summary clause).
        let dv = fake_dir_view(3);
        assert_eq!(dv.info_string(), Some("3 entries".to_string()));
    }

    #[test]
    fn info_string_zero_children() {
        // 0 children + 1 root = 1 line → "0 entries". The
        // saturating_sub guards against the empty-tree edge case.
        let dv = fake_dir_view(0);
        assert_eq!(dv.info_string(), Some("0 entries".to_string()));
    }
}
