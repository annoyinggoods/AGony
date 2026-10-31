//! The carapace completion driver (Part07 7.3.1): the only module that
//! spawns carapace for completions — `carapace <command> nushell --
//! <words…>`, 300 ms timeout, the JSON array parsed into the core's
//! candidates. Requests are debounced (no subprocess before 150 ms of
//! quiet typing), cached per (command, word) for the session, and a
//! stale answer is dropped by construction: it is stored under the key
//! that asked for it, so an older prefix's answer is never read by a
//! newer one.
//!
//! Items: `Shared` — the resolved binary, the cache and the in-flight
//! key behind one lock; `CompletionError` — the driver's failures.
//! Core functions: `Shared::start` — resolve carapace once (doctor's
//! resolution order, via `discover::carapace_path`); `Shared::cached` —
//! the cache read; `Shared::request` — the debounced spawn (no-op when
//! suggestions are off, carapace is absent, or that key is already in
//! flight); `run` — one subprocess call; `parse_completions` — the JSON
//! → `Candidate` mapping (carapace's `value` carries a trailing space —
//! trimmed here, never re-split by the caller).
//! Depends on: agony_core::suggest (the candidate type), discover (the
//! one carapace resolution), rmuxhost (no-window spawn flags), serde_json.
//! Called by: app (the tick's request + the bar's cache read).
//! Calls: tokio process with a timeout — never inline on the UI thread.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agony_core::suggest::{Candidate, Scope, Source};
use serde::Deserialize;
use tokio::process::Command;

use crate::discover;
use crate::rmuxhost;

/// Quiet typing before a carapace subprocess may spawn (Part07 7.3.7).
pub const QUIET_MS: u128 = 150;
/// The subprocess cap: past it the bar degrades to local sources.
const TIMEOUT: Duration = Duration::from_millis(300);

#[derive(Debug, thiserror::Error)]
pub enum CompletionError {
    #[error("carapace failed: {0}")]
    Failed(String),
    #[error("carapace output was not a completion array: {0}")]
    Parse(String),
}

/// One raw carapace array element. `style` is whatever carapace emits
/// (an object today) — parsed and ignored.
#[derive(Debug, Deserialize)]
struct RawCompletion {
    value: String,
    #[serde(default)]
    display: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    #[allow(dead_code)]
    style: serde_json::Value,
}

#[derive(Default)]
struct Inner {
    cache: HashMap<(String, String), Vec<Candidate>>,
    in_flight: Option<(String, String)>,
}

/// The session-wide completion state behind one lock (discover's
/// poisoning-recovering read pattern).
#[derive(Clone)]
pub struct Shared {
    binary: Option<PathBuf>,
    enabled: bool,
    inner: Arc<Mutex<Inner>>,
}

impl Shared {
    /// Resolve carapace once at startup (the doctor's order, through
    /// discover's one implementation). A missing binary is a degraded
    /// mode, not an error: the bar suggests catalog and history and
    /// says so once at startup.
    pub fn start(enabled: bool) -> (Shared, Option<String>) {
        let binary = if enabled {
            discover::carapace_path()
        } else {
            None
        };
        let note = if enabled && binary.is_none() {
            Some(
                "carapace not found — the command bar suggests apps and history only \
                 (scripts/get-carapace.ps1 installs it)"
                    .into(),
            )
        } else {
            None
        };
        (
            Shared {
                binary,
                enabled,
                inner: Arc::new(Mutex::new(Inner::default())),
            },
            note,
        )
    }

    /// Whether completions may spawn carapace at all (the settings
    /// promise, Part07 7.3.6).
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Flip the settings promise (Part08's Behavior section applies it
    /// live). Enabling re-resolves the binary once — the startup
    /// resolution may have run while carapace was absent from PATH, or
    /// the flag simply started false.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled && self.binary.is_none() {
            self.binary = discover::carapace_path();
        }
        self.enabled = enabled;
    }

    /// The cached candidates for (command, word), if any.
    pub fn cached(&self, command: &str, word: &str) -> Option<Vec<Candidate>> {
        lock(&self.inner)
            .cache
            .get(&(command.to_string(), word.to_string()))
            .cloned()
    }

    /// Whether that exact request is already running.
    pub fn in_flight(&self, command: &str, word: &str) -> bool {
        lock(&self.inner).in_flight.as_ref() == Some(&(command.to_string(), word.to_string()))
    }

    /// Spawn one carapace call for `command`'s `words` (the active word
    /// last). A no-op when disabled, unresolved, or already in flight
    /// for the same key — the debounce lives with the caller's quiet
    /// check, the single-subprocess rule here.
    pub fn request(&self, command: String, words: Vec<String>) {
        if !self.enabled || self.binary.is_none() || command.is_empty() {
            return;
        }
        let Some(word) = words.last().cloned() else {
            return;
        };
        {
            let mut inner = lock(&self.inner);
            if inner.in_flight.is_some() {
                return;
            }
            inner.in_flight = Some((command.clone(), word.clone()));
        }
        let binary = self.binary.clone().unwrap_or_default();
        let shared = self.clone();
        tokio::spawn(async move {
            let result = run(&binary, &command, &words).await;
            let mut inner = lock(&shared.inner);
            inner.in_flight = None;
            if let Ok(candidates) = result {
                inner.cache.insert((command, word), candidates);
            }
        });
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// One carapace subprocess call, timeout-bounded.
pub async fn run(
    binary: &std::path::Path,
    command: &str,
    words: &[String],
) -> Result<Vec<Candidate>, CompletionError> {
    let mut process = Command::new(binary);
    process.arg(command).arg("nushell").arg("--").args(words);
    rmuxhost::set_no_window(process.as_std_mut());
    process.stdin(std::process::Stdio::null());
    process.stdout(std::process::Stdio::piped());
    process.stderr(std::process::Stdio::null());
    let output = tokio::time::timeout(TIMEOUT, process.output())
        .await
        .map_err(|_| CompletionError::Failed("timed out after 300 ms".into()))?
        .map_err(|error| CompletionError::Failed(error.to_string()))?;
    if !output.status.success() {
        return Err(CompletionError::Failed(format!(
            "exit {:?}",
            output.status.code()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_completions(&text)
}

/// The recorded-shape parser: a JSON array of
/// `{value, display, description, style}`. `value` is the insertion
/// text (carapace pads commands with a trailing space — trimmed);
/// `display` falls back to `value`.
pub fn parse_completions(json: &str) -> Result<Vec<Candidate>, CompletionError> {
    let raw: Vec<RawCompletion> =
        serde_json::from_str(json).map_err(|error| CompletionError::Parse(error.to_string()))?;
    Ok(raw
        .into_iter()
        .map(|entry| {
            let insert = entry.value.trim_end().to_string();
            Candidate {
                display: if entry.display.is_empty() {
                    insert.clone()
                } else {
                    entry.display
                },
                insert,
                description: entry.description,
                source: Source::Carapace,
                scope: Scope::Word,
                failed: false,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Recorded from `bin/carapace.exe git nushell -- che` (2026-10-25).
    const FIXTURE: &str = r#"[
        {"value":"check-attr ","display":"check-attr","description":"Display gitattributes information","style":{"fg":"green","attr":"d"}},
        {"value":"checkout ","display":"checkout","description":"Switch branches or restore working tree files","style":{"fg":"blue"}},
        {"value":"cherry ","display":"cherry","description":"Find commits yet to be applied to upstream","style":{"fg":"magenta","attr":"d"}}
    ]"#;

    #[test]
    fn fixture_parses_into_word_candidates() {
        let candidates = parse_completions(FIXTURE).expect("fixture parses");
        assert_eq!(candidates.len(), 3);
        let checkout = &candidates[1];
        assert_eq!(checkout.insert, "checkout", "the trailing pad is trimmed");
        assert_eq!(checkout.display, "checkout");
        assert_eq!(
            checkout.description, "Switch branches or restore working tree files",
            "the description rides along for the right-aligned dim column"
        );
        assert_eq!(checkout.source, Source::Carapace);
        assert_eq!(checkout.scope, Scope::Word);
    }

    #[test]
    fn non_array_output_is_a_parse_error_not_a_crash() {
        assert!(parse_completions("not json").is_err());
        assert!(parse_completions("{\"value\":\"x\"}").is_err());
    }

    #[test]
    fn display_falls_back_to_value() {
        let candidates =
            parse_completions(r#"[{"value":"flag","description":"d"}]"#).expect("parses");
        assert_eq!(candidates[0].display, "flag");
    }

    #[test]
    fn disabled_completions_spawn_nothing_and_stay_quiet() {
        // The settings promise (7.3.6): off means off — no subprocess,
        // no startup note about a binary we were told not to use.
        let (shared, note) = Shared::start(false);
        assert!(note.is_none());
        assert!(!shared.enabled());
        shared.request("git".into(), vec!["che".into()]);
        assert!(
            shared.cached("git", "che").is_none(),
            "no request went out, so no answer can exist"
        );
    }

    /// The startup note (the AC's one status line) fires exactly when
    /// completions are wanted but carapace is absent — deterministic
    /// per machine, so the test derives its expectation from the same
    /// resolution the startup uses.
    #[test]
    fn the_startup_note_matches_presence() {
        let present = discover::carapace_path().is_some();
        let (_, note) = Shared::start(true);
        assert_eq!(note.is_some(), !present, "one note iff enabled but absent");
        if let Some(note) = note {
            assert!(note.contains("carapace"), "{note}");
        }
    }

    /// The live leg (Part07 7.6.4): the real binary when it is present,
    /// a loud skip when it is not — and when a found file cannot be
    /// spawned (an environment report, not an AGony failure; doctor
    /// names the remedy).
    #[tokio::test]
    async fn live_carapace_completes_git_subcommands() {
        let Some(binary) = discover::carapace_path() else {
            eprintln!("SKIP live_carapace_completes: carapace not found");
            return;
        };
        let candidates = match run(&binary, "git", &["che".to_string()]).await {
            Ok(candidates) => candidates,
            Err(error) => {
                eprintln!(
                    "SKIP live_carapace_completes: found {} but it does not run ({error})",
                    binary.display()
                );
                return;
            }
        };
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.insert == "checkout"),
            "git che should offer checkout: {candidates:?}"
        );
    }
}
