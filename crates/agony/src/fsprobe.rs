//! The file-panel walker (Part05 5.2): an ignore-aware directory scan on
//! a worker task, producing an immutable snapshot the panel renders.
//! Walking is a worker, rendering is a snapshot — the panel never blocks.
//!
//! The `ignore` crate (ripgrep's engine) provides the git-exact semantics
//! (`.gitignore`, hidden-file rules); the depth and entry caps bound a
//! walk over a huge tree. The stream is sorted by path and the per-node
//! children are then ordered directories-first — deterministic snapshots
//! (a literal breadth-first cap ordering was traded for the crate's
//! semantics; noted in Part05's Implementation Notes).
//!
//! Items: `Options` — the walk knobs (mirrors `files` settings);
//! `Snapshot` — roots plus the truncated/error flags; `SharedTree` — the
//! snapshot the worker fills and the panel reads.
//! Core functions: `walk` — the one synchronous walker (the test seam);
//! `start_walk` — spawn it on a blocking worker and return the shared
//! snapshot; `lock` — the poisoning-recovering lock (discover's pattern).
//! Depends on: agony_core::tree (the snapshot shape), rmuxhost
//! (`set_no_window` is not needed — no subprocess), tokio blocking.
//! Called by: the panel (open/refresh), app (the refresh triggers).
//! Calls: std::fs through the `ignore` crate only.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use agony_core::tree::TreeNode;
use ignore::WalkBuilder;

/// The walk knobs, read once when a refresh starts — never mid-walk.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub show_hidden: bool,
    pub respect_ignore: bool,
    pub max_depth: usize,
    pub max_entries: usize,
}

impl Options {
    pub fn from_settings(settings: &agony_core::config::FilesSettings) -> Options {
        Options {
            show_hidden: settings.show_hidden,
            respect_ignore: settings.respect_ignore,
            max_depth: settings.max_depth,
            max_entries: settings.max_entries,
        }
    }
}

/// One walk's result: the tree (dirs first per level), whether the entry
/// cap truncated it, and the first error, if any.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapshot {
    pub roots: Vec<TreeNode>,
    pub truncated: bool,
    pub error: Option<String>,
}

pub type SharedTree = Arc<Mutex<Snapshot>>;

/// Lock the shared snapshot, recovering from poisoning (discover's rule:
/// snapshot data, not an invariant).
pub fn lock(shared: &SharedTree) -> std::sync::MutexGuard<'_, Snapshot> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The one walker: `root`'s tree under the caps, deterministic. Errors
/// degrade to a note in the snapshot — a partial tree beats no tree.
pub fn walk(root: &Path, options: &Options) -> Snapshot {
    let mut snapshot = Snapshot::default();
    let mut builder = WalkBuilder::new(root);
    builder.hidden(!options.show_hidden);
    builder.git_ignore(options.respect_ignore);
    builder.parents(options.respect_ignore);
    builder.require_git(false);
    builder.sort_by_file_path(|a, b| a.cmp(b));
    if options.max_depth > 0 {
        builder.max_depth(Some(options.max_depth));
    }
    let mut roots: Vec<TreeNode> = Vec::new();
    for entry in builder.build() {
        match entry {
            Ok(entry) => insert(&mut roots, root, entry.path(), options, &mut snapshot),
            Err(error) => {
                snapshot.error.get_or_insert_with(|| error.to_string());
            }
        }
        if snapshot.truncated {
            break;
        }
    }
    sort_dir_first(&mut roots);
    snapshot.roots = roots;
    snapshot
}

/// Insert one walked path into the tree (skipping the root itself).
fn insert(
    roots: &mut Vec<TreeNode>,
    root: &Path,
    path: &Path,
    options: &Options,
    snapshot: &mut Snapshot,
) {
    if path == root {
        return;
    }
    let Ok(relative) = path.strip_prefix(root) else {
        return;
    };
    let parts: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let Some((name, ancestors)) = parts.split_last() else {
        return;
    };
    if count_nodes(roots) >= options.max_entries {
        snapshot.truncated = true;
        return;
    }
    let dir = descend(roots, ancestors);
    let leaf = path.is_file();
    if dir.iter().any(|node| node.label == *name) {
        return;
    }
    let node = if leaf {
        TreeNode::file(name.clone())
    } else {
        TreeNode::dir(name.clone())
    };
    dir.push(node);
}

/// Walk the index path, creating directory nodes as needed.
fn descend<'a>(roots: &'a mut Vec<TreeNode>, path: &[String]) -> &'a mut Vec<TreeNode> {
    let mut current = roots;
    for label in path {
        let index = match current.iter().position(|node| node.label == *label) {
            Some(index) => index,
            None => {
                current.push(TreeNode::dir(label.clone()));
                current.len() - 1
            }
        };
        current = &mut current[index].children;
    }
    current
}

fn count_nodes(roots: &[TreeNode]) -> usize {
    roots
        .iter()
        .map(|node| 1 + count_nodes(&node.children))
        .sum()
}

/// Order every level's children directories-first, then by label — the
/// panel's display order, deterministic across runs.
fn sort_dir_first(nodes: &mut [TreeNode]) {
    nodes.sort_by(|a, b| a.leaf.cmp(&b.leaf).then_with(|| a.label.cmp(&b.label)));
    for node in nodes {
        sort_dir_first(&mut node.children);
    }
}

/// Spawn the walker on a blocking task and return the snapshot it will
/// fill. The first read sees an empty snapshot; the panel renders
/// whatever is present and the next draw picks the finished walk up.
pub fn start_walk(root: PathBuf, options: Options) -> SharedTree {
    let shared: SharedTree = Arc::new(Mutex::new(Snapshot::default()));
    let worker = shared.clone();
    tokio::task::spawn_blocking(move || {
        let snapshot = walk(&root, &options);
        *lock(&worker) = snapshot;
    });
    shared
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("agony-fsprobe-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp tree root");
        dir
    }

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("parent dir");
        }
        fs::write(path, text).expect("file written");
    }

    fn labels(roots: &[TreeNode]) -> Vec<String> {
        roots.iter().map(|n| n.label.clone()).collect()
    }

    fn default_options() -> Options {
        Options {
            show_hidden: false,
            respect_ignore: true,
            max_depth: 8,
            max_entries: 2000,
        }
    }

    #[test]
    fn walk_orders_directories_first_and_skips_hidden() {
        let root = temp_tree("order");
        write(&root.join("zeta.txt"), "z");
        write(&root.join("src").join("main.rs"), "m");
        write(&root.join("src").join("lib").join("a.rs"), "a");
        write(&root.join(".hidden"), "h");
        write(&root.join(".hid").join("inner.txt"), "i");
        let snapshot = walk(&root, &default_options());
        assert!(snapshot.error.is_none());
        assert!(!snapshot.truncated);
        assert_eq!(labels(&snapshot.roots), vec!["src", "zeta.txt"]);
        let src = &snapshot.roots[0];
        assert_eq!(labels(&src.children), vec!["lib", "main.rs"]);
        assert_eq!(labels(&src.children[0].children), vec!["a.rs"]);
    }

    #[test]
    fn gitignore_rules_are_respected_and_togglable() {
        let root = temp_tree("ignore");
        write(&root.join(".gitignore"), "ignored-dir/\n*.tmp\n");
        write(&root.join("kept.txt"), "k");
        write(&root.join("dropped.tmp"), "d");
        write(&root.join("ignored-dir").join("x.txt"), "x");
        // Respecting: the ignore rules drop ignored-dir and *.tmp; the
        // .gitignore file itself is a hidden dotfile (show_hidden=false).
        let respecting = walk(&root, &default_options());
        assert_eq!(labels(&respecting.roots), vec!["kept.txt"]);
        let mut loose = default_options();
        loose.respect_ignore = false;
        loose.show_hidden = true;
        let loose_walk = walk(&root, &loose);
        let names = labels(&loose_walk.roots);
        assert!(names.contains(&"dropped.tmp".to_string()));
        assert!(names.contains(&"ignored-dir".to_string()));
        assert!(names.contains(&".gitignore".to_string()));
    }

    #[test]
    fn entry_cap_truncates_the_snapshot() {
        let root = temp_tree("cap");
        for n in 0..50 {
            write(&root.join(format!("f{n:02}.txt")), "x");
        }
        let mut capped = default_options();
        capped.max_entries = 10;
        let snapshot = walk(&root, &capped);
        assert!(snapshot.truncated, "the cap must be reported");
        let total = count_nodes(&snapshot.roots);
        assert!(total <= 11, "the cap bounds the tree: {total}");
    }

    #[test]
    fn depth_cap_bounds_the_descent() {
        let root = temp_tree("depth");
        write(
            &root
                .join("a")
                .join("b")
                .join("c")
                .join("d")
                .join("deep.txt"),
            "x",
        );
        let mut shallow = default_options();
        shallow.max_depth = 2;
        let snapshot = walk(&root, &shallow);
        let text = format!("{:?}", snapshot.roots);
        assert!(!text.contains("deep.txt"), "beyond the depth cap: {text}");
    }

    #[test]
    fn a_missing_root_degrades_to_an_error_snapshot() {
        let missing = std::env::temp_dir().join("agony-fsprobe-no-such-root");
        let snapshot = walk(&missing, &default_options());
        assert!(snapshot.error.is_some() || snapshot.roots.is_empty());
        assert!(snapshot.roots.is_empty());
    }

    #[test]
    fn hidden_toggle_surfaces_dot_entries() {
        let root = temp_tree("hidden");
        write(&root.join(".visible-secret"), "s");
        let mut showing = default_options();
        showing.show_hidden = true;
        let snapshot = walk(&root, &showing);
        assert!(labels(&snapshot.roots).contains(&".visible-secret".to_string()));
    }
}
