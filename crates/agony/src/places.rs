//! The common places (Part09 9.2.4): the known user folders (Home,
//! Desktop, Documents, Downloads, Pictures, Music, Videos — each
//! `<home>/<name>`, missing folders hidden) and the machine's drives
//! (Windows: A:–Z: probed with `fs::metadata`, existing shown; Unix: the
//! single root). Resolved once per menu open, never per draw.
//!
//! `std::fs` has no timeout: a slow drive makes one slow menu open — the
//! resolution is deliberately off the draw path so the cost lands once.
//!
//! Items: `Place` — one known folder (label + path); `Places` — the
//! resolved set (known folders + drive labels).
//! Core functions: `known` — the existing known folders under a home;
//! `drives_with` — the drive letters passing a probe (the test seam);
//! `resolve` / `resolve_in` — the whole set for the menu.
//! Depends on: crate::settings (the home path).
//! Called by: app (at menu open) and startmenu (the rows).
//! Calls: std::fs (metadata and read_dir probes only).

use std::path::{Path, PathBuf};

use crate::settings;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Places {
    pub known: Vec<Place>,
    /// Drive identifiers as displayed ("C:" on Windows, "/" on Unix).
    pub drives: Vec<String>,
}

/// Resolve for the real home directory (once per menu open).
pub fn resolve() -> Places {
    let home = settings::home_dir();
    resolve_in(&home)
}

/// Resolve for an explicit home (the tests' seam).
pub fn resolve_in(home: &Path) -> Places {
    Places {
        known: known(home),
        drives: drives_with(probe_exists),
    }
}

/// The known folders that exist under `home`, in display order.
pub fn known(home: &Path) -> Vec<Place> {
    KNOWN_FOLDERS
        .iter()
        .map(|(label, name)| Place {
            label: label.to_string(),
            path: home.join(name),
        })
        .filter(|place| probe_exists(&place.path))
        .collect()
}

/// (label, folder name under the home directory), display order.
const KNOWN_FOLDERS: [(&str, &str); 7] = [
    ("Home", ""),
    ("Desktop", "Desktop"),
    ("Documents", "Documents"),
    ("Downloads", "Downloads"),
    ("Pictures", "Pictures"),
    ("Music", "Music"),
    ("Videos", "Videos"),
];

/// The drives passing `probe` — the seam's fork (`platform::drive_labels_with`):
/// all 26 letters on Windows, the single root on Unix. The probe is
/// injected so tests never touch real drives.
pub fn drives_with(probe: impl Fn(&Path) -> bool) -> Vec<String> {
    crate::platform::drive_labels_with(probe)
}

/// The one existence probe (read_dir over metadata: a place must be a
/// readable directory, not merely a stat-able entry).
fn probe_exists(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fixture_home(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("agony-places-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        for folder in ["Desktop", "Documents", "Downloads"] {
            fs::create_dir_all(home.join(folder)).expect("fixture folder");
        }
        fs::write(home.join("file.txt"), "not a folder").expect("distractor");
        home
    }

    #[test]
    fn known_folders_list_only_the_existing_ones_in_order() {
        let home = fixture_home("known");
        let places = known(&home);
        let labels: Vec<&str> = places.iter().map(|place| place.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Home", "Desktop", "Documents", "Downloads"],
            "missing folders hidden, order kept"
        );
        assert_eq!(places[1].path, home.join("Desktop"));
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn a_missing_home_resolves_to_home_only_or_nothing() {
        let ghost = std::env::temp_dir().join("agony-places-ghost-never");
        let _ = fs::remove_dir_all(&ghost);
        assert!(
            known(&ghost).is_empty(),
            "nothing exists under a ghost home"
        );
    }

    #[test]
    fn drive_probe_keeps_only_existing_labels() {
        let drives = drives_with(|path| path == Path::new("C:\\") || path == Path::new("/"));
        if crate::platform::is_windows() {
            assert_eq!(
                drives,
                vec!["C:".to_string()],
                "only the probed-existing letters"
            );
        } else {
            assert_eq!(drives, vec!["/".to_string()], "Unix: the single root");
        }
        assert!(
            drives_with(|_| false).is_empty(),
            "nothing exists: no drives"
        );
    }

    #[test]
    fn resolve_in_shapes_the_whole_set() {
        let home = fixture_home("resolve");
        let places = resolve_in(&home);
        assert_eq!(places.known.len(), 4);
        // The real probe runs here: on Windows at least one drive
        // (the system drive) answers; on Unix the root does.
        assert!(!drives_with(probe_exists).is_empty());
        let _ = fs::remove_dir_all(&home);
    }
}
