//! A generic labeled tree and its cursor (Part05): the shape the file
//! panel renders and navigates. Not a filesystem type — the shell's
//! walker maps paths onto it; this module knows labels, nesting and
//! leaves, nothing else.
//!
//! Items: `TreeNode` — one node (label, children, leaf); `TreeCursor` —
//! the selection path plus the expanded-dir set, the panel's whole
//! navigation state; `VisibleRow` — one rendered row (path indices,
//! depth, label, leaf, expanded flag).
//! Core functions: `visible` — the rows to render, depth-first, only
//! under expanded dirs; `TreeCursor::move_up` / `move_down` /
//! `move_left` / `move_right` — the terminal-native quartet as pure
//! state moves; `TreeCursor::clamp` — re-validate against a new
//! snapshot; `flatten_paths` — every node's joined path (the search
//! overlay's corpus).
//! Depends on: nothing.
//! Called by: the shell's file panel (`panel`, `fsprobe`).
//! Calls: nothing.

/// One node of a labeled tree. `leaf` marks nodes that can never hold
/// children (files); a dir with no children is a non-leaf with an empty
/// `children` vector.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub label: String,
    pub children: Vec<TreeNode>,
    pub leaf: bool,
}

impl TreeNode {
    pub fn dir(label: impl Into<String>) -> TreeNode {
        TreeNode {
            label: label.into(),
            children: Vec::new(),
            leaf: false,
        }
    }

    pub fn file(label: impl Into<String>) -> TreeNode {
        TreeNode {
            label: label.into(),
            children: Vec::new(),
            leaf: true,
        }
    }
}

/// One rendered row: where it sits in the tree (indices from the roots),
/// how deep, what it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisibleRow {
    pub path: Vec<usize>,
    pub depth: usize,
    pub label: String,
    pub leaf: bool,
    pub expanded: bool,
}

/// The panel's navigation state: the selected node (an index path from
/// the roots) and which dirs are expanded. Pure data — every move is a
/// pure function of (roots, cursor).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeCursor {
    selected: Vec<usize>,
    expanded: Vec<Vec<usize>>,
}

impl TreeCursor {
    pub fn new() -> TreeCursor {
        TreeCursor {
            selected: vec![0],
            expanded: Vec::new(),
        }
    }

    /// The selected node's index path (empty when the tree is empty).
    pub fn selected(&self) -> &[usize] {
        &self.selected
    }

    /// Whether the node at `path` is an expanded directory.
    pub fn is_expanded(&self, path: &[usize]) -> bool {
        self.expanded.iter().any(|p| p == path)
    }

    /// The visible rows, depth-first; a directory's children appear only
    /// when it is expanded.
    pub fn visible(&self, roots: &[TreeNode]) -> Vec<VisibleRow> {
        let mut rows = Vec::new();
        collect_rows(roots, &mut Vec::new(), &self.expanded, 0, &mut rows);
        rows
    }

    /// Move to the previous visible row (clamps at the first).
    pub fn move_up(&mut self, roots: &[TreeNode]) {
        self.step(roots, -1);
    }

    /// Move to the next visible row (clamps at the last).
    pub fn move_down(&mut self, roots: &[TreeNode]) {
        self.step(roots, 1);
    }

    fn step(&mut self, roots: &[TreeNode], direction: i32) {
        let rows = self.visible(roots);
        let Some(at) = rows.iter().position(|row| row.path == self.selected) else {
            self.clamp(roots);
            return;
        };
        let moved = at as i32 + direction;
        if moved >= 0 && (moved as usize) < rows.len() {
            self.selected = rows[moved as usize].path.clone();
        }
    }

    /// Left: collapse the selected dir when it is expanded, otherwise move
    /// to its parent (the terminal-native behavior).
    pub fn move_left(&mut self) {
        if self.is_expanded(&self.selected) {
            self.expanded.retain(|path| path != &self.selected);
            return;
        }
        if self.selected.len() > 1 {
            self.selected.pop();
        }
    }

    /// Right: expand the selected dir (no-op on files and already-expanded
    /// dirs). Enter-on-a-file is the shell's call (it launches), so it is
    /// not a cursor move.
    pub fn move_right(&mut self, roots: &[TreeNode]) {
        let Some(node) = node_at(roots, &self.selected) else {
            return;
        };
        let expandable = !node.leaf && !self.is_expanded(&self.selected);
        if expandable {
            self.expanded.push(self.selected.clone());
        }
    }

    /// After a snapshot swap, drop selection and expansion paths that no
    /// longer resolve (expansions must still point at directories),
    /// keeping whatever still does.
    pub fn clamp(&mut self, roots: &[TreeNode]) {
        self.expanded
            .retain(|path| node_at(roots, path).is_some_and(|n| !n.leaf));
        if node_at(roots, &self.selected).is_none() {
            self.selected = vec![0];
            if node_at(roots, &self.selected).is_none() {
                self.selected.clear();
            }
        }
    }

    /// Select an explicit visible row (a click).
    pub fn select(&mut self, path: &[usize]) {
        if !path.is_empty() {
            self.selected = path.to_vec();
        }
    }
}

fn collect_rows(
    nodes: &[TreeNode],
    prefix: &mut Vec<usize>,
    expanded: &[Vec<usize>],
    depth: usize,
    rows: &mut Vec<VisibleRow>,
) {
    for (index, node) in nodes.iter().enumerate() {
        prefix.push(index);
        let is_open = !node.leaf && expanded.contains(&prefix.as_slice().to_vec());
        rows.push(VisibleRow {
            path: prefix.clone(),
            depth,
            label: node.label.clone(),
            leaf: node.leaf,
            expanded: is_open,
        });
        if is_open {
            collect_rows(&node.children, prefix, expanded, depth + 1, rows);
        }
        prefix.pop();
    }
}

/// The node an index path points at, or None when it does not resolve.
pub fn node_at<'a>(roots: &'a [TreeNode], path: &[usize]) -> Option<&'a TreeNode> {
    let (&first, rest) = path.split_first()?;
    let node = roots.get(first)?;
    if rest.is_empty() {
        return Some(node);
    }
    node_at(&node.children, rest)
}

/// Every node's joined path, depth-first (the search corpus). Directories
/// carry a trailing separator so "src/" matches as a directory.
pub fn flatten_paths(roots: &[TreeNode], separator: &str) -> Vec<String> {
    let mut paths = Vec::new();
    push_paths(roots, String::new(), separator, &mut paths);
    paths
}

fn push_paths(nodes: &[TreeNode], prefix: String, sep: &str, out: &mut Vec<String>) {
    for node in nodes {
        let joined = format!("{prefix}{}", node.label);
        out.push(if node.leaf {
            joined.clone()
        } else {
            format!("{joined}{sep}")
        });
        if !node.children.is_empty() {
            push_paths(&node.children, format!("{joined}{sep}"), sep, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// src/ (dir) with main.rs and lib/ (dir) with a.rs, plus top-level
    /// readme.md — dirs before files, the panel's order.
    fn tree() -> Vec<TreeNode> {
        let mut lib = TreeNode::dir("lib");
        lib.children.push(TreeNode::file("a.rs"));
        let mut src = TreeNode::dir("src");
        src.children.push(TreeNode::file("main.rs"));
        src.children.push(lib);
        vec![src, TreeNode::file("readme.md")]
    }

    fn labels(rows: &[VisibleRow]) -> Vec<String> {
        rows.iter().map(|row| row.label.clone()).collect()
    }

    #[test]
    fn collapsed_tree_shows_only_the_roots() {
        let cursor = TreeCursor::new();
        assert_eq!(labels(&cursor.visible(&tree())), ["src", "readme.md"]);
    }

    #[test]
    fn move_down_visits_roots_then_expands_walk_children() {
        let roots = tree();
        let mut cursor = TreeCursor::new();
        cursor.move_right(&roots); // expand src
        assert_eq!(
            labels(&cursor.visible(&roots)),
            ["src", "main.rs", "lib", "readme.md"],
            "children appear under the expanded dir"
        );
        cursor.move_down(&roots);
        cursor.move_down(&roots);
        assert_eq!(cursor.selected(), &[0, 1], "selection walks into children");
        cursor.move_right(&roots); // expand lib
        assert_eq!(
            labels(&cursor.visible(&roots)),
            ["src", "main.rs", "lib", "a.rs", "readme.md"]
        );
    }

    #[test]
    fn move_up_clamps_at_the_first_row() {
        let roots = tree();
        let mut cursor = TreeCursor::new();
        cursor.move_up(&roots);
        assert_eq!(cursor.selected(), &[0]);
    }

    #[test]
    fn move_left_collapses_then_jumps_to_the_parent() {
        let roots = tree();
        let mut cursor = TreeCursor::new();
        cursor.move_right(&roots); // expand src
        cursor.move_down(&roots); // main.rs
        cursor.move_left(); // from a file: jump to the parent
        assert_eq!(cursor.selected(), &[0]);
        cursor.move_left(); // src is expanded: collapse
        assert!(!cursor.is_expanded(&[0]));
        assert_eq!(
            labels(&cursor.visible(&roots)),
            ["src", "readme.md"],
            "collapsed again"
        );
    }

    #[test]
    fn move_right_is_a_noop_on_files_and_open_dirs() {
        let roots = tree();
        let mut cursor = TreeCursor::new();
        cursor.move_right(&roots); // expand src
        cursor.move_right(&roots); // already expanded: no-op
        assert!(cursor.is_expanded(&[0]));
        cursor.move_down(&roots);
        cursor.move_right(&roots); // main.rs is a file: no-op
        assert_eq!(cursor.visible(&roots).len(), 4);
    }

    #[test]
    fn clamp_survives_a_snapshot_swap() {
        let roots = tree();
        let mut cursor = TreeCursor::new();
        cursor.move_right(&roots); // expand src = [0]
        cursor.move_down(&roots); // main.rs = [0, 0]
        let replacement = vec![TreeNode::file("solo.md"), TreeNode::dir("new")];
        cursor.clamp(&replacement);
        assert_eq!(cursor.selected(), &[0], "stale selection resets to the top");
        assert!(
            cursor.expanded.is_empty(),
            "expansion pointing at a file is dropped"
        );
        // A still-valid expansion on a still-directory is kept.
        let mut again = tree();
        cursor.select(&[0]);
        cursor.move_right(&again);
        again[0].children.push(TreeNode::file("extra.rs"));
        cursor.clamp(&again);
        assert!(cursor.is_expanded(&[0]));
    }

    #[test]
    fn flatten_paths_joins_with_the_separator() {
        let paths = flatten_paths(&tree(), "\\");
        assert_eq!(
            paths,
            vec![
                "src\\",
                "src\\main.rs",
                "src\\lib\\",
                "src\\lib\\a.rs",
                "readme.md"
            ]
        );
    }

    #[test]
    fn empty_tree_clamps_to_an_empty_selection() {
        let mut cursor = TreeCursor::new();
        cursor.clamp(&[]);
        assert!(cursor.selected().is_empty());
        assert!(cursor.visible(&[]).is_empty());
        cursor.move_down(&[]);
        assert!(
            cursor.selected().is_empty(),
            "moves are no-ops on emptiness"
        );
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    /// A random tree from a DFS step spec: true opens a dir, false
    /// closes the deepest open one (leaving a file sibling next to it).
    fn shape_tree(spec: &[bool]) -> Vec<TreeNode> {
        let mut roots: Vec<TreeNode> = Vec::new();
        let mut stack: Vec<TreeNode> = Vec::new();
        for (count, &step) in spec.iter().enumerate() {
            let label = format!("n{count}");
            if step {
                stack.push(TreeNode::dir(label));
                continue;
            }
            if let Some(mut node) = stack.pop() {
                node.children.push(TreeNode::file(format!("{label}.f")));
                attach_node(&mut stack, &mut roots, node);
            }
        }
        while let Some(mut node) = stack.pop() {
            node.children.push(TreeNode::file("tail.f".to_string()));
            attach_node(&mut stack, &mut roots, node);
        }
        roots
    }

    fn attach_node(stack: &mut [TreeNode], roots: &mut Vec<TreeNode>, node: TreeNode) {
        match stack.last_mut() {
            Some(parent) => parent.children.push(node),
            None => roots.push(node),
        }
    }

    /// Every directory's index path, depth-first.
    fn dir_paths(roots: &[TreeNode]) -> Vec<Vec<usize>> {
        let mut out = Vec::new();
        walk_dirs(roots, &mut Vec::new(), &mut out);
        out
    }

    fn walk_dirs(nodes: &[TreeNode], prefix: &mut Vec<usize>, out: &mut Vec<Vec<usize>>) {
        for (index, node) in nodes.iter().enumerate() {
            prefix.push(index);
            if !node.leaf {
                out.push(prefix.clone());
                walk_dirs(&node.children, prefix, out);
            }
            prefix.pop();
        }
    }

    /// The visible rows, recomputed independently: DFS over expanded
    /// dirs only, as (path, depth, label, leaf, expanded) tuples.
    type RowTuple = (Vec<usize>, usize, String, bool, bool);

    fn expected_rows(
        roots: &[TreeNode],
        expanded: &std::collections::HashSet<Vec<usize>>,
    ) -> Vec<RowTuple> {
        let mut out = Vec::new();
        walk_rows(roots, &mut Vec::new(), expanded, 0, &mut out);
        out
    }

    fn walk_rows(
        nodes: &[TreeNode],
        prefix: &mut Vec<usize>,
        expanded: &std::collections::HashSet<Vec<usize>>,
        depth: usize,
        out: &mut Vec<RowTuple>,
    ) {
        for (index, node) in nodes.iter().enumerate() {
            prefix.push(index);
            let open = !node.leaf && expanded.contains(prefix.as_slice());
            out.push((prefix.clone(), depth, node.label.clone(), node.leaf, open));
            if open {
                walk_rows(&node.children, prefix, expanded, depth + 1, out);
            }
            prefix.pop();
        }
    }

    fn move_cursor(cursor: &mut TreeCursor, roots: &[TreeNode], kind: u8) {
        match kind {
            0 => cursor.move_up(roots),
            1 => cursor.move_down(roots),
            2 => cursor.move_left(),
            3 => cursor.move_right(roots),
            4 => select_last_row(cursor, roots),
            _ => {}
        }
    }

    fn select_last_row(cursor: &mut TreeCursor, roots: &[TreeNode]) {
        if let Some(row) = cursor.visible(roots).last() {
            cursor.select(&row.path);
        }
    }

    fn assert_selection_resolves(cursor: &TreeCursor, roots: &[TreeNode]) {
        if roots.is_empty() {
            assert!(
                cursor.selected().is_empty(),
                "selection survives on an empty tree"
            );
            return;
        }
        assert!(
            node_at(roots, cursor.selected()).is_some(),
            "selection left the tree"
        );
    }

    /// Expand a random subset of the tree's directories.
    fn expand_random_dirs(cursor: &mut TreeCursor, roots: &[TreeNode], bits: u64) {
        for (index, path) in dir_paths(roots).into_iter().enumerate() {
            if bits >> index & 1 != 0 {
                cursor.expanded.push(path);
            }
        }
    }

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// visible() is exactly the DFS over expanded dirs, whatever the
        /// tree and the expansion.
        #[test]
        fn prop_visible_rows_are_the_expanded_dfs(
            spec in proptest::collection::vec(any::<bool>(), 0..20),
            expand_bits in any::<u64>(),
        ) {
            let roots = shape_tree(&spec);
            let mut cursor = TreeCursor::new();
            expand_random_dirs(&mut cursor, &roots, expand_bits);
            let expanded: std::collections::HashSet<Vec<usize>> =
                cursor.expanded.iter().cloned().collect();
            let got: Vec<RowTuple> = cursor
                .visible(&roots)
                .into_iter()
                .map(|row| (row.path, row.depth, row.label, row.leaf, row.expanded))
                .collect();
            prop_assert_eq!(got, expected_rows(&roots, &expanded));
        }

        /// The navigation quartet keeps the selection on a real node.
        #[test]
        fn prop_cursor_moves_stay_on_real_nodes(
            spec in proptest::collection::vec(any::<bool>(), 0..20),
            moves in proptest::collection::vec(0u8..6, 0..30),
        ) {
            let roots = shape_tree(&spec);
            let mut cursor = TreeCursor::new();
            cursor.clamp(&roots);
            for kind in moves {
                move_cursor(&mut cursor, &roots, kind);
                assert_selection_resolves(&cursor, &roots);
            }
        }

        /// clamp drops exactly the paths that no longer resolve.
        #[test]
        fn prop_clamp_keeps_exactly_the_live_paths(
            spec_a in proptest::collection::vec(any::<bool>(), 0..20),
            spec_b in proptest::collection::vec(any::<bool>(), 0..20),
            expand_bits in any::<u64>(),
        ) {
            let roots = shape_tree(&spec_a);
            let mut cursor = TreeCursor::new();
            expand_random_dirs(&mut cursor, &roots, expand_bits);
            if let Some(path) = dir_paths(&roots).last() {
                cursor.select(path);
            }
            let replacement = shape_tree(&spec_b);
            cursor.clamp(&replacement);
            for path in &cursor.expanded {
                let node = node_at(&replacement, path).expect("expansion survives clamp");
                assert!(!node.leaf, "an expansion on a file survived clamp");
            }
            assert_selection_resolves(&cursor, &replacement);
        }

        /// flatten_paths lists every node once, dirs marked with the
        /// separator, parents before children.
        #[test]
        fn prop_flatten_lists_every_node_once(
            spec in proptest::collection::vec(any::<bool>(), 0..20),
        ) {
            let roots = shape_tree(&spec);
            let mut expected = Vec::new();
            collect_flatten(&roots, String::new(), "\\", &mut expected);
            prop_assert_eq!(flatten_paths(&roots, "\\"), expected);
        }
    }

    fn collect_flatten(nodes: &[TreeNode], prefix: String, sep: &str, out: &mut Vec<String>) {
        for node in nodes {
            let joined = format!("{prefix}{}", node.label);
            out.push(if node.leaf {
                joined.clone()
            } else {
                format!("{joined}{sep}")
            });
            if !node.children.is_empty() {
                collect_flatten(&node.children, format!("{joined}{sep}"), sep, out);
            }
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn move_up_steps_back_and_select_targets_a_row() {
        let roots = tree();
        let mut cursor = TreeCursor::new();
        cursor.move_right(&roots); // expand src
        cursor.move_down(&roots); // main.rs
        cursor.move_up(&roots);
        assert_eq!(cursor.selected(), &[0], "up returns to the parent row");
        cursor.move_down(&roots);
        cursor.select(&[1]);
        assert_eq!(cursor.selected(), &[1], "select moves to the named row");
        cursor.select(&[]);
        assert_eq!(cursor.selected(), &[1], "an empty path never selects");
    }

    #[test]
    fn move_right_on_a_file_expands_nothing() {
        let roots = tree();
        let mut cursor = TreeCursor::new();
        cursor.move_down(&roots); // readme.md (a file)
        cursor.move_right(&roots);
        assert!(!cursor.is_expanded(&[1]), "a file never expands");
        assert_eq!(cursor.visible(&roots).len(), 2);
    }
}
