//! Source-discipline invariants scanned over the shell's (and the core's)
//! sources: colors flow only through the theme applier, and nothing under
//! `src/` references the documentation trees at runtime.

use std::fs;
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).expect("source dir exists");
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
    files.sort();
    files
}

fn shell_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn core_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../agony-core/src")
}

/// Part02's AC: every color in the UI flows through the theme applier —
/// a `Color::` outside theme.rs (and the core's built-in palette, which
/// names no ratatui color at all) is a defect.
#[test]
fn invariant_colors_only_in_theme() {
    for file in rs_files(&shell_src()) {
        let is_theme = file.file_name().is_some_and(|name| name == "theme.rs");
        if is_theme {
            continue;
        }
        let text = fs::read_to_string(&file).expect("readable source file");
        for (number, line) in text.lines().enumerate() {
            assert!(
                !line.contains("Color::"),
                "{}:{} names a color outside theme.rs: {line}",
                file.display(),
                number + 1
            );
        }
    }
}

/// AGENTS.md: nothing under `src/` may read from the documentation trees
/// at runtime — a path reference into them is a defect, not a design.
#[test]
fn invariant_no_documentation_paths_in_sources() {
    for dir in [shell_src(), core_src()] {
        for file in rs_files(&dir) {
            let text = fs::read_to_string(&file).expect("readable source file");
            for (number, line) in text.lines().enumerate() {
                assert!(
                    !line.contains("REVIEW/"),
                    "{}:{} references the documentation tree: {line}",
                    file.display(),
                    number + 1
                );
            }
        }
    }
}
