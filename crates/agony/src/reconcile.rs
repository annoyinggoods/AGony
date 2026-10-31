//! The one bridge between the core's window state and the daemon's panes
//! (Part04 4.4). No other module names a pane or a session: when a
//! window's inner size changes, `resize_all` resizes its pane; when a
//! window closes, `close_pane` kills the pane; when a window opens,
//! `launch` spawns the program through rmuxhost's one launcher.
//!
//! Two rmux rules are restated here because this module is where they
//! bite: the daemon exits when its last session dies (the app keeps the
//! anchor session `agony` alive for exactly that), and a child program
//! overwrites the pane title — panes are located by position/id, never
//! by title (`adopt` reads a title only as best-effort *recovery text*,
//! never as identity).
//!
//! One SDK rule lives here too: pane process/exit state exists only on
//! the pane-scoped `Pane::info()` snapshot — the window-level
//! `Window::info()` snapshot leaves it `Unknown` by construction — so
//! every liveness read (`adopt`'s dead-pane skip, the exit watcher's
//! poll, `pane_cwd`, the app's input-error path) goes through the pane
//! handle.
//!
//! Items: `Panes` — window id → pane record (`WindowPane`: pane handle,
//! driver, session name, last issued grid size); `LaunchError` — the
//! bridge's failures.
//! Core functions: `launch` — open a core window and spawn its program
//! into a fresh named session (`<base>-w<id>`; ids never repeat, so a
//! name is never reused while alive); `adopt` — reattach windows to
//! surviving `<base>-w<id>` sessions after a restart or detach, skipping
//! (and killing) sessions whose pane already exited — a keep-alive
//! zombie would otherwise come back as an empty titled window;
//! `resize_all` — bring every pane to its window's current inner size
//! (called after resizes and at drag end — coalescing); `refresh_all` —
//! snapshot every pane for the draw pass; `forward_key` / `click` —
//! input into the right pane; `close_pane` — kill one window's pane.
//! Depends on: agony_core::layout (window math), rmuxhost (the seam),
//! input (key forwarding), ratatui-rmux (drivers).
//! Called by: app (the loop), selftest (the window scenario), its own
//! daemon-backed test.
//! Calls: rmux-sdk through rmuxhost only.

use std::collections::HashMap;
use std::path::Path;

use agony_core::layout::{effective_rect, inner_rect, WmState};
use ratatui_rmux::PaneDriver;
use rmux_sdk::{Pane, PaneProcessState, Rmux, Session, SessionName};
use thiserror::Error;

use crate::input;
use crate::rmuxhost::{self, RmuxHostError};

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error(transparent)]
    Host(#[from] RmuxHostError),
    #[error("window spawn failed: {0}")]
    Spawn(String),
}

struct WindowPane {
    pane: Pane,
    driver: PaneDriver,
    /// The pane's engine-side key (the cwd lookup and the exit watcher
    /// find `PaneInfo` by it).
    pane_id: Option<u32>,
    /// The owning session handle: a one-pane window cannot resize its
    /// pane directly (rmux redistributes only between sibling panes), so
    /// the reconciler resizes the *window* and the pane follows.
    session: Session,
    size: (u16, u16),
}

/// Every live window's pane, keyed by core window id.
#[derive(Default)]
pub struct Panes {
    records: HashMap<u64, WindowPane>,
}

impl Panes {
    /// Open a core window and spawn `argv` into its own session
    /// `<base>-w<id>`, sized to the window's inner rect. On a spawn
    /// failure the half-opened window is rolled back (and the stray
    /// session killed) and the error surfaces — never a silent no-op.
    pub async fn launch(
        &mut self,
        rmux: &Rmux,
        base: &str,
        wm: &mut WmState,
        title: &str,
        argv: &[&str],
        cwd: Option<&Path>,
    ) -> Result<u64, LaunchError> {
        let id = wm.open(title, None);
        let Some(rect) = wm.window(id).map(|w| effective_rect(w, wm.desktop)) else {
            wm.close(id);
            return Err(LaunchError::Spawn("window vanished at open".into()));
        };
        let inner = inner_rect(rect);
        let session = rmuxhost::window_session(base, id);
        let spawned =
            rmuxhost::fresh_named_session(rmux, &session, inner.width, inner.height, argv, cwd)
                .await;
        let (session_handle, pane) = match spawned {
            Ok(pair) => pair,
            Err(error) => {
                wm.close(id);
                kill_quietly(rmux, &session).await;
                return Err(error.into());
            }
        };
        let pane_id = pane.id().await.ok().flatten().map(|id| id.as_u32());
        wm.set_pane(id, pane_id);
        self.records.insert(
            id,
            WindowPane {
                driver: PaneDriver::new(pane.clone()),
                pane,
                pane_id,
                session: session_handle,
                size: (inner.width, inner.height),
            },
        );
        Ok(id)
    }

    /// Reattach windows to surviving `<base>-w<id>` sessions (a restart or
    /// a detach left them running). Panes are located by position and id;
    /// the pane's current title is the best available recovery label (the
    /// manifest name is gone by then — the clobbered-title rule forbids
    /// titles as identity, not as a fallback label). A session whose pane
    /// already exited (a keep-alive zombie — the user closed the shell)
    /// is skipped and its stray session killed: adopting it would
    /// resurrect an empty titled window. Returns how many windows were
    /// adopted.
    pub async fn adopt(&mut self, rmux: &Rmux, base: &str, wm: &mut WmState) -> usize {
        let names = rmux.list_sessions().await.unwrap_or_default();
        let candidates: Vec<String> = names
            .iter()
            .map(|name| name.as_str().to_string())
            .filter(|name| is_window_session(base, name))
            .collect();
        let mut adopted = 0;
        for name in candidates {
            let Some((session, pane)) = root_pane(rmux, &name).await else {
                continue;
            };
            if pane_has_exited(&pane).await {
                let _ = session.kill().await;
                continue;
            }
            let title = pane
                .title()
                .await
                .ok()
                .flatten()
                .unwrap_or_else(|| name.clone());
            let pane_id = pane.id().await.ok().flatten().map(|id| id.as_u32());
            let id = wm.open(&title, pane_id);
            self.records.insert(
                id,
                WindowPane {
                    driver: PaneDriver::new(pane.clone()),
                    pane,
                    pane_id,
                    session,
                    // Unknown until the first sync: force a resize pass.
                    size: (0, 0),
                },
            );
            adopted += 1;
        }
        adopted
    }

    /// The draw pass's snapshot source for one window.
    pub fn driver(&self, id: u64) -> Option<&PaneDriver> {
        self.records.get(&id).map(|record| &record.driver)
    }

    /// Bring every pane to its window's current inner size. Only panes
    /// whose target differs from the last issued size are resized, so an
    /// overlap (a paint-order concern, not a geometry one) never resizes
    /// a pane, and a drag issues nothing until its end. Resize failures
    /// are returned for the status line — never swallowed.
    pub async fn resize_all(&mut self, wm: &WmState) -> Vec<String> {
        let targets = self.targets(wm);
        let mut errors = Vec::new();
        for (id, target) in targets {
            let applied = apply_resize(&mut self.records, id, target).await;
            if let Err(error) = applied {
                errors.push(format!("resize window {id}: {error}"));
            }
        }
        errors
    }

    fn targets(&self, wm: &WmState) -> Vec<(u64, (u16, u16))> {
        wm.windows
            .iter()
            .filter_map(|window| {
                self.records.get(&window.id)?;
                let rect = effective_rect(window, wm.desktop);
                let inner = inner_rect(rect);
                Some((window.id, (inner.width, inner.height)))
            })
            .collect()
    }

    /// Snapshot every pane for the draw pass. Errors are dropped: a dead
    /// pane paints its last state until the window closes.
    pub async fn refresh_all(&mut self) {
        for record in self.records.values_mut() {
            let _ = record.driver.refresh().await;
        }
    }

    /// One key press into a window's pane (the error, if any, is the
    /// caller's status line).
    pub async fn forward_key(
        &self,
        id: u64,
        key: crossterm::event::KeyEvent,
    ) -> Result<(), RmuxHostError> {
        let Some(record) = self.records.get(&id) else {
            return Ok(());
        };
        input::forward_key(&record.pane, key).await
    }

    /// One interior click into a window's pane, at pane-relative cells.
    pub async fn click(&self, id: u64, row: u16, col: u16) -> Result<(), RmuxHostError> {
        let Some(record) = self.records.get(&id) else {
            return Ok(());
        };
        record
            .pane
            .mouse()
            .click(row, col)
            .await
            .map_err(|e| RmuxHostError::Input(e.to_string()))
    }

    /// A clonable handle on the window's pane (the command bar's exit
    /// watcher holds it across ticks; when the window closes, the
    /// watcher's info call simply fails then).
    pub fn pane_handle(&self, id: u64) -> Option<Pane> {
        self.records.get(&id).map(|record| record.pane.clone())
    }

    /// The pane's engine-side key, when one was resolved.
    pub fn pane_key(&self, id: u64) -> Option<u32> {
        self.records.get(&id).and_then(|record| record.pane_id)
    }

    /// The pane's working directory from the daemon's pane info (the
    /// command bar's context, Part07 7.2.2) — read through the
    /// pane-scoped snapshot, the only one that carries it. None when the
    /// window has no pane or the daemon could not resolve one — the
    /// caller falls back to the desktop's cwd.
    pub async fn pane_cwd(&self, id: u64) -> Option<String> {
        let record = self.records.get(&id)?;
        let pane_key = record.pane_id?;
        let info = record.pane.info().await.ok()?;
        info.pane(rmux_sdk::PaneId::from(pane_key))?
            .working_directory
            .clone()
    }

    /// The records map's emptiness (the rollback test's check).
    #[cfg(test)]
    pub fn records_is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// The session names under management (the daemon-backed tests'
    /// diagnostic).
    #[cfg(test)]
    pub fn session_names(&self) -> Vec<String> {
        self.records
            .values()
            .map(|r| r.session.name().as_str().to_string())
            .collect()
    }

    /// Kill one window's pane (its session dies with its last pane — the
    /// anchor session keeps the daemon alive). Idempotent.
    pub async fn close_pane(&mut self, id: u64) {
        let Some(record) = self.records.remove(&id) else {
            return;
        };
        let _ = record.pane.close().await;
    }
}

async fn apply_resize(
    records: &mut HashMap<u64, WindowPane>,
    id: u64,
    target: (u16, u16),
) -> Result<(), String> {
    let Some(record) = records.get_mut(&id) else {
        return Ok(());
    };
    if record.size == target {
        return Ok(());
    }
    record
        .session
        .window(0)
        .resize(Some(target.0), Some(target.1))
        .await
        .map_err(|e| e.to_string())?;
    record.size = target;
    let _ = record.driver.refresh().await;
    Ok(())
}

/// A window session name under `base`: exactly `<base>-w<digits>`.
pub fn is_window_session(base: &str, name: &str) -> bool {
    let Some(rest) = name.strip_prefix(&format!("{base}-w")) else {
        return false;
    };
    !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit())
}

/// The session handle plus its root pane, by position then id — never
/// by title.
async fn root_pane(rmux: &Rmux, session: &str) -> Option<(Session, Pane)> {
    let name = SessionName::new(session).ok()?;
    let handle = rmux.session(name).await.ok()?;
    if !handle.exists().await.unwrap_or(false) {
        return None;
    }
    let panes = handle.window(0).panes().await.ok()?;
    let first = panes.first()?;
    let pane = handle.pane_by_id(first.id).await.ok()?;
    Some((handle, pane))
}

/// The root pane's liveness from the pane-scoped info snapshot — the
/// only SDK path that carries process/exit state. An unreadable or
/// silent daemon means not-exited: `adopt` never skips on doubt, only
/// on a positively reported exit.
pub(crate) async fn pane_has_exited(pane: &Pane) -> bool {
    let Ok(id) = pane.id().await else {
        return false;
    };
    let Some(id) = id else {
        return false;
    };
    let Ok(snapshot) = pane.info().await else {
        return false;
    };
    snapshot.pane(id).is_some_and(|info| {
        matches!(info.process, PaneProcessState::Exited) || info.exit_state.is_some()
    })
}

async fn kill_quietly(rmux: &Rmux, session: &str) {
    let Ok(name) = SessionName::new(session) else {
        return;
    };
    if let Ok(handle) = rmux.session(name).await {
        let _ = handle.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agony_core::layout::{Edge, Rect};

    fn base() -> &'static str {
        rmuxhost::TEST_SESSION
    }

    fn missing_base() -> &'static str {
        rmuxhost::TEST_SESSION_MISSING
    }

    #[test]
    fn window_session_names_are_recognized_exactly() {
        assert!(is_window_session("agony", "agony-w1"));
        assert!(is_window_session("agony", "agony-w12"));
        assert!(!is_window_session("agony", "agony-w"));
        assert!(!is_window_session("agony", "agony-w1x"));
        assert!(!is_window_session("agony", "agony-selftest-w1"));
        assert!(!is_window_session("agony", "agony"));
    }

    /// Part04 4.7.4: open, resize, close against the real daemon seam —
    /// skipped loudly when no daemon is reachable.
    #[tokio::test]
    async fn windows_open_resize_and_close_through_the_daemon() {
        let rmux = match rmuxhost::connect().await {
            Ok(rmux) => rmux,
            Err(error) => {
                eprintln!("SKIP windows_open_resize_and_close: no daemon ({error})");
                return;
            }
        };
        let mut wm = WmState::new(Rect::new(0, 0, 80, 24));
        let mut panes = Panes::default();
        rmuxhost::ensure_test_anchor(&rmux)
            .await
            .expect("the test anchor exists");
        let argv = crate::platform::idle_shell_argv();
        let id = panes
            .launch(&rmux, base(), &mut wm, "gate", &argv, None)
            .await
            .expect("window opens");
        assert!(panes.driver(id).is_some());
        assert_eq!(
            panes.session_names(),
            vec![rmuxhost::window_session(base(), id)]
        );

        // Resize the window's edge; the reconciler must bring the pane to
        // the new inner size (polled — ConPTY reflow is asynchronous).
        wm.resize_edge(id, Edge::Right, -20);
        let _ = panes.resize_all(&wm).await;
        let target = inner_rect_of(&wm, id);
        assert_pane_reaches(&mut panes, id, target).await;

        // Close: the pane is killed and its session dies with it; the
        // daemon itself must survive (the anchor rule).
        panes.close_pane(id).await;
        wm.close(id);
        assert!(panes.driver(id).is_none());
        let gone = async {
            let name = SessionName::new(rmuxhost::window_session(base(), id)).expect("name");
            match rmux.session(name).await {
                Ok(handle) => !handle.exists().await.unwrap_or(true),
                Err(_) => true,
            }
        };
        assert!(gone.await, "the window session died with its pane");
        let daemon_alive = rmux.list_sessions().await.is_ok();
        assert!(daemon_alive, "the daemon survived the window close");
    }

    /// A missing exec must fail loudly and roll the window back — the
    /// Part03 failure-mode rule, ported to the window launcher (skipped
    /// loudly without a daemon).
    #[tokio::test]
    async fn launch_fails_loudly_and_rolls_back_when_exec_is_missing() {
        let rmux = match rmuxhost::connect().await {
            Ok(rmux) => rmux,
            Err(error) => {
                eprintln!("SKIP launch_fails_loudly: no daemon ({error})");
                return;
            }
        };
        rmuxhost::ensure_test_anchor(&rmux)
            .await
            .expect("the test anchor exists");
        let mut wm = WmState::new(Rect::new(0, 0, 80, 24));
        let mut panes = Panes::default();
        let argv = ["agony-no-such-binary-test.exe"];
        let before = wm.windows.len();
        let result = panes
            .launch(&rmux, missing_base(), &mut wm, "missing", &argv, None)
            .await;
        let error = result.expect_err("a missing exec must fail, not silently no-op");
        assert!(
            error.to_string().contains("spawn")
                || error.to_string().contains("agony-no-such-binary-test"),
            "error should name the failure: {error}"
        );
        assert_eq!(
            wm.windows.len(),
            before,
            "the half-opened window is rolled back"
        );
        assert!(panes.driver(0).is_none() && panes.records_is_empty());
    }

    /// The dead-pane adoption skip: a session whose pane already exited
    /// (a keep-alive zombie) must not come back as an empty titled
    /// window — it is skipped and its stray session killed, while a live
    /// sibling under the same base is adopted (skipped loudly without a
    /// daemon).
    #[tokio::test]
    async fn adopt_skips_a_session_whose_pane_already_exited() {
        let rmux = match rmuxhost::connect().await {
            Ok(rmux) => rmux,
            Err(error) => {
                eprintln!(
                    "SKIP adopt_skips_a_session_whose_pane_already_exited: no daemon ({error})"
                );
                return;
            }
        };
        let base = rmuxhost::TEST_SESSION_ADOPT;
        rmuxhost::ensure_test_anchor(&rmux)
            .await
            .expect("the test anchor exists");
        let live_name = format!("{base}-w1");
        let zombie_name = format!("{base}-w2");
        kill_quietly(&rmux, &live_name).await;
        kill_quietly(&rmux, &zombie_name).await;

        let idle = crate::platform::idle_shell_argv();
        let (_live_session, _live_pane) =
            rmuxhost::fresh_named_session(&rmux, &live_name, 80, 24, &idle, None)
                .await
                .expect("live window session spawns");
        let exiting = crate::platform::exiting_child_argv();
        let (_zombie_session, zombie_pane) =
            rmuxhost::fresh_named_session(&rmux, &zombie_name, 80, 24, &exiting, None)
                .await
                .expect("zombie window session spawns");
        assert!(
            wait_until_reported_exited(&zombie_pane).await,
            "the daemon must report the exited pane before adopt runs"
        );

        let mut wm = WmState::new(Rect::new(0, 0, 80, 24));
        let mut panes = Panes::default();
        let adopted = panes.adopt(&rmux, base, &mut wm).await;

        assert_eq!(adopted, 1, "only the live session is adopted");
        assert_eq!(panes.session_names(), vec![live_name.clone()]);
        let names = rmux.list_sessions().await.unwrap_or_default();
        assert!(
            names.iter().all(|name| name.as_str() != zombie_name),
            "the zombie session was killed, not adopted"
        );

        panes
            .close_pane(wm.windows.last().map(|w| w.id).unwrap_or(1))
            .await;
        kill_quietly(&rmux, &live_name).await;
        kill_quietly(&rmux, &zombie_name).await;
    }

    /// Poll the pane-scoped liveness read until the daemon reports the
    /// exit (the child's death is asynchronous to the spawn call).
    async fn wait_until_reported_exited(pane: &Pane) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if pane_has_exited(pane).await {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    fn inner_rect_of(wm: &WmState, id: u64) -> (u16, u16) {
        let rect = effective_rect(wm.window(id).expect("window"), wm.desktop);
        let inner = inner_rect(rect);
        (inner.width, inner.height)
    }

    async fn assert_pane_reaches(panes: &mut Panes, id: u64, target: (u16, u16)) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
        loop {
            if let Some(driver) = panes.driver(id) {
                let snapshot = &driver.state().snapshot;
                if snapshot.cols == target.0 && snapshot.rows == target.1 {
                    return;
                }
                eprintln!(
                    "pane at {}x{}, waiting for {}x{}",
                    snapshot.cols, snapshot.rows, target.0, target.1
                );
            }
            assert!(
                std::time::Instant::now() < deadline,
                "pane never reached {target:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            panes.refresh_all().await;
        }
    }
}
