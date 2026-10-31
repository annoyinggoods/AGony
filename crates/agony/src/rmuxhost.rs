//! The daemon seam: connect, keep sessions alive, launch programs into
//! panes. Migrated from the spike (`spikes/pane-embed/src/main.rs`).
//!
//! Two rmux rules live here and nowhere else:
//!
//! 1. The daemon exits when its last session dies — so sessions are
//!    reused, never kill-then-created (`fresh_session` / `ensure_session`).
//! 2. A child program overwrites the pane title with its own console
//!    title — so panes are located by position/id, never by title
//!    (`pane_by_position`).
//!
//! Items: `RmuxHostError` — the seam's failures; `SESSION`/`SELFTEST_SESSION`
//! — the app's and the gate's session names.
//! Core functions: `connect` — the three-step self-healing connect;
//! `probe_connect` — the non-mutating probe (doctor); `ensure_daemon_env` /
//! `rmux_builder` — SDK wiring (the endpoint comes from `platform`, the
//! one OS fork); `fresh_session` / `fresh_named_session` /
//! `launch` — (re)spawn a program into a session's root pane with
//! `kill_existing(true)`; `window_session` — the per-window session
//! names (Part04); `manifest_argv` — the one argv builder for manifests;
//! `ensure_test_anchor` — the per-run test anchor (the daemon-exit rule
//! applied to `cargo test`); `teardown` — full kill for
//! `agony selftest --cleanup`.
//! Re-exports: `find_command_path` / `set_no_window` (the seam's —
//! Part12 12.2.3; kept so the seven call sites stay unchanged).
//! Depends on: rmux-sdk (the daemon protocol), agony_core::manifest
//! (`manifest_argv`'s source), crate::platform (endpoint, PATH probe,
//! window suppression).
//! Called by: app/reconcile (run + windows), selftest (its session and
//! the window scenario), doctor, discover (the shell choice), tests
//! (their own named sessions).
//! Calls: rmux-sdk, tokio time, std::process (the in-job daemon start).

use std::env;
use std::error::Error;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rmux_sdk::{EnsureSession, Pane, Rmux, Session, SessionName, TerminalSizeSpec};
use thiserror::Error;

pub const SESSION: &str = "agony";

/// The window session name under a base (Part04): `<base>-w<id>`. Window
/// ids never repeat within a run, so a name is never reused while its
/// session lives; a leftover session from a crashed run is adopted (or,
/// on a name collision, replaced by the reuse rule — `kill_existing`).
pub fn window_session(base: &str, window_id: u64) -> String {
    format!("{base}-w{window_id}")
}
const SDK_DAEMON_BINARY_ENV: &str = "RMUX_SDK_DAEMON_BINARY";
/// Tests never touch the app's live session (spawning into it with
/// `kill_existing` kills the desktop's running program) — each test owns a
/// named session instead, the same rule doctor follows with its own.
#[cfg(test)]
pub const TEST_SESSION: &str = "agony-test";
#[cfg(test)]
pub const TEST_SESSION_MISSING: &str = "agony-test-missing";
#[cfg(test)]
pub const TEST_SESSION_ADOPT: &str = "agony-test-adopt";
#[cfg(test)]
pub const TEST_SESSION_EXIT: &str = "agony-test-exit";
/// The per-run test anchor (the daemon-exit rule for `cargo test`): no
/// test ever kills it, so a test's own-session cleanup can never leave
/// the daemon with zero sessions.
#[cfg(test)]
pub const TEST_ANCHOR_SESSION: &str = "agony-test-anchor";
/// The anchor proof's own throwaway session — killable, because the
/// anchor holds the daemon up.
#[cfg(test)]
pub const TEST_SESSION_PROOF: &str = "agony-test-proof";
/// The self-test gate's session — same isolation rule as the tests'.
pub const SELFTEST_SESSION: &str = "agony-selftest";

#[derive(Debug, Error)]
pub enum RmuxHostError {
    #[error("cannot reach the rmux daemon: {0}")]
    Connect(String),
    #[error("rmux session error: {0}")]
    Session(String),
    #[error("rmux spawn error: {0}")]
    Spawn(String),
    #[error("rmux pane error: {0}")]
    Pane(String),
    #[error("rmux input error: {0}")]
    Input(String),
}

/// Point the SDK at the `rmux` binary before the first builder call. The
/// spike did this in `main`; here it also covers tests and verbs.
pub fn ensure_daemon_env() {
    env::set_var(SDK_DAEMON_BINARY_ENV, "rmux");
}

pub fn rmux_builder() -> rmux_sdk::RmuxBuilder {
    crate::platform::apply_daemon_endpoint(Rmux::builder())
}

/// The three-step self-healing connect (spike lines 74-85): a running
/// daemon, the SDK's own launcher, then our own in-job daemon start for
/// hosts whose job objects refuse breakaway.
pub async fn connect() -> Result<Rmux, RmuxHostError> {
    ensure_daemon_env();
    let builder = |secs| rmux_builder().default_timeout(Duration::from_secs(secs));
    if let Ok(rmux) = builder(5).connect().await {
        return Ok(rmux);
    }
    if let Ok(rmux) = builder(5).connect_or_start().await {
        return Ok(rmux);
    }
    start_daemon_in_job().map_err(|e| RmuxHostError::Connect(e.to_string()))?;
    connect_with_retry().await
}

/// A non-mutating reachability probe: never starts anything (doctor's rule
/// is to report, not repair).
pub async fn probe_connect() -> Result<Rmux, RmuxHostError> {
    ensure_daemon_env();
    rmux_builder()
        .default_timeout(Duration::from_secs(3))
        .connect()
        .await
        .map_err(|e| RmuxHostError::Connect(describe(&e)))
}

async fn connect_with_retry() -> Result<Rmux, RmuxHostError> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let attempt = rmux_builder()
            .default_timeout(Duration::from_secs(5))
            .connect()
            .await;
        if let Ok(rmux) = attempt {
            return Ok(rmux);
        }
        if Instant::now() > deadline {
            return Err(RmuxHostError::Connect(
                "connect, SDK start and in-job start all failed; \
                 try powershell -File scripts/start_daemon.ps1"
                    .into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Start the daemon as a plain child (no job breakaway), the way
/// `scripts/start_daemon.ps1` does, so restricted hosts still get a daemon.
fn start_daemon_in_job() -> io::Result<()> {
    let mut command = Command::new("rmux");
    command.arg("--__internal-daemon");
    command.args(crate::platform::daemon_endpoint_args());
    command
        .arg("--config-default")
        .arg("--config-quiet")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    set_no_window(&mut command);
    command.spawn()?;
    Ok(())
}

/// The seam's window-suppression fork, re-exported (Part12 12.2.3) so
/// the seven call sites here and in complete/catalog/installer/discover
/// stay unchanged.
pub use crate::platform::set_no_window;

/// The seam's one PATH probe, re-exported for the same reason.
pub use crate::platform::find_command_path;

/// Reuse the app session if it exists (the daemon exits when its last
/// session dies), otherwise create a fresh detached one, then launch the
/// given program into the root pane.
pub async fn fresh_session(
    rmux: &Rmux,
    cols: u16,
    rows: u16,
    argv: &[&str],
) -> Result<(Session, Pane), RmuxHostError> {
    fresh_named_session(rmux, SESSION, cols, rows, argv, None).await
}

/// The same, under an explicit session name (doctor and the windows'
/// reconciler use their own), with an optional working directory.
pub async fn fresh_named_session(
    rmux: &Rmux,
    name: &str,
    cols: u16,
    rows: u16,
    argv: &[&str],
    cwd: Option<&Path>,
) -> Result<(Session, Pane), RmuxHostError> {
    let session_name = SessionName::new(name).map_err(|e| RmuxHostError::Session(e.to_string()))?;
    let session = ensure_session(rmux, session_name, cols, rows).await?;
    let pane = launch(&session, cols, rows, argv, cwd).await?;
    Ok((session, pane))
}

/// The session handle when the session exists, else None (an unreachable
/// daemon counts as absent — the caller takes the create path).
async fn existing_session(rmux: &Rmux, name: &str) -> Option<Session> {
    let name = SessionName::new(name).ok()?;
    let session = rmux.session(name).await.ok()?;
    session.exists().await.unwrap_or(false).then_some(session)
}

async fn ensure_session(
    rmux: &Rmux,
    name: SessionName,
    cols: u16,
    rows: u16,
) -> Result<Session, RmuxHostError> {
    if let Some(session) = existing_session(rmux, name.as_str()).await {
        return Ok(session);
    }
    rmux.ensure_session(
        EnsureSession::named(name)
            .create_only()
            .detached(true)
            .size(TerminalSizeSpec::new(cols, rows)),
    )
    .await
    .map_err(|e| RmuxHostError::Session(describe(&e)))
}

/// The one launcher every surface reuses (start menu today; desktop icons,
/// taskbar, command bar later): replace the root pane's program
/// (`kill_existing(true)`), keep the session, return the new pane.
pub async fn launch(
    session: &Session,
    cols: u16,
    rows: u16,
    argv: &[&str],
    cwd: Option<&Path>,
) -> Result<Pane, RmuxHostError> {
    let root = session.pane(0, 0);
    root.resize(TerminalSizeSpec::new(cols, rows))
        .await
        .map_err(|e| RmuxHostError::Session(describe(&e)))?;
    let cwd = cwd
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| env::temp_dir()));
    root.spawn(argv.iter().copied())
        .cwd(cwd)
        .kill_existing(true)
        .title("agony pane")
        .keep_alive_on_exit(true)
        .await
        .map_err(|e| RmuxHostError::Spawn(describe(&e)))?;
    tokio::time::sleep(Duration::from_millis(450)).await;
    pane_by_position(session).await
}

/// The argv a manifest runs as: `exec` resolved through the discovery
/// cache when available, plus its fixed `args`. The one builder — the
/// start menu's launches and the panel's `open_with` both go through it.
pub fn manifest_argv(
    manifest: &agony_core::manifest::AppManifest,
    resolved: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![resolved.unwrap_or(&manifest.exec).to_string()];
    argv.extend(manifest.args.iter().cloned());
    argv
}

async fn pane_by_position(session: &Session) -> Result<Pane, RmuxHostError> {
    let panes = session
        .window(0)
        .panes()
        .await
        .map_err(|e| RmuxHostError::Pane(describe(&e)))?;
    let first = panes
        .first()
        .ok_or_else(|| RmuxHostError::Pane("session has no panes after spawn".into()))?;
    session
        .pane_by_id(first.id)
        .await
        .map_err(|e| RmuxHostError::Pane(describe(&e)))
}

/// The per-run test anchor — the app's own anchor rule (`fresh_session`
/// in `run`), applied to `cargo test`: an idle session no test kills, so
/// a test's own-session cleanup can never leave the daemon with zero
/// sessions (it exits then, racing the next test's reconnect — the
/// logged isolation flake). Every daemon-backed test calls this first.
/// A vanished anchor is rebuilt; a create lost to a parallel test is won
/// by re-checking existence.
#[cfg(test)]
pub async fn ensure_test_anchor(rmux: &Rmux) -> Result<(), RmuxHostError> {
    if existing_session(rmux, TEST_ANCHOR_SESSION).await.is_some() {
        return Ok(());
    }
    let anchored = fresh_named_session(
        rmux,
        TEST_ANCHOR_SESSION,
        80,
        24,
        &crate::platform::idle_shell_argv(),
        None,
    )
    .await;
    let Err(error) = anchored else {
        return Ok(());
    };
    if existing_session(rmux, TEST_ANCHOR_SESSION).await.is_some() {
        return Ok(());
    }
    Err(error)
}

/// Full teardown for `agony selftest --cleanup`: kill the gate's session,
/// then shut the daemon down. Consumes the connection (shutdown does).
pub async fn teardown(rmux: Rmux) -> Result<(), RmuxHostError> {
    if let Ok(session) = rmux
        .session(
            SessionName::new(SELFTEST_SESSION)
                .map_err(|e| RmuxHostError::Session(e.to_string()))?,
        )
        .await
    {
        let _ = session.kill().await;
    }
    let _ = rmux.shutdown().await;
    Ok(())
}

fn describe(error: &dyn Error) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connects_and_creates_a_session() {
        let rmux = match connect().await {
            Ok(rmux) => rmux,
            Err(error) => {
                eprintln!("SKIP connects_and_creates_a_session: no daemon ({error})");
                return;
            }
        };
        ensure_test_anchor(&rmux)
            .await
            .expect("the test anchor exists");
        let (session, pane) = fresh_named_session(
            &rmux,
            TEST_SESSION,
            80,
            24,
            &crate::platform::idle_shell_argv(),
            None,
        )
        .await
        .expect("fresh session works");
        assert!(session.exists().await.unwrap_or(false));
        assert!(pane.keyboard().press("Enter").await.is_ok());
        // The test session is deliberately left alive: the fixed name
        // means the next run reuses it (never kill-then-create).
    }

    /// The test-isolation fix (the logged flake): killing this test's own
    /// session as cleanup must leave the daemon alive — `ensure_test_anchor`
    /// guarantees a second session exists, so the daemon's exit-on-last-
    /// session rule cannot fire between tests (skipped loudly without a
    /// daemon).
    #[tokio::test]
    async fn killing_a_test_session_leaves_the_daemon_anchored() {
        let rmux = match connect().await {
            Ok(rmux) => rmux,
            Err(error) => {
                eprintln!(
                    "SKIP killing_a_test_session_leaves_the_daemon_anchored: no daemon ({error})"
                );
                return;
            }
        };
        ensure_test_anchor(&rmux)
            .await
            .expect("the test anchor exists");
        let (session, _pane) = fresh_named_session(
            &rmux,
            TEST_SESSION_PROOF,
            80,
            24,
            &crate::platform::idle_shell_argv(),
            None,
        )
        .await
        .expect("the throwaway session spawns");
        session.kill().await.expect("the throwaway session dies");
        assert!(
            rmux.list_sessions().await.is_ok(),
            "the daemon survived the last non-anchor session's kill"
        );
        ensure_test_anchor(&rmux)
            .await
            .expect("the anchor still holds after the kill");
    }
}
