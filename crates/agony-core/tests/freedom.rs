//! The platform-freedom grep, as a test (the AGENTS.md invariant).
//!
//! Scans `agony-core/src` for the forbidden patterns: OS conditionals,
//! the multiplexer, filesystem access and clock reads. This file lives in
//! `tests/` (a separate compilation unit) so its own scanning code cannot
//! trip the grep it enforces.

use std::fs;
use std::path::{Path, PathBuf};

fn core_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn forbidden(line: &str) -> bool {
    let squashed: String = line
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    squashed.contains("rmux")
        || squashed.contains("std::fs")
        || squashed.contains("instant::now")
        || squashed.contains("cfg(target")
}

fn source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).expect("core src dir exists");
    for entry in entries.flatten() {
        let path = entry.path();
        let is_source = path.extension().is_some_and(|ext| ext == "rs");
        if is_source {
            files.push(path);
        }
    }
    files.sort();
    files
}

#[test]
fn invariant_platform_freedom_grep_is_empty() {
    for file in source_files(&core_src()) {
        let text = fs::read_to_string(&file).expect("readable source file");
        for (number, line) in text.lines().enumerate() {
            assert!(
                !forbidden(line),
                "{}:{} violates platform freedom: {line}",
                file.display(),
                number + 1
            );
        }
    }
}
