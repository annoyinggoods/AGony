//! App discovery (Part03): where the app list comes from.
//!
//! Sources, in merge order: embedded built-in manifests, then the app
//! directory's catalog (the seed plus the remote source, Part11 §11.2),
//! then the user's manifests in `<config>/agony/apps/*.toml` — a later
//! layer wins on id collision, via the core's one merge. A PATH scan
//! (plus `<config>/agony/bin`, where bundle installs land) marks each
//! app installed (resolved path) or greyed (recipe only); carapace-known
//! commands still become candidates.
//!
//! Items: `DiscoverError` — the failures; `CachedApp` — one app as the UI
//! sees it (manifest + resolved path + candidate + catalog flags);
//! `CatalogCache` — the weekly-stamped result; `DiscoveryState`/`Shared` —
//! the snapshot the worker fills and the UI reads; `lock` — the
//! poisoning-recovering lock every reader goes through.
//! Core functions: `builtins` — the embedded manifests (shell exec
//! substituted); `scan_path_dirs` — name-to-path over PATH, first dir
//! wins; `refresh` / `refresh_in` — the weekly-throttled scan writing
//! `<config>/agony/catalog.json` (the catalog layer rides in the same
//! scan); `start_worker` / `spawn_refresh` / `refresh_into` — the
//! startup worker and its re-run after an in-app install; `carapace_path` — where the
//! completion binary lives; `apps_dir` / `cache_path` /
//! `invalidate_cache` / `user_manifest_files` / `lookup_path` /
//! `parse_carapace_list` / `candidate_apps` / `is_fresh` / `read_cache`
//! / `write_cache` — the pieces the tests and the CLI verbs also reach.
//! Depends on: agony_core::manifest (the schema + the one merge),
//! crate::catalog (the seed + remote layer), settings (the config dir),
//! crate::platform (the login-shell choice, the executable table).
//! Called by: app (the worker + the snapshot), apps (the CLI verbs),
//! startmenu (entry building), doctor (carapace check).
//! Calls: std::fs under the config dir and PATH dirs, tokio process for
//! `carapace --list` and the remote catalog fetch (timeout-bounded,
//! never on the UI thread).

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use agony_core::manifest::{self, AppManifest, Category};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::catalog;
use crate::rmuxhost;
use crate::settings;

const CACHE_FILE: &str = "catalog.json";
const WEEK_SECS: u64 = 7 * 24 * 3600;
const CARAPACE_TIMEOUT: Duration = Duration::from_secs(20);

const BUILTINS: [(&str, &str); 9] = [
    ("shell.toml", include_str!("../manifests/shell.toml")),
    (
        "superfile.toml",
        include_str!("../manifests/superfile.toml"),
    ),
    ("glow.toml", include_str!("../manifests/glow.toml")),
    ("slides.toml", include_str!("../manifests/slides.toml")),
    ("vhs.toml", include_str!("../manifests/vhs.toml")),
    ("gum.toml", include_str!("../manifests/gum.toml")),
    ("gambit.toml", include_str!("../manifests/gambit.toml")),
    ("portal.toml", include_str!("../manifests/portal.toml")),
    ("flapioca.toml", include_str!("../manifests/flapioca.toml")),
];

#[derive(Debug, Error)]
pub enum DiscoverError {
    #[error("discovery io failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("catalog cache write failed: {0}")]
    CacheWrite(#[from] serde_json::Error),
    #[error("carapace import failed: {0}")]
    Carapace(String),
}

/// One app as the UI sees it: its manifest plus what discovery learned.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CachedApp {
    pub manifest: AppManifest,
    /// The resolved executable path when the scan found it, else None.
    pub path: Option<String>,
    /// True when this entry was seeded by the carapace import, not a manifest.
    pub candidate: bool,
    /// True when this entry came from the app directory's catalog
    /// (Part11) and no built-in or user manifest claims the id: the menu
    /// hides it while it is greyed; the directory is its home.
    #[serde(default)]
    pub catalog: bool,
}

/// The cached discovery result: one weekly stamp plus every known app.
/// `schema` bumps whenever a built-in or the CachedApp shape changes, so
/// a cache written by an older build never serves stale fields (a
/// pre-schema cache reads as 0 and rescans immediately).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CatalogCache {
    #[serde(default)]
    pub schema: u64,
    pub scanned_unix: u64,
    pub apps: Vec<CachedApp>,
}

/// The current cache schema; bumped with the built-ins' shape.
const CACHE_SCHEMA: u64 = 1;

/// What the UI reads: the last cache plus how the latest refresh went.
#[derive(Clone, Debug, Default)]
pub struct DiscoveryState {
    pub cache: Option<CatalogCache>,
    pub error: Option<String>,
    pub notes: Vec<String>,
}

/// The shared snapshot the worker fills and the UI reads (never scans).
pub type Shared = Arc<Mutex<DiscoveryState>>;

/// Lock the shared state, recovering from poisoning (a panic in another
/// holder leaves usable data — discovery state is data, not an invariant).
/// The one implementation (audit F3): the worker and every reader go through
/// it.
pub fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, DiscoveryState> {
    match shared.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The user manifests' directory: `<config>/agony/apps`.
pub fn apps_dir() -> PathBuf {
    crate::settings::config_dir().join("apps")
}

/// The catalog cache file: `<config>/agony/catalog.json`.
pub fn cache_path(config_dir: &Path) -> PathBuf {
    config_dir.join(CACHE_FILE)
}

/// Drop the cached scan. The catalog's inputs changed (a user manifest was
/// added), so the next refresh must rescan instead of waiting out the week.
pub fn invalidate_cache() {
    let _ = fs::remove_file(cache_path(&crate::settings::config_dir()));
}

// ---------------------------------------------------------------- built-ins

/// The embedded built-in manifests, validated; the shell manifest's exec
/// is substituted with the detected shell program. A broken embedded
/// manifest is a repository bug — it is skipped with a note, never a panic.
pub fn builtins() -> (Vec<AppManifest>, Vec<String>) {
    let mut manifests = Vec::new();
    let mut notes = Vec::new();
    for (file, text) in BUILTINS {
        match AppManifest::from_toml(text).and_then(|m| {
            m.validate()?;
            Ok(m)
        }) {
            Ok(manifest) => manifests.push(manifest),
            Err(error) => notes.push(format!("builtin manifest {file} ignored: {error}")),
        }
    }
    substitute_shell(&mut manifests);
    (manifests, notes)
}

/// The shell manifest does not pin a binary: the detected login shell
/// (the seam's choice) takes its exec's place.
fn substitute_shell(manifests: &mut [AppManifest]) {
    if let Some(shell) = manifests.iter_mut().find(|m| m.id == "shell") {
        shell.exec = crate::platform::login_shell();
    }
}

/// The user's manifests: `(filename, contents)` pairs, sorted, ready for
/// the core's `load_catalog`. A missing directory is a normal first run.
pub fn user_manifest_files(dir: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let mut files = Vec::new();
    let mut notes = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return (files, notes),
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().map(|ext| ext == "toml").unwrap_or(false))
        .collect();
    paths.sort();
    for path in paths {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match fs::read_to_string(&path) {
            Ok(text) => files.push((name, text)),
            Err(error) => notes.push(format!("user manifest {name} unreadable: {error}")),
        }
    }
    (files, notes)
}

// ------------------------------------------------------------- the PATH scan

/// Every executable on PATH: lowercase file stem to full path. Directories
/// are scanned in PATH order and the first occurrence wins — the same
/// order the shell would resolve with.
pub fn scan_path_dirs(path_env: &str) -> HashMap<String, PathBuf> {
    let mut map = HashMap::new();
    for dir in path_env
        .split(path_separator())
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        scan_dir(Path::new(dir), &mut map);
    }
    map
}

fn path_separator() -> char {
    crate::platform::path_separator()
}

fn scan_dir(dir: &Path, map: &mut HashMap<String, PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        consider_executable(&entry.path(), map);
    }
}

fn consider_executable(path: &Path, map: &mut HashMap<String, PathBuf>) {
    if !path.is_file() {
        return;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    if !is_executable_name(name) || !is_executable_file(path) {
        return;
    }
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    let key = stem.to_lowercase();
    if key.is_empty() {
        return;
    }
    map.entry(key).or_insert_with(|| path.to_path_buf());
}

/// The seam's executable-extension table (Part12 12.2.3), shared by
/// `is_executable_name` and `exec_key`'s stripper below — one table,
/// three users.
use crate::platform::EXECUTABLE_EXTENSIONS;

fn is_executable_name(name: &str) -> bool {
    crate::platform::is_executable_name(name)
}

fn is_executable_file(path: &Path) -> bool {
    crate::platform::is_executable_file(path)
}

/// The lowercase lookup key for a manifest's `exec` (extension stripped).
/// Shared with the installer's archive search (the reuse rule: one
/// exec-matching rule, in one place).
pub(crate) fn exec_key(exec: &str) -> String {
    let lower = exec.to_lowercase();
    strip_executable_extension(&lower).to_string()
}

/// Strip a Windows executable extension, if any (mirrors `exec_key`, which
/// applies the same table on every platform).
fn strip_executable_extension(lower: &str) -> &str {
    for ext in EXECUTABLE_EXTENSIONS {
        if let Some(stem) = lower.strip_suffix(ext) {
            return stem;
        }
    }
    lower
}

/// Resolve a manifest's `exec` through the scan: an exec that carries a
/// path separator is used as-is; a bare name is matched against the scan
/// (case-insensitively, extension-insensitively).
pub fn lookup_path(map: &HashMap<String, PathBuf>, exec: &str) -> Option<String> {
    if exec.contains('\\') || exec.contains('/') {
        return Some(exec.to_string());
    }
    map.get(&exec_key(exec))
        .map(|path| path.to_string_lossy().into_owned())
}

// ------------------------------------------------------------ carapace import

#[derive(Deserialize)]
struct CarapaceEntry {
    #[serde(default)]
    description: String,
}

/// Parse `carapace --list` output: a JSON map of command to completers.
/// Returns `(command, description)` pairs sorted by command, so candidate
/// order is deterministic across runs.
pub fn parse_carapace_list(json: &str) -> Result<Vec<(String, String)>, DiscoverError> {
    let map: HashMap<String, Vec<CarapaceEntry>> = serde_json::from_str(json)?;
    let mut pairs: Vec<(String, String)> = map
        .into_iter()
        .filter_map(|(command, entries)| {
            entries
                .into_iter()
                .next()
                .map(|entry| (command, entry.description))
        })
        .collect();
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(pairs)
}

/// Materialize candidates: a carapace-known command becomes a minimal app
/// only when it is actually in PATH and no manifest already claims its exec.
pub fn candidate_apps(
    candidates: &[(String, String)],
    map: &HashMap<String, PathBuf>,
    taken_execs: &HashSet<String>,
) -> Vec<CachedApp> {
    candidates
        .iter()
        .filter_map(|(command, description)| {
            let key = exec_key(command);
            if taken_execs.contains(&key) {
                return None;
            }
            let path = map.get(&key)?;
            Some(CachedApp {
                manifest: candidate_manifest(command, description),
                path: Some(path.to_string_lossy().into_owned()),
                candidate: true,
                catalog: false,
            })
        })
        .collect()
}

fn candidate_manifest(command: &str, description: &str) -> AppManifest {
    let mut manifest = AppManifest::minimal(command, command, command, Category::System);
    manifest.description = description.to_string();
    manifest.terminal_app = false;
    manifest
}

async fn import_carapace(
    binary: Option<&Path>,
    map: &HashMap<String, PathBuf>,
    taken: &HashSet<String>,
) -> (Vec<CachedApp>, Vec<String>) {
    let Some(binary) = binary else {
        return (Vec::new(), Vec::new());
    };
    let json = match run_carapace_list(binary).await {
        Ok(json) => json,
        Err(error) => return (Vec::new(), vec![error.to_string()]),
    };
    let candidates = match parse_carapace_list(&json) {
        Ok(candidates) => candidates,
        Err(error) => return (Vec::new(), vec![error.to_string()]),
    };
    (candidate_apps(&candidates, map, taken), Vec::new())
}

async fn run_carapace_list(binary: &Path) -> Result<String, DiscoverError> {
    let mut command = tokio::process::Command::new(binary);
    command.arg("--list");
    rmuxhost::set_no_window(command.as_std_mut());
    let output = tokio::time::timeout(CARAPACE_TIMEOUT, command.output())
        .await
        .map_err(|_| DiscoverError::Carapace("timed out after 20s".into()))?
        .map_err(DiscoverError::Io)?;
    if !output.status.success() {
        return Err(DiscoverError::Carapace(format!(
            "exit code {:?}",
            output.status.code()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Where the completion binary lives: `AGONY_CARAPACE`, the repo's `bin/`,
/// then PATH. Shared with `agony doctor`.
pub fn carapace_path() -> Option<PathBuf> {
    let from_env = env::var_os("AGONY_CARAPACE").map(PathBuf::from);
    let candidates = [from_env, Some(repo_carapace()), path_lookup("carapace")];
    // A file that exists but cannot be executed (no exec bit on Unix) is
    // not a completion binary — the 2026-10-27 Linux CI finding.
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.is_file() && is_executable_file(path))
}

fn repo_carapace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bin/carapace.exe")
}

fn path_lookup(name: &str) -> Option<PathBuf> {
    let path_env = env::var("PATH").unwrap_or_default();
    scan_path_dirs(&path_env).get(&exec_key(name)).cloned()
}

// --------------------------------------------------------- the cached scan

/// The cached result is fresh for one week.
pub fn is_fresh(scanned_unix: u64, now_unix: u64) -> bool {
    now_unix.saturating_sub(scanned_unix) < WEEK_SECS
}

pub fn read_cache(config_dir: &Path) -> Option<CatalogCache> {
    let text = fs::read_to_string(cache_path(config_dir)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_cache(config_dir: &Path, cache: &CatalogCache) -> Result<(), DiscoverError> {
    let text = serde_json::to_string_pretty(cache)?;
    fs::create_dir_all(config_dir)?;
    fs::write(cache_path(config_dir), text)?;
    Ok(())
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The full refresh: return the cache when fresh, else scan PATH and the
/// bundle bin dir, load the catalog (built-ins + catalog + user files),
/// import carapace candidates, and write the cache. Returns the cache
/// plus any notes (a broken user manifest, a failed carapace run or
/// remote fetch).
pub async fn refresh(config_dir: &Path) -> Result<(CatalogCache, Vec<String>), DiscoverError> {
    let now = unix_now();
    let path_env = env::var("PATH").unwrap_or_default();
    let carapace = carapace_path();
    let catalog_url = settings::catalog_url();
    refresh_in(
        config_dir,
        now,
        &path_env,
        carapace.as_deref(),
        Some(&catalog_url),
    )
    .await
}

/// `refresh` with injectable inputs — the seam the tests use (a fake PATH,
/// a fake clock, no carapace, no remote). The one-week throttle lives
/// here so the injected clock decides freshness. `catalog_url = None`
/// keeps the merge to the seed (the tests' shape).
pub async fn refresh_in(
    config_dir: &Path,
    now_unix: u64,
    path_env: &str,
    carapace: Option<&Path>,
    catalog_url: Option<&str>,
) -> Result<(CatalogCache, Vec<String>), DiscoverError> {
    if let Some(cached) = read_cache(config_dir) {
        if cached.schema == CACHE_SCHEMA && is_fresh(cached.scanned_unix, now_unix) {
            return Ok((cached, Vec::new()));
        }
    }
    let map = scan_targets(path_env, &settings::bundle_bin_dir());
    let (builtin_manifests, mut notes) = builtins();
    let (catalog_entries, catalog_notes) =
        catalog::load_layer(config_dir, now_unix, catalog_url, WEEK_SECS, catalog::fetch).await;
    notes.extend(catalog_notes);
    let (user_files, user_notes) = user_manifest_files(&config_dir.join("apps"));
    notes.extend(user_notes);
    let refs: Vec<(&str, &str)> = user_files
        .iter()
        .map(|(name, text)| (name.as_str(), text.as_str()))
        .collect();
    let (catalog_merged, issues) =
        manifest::load_catalog(&builtin_manifests, &catalog_entries, &refs);
    notes.extend(
        issues
            .into_iter()
            .map(|issue| format!("user manifest {} ignored: {}", issue.file, issue.error)),
    );
    let catalog_ids: HashSet<String> = catalog_entries.iter().map(|m| m.id.clone()).collect();
    let taken: HashSet<String> = catalog_merged
        .apps
        .iter()
        .map(|m| exec_key(&m.exec))
        .collect();
    let mut apps: Vec<CachedApp> = catalog_merged
        .apps
        .iter()
        .map(|m| CachedApp {
            manifest: m.clone(),
            path: lookup_path(&map, &m.exec),
            candidate: false,
            catalog: catalog_ids.contains(&m.id),
        })
        .collect();
    let (imported, import_notes) = import_carapace(carapace, &map, &taken).await;
    notes.extend(import_notes);
    apps.extend(imported);
    let cache = CatalogCache {
        schema: CACHE_SCHEMA,
        scanned_unix: now_unix,
        apps,
    };
    write_cache(config_dir, &cache)?;
    Ok((cache, notes))
}

/// The executable lookup map: the bundle bin dir first (PATH wins on a
/// name collision — the same first-dir-wins rule the PATH scan uses).
fn scan_targets(path_env: &str, bundle_bin_dir: &Path) -> HashMap<String, PathBuf> {
    let mut map = scan_path_dirs(&bundle_bin_dir.to_string_lossy());
    for (key, path) in scan_path_dirs(path_env) {
        map.insert(key, path);
    }
    map
}

/// Spawn the once-per-startup refresh on a worker task and return the
/// snapshot the UI reads. The cache (even a stale one) is loaded first, so
/// the very first menu paint already has data.
pub fn start_worker(config_dir: PathBuf) -> Shared {
    let shared = Arc::new(Mutex::new(DiscoveryState {
        cache: read_cache(&config_dir),
        error: None,
        notes: Vec::new(),
    }));
    spawn_refresh(shared.clone(), config_dir);
    shared
}

/// Re-run the refresh into an existing snapshot — the seam an in-app
/// install calls (Part11 §11.4.1): the cache is invalidated first, so
/// the entry turns live without a restart.
pub fn spawn_refresh(shared: Shared, config_dir: PathBuf) {
    invalidate_at(&config_dir);
    tokio::spawn(refresh_into(shared, config_dir));
}

/// The awaited refresh into a snapshot — the installer's task awaits
/// this so the directory's outcome lands after the entry turned live.
pub async fn refresh_into(shared: Shared, config_dir: PathBuf) {
    let result = refresh(&config_dir).await;
    let mut state = lock(&shared);
    match result {
        Ok((cache, notes)) => {
            state.notes.extend(notes);
            state.cache = Some(cache);
        }
        Err(error) => state.error = Some(error.to_string()),
    }
}

/// Drop a specific config dir's cached scan (the injectable core of
/// `invalidate_cache`).
fn invalidate_at(config_dir: &Path) {
    let _ = fs::remove_file(cache_path(config_dir));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = format!(
            "agony-discover-{tag}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let dir = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir created");
        dir
    }

    fn touch(dir: &Path, name: &str) {
        let path = dir.join(name);
        fs::write(&path, b"").expect("file created");
        // Unix discovery requires the exec bit (the seam's
        // `is_executable_file`), so the fixture sets it there.
        crate::platform::make_executable(&path);
    }

    fn one_app(id: &str) -> CachedApp {
        CachedApp {
            manifest: AppManifest::minimal(id, id, id, Category::System),
            path: Some(format!("C:\\fake\\{id}.exe")),
            candidate: false,
            catalog: false,
        }
    }

    #[test]
    fn scan_path_dirs_finds_executables_first_dir_wins() {
        let a = temp_dir("path-a");
        let b = temp_dir("path-b");
        touch(&a, "spf.exe");
        touch(&a, "glow.EXE");
        // A plain file, never chmod'd: on Windows the name excludes it,
        // on Unix the missing exec bit does (the seam's two rules).
        fs::write(a.join("readme.txt"), b"").expect("distractor written");
        touch(&b, "spf.exe");
        let path_env = format!("{}{}{}", a.display(), path_separator(), b.display());
        let map = scan_path_dirs(&path_env);
        assert_eq!(map.len(), 2, "txt files are not executables: {map:?}");
        assert_eq!(map["spf"], a.join("spf.exe"), "first PATH dir wins");
        assert_eq!(map["glow"], a.join("glow.EXE"));
    }

    #[test]
    fn cache_round_trips_through_json() {
        let dir = temp_dir("cache");
        let cache = CatalogCache {
            schema: 0,
            scanned_unix: 42,
            apps: vec![one_app("spf")],
        };
        write_cache(&dir, &cache).expect("cache written");
        assert_eq!(read_cache(&dir), Some(cache));
    }

    #[test]
    fn a_broken_cache_file_reads_as_absent() {
        let dir = temp_dir("broken-cache");
        fs::write(cache_path(&dir), "{not json").expect("junk written");
        assert_eq!(read_cache(&dir), None);
    }

    #[test]
    fn cache_is_fresh_for_one_week() {
        let now = 1_000_000;
        assert!(is_fresh(now, now));
        assert!(is_fresh(now - WEEK_SECS + 1, now));
        assert!(!is_fresh(now - WEEK_SECS, now));
        assert!(!is_fresh(0, now));
    }

    #[tokio::test]
    async fn refresh_in_keeps_a_fresh_cache_without_rescanning() {
        let dir = temp_dir("throttle");
        let cached = CatalogCache {
            schema: CACHE_SCHEMA,
            scanned_unix: 500,
            apps: vec![one_app("only-old")],
        };
        write_cache(&dir, &cached).expect("cache written");
        // A PATH that would find different apps, and a later clock: the
        // fresh cache wins, so the scan never runs.
        let (result, notes) = refresh_in(&dir, 500 + 60, "C:\\nonexistent-agony-path", None, None)
            .await
            .expect("refresh");
        assert_eq!(result, cached);
        assert!(notes.is_empty());
    }

    #[tokio::test]
    async fn refresh_in_rescans_a_stale_cache_against_the_path() {
        let dir = temp_dir("stale");
        let bin = temp_dir("stale-bin");
        touch(&bin, "spf.exe");
        let stale = CatalogCache {
            schema: 0,
            scanned_unix: 100,
            apps: Vec::new(),
        };
        write_cache(&dir, &stale).expect("stale cache written");
        let (result, _notes) = refresh_in(
            &dir,
            100 + WEEK_SECS + 5,
            &bin.display().to_string(),
            None,
            None,
        )
        .await
        .expect("refresh");
        let superfile = result
            .apps
            .iter()
            .find(|app| app.manifest.id == "superfile")
            .expect("superfile in the built-ins");
        assert_eq!(
            superfile.path.as_deref(),
            Some(bin.join("spf.exe").to_str().expect("utf8"))
        );
        assert!(!superfile.candidate);
        assert!(!superfile.catalog, "a built-in is never a catalog entry");
        let shell = result
            .apps
            .iter()
            .find(|app| app.manifest.id == "shell")
            .expect("shell in the built-ins");
        // Windows: a bare exec name that the fake PATH never holds. Unix:
        // the shell's exec is an absolute $SHELL path, which resolves
        // as-is by design.
        let shell_path = if crate::platform::is_windows() {
            None
        } else {
            Some(crate::platform::login_shell())
        };
        assert_eq!(
            shell.path.as_deref(),
            shell_path.as_deref(),
            "the fake PATH holds no bare-name shell"
        );
        assert_eq!(read_cache(&dir), Some(result));
    }

    #[tokio::test]
    async fn refresh_in_applies_user_overrides_and_reports_issues() {
        let dir = temp_dir("user");
        let bin = temp_dir("user-bin");
        touch(&bin, "spf.exe");
        let apps = dir.join("apps");
        fs::create_dir_all(&apps).expect("apps dir");
        fs::write(
            apps.join("superfile.toml"),
            "[app]\nid = \"superfile\"\nname = \"My Superfile\"\nexec = \"spf\"\ncategory = \"files\"\n",
        )
        .expect("user manifest written");
        fs::write(apps.join("broken.toml"), "[app]\nid = \"Bad Id\"\n").expect("broken written");
        let (result, notes) = refresh_in(
            &dir,
            100 + WEEK_SECS + 5,
            &bin.display().to_string(),
            None,
            None,
        )
        .await
        .expect("refresh");
        let overridden = result
            .apps
            .iter()
            .find(|app| app.manifest.id == "superfile")
            .expect("override keeps the id");
        assert_eq!(overridden.manifest.name, "My Superfile");
        assert!(
            notes.iter().any(|note| note.contains("broken.toml")),
            "the broken file must be reported: {notes:?}"
        );
    }

    #[tokio::test]
    async fn refresh_in_merges_the_seed_catalog_and_marks_its_entries() {
        let dir = temp_dir("catalog");
        let bin = temp_dir("catalog-bin");
        touch(&bin, "bat.exe");
        let (result, notes) = refresh_in(
            &dir,
            100 + WEEK_SECS + 5,
            &bin.display().to_string(),
            None,
            None,
        )
        .await
        .expect("refresh");
        assert!(
            notes.iter().all(|note| !note.contains("catalog")),
            "the seed must parse clean: {notes:?}"
        );
        let bat = result
            .apps
            .iter()
            .find(|app| app.manifest.id == "bat")
            .expect("a seed catalog entry reaches the cache");
        assert!(bat.catalog, "catalog entries carry the flag");
        assert_eq!(
            bat.path.as_deref(),
            Some(bin.join("bat.exe").to_str().expect("utf8")),
            "an installed catalog entry resolves like any other"
        );
        let lazygit = result
            .apps
            .iter()
            .find(|app| app.manifest.id == "lazygit")
            .expect("the catalog carries lazygit");
        assert!(lazygit.catalog);
        assert_eq!(lazygit.path, None);
    }

    #[test]
    fn scan_targets_gives_path_priority_over_the_bundle_bin_dir() {
        let path_dir = temp_dir("targets-path");
        let bundle_dir = temp_dir("targets-bin");
        touch(&path_dir, "spf.exe");
        touch(&bundle_dir, "spf.exe");
        touch(&bundle_dir, "gambit.exe");
        let map = scan_targets(&path_dir.display().to_string(), &bundle_dir);
        assert_eq!(
            map["spf"],
            path_dir.join("spf.exe"),
            "PATH order wins on a name collision"
        );
        assert_eq!(
            map["gambit"],
            bundle_dir.join("gambit.exe"),
            "the bundle dir fills the gaps"
        );
    }

    #[test]
    fn carapace_list_fixture_parses() {
        let fixture = r#"{
  "git": [
    { "name": "git", "description": "Version control", "group": "common" }
  ],
  "cargo": [
    { "name": "cargo", "description": "Rust package manager", "group": "common" }
  ]
}"#;
        let parsed = parse_carapace_list(fixture).expect("fixture parses");
        assert_eq!(
            parsed,
            vec![
                ("cargo".to_string(), "Rust package manager".to_string()),
                ("git".to_string(), "Version control".to_string()),
            ]
        );
    }

    #[test]
    fn candidates_need_path_and_an_unclaimed_exec() {
        let mut map = HashMap::new();
        map.insert("git".to_string(), PathBuf::from("C:\\bin\\git.exe"));
        map.insert("glow".to_string(), PathBuf::from("C:\\bin\\glow.exe"));
        let mut taken = HashSet::new();
        taken.insert("glow".to_string());
        let candidates = vec![
            ("git".to_string(), "Version control".to_string()),
            ("glow".to_string(), "markdown".to_string()),
            ("nothere".to_string(), "not on PATH".to_string()),
        ];
        let apps = candidate_apps(&candidates, &map, &taken);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].manifest.id, "git");
        assert_eq!(apps[0].manifest.category, Category::System);
        assert!(!apps[0].manifest.terminal_app);
        assert!(apps[0].candidate);
        assert_eq!(apps[0].path.as_deref(), Some("C:\\bin\\git.exe"));
    }

    #[tokio::test]
    async fn builtins_all_validate_and_the_shell_exec_is_substituted() {
        let (manifests, notes) = builtins();
        assert!(
            notes.is_empty(),
            "embedded manifests must all parse: {notes:?}"
        );
        assert_eq!(manifests.len(), BUILTINS.len());
        let ids: Vec<&str> = manifests.iter().map(|m| m.id.as_str()).collect();
        for expected in [
            "shell",
            "superfile",
            "glow",
            "slides",
            "vhs",
            "gum",
            "gambit",
            "portal",
            "flapioca",
        ] {
            assert!(ids.contains(&expected), "{expected} missing from {ids:?}");
        }
        let shell = manifests.iter().find(|m| m.id == "shell").expect("shell");
        assert_eq!(shell.exec, crate::platform::login_shell());
        for manifest in &manifests {
            let expected = if manifest.id == "flapioca" {
                "GPL-3.0"
            } else {
                "MIT"
            };
            assert_eq!(
                manifest.license.as_deref(),
                Some(expected),
                "{} carries its license",
                manifest.id
            );
        }
        // The bundled set (10.3.1) and the GPL shelf (10.3.3), as flags.
        let bundled: Vec<&str> = manifests
            .iter()
            .filter(|m| m.bundled)
            .map(|m| m.id.as_str())
            .collect();
        assert_eq!(
            bundled,
            vec!["superfile", "glow", "slides", "vhs", "gum", "gambit"]
        );
        let flapioca = manifests
            .iter()
            .find(|m| m.id == "flapioca")
            .expect("flapioca");
        assert!(!flapioca.bundled, "the GPL shelf is never bundled");
        assert!(flapioca.install.is_some(), "flapioca ships its recipe");
        // The release recipes the bundle installer runs (10.3.2): the
        // five Windows assets are pinned and hash-checked; slides has no
        // Windows asset and must not pretend otherwise.
        for id in ["superfile", "glow", "vhs", "gum", "gambit"] {
            let release = manifests
                .iter()
                .find(|m| m.id == id)
                .and_then(|m| m.install.as_ref())
                .and_then(|install| install.release.as_ref());
            let release = release.unwrap_or_else(|| panic!("{id} pins a release asset"));
            assert_eq!(release.sha256.len(), 64, "{id}'s hash is a sha256");
            assert!(release.url.starts_with("https://"), "{id}'s url is https");
        }
        let slides = manifests.iter().find(|m| m.id == "slides").expect("slides");
        let slides_release = slides
            .install
            .as_ref()
            .and_then(|install| install.release.as_ref());
        assert!(
            slides_release.is_none(),
            "slides has no Windows asset to pin"
        );
    }

    #[test]
    fn lookup_path_treats_separated_execs_as_explicit_paths() {
        let mut map = HashMap::new();
        map.insert("spf".to_string(), PathBuf::from("C:\\bin\\spf.exe"));
        assert_eq!(
            lookup_path(&map, "spf").as_deref(),
            Some("C:\\bin\\spf.exe")
        );
        assert_eq!(
            lookup_path(&map, "SPF.EXE").as_deref(),
            Some("C:\\bin\\spf.exe"),
            "lookup is case- and extension-insensitive"
        );
        assert_eq!(
            lookup_path(&map, "C:\\tools\\my spf.exe").as_deref(),
            Some("C:\\tools\\my spf.exe")
        );
        assert_eq!(lookup_path(&map, "nothere"), None);
    }
}
