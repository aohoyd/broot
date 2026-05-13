use {
    super::*,
    crate::{
        app::AppContext,
        errors::TreeBuildError,
        file_sum::FileSum,
        git::TreeGitStatus,
        task_sync::{
            ComputationResult,
            Dam,
        },
        tree_build::{
            BuildReport,
            TreeBuilder,
        },
    },
    rustc_hash::FxHashMap,
    std::{
        cmp::Ord,
        mem,
        path::{
            Path,
            PathBuf,
        },
    },
};

/// The tree which may be displayed, with one line per visible line of the panel.
///
/// In the tree structure, every "node" is just a line, there's
///  no link from a child to its parent or from a parent to its children.
#[derive(Debug, Clone)]
pub struct Tree {
    pub lines: Vec<TreeLine>,
    pub next_line_id: usize,
    pub selection: usize, // there's always a selection (starts with root, which is 0)
    pub options: TreeOptions,
    pub scroll: usize, // the number of lines at the top hidden because of scrolling
    pub total_search: bool, // whether the search was made on all children
    pub git_status: ComputationResult<TreeGitStatus>,
    pub build_report: BuildReport,
}

impl Tree {
    /// rebuild the tree with the same root, height, and options
    pub fn refresh(
        &mut self,
        page_height: usize,
        con: &AppContext,
    ) -> Result<(), TreeBuildError> {
        let builder = TreeBuilder::from(
            self.root().to_path_buf(),
            self.options.clone(),
            // `+ 1`: root sits in the frame title, not the body, so we
            // need `page_height` children + 1 root to fill the body.
            page_height + 1,
            con,
        )?;
        self.total_search = false; // on refresh we always do a non total search
        let mut tree = builder
            .build_tree(self.total_search, &Dam::unlimited())
            .unwrap(); // should not fail
        let selected_path = self.selected_line().path.to_path_buf();
        mem::swap(&mut self.lines, &mut tree.lines);
        self.scroll = 0;
        if !self.try_select_path(&selected_path) && self.selection >= self.lines.len() {
            self.selection = 0;
        }
        self.make_selection_visible(page_height);
        Ok(())
    }

    /// do what must be done after line additions or removals:
    /// - sort the lines
    /// - compute left branches
    pub fn after_lines_changed(&mut self) {
        // we need to order the lines to build the tree.
        // It's a little complicated because
        //  - we want a case insensitive sort
        //  - we still don't want to confuse the children of AA and Aa
        //  - a node can come from a not parent node, when we followed a link
        let mut id_parents: FxHashMap<TreeLineId, TreeLineId> = FxHashMap::default();
        let mut id_lines: FxHashMap<TreeLineId, &TreeLine> = FxHashMap::default();
        for line in &self.lines[..] {
            if let Some(parent_id) = line.parent_id {
                id_parents.insert(line.id, parent_id);
            }
            id_lines.insert(line.id, line);
        }
        let mut sort_paths: FxHashMap<TreeLineId, String> = FxHashMap::default();
        for line in &self.lines[1..] {
            let mut sort_path = String::new();
            let mut id = line.id;
            while let Some(l) = id_lines.get(&id) {
                let lower_name = l
                    .path
                    .file_name()
                    .map_or("".to_string(), |name| name.to_string_lossy().to_lowercase());
                let sort_prefix = match self.options.sort {
                    Sort::TypeDirsFirst => {
                        if l.is_dir() {
                            "              "
                        } else {
                            l.path.extension().and_then(|s| s.to_str()).unwrap_or("")
                        }
                    }
                    Sort::TypeDirsLast => {
                        if l.is_dir() {
                            "~~~~~~~~~~~~~~"
                        } else {
                            l.path.extension().and_then(|s| s.to_str()).unwrap_or("")
                        }
                    }
                    _ => "",
                };
                sort_path = format!(
                    "{}{}-{}/{}",
                    sort_prefix,
                    lower_name,
                    id, // to be sure to separate paths having the same lowercase
                    sort_path,
                );
                if let Some(&parent_id) = id_parents.get(&id) {
                    id = parent_id;
                } else {
                    break;
                }
            }
            sort_paths.insert(line.id, sort_path);
        }
        self.lines[1..].sort_by_key(|line| sort_paths.get(&line.id).unwrap());

        let mut best_index = 0; // index of the line with the best score
        for i in 1..self.lines.len() {
            if self.lines[i].score > self.lines[best_index].score {
                best_index = i;
            }
            for d in 0..self.lines[i].left_branches.len() {
                self.lines[i].left_branches[d] = false;
            }
        }
        // then we discover the branches (for the drawing)
        // and we mark the last children as pruning, if they have unlisted brothers
        let mut last_parent_index: usize = self.lines.len() + 1;
        for end_index in (1..self.lines.len()).rev() {
            let depth = (self.lines[end_index].depth - 1) as usize;
            let start_index = {
                let parent_index = match self.lines[end_index].parent_id {
                    Some(parent_id) => {
                        let mut index = end_index;
                        loop {
                            index -= 1;
                            if self.lines[index].id == parent_id {
                                break;
                            }
                            if index == 0 {
                                break;
                            }
                        }
                        index
                    }
                    None => end_index, // Should not happen
                };
                if parent_index != last_parent_index {
                    // the line at end_index is the last listed child of the line at parent_index
                    let unlisted = self.lines[parent_index].unlisted;
                    if unlisted > 0 && self.lines[end_index].nb_kept_children == 0 {
                        if best_index == end_index {
                            //debug!("Avoiding to prune the line with best score");
                        } else {
                            //debug!("turning {:?} into Pruning", self.lines[end_index].path);
                            self.lines[end_index].line_type = TreeLineType::Pruning;
                            self.lines[end_index].unlisted = unlisted + 1;
                            self.lines[end_index].name = format!("{} unlisted", unlisted + 1);
                            self.lines[parent_index].unlisted = 0;
                        }
                    }
                    last_parent_index = parent_index;
                }
                parent_index + 1
            };
            for i in start_index..=end_index {
                self.lines[i].left_branches[depth] = true;
            }
        }
        if self.options.needs_sum() {
            time!("fetch_file_sum", self.fetch_regular_file_sums()); // not the dirs, only simple files
            self.sort_siblings(); // does nothing when sort mode is None
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1
    }

    pub fn has_branch(
        &self,
        line_index: usize,
        depth: usize,
    ) -> bool {
        if line_index >= self.lines.len() {
            return false;
        }
        let line = &self.lines[line_index];
        depth < usize::from(line.depth) && line.left_branches[depth]
    }

    /// select another line
    ///
    /// For example the following one if dy is 1.
    pub fn move_selection(
        &mut self,
        dy: i32,
        page_height: usize,
        cycle: bool,
    ) {
        let l = self.lines.len();
        if l == 0 {
            return;
        }
        // we find the new line to select
        loop {
            if dy < 0 {
                let ady = (-dy) as usize;
                let ady = ady % l;
                if !cycle && self.selection < ady {
                    break;
                }
                self.selection = (self.selection + l - ady) % l;
            } else {
                let dy = dy as usize;
                if !cycle && self.selection + dy >= l {
                    break;
                }
                self.selection = (self.selection + dy) % l;
            }
            if self.lines[self.selection].is_selectable() {
                break;
            }
        }
        // we adjust the scroll.
        //
        // Geometry note: the body renders `lines[1..]` (root excluded), so
        // valid visible indices are `scroll + 1 ..= scroll + page_height`
        // and the max valid scroll is `lines.len() - 1 - page_height`.
        // `saturating_sub` guards the "everything fits" path (l == 0 already
        // returned above; if l <= page_height + 1 the body fits with scroll
        // at 0).
        if l > page_height {
            let max_scroll = l.saturating_sub(1).saturating_sub(page_height);
            if self.selection < 3 {
                self.scroll = 0;
            } else if self.selection < self.scroll + 3 {
                self.scroll = self.selection - 3;
            } else if self.selection + 3 > l {
                self.scroll = max_scroll;
            } else if self.selection + 3 > self.scroll + page_height {
                self.scroll = (self.selection + 3 - page_height).min(max_scroll);
            }
        }
    }

    /// Scroll the desired amount and return true, or return false if it's
    /// already at end or the tree fits the page.
    ///
    /// The body renders `lines[1..]` (root is in the frame title), so the
    /// scrollable content length is `lines.len() - 1` and the max valid
    /// scroll is `lines.len() - 1 - page_height`.
    pub fn try_scroll(
        &mut self,
        dy: i32,
        page_height: usize,
    ) -> bool {
        // root is excluded from scrollable rows
        if self.lines.len().saturating_sub(1) <= page_height {
            return false;
        }
        if dy < 0 {
            // scroll up
            if self.scroll == 0 {
                return false;
            }
            let ady = -dy as usize;
            if ady < self.scroll {
                self.scroll -= ady;
            } else {
                self.scroll = 0;
            }
        } else {
            // scroll down
            let max = self.lines.len() - 1 - page_height;
            if self.scroll >= max {
                return false;
            }
            self.scroll = (self.scroll + dy as usize).min(max);
        }
        self.select_visible_line(page_height);
        true
    }

    /// try to select a line by index of visible line
    /// (works if the resolved line is selectable)
    ///
    /// Body row 0 corresponds to `lines[1]` because the root row
    /// (`lines[0]`) is no longer painted into the tree body — it lives
    /// in the frame title. Clicking the body therefore cannot select
    /// the root: callers wishing to navigate up should use `:back`,
    /// `Esc`, or the goto modal.
    pub fn try_select_y(
        &mut self,
        y: usize,
    ) -> bool {
        // body row y -> tree.lines[y + 1 + scroll]
        let idx = y + 1 + self.scroll;
        if idx < self.lines.len() && self.lines[idx].is_selectable() {
            self.selection = idx;
            return true;
        }
        false
    }
    /// fix the selection so that it's a selectable visible line
    fn select_visible_line(
        &mut self,
        page_height: usize,
    ) {
        // Body renders `lines[1..]` (root excluded). Visible body lines map
        // to `lines[scroll + 1 ..= scroll + page_height]`, so the last
        // visible index is `scroll + page_height` (inclusive). Selection
        // strictly greater than that is below the viewport.
        if self.selection < self.scroll || self.selection > self.scroll + page_height {
            self.selection = self.scroll;
            let l = self.lines.len();
            loop {
                self.selection = (self.selection + l + 1) % l;
                if self.lines[self.selection].is_selectable() {
                    break;
                }
            }
        }
    }

    pub fn make_selection_visible(
        &mut self,
        page_height: usize,
    ) {
        // Effective scrollable content excludes the root (lines[0]), so
        // the "everything fits" check uses `lines.len() - 1` and the
        // max scroll is also `lines.len() - 1 - page_height`.
        //
        // Visible range of `selection` for a given `scroll` is
        // `[scroll + 1, scroll + page_height]` (inclusive), so the
        // scroll-forward branch must fire on `selection > scroll + page_height`
        // (strictly), not `>=`. Otherwise a selection sitting exactly on
        // the last visible row would trigger a needless scroll, and (for
        // selections near the end of the tree) push `scroll` one row past
        // `max_scroll`. Mirrors `select_visible_line`'s visibility test.
        let content_len = self.lines.len().saturating_sub(1);
        if page_height >= content_len || self.selection < 3 {
            self.scroll = 0;
        } else if self.selection <= self.scroll {
            self.scroll = self.selection - 2;
        } else if self.selection > self.lines.len() - 2 {
            self.scroll = self.lines.len() - 1 - page_height;
        } else if self.selection > self.scroll + page_height {
            self.scroll = self.selection + 1 - page_height;
        }
    }
    pub fn selected_line(&self) -> &TreeLine {
        &self.lines[self.selection]
    }
    pub fn root(&self) -> &PathBuf {
        &self.lines[0].path
    }
    pub fn is_root_selected(&self) -> bool {
        self.selection == 0
    }
    /// select the line with the best matching score
    pub fn try_select_best_match(&mut self) {
        let mut best_score = 0;
        for (idx, line) in self.lines.iter().enumerate() {
            if !line.is_selectable() {
                continue;
            }
            if best_score > line.score {
                continue;
            }
            if line.score == best_score {
                // in case of equal scores, we prefer the shortest path
                if self.lines[idx].depth >= self.lines[self.selection].depth {
                    continue;
                }
            }
            best_score = line.score;
            self.selection = idx;
        }
    }
    /// return true when we could select the given path
    pub fn try_select_path(
        &mut self,
        path: &Path,
    ) -> bool {
        for (idx, line) in self.lines.iter().enumerate() {
            if !line.is_selectable() {
                continue;
            }
            if path == line.path {
                self.selection = idx;
                return true;
            }
        }
        false
    }
    pub fn try_select_first(&mut self) -> bool {
        for idx in 0..self.lines.len() {
            let line = &self.lines[idx];
            if line.is_selectable() {
                self.selection = idx;
                self.scroll = 0;
                return true;
            }
        }
        false
    }
    pub fn try_select_last(
        &mut self,
        page_height: usize,
    ) -> bool {
        for idx in (0..self.lines.len()).rev() {
            let line = &self.lines[idx];
            if line.is_selectable() {
                self.selection = idx;
                self.make_selection_visible(page_height);
                return true;
            }
        }
        false
    }
    pub fn try_select_previous_same_depth(
        &mut self,
        page_height: usize,
    ) -> bool {
        let depth = self.lines[self.selection].depth;
        for di in (0..self.lines.len()).rev() {
            let idx = (self.selection + di) % self.lines.len();
            let line = &self.lines[idx];
            if !line.is_selectable() || line.depth != depth {
                continue;
            }
            self.selection = idx;
            self.make_selection_visible(page_height);
            return true;
        }
        false
    }
    pub fn try_select_next_same_depth(
        &mut self,
        page_height: usize,
    ) -> bool {
        let depth = self.lines[self.selection].depth;
        for di in 0..self.lines.len() {
            let idx = (self.selection + di + 1) % self.lines.len();
            let line = &self.lines[idx];
            if !line.is_selectable() || line.depth != depth {
                continue;
            }
            self.selection = idx;
            self.make_selection_visible(page_height);
            return true;
        }
        false
    }
    pub fn try_select_previous_filtered<F>(
        &mut self,
        filter: F,
        page_height: usize,
    ) -> bool
    where
        F: Fn(&TreeLine) -> bool,
    {
        for di in (0..self.lines.len()).rev() {
            let idx = (self.selection + di) % self.lines.len();
            let line = &self.lines[idx];
            if !line.is_selectable() {
                continue;
            }
            if !filter(line) {
                continue;
            }
            if line.score > 0 {
                self.selection = idx;
                self.make_selection_visible(page_height);
                return true;
            }
        }
        false
    }
    pub fn try_select_next_filtered<F>(
        &mut self,
        filter: F,
        page_height: usize,
    ) -> bool
    where
        F: Fn(&TreeLine) -> bool,
    {
        for di in 0..self.lines.len() {
            let idx = (self.selection + di + 1) % self.lines.len();
            let line = &self.lines[idx];
            if !line.is_selectable() {
                continue;
            }
            if !filter(line) {
                continue;
            }
            if line.score > 0 {
                self.selection = idx;
                self.make_selection_visible(page_height);
                return true;
            }
        }
        false
    }

    pub fn has_dir_missing_sum(&self) -> bool {
        self.options.needs_sum()
            && self
                .lines
                .iter()
                .any(|line| line.line_type == TreeLineType::Dir && line.sum.is_none())
    }

    pub fn is_missing_git_status_computation(&self) -> bool {
        self.git_status.is_not_computed()
    }

    /// fetch the file_sums of regular files (thus avoiding the
    /// long computation which is needed for directories)
    pub fn fetch_regular_file_sums(&mut self) {
        for i in 1..self.lines.len() {
            match self.lines[i].line_type {
                TreeLineType::Dir | TreeLineType::Pruning => {}
                _ => {
                    self.lines[i].sum = Some(FileSum::from_file(&self.lines[i].path));
                }
            }
        }
        self.sort_siblings();
    }

    /// compute the file_sum of one directory
    ///
    /// To compute the size of all of them, this should be called until
    ///  has_dir_missing_sum returns false
    pub fn fetch_some_missing_dir_sum(
        &mut self,
        dam: &Dam,
        con: &AppContext,
    ) {
        // we prefer to compute the root directory last: its computation
        // is faster when its first level children are already computed
        for i in (0..self.lines.len()).rev() {
            if self.lines[i].sum.is_none() && self.lines[i].line_type == TreeLineType::Dir {
                self.lines[i].sum = FileSum::from_dir(&self.lines[i].path, dam, con);
                self.sort_siblings();
                return;
            }
        }
    }

    /// Sort files according to the sort option
    ///
    /// (does nothing if it's None)
    fn sort_siblings(&mut self) {
        match self.options.sort {
            Sort::Count => {
                // we'll try to keep the same path selected
                let selected_path = self.selected_line().path.to_path_buf();
                self.lines[1..].sort_by(|a, b| {
                    let account = a.sum.map_or(0, |s| s.to_count());
                    let bcount = b.sum.map_or(0, |s| s.to_count());
                    bcount.cmp(&account)
                });
                self.try_select_path(&selected_path);
            }
            Sort::Date => {
                let selected_path = self.selected_line().path.to_path_buf();
                self.lines[1..].sort_by(|a, b| {
                    let adate = a.sum.map_or(0, |s| s.to_seconds());
                    let bdate = b.sum.map_or(0, |s| s.to_seconds());
                    bdate.cmp(&adate)
                });
                self.try_select_path(&selected_path);
            }
            Sort::Size => {
                let selected_path = self.selected_line().path.to_path_buf();
                self.lines[1..].sort_by(|a, b| {
                    let asize = a.sum.map_or(0, |s| s.to_size());
                    let bsize = b.sum.map_or(0, |s| s.to_size());
                    bsize.cmp(&asize)
                });
                self.try_select_path(&selected_path);
            }
            _ => {}
        }
    }

    /// compute and return the size of the root
    pub fn total_sum(&self) -> FileSum {
        if let Some(sum) = self.lines[0].sum {
            // if the real total sum is computed, it's in the root line
            sum
        } else {
            // if we don't have the sum in root, the nearest estimate is
            // the sum of sums of lines at depth 1
            let mut sum = FileSum::zero();
            for i in 1..self.lines.len() {
                if self.lines[i].depth == 1 {
                    if let Some(line_sum) = self.lines[i].sum {
                        sum += line_sum;
                    }
                }
            }
            sum
        }
    }

    /// Add to the tree the lines which are in the given path but not already in the tree.
    ///
    /// Fail if the path is not a descendant of the tree root.
    fn add_lines_to_path(
        &mut self,
        target_path: &Path,
        con: &AppContext,
    ) -> Result<(), TreeBuildError> {
        let mut path = target_path;
        let mut paths_to_add = Vec::new();
        // find the closest parent already in the tree
        let mut present_ancestor_idx = loop {
            let idx = self.lines.iter().position(|line| line.path == path);
            if let Some(idx) = idx {
                break idx;
            }
            paths_to_add.push(path);
            let Some(parent) = path.parent() else {
                warn!("no ancestor in the tree for {:?}", path);
                return Err(TreeBuildError::NotARootDescendant {
                    path: path.display().to_string(),
                });
            };
            path = parent;
        };

        let present_ancestor = &mut self.lines[present_ancestor_idx];

        //debug!("present ancestor: {:#?}", &present_ancestor);
        if present_ancestor.line_type.is_pruning() {
            info!("unpruning {:?}", &present_ancestor.path);
            present_ancestor.unprune();
            // we should in exchange prune another one ?
        }

        debug!("show -> paths to add: {:?}", paths_to_add);
        if paths_to_add.is_empty() {
            return Ok(());
        }
        present_ancestor.nb_kept_children += 1;

        // adding the new lines
        while let Some(path_to_add) = paths_to_add.pop() {
            info!("adding {:?}", path_to_add);
            let new_line_id = self.next_line_id;
            self.next_line_id += 1;
            let parent = &self.lines[present_ancestor_idx];
            let depth = parent.depth + 1;

            // The 1 kept_children here might be a trick to avoid the file
            // being changed to Pruning in the after_lines_changed method...
            let nb_kept_children = 1;

            let subpath = path_to_add
                .strip_prefix(self.root())
                .map_err(|_| {
                    // not supposed to happen at this point as we're adding a descendant
                    TreeBuildError::NotARootDescendant {
                        path: path.display().to_string(),
                    }
                })?
                .to_string_lossy()
                .to_string();

            let line = TreeLineBuilder {
                id: new_line_id,
                path: path_to_add.to_path_buf(),
                subpath,
                parent_id: Some(parent.id),
                depth,
                unlisted: 0,
                nb_kept_children,
                has_error: false,
                score: 1,
                direct_match: true,
            }
            .build(con)?;

            present_ancestor_idx = self.lines.len();
            self.lines.push(line);
        }
        self.after_lines_changed();
        Ok(())
    }

    pub fn show_path(
        &mut self,
        path: &Path,
        con: &AppContext,
    ) -> Result<(), TreeBuildError> {
        self.add_lines_to_path(path, con)?;
        let selected = self.try_select_path(path);
        if !selected {
            warn!("failed to select {:?}", path);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for `Tree` — specifically the click-to-line mapping
    //! after the "skip root row in body" change. Body row 0 must now
    //! resolve to `lines[1]`, not `lines[0]`.
    //!
    //! Constructing a real `Tree` is awkward because `TreeLine` carries an
    //! `fs::Metadata` field that can only be obtained from an actual
    //! filesystem entry. We reuse `symlink_metadata(".")` for every test
    //! line — the metadata content is irrelevant to the selection logic
    //! under test; we only need a value of the right type.

    use {
        super::*,
        std::{
            fs,
            path::PathBuf,
        },
    };

    /// Build a synthetic `TreeLine` for use in selection-math tests.
    /// All filesystem-dependent fields share the metadata of `"."`.
    fn fake_line(
        id: TreeLineId,
        path: &str,
        line_type: TreeLineType,
    ) -> TreeLine {
        let metadata = fs::symlink_metadata(".").expect("symlink_metadata of current dir works");
        TreeLine {
            id,
            parent_id: if id == 0 { None } else { Some(0) },
            left_branches: vec![false; 0].into_boxed_slice(),
            depth: if id == 0 { 0 } else { 1 },
            path: PathBuf::from(path),
            subpath: path.to_string(),
            icon: None,
            name: path.to_string(),
            line_type,
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

    /// Build a `Tree` whose `lines.len() == n_children + 1` (root + children).
    /// All children are `TreeLineType::File` and therefore selectable.
    fn fake_tree(n_children: usize) -> Tree {
        let mut lines = Vec::with_capacity(n_children + 1);
        lines.push(fake_line(0, "/", TreeLineType::Dir));
        for i in 1..=n_children {
            lines.push(fake_line(i, &format!("/c{i}"), TreeLineType::File));
        }
        Tree {
            lines,
            next_line_id: n_children + 1,
            selection: 0,
            options: TreeOptions::default(),
            scroll: 0,
            total_search: false,
            git_status: ComputationResult::None,
            build_report: BuildReport::default(),
        }
    }

    #[test]
    fn try_select_y_maps_with_offset() {
        // 5 lines = root + 4 children. Valid indices for click selection
        // are body rows 0..=3, mapping to tree.lines[1..=4].
        let mut tree = fake_tree(4);
        assert_eq!(tree.lines.len(), 5);

        // y=0 -> lines[1] (first child)
        assert!(tree.try_select_y(0));
        assert_eq!(tree.selection, 1);

        // y=2 -> lines[3]
        assert!(tree.try_select_y(2));
        assert_eq!(tree.selection, 3);

        // y=3 -> lines[4] (last child)
        assert!(tree.try_select_y(3));
        assert_eq!(tree.selection, 4);
    }

    #[test]
    fn try_select_y_out_of_bounds_is_noop() {
        // 5 lines, click at body row 4 (would resolve to lines[5]) is OOB.
        // Existing semantics: return false, leave selection unchanged.
        let mut tree = fake_tree(4);
        tree.selection = 2;

        assert!(!tree.try_select_y(4));
        assert_eq!(tree.selection, 2, "OOB click must not change selection");

        // Even further OOB.
        assert!(!tree.try_select_y(100));
        assert_eq!(tree.selection, 2);
    }

    #[test]
    fn try_select_y_with_small_tree() {
        // 2 lines = root + 1 child. Only body row 0 is valid.
        let mut tree = fake_tree(1);
        assert_eq!(tree.lines.len(), 2);

        // y=0 -> lines[1].
        assert!(tree.try_select_y(0));
        assert_eq!(tree.selection, 1);

        // y=1 -> lines[2] (OOB). No-op, no panic.
        tree.selection = 1;
        assert!(!tree.try_select_y(1));
        assert_eq!(tree.selection, 1);
    }

    #[test]
    fn try_select_y_with_scroll_offset() {
        // 10 lines, scroll = 2: visible window starts at lines[3]
        // (= 1 + scroll + 0). Body row 0 -> lines[3].
        let mut tree = fake_tree(9);
        assert_eq!(tree.lines.len(), 10);
        tree.scroll = 2;

        assert!(tree.try_select_y(0));
        assert_eq!(tree.selection, 3);

        assert!(tree.try_select_y(5));
        assert_eq!(tree.selection, 8);

        // y=7 -> lines[10] (OOB). Selection unchanged.
        let prev = tree.selection;
        assert!(!tree.try_select_y(7));
        assert_eq!(tree.selection, prev);
    }

    #[test]
    fn try_select_y_with_root_only_tree() {
        // 1 line = root only. With the body offset (`y + 1 + scroll`)
        // every body row resolves out-of-bounds; no click can select
        // anything. This is the boundary that distinguishes +1 from
        // +0 offset — under the old +0 mapping, y=0 would have
        // selected lines[0] (the root) and the test would fail.
        let mut tree = fake_tree(0);
        assert_eq!(tree.lines.len(), 1);

        // y=0 -> lines[1] (OOB)
        let prev = tree.selection;
        assert!(!tree.try_select_y(0));
        assert_eq!(tree.selection, prev);

        // y=5 -> lines[6] (OOB)
        assert!(!tree.try_select_y(5));
        assert_eq!(tree.selection, prev);
    }

    #[test]
    fn try_select_y_skips_unselectable_line() {
        // Replace one child with a `Pruning` line, which is unselectable.
        let mut tree = fake_tree(3);
        tree.lines[2].line_type = TreeLineType::Pruning;

        // y=1 resolves to lines[2] which is Pruning -> returns false.
        let prev = tree.selection;
        assert!(!tree.try_select_y(1));
        assert_eq!(tree.selection, prev);

        // y=0 -> lines[1] (selectable file)
        assert!(tree.try_select_y(0));
        assert_eq!(tree.selection, 1);
    }

    //
    // Scroll math: with the root-skip rendering, the scrollable content
    // length is `lines.len() - 1` and the max valid scroll is
    // `lines.len() - 1 - page_height`. These tests pin the off-by-one
    // fixes in `try_scroll` and `make_selection_visible`.
    //

    #[test]
    fn try_scroll_exact_fit_does_not_scroll() {
        // 5 lines = root + 4 children, page_height = 4 → all four
        // children fit on screen with no scroll needed. The
        // pre-fix code returned `true` here (allowing scroll past
        // the visible range), the fix returns `false`.
        let mut tree = fake_tree(4);
        assert_eq!(tree.lines.len(), 5);
        assert!(!tree.try_scroll(1, 4));
        assert_eq!(tree.scroll, 0);
    }

    #[test]
    fn try_scroll_max_is_lines_len_minus_one_minus_page_height() {
        // 10 lines = root + 9 children, page_height = 3 → at the
        // bottom, the last visible row is body row 2, mapping to
        // lines[2 + 1 + scroll] = lines[scroll + 3]. For this to be
        // lines[9] (the last child), scroll = 6. So max scroll = 6,
        // not 7 (which would put body row 2 at lines[10], OOB).
        let mut tree = fake_tree(9);
        assert!(tree.try_scroll(10, 3)); // request scroll 10, will be clamped
        assert_eq!(
            tree.scroll, 6,
            "max scroll for 10 lines / page 3 must be 6, not 7",
        );

        // Now we should be unable to scroll further down.
        assert!(!tree.try_scroll(1, 3));
        assert_eq!(tree.scroll, 6);
    }

    #[test]
    fn make_selection_visible_exact_fit_resets_scroll() {
        // 5 lines, page_height = 4 → content fits exactly (4 children
        // == 4 rows); scroll must reset to 0 regardless of selection.
        let mut tree = fake_tree(4);
        tree.scroll = 7; // junk scroll
        tree.selection = 3;
        tree.make_selection_visible(4);
        assert_eq!(tree.scroll, 0);
    }

    #[test]
    fn make_selection_visible_clamps_to_max_scroll() {
        // 10 lines, page_height = 3. Selecting the last line should
        // produce scroll = 6 (not 7 — see try_scroll_max test above).
        let mut tree = fake_tree(9);
        tree.selection = 9;
        tree.make_selection_visible(3);
        assert_eq!(
            tree.scroll, 6,
            "selecting the last line must produce max scroll = lines.len() - 1 - page_height",
        );
    }

    #[test]
    fn make_selection_visible_second_to_last_stays_at_max() {
        // 10 lines, page_height = 3 → max scroll = 6.
        // With selection = lines.len() - 2 = 8 (still visible at body
        // row 2 when scroll = 6, since `scroll + page_height == 9` is
        // the last visible index — wait: 8 == 6 + 2 == body row 1
        // when scroll = 6; visible). The scroll-forward branch must
        // NOT fire when selection sits exactly on the last visible row
        // for a given scroll; and even when it does fire from a smaller
        // scroll, the result must clamp to max = 6, not 7.
        let mut tree = fake_tree(9);
        tree.selection = 8;
        tree.scroll = 4; // not yet showing line 8 (visible: scroll+1..=scroll+3 = 5..=7)
        tree.make_selection_visible(3);
        assert!(
            tree.scroll <= 6,
            "scroll must not exceed max=6 for 10 lines / page 3, got {}",
            tree.scroll,
        );
        // The selection must end up visible: scroll+1 <= 8 <= scroll+3.
        assert!(tree.selection >= tree.scroll + 1);
        assert!(tree.selection <= tree.scroll + 3);
    }

    #[test]
    fn make_selection_visible_does_not_scroll_when_selection_at_last_visible_row() {
        // 10 lines, page_height = 3. With scroll = 5, visible body
        // lines are scroll+1..=scroll+3 = 6..=8. Setting selection = 8
        // (last visible row) must NOT trigger any scroll — the row is
        // still in view. Pre-fix code used `>=`, which fired here and
        // pushed scroll to 7 (one past max).
        let mut tree = fake_tree(9);
        tree.scroll = 5;
        tree.selection = 8;
        tree.make_selection_visible(3);
        assert_eq!(
            tree.scroll, 5,
            "selection on the last visible row must not trigger a scroll",
        );
    }
}
