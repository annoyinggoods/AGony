//! The platform-seam grep, as a test (Part12 12.2.3's invariant).
//!
//! Scans `agony/src` for OS forks outside `platform.rs` — the compile-time
//! and runtime cfg forms, `env::consts::OS`, and the `std::os::<platform>`
//! modules — and fails on a stray one. This file lives in `tests/` (a
//! separate compilation unit) so its own scanning code cannot trip the
//! grep it enforces. The audit that named every fork this moved into the
//! seam: `REVIEW/audit/2026-10-26-part12-platform-seam.md`.

use std::fs;
use std::path::{Path, PathBuf};

fn shell_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Substrings that mean "an OS fork" when found outside the seam. The
/// squashed form (lowercase, whitespace-free) is matched, so spacing
/// tricks do not sneak past; `#[cfg(test)]` never matches any of these.
fn forbidden(line: &str) -> bool {
    let squashed: String = line
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    [
        "cfg(target_os",
        "cfg(windows",
        "cfg(not(windows",
        "cfg(unix",
        "cfg(not(unix",
        "cfg!(windows",
        "cfg!(not(windows",
        "cfg!(unix",
        "cfg!(not(unix",
        "cfg!(target_os",
        "env::consts::os",
        "os::windows",
        "os::unix",
    ]
    .iter()
    .any(|needle| squashed.contains(needle))
}

fn source_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let entries = fs::read_dir(dir).expect("shell src dir exists");
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

/// The one exempt file: the seam itself.
fn seam_file() -> PathBuf {
    shell_src().join("platform.rs")
}

#[test]
fn invariant_os_forks_live_only_in_platform_rs() {
    let seam = seam_file();
    let mut strays = Vec::new();
    for file in source_files(&shell_src()) {
        if file == seam {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap_or_default();
        for (number, line) in text.lines().enumerate() {
            if forbidden(line) {
                strays.push(format!(
                    "{}:{}: {}",
                    file.display(),
                    number + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        strays.is_empty(),
        "OS forks outside platform.rs (move them into the seam):\n{}",
        strays.join("\n")
    );
}
