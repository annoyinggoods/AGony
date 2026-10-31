//! The app directory's catalog source (Part11 §11.2): the checked-in
//! seed file plus the same format fetched from the settings' URL, cached
//! with a weekly stamp like Part03's carapace import. A failed or broken
//! remote fetch degrades to the stale cache, then to the seed — the seed
//! is always present, the remote is additive, and neither can break
//! startup.
//!
//! Items: `SEED` — the embedded `catalog/terminal-apps.toml`;
//! `Stamp` — the cache's (mtime, url) record.
//! Core functions: `parse` — one catalog text into manifests (the first
//! invalid entry fails that text, not the run); `load_layer` — seed +
//! remote cache/fetch, the discovery merge's middle layer; `fetch` — the
//! real transport (curl through the PATH, timeout-bounded); `stamp_path`
//! / `cache_path` — the two cache files.
//! Depends on: agony_core::manifest (CatalogFile, AppManifest).
//! Called by: discover (the refresh merge), the tests.
//! Calls: std::fs under the config dir only; tokio process for curl.

use std::path::{Path, PathBuf};
use std::time::Duration;

use agony_core::manifest::{AppManifest, CatalogFile};
use serde::{Deserialize, Serialize};

use crate::rmuxhost;

/// The checked-in seed (Part11 §11.2.1): always present, even offline.
pub const SEED: &str = include_str!("../../../catalog/terminal-apps.toml");

const CACHE_FILE: &str = "remote-catalog.toml";
const STAMP_FILE: &str = "remote-catalog.json";
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const CURL: &str = "curl";

/// The cache's freshness record: which URL produced the cache, and when.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    pub fetched_unix: u64,
    pub url: String,
}

/// Parse one catalog text; an invalid entry fails the whole text (a
/// catalog is curated), and the caller falls back to the next source.
pub fn parse(text: &str, origin: &str, notes: &mut Vec<String>) -> Vec<AppManifest> {
    match CatalogFile::from_toml(text) {
        Ok(file) => file.app,
        Err(error) => {
            notes.push(format!("catalog {origin} ignored: {error}"));
            Vec::new()
        }
    }
}

pub fn cache_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CACHE_FILE)
}

pub fn stamp_path(config_dir: &Path) -> PathBuf {
    config_dir.join(STAMP_FILE)
}

/// The week window the discovery cache uses; the remote catalog shares it.
pub fn is_fresh(stamp: &Stamp, url: &str, now_unix: u64, week_secs: u64) -> bool {
    stamp.url == url && now_unix.saturating_sub(stamp.fetched_unix) < week_secs
}

pub fn read_stamp(config_dir: &Path) -> Option<Stamp> {
    let text = std::fs::read_to_string(stamp_path(config_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The catalog layer (11.2.2–11.2.3): the seed's entries, then the
/// remote's (a fresh cache wins; a stale one refetches through `fetch`;
/// any failure falls back to what is on disk, then to the seed alone).
/// Only a body that PARSES is cached — a transport success that is not a
/// catalog (the UI review's `404: Not Found` finding: an error page was
/// persisted as the cache and parsed garbage on every startup) is
/// reported and written nowhere; an on-disk cache that cannot parse is
/// treated as absent and healed by the refetch.
pub async fn load_layer<F, Fut>(
    config_dir: &Path,
    now_unix: u64,
    url: Option<&str>,
    week_secs: u64,
    fetch: F,
) -> (Vec<AppManifest>, Vec<String>)
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    let mut notes = Vec::new();
    let mut entries = parse(SEED, "seed", &mut notes);
    let Some(url) = url.filter(|url| !url.trim().is_empty()) else {
        return (entries, notes);
    };
    let cached = std::fs::read_to_string(cache_path(config_dir)).ok();
    let stamp = read_stamp(config_dir);
    let cached_valid = cached.as_ref().and_then(|text| {
        let mut probe = Vec::new();
        let parsed = parse(text, "remote (cached)", &mut probe);
        probe.is_empty().then_some(parsed)
    });
    if let Some(remote) = cached_valid {
        if stamp
            .as_ref()
            .is_some_and(|stamp| is_fresh(stamp, url, now_unix, week_secs))
        {
            entries.extend(remote);
            return (entries, notes);
        }
    }
    match fetch(url.to_string()).await {
        Ok(text) => {
            let mut parse_notes = Vec::new();
            let remote = parse(&text, "remote", &mut parse_notes);
            if parse_notes.is_empty() {
                let write = write_cache(config_dir, url, now_unix, &text);
                if let Err(error) = write {
                    notes.push(format!("remote catalog cache write failed: {error}"));
                }
                entries.extend(remote);
            } else {
                // A transport success that is not a catalog: never cached.
                notes.push(format!(
                    "remote catalog body ignored (not a catalog — not cached): {}",
                    parse_notes.join("; ")
                ));
                if let Some(text) = cached {
                    entries.extend(parse(&text, "remote (stale)", &mut notes));
                }
            }
        }
        Err(error) => {
            notes.push(format!(
                "remote catalog fetch failed: {error} — using the seed"
            ));
            if let Some(text) = cached {
                entries.extend(parse(&text, "remote (stale)", &mut notes));
            }
        }
    }
    (entries, notes)
}

fn write_cache(config_dir: &Path, url: &str, now_unix: u64, text: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(config_dir)?;
    std::fs::write(cache_path(config_dir), text)?;
    let stamp = Stamp {
        fetched_unix: now_unix,
        url: url.to_string(),
    };
    std::fs::write(stamp_path(config_dir), serde_json::to_string(&stamp)?)?;
    Ok(())
}

/// The real transport: `curl` through the PATH, redirect-following,
/// timeout-bounded, never a window. Pipe names never reach this module,
/// so the shell-mangling gotcha does not apply.
pub async fn fetch(url: String) -> Result<String, String> {
    let mut command = tokio::process::Command::new(CURL);
    command.args(["-sSL", "--max-time", "45", &url]);
    rmuxhost::set_no_window(command.as_std_mut());
    let output = tokio::time::timeout(FETCH_TIMEOUT, command.output())
        .await
        .map_err(|_| "timed out after 60s".to_string())?
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!("curl exited with {:?}", output.status.code()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = format!(
            "agony-catalog-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    const WEEK: u64 = 7 * 24 * 3600;

    /// Write only the freshness stamp (the heal test pairs it with a
    /// hand-written garbage cache).
    fn write_stamp(dir: &Path, url: &str, now_unix: u64) {
        let stamp = Stamp {
            fetched_unix: now_unix,
            url: url.to_string(),
        };
        std::fs::write(
            stamp_path(dir),
            serde_json::to_string(&stamp).expect("stamp json"),
        )
        .expect("stamp written");
    }

    #[test]
    fn the_seed_parses_and_validates_whole() {
        let mut notes = Vec::new();
        let entries = parse(SEED, "seed", &mut notes);
        assert!(notes.is_empty(), "the seed must be clean: {notes:?}");
        assert!(entries.len() >= 55, "the seed carries the curated set");
        for entry in &entries {
            assert!(entry.license.is_some(), "{} names a license", entry.id);
        }
    }

    #[test]
    fn a_broken_catalog_text_becomes_one_note() {
        let mut notes = Vec::new();
        let entries = parse("[[app]]\nid = \"Bad Id\"\n", "remote", &mut notes);
        assert!(entries.is_empty());
        assert_eq!(notes.len(), 1);
        assert!(notes[0].contains("remote"));
    }

    #[tokio::test]
    async fn no_url_means_the_seed_alone() {
        let dir = temp_dir("nourl");
        let (entries, notes) = load_layer(&dir, 1000, None, WEEK, |_url| async {
            Err("must not fetch".to_string())
        })
        .await;
        assert!(!entries.is_empty(), "the seed is always present");
        assert!(notes.is_empty());
    }

    #[tokio::test]
    async fn a_fresh_cache_beats_the_fetch() {
        let dir = temp_dir("fresh");
        let url = "https://example.com/catalog.toml";
        write_cache(
            &dir,
            url,
            1000,
            "[[app]]\nid = \"from-cache\"\nname = \"C\"\nexec = \"c\"\ncategory = \"dev\"\n",
        )
        .expect("cache written");
        let (entries, notes) = load_layer(&dir, 1000 + 60, Some(url), WEEK, |_url| async {
            Err("must not fetch".to_string())
        })
        .await;
        assert!(notes.is_empty());
        assert!(
            entries.iter().any(|m| m.id == "from-cache"),
            "the cached entry merged in"
        );
    }

    #[tokio::test]
    async fn a_stale_cache_refetches_and_rewrites() {
        let dir = temp_dir("stale");
        let url = "https://example.com/catalog.toml";
        write_cache(
            &dir,
            url,
            1000,
            "[[app]]\nid = \"old\"\nname = \"O\"\nexec = \"o\"\ncategory = \"dev\"\n",
        )
        .expect("stale cache written");
        let (entries, notes) = load_layer(&dir, 1000 + WEEK + 5, Some(url), WEEK, |_url| async {
            Ok(
                "[[app]]\nid = \"new\"\nname = \"N\"\nexec = \"n\"\ncategory = \"dev\"\n"
                    .to_string(),
            )
        })
        .await;
        assert!(notes.is_empty(), "{notes:?}");
        assert!(entries.iter().any(|m| m.id == "new"));
        assert!(
            !entries.iter().any(|m| m.id == "old"),
            "the cache was replaced"
        );
        let cached = std::fs::read_to_string(cache_path(&dir)).expect("cache rewritten");
        assert!(cached.contains("new"));
    }

    #[tokio::test]
    async fn a_fetch_body_that_does_not_parse_is_never_cached() {
        // The UI review's finding: a 404 body was persisted as the cache
        // and parsed as garbage on every startup. The regression: the
        // body is reported, nothing is written.
        let dir = temp_dir("404");
        let url = "https://example.com/catalog.toml";
        let (entries, notes) = load_layer(&dir, 1000, Some(url), WEEK, |_url| async {
            Ok("404: Not Found".to_string())
        })
        .await;
        assert!(
            !entries.iter().any(|m| m.id == "404: Not Found"),
            "the error body became no entry"
        );
        assert!(
            notes
                .iter()
                .any(|note| note.contains("not a catalog — not cached")),
            "the refusal is named: {notes:?}"
        );
        assert!(
            !cache_path(&dir).exists(),
            "the error body must not become the cache"
        );
        assert!(
            !stamp_path(&dir).exists(),
            "no freshness stamp for a body that never parsed"
        );
        // The seed still carries the layer.
        assert!(!entries.is_empty());
    }

    #[tokio::test]
    async fn an_unparseable_cache_is_treated_as_absent_and_refetched() {
        // A garbage cache with a fresh stamp healed: it is ignored, the
        // fetch runs, a good body replaces the garbage.
        let dir = temp_dir("heal");
        let url = "https://example.com/catalog.toml";
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(cache_path(&dir), "404: Not Found").expect("garbage cache");
        write_stamp(&dir, url, 1000);
        let (entries, notes) = load_layer(&dir, 1000 + 60, Some(url), WEEK, |_url| async {
            Ok(
                "[[app]]\nid = \"healed\"\nname = \"H\"\nexec = \"h\"\ncategory = \"dev\"\n"
                    .to_string(),
            )
        })
        .await;
        assert!(
            entries.iter().any(|m| m.id == "healed"),
            "the refetch went through: {notes:?}"
        );
        let cached = std::fs::read_to_string(cache_path(&dir)).expect("cache rewritten");
        assert!(cached.contains("healed"), "the garbage was replaced");
        assert!(
            !notes
                .iter()
                .any(|note| note.contains("remote (cached) ignored")),
            "no parse-error note for the healed cache: {notes:?}"
        );
    }

    #[test]
    fn freshness_includes_the_url() {
        let stamp = Stamp {
            fetched_unix: 1000,
            url: "https://a".into(),
        };
        assert!(is_fresh(&stamp, "https://a", 1000 + 60, WEEK));
        assert!(
            !is_fresh(&stamp, "https://b", 1000 + 60, WEEK),
            "a url change refetches"
        );
        assert!(!is_fresh(&stamp, "https://a", 1000 + WEEK, WEEK));
    }
}
