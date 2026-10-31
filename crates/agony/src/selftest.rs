//! The self-test gate: end-to-end render and input through a real rmux
//! daemon into a ratatui TestBackend buffer. Migrated from the spike's
//! `self_test` / `wait_for_text` (lines 330-400), plus the resize phase
//! Part02's pitfalls demand and the two-window focus scenario Part04
//! 4.7.8 adds.
//!
//! Core functions: `run` — connect, spawn the platform gate child
//! (`platform::gate_child`: cmd.exe on Windows, an echoing shell
//! preamble on Unix), assert the child's banner renders (render path),
//! type a line and see it echoed (input path), resize and see the pane
//! reflow, then open two windows through the reconciler, focus the
//! second, type and prove the text lands in the focused pane alone;
//! prints the `self-test: PASS` line the AGENTS.md checklist greps for.
//! `--cleanup` additionally kills the session and shuts the daemon
//! down; a plain run detaches, so the next run starts instantly.
//! `wait_for_text` — poll-and-render until a needle appears (doctor's
//! pane round-trip reuses it). `buffer_text` — the one test-buffer
//! reader (taskbar/startmenu tests and doctor reuse it).
//! Depends on: rmuxhost (connect + fresh session), reconcile (the
//! window scenario), platform (the gate child), theme (the base style
//! is the applier's — no color is named here).
//! Called by: `agony selftest` (main), doctor, the widget tests.
//! Calls: rmux-sdk through the host seam, ratatui TestBackend.

use std::io;
use std::time::{Duration, Instant};

use agony_core::layout::{Rect, WmState};
use agony_core::thememodel::Role;
use ratatui::{backend::TestBackend, style::Style, Terminal};
use ratatui_rmux::{PaneDriver, PaneWidget};
use rmux_sdk::{Pane, Rmux};
use thiserror::Error;

use crate::reconcile::Panes;
use crate::rmuxhost::{self, RmuxHostError};
use crate::theme::Applier;

#[derive(Debug, Error)]
pub enum SelftestError {
    #[error(transparent)]
    Host(#[from] RmuxHostError),
    #[error("self-test: FAIL - {0}")]
    Fail(String),
    #[error("self-test terminal error: {0}")]
    Terminal(#[from] io::Error),
}

const INPUT_TEXT: &str = "echo AGONY_INPUT_OK";
const INPUT_NEEDLE: &str = "AGONY_INPUT_OK";
const WINDOW_TEXT: &str = "echo AGONY_WINDOW2_OK";
const WINDOW_NEEDLE: &str = "AGONY_WINDOW2_OK";

pub async fn run(cleanup: bool) -> Result<(), SelftestError> {
    println!("self-test: connecting to rmux daemon + session...");
    let rmux = rmuxhost::connect().await?;
    // The gate owns its sessions: running it must never touch a live
    // desktop's panes (the app session is out of bounds for every verb).
    let gate = crate::platform::gate_child();
    let (session, pane) =
        rmuxhost::fresh_named_session(&rmux, rmuxhost::SELFTEST_SESSION, 100, 30, &gate.argv, None)
            .await?;
    let mut driver = PaneDriver::new(pane.clone());
    let mut terminal = Terminal::new(TestBackend::new(100, 30))?;

    // Phase 1 (render): the gate child's opening output (cmd's startup
    // banner on Windows, the preamble echo elsewhere) through the PTY ->
    // snapshot -> PaneWidget -> buffer.
    println!("self-test: phase 1 — render (waiting for the gate child's banner)...");
    let rendered = wait_for_text(&mut terminal, &mut driver, gate.render_needle, 15).await;

    // Phase 2 (input): typed keys echo back through the same chain.
    println!("self-test: phase 2 — input (typing {INPUT_TEXT:?})...");
    type_line(&pane, INPUT_TEXT).await?;
    let echoed = wait_for_text(&mut terminal, &mut driver, INPUT_NEEDLE, 10).await;

    // Phase 3 (resize): the window (and with it the pane's grid — a
    // one-pane window cannot resize its pane directly) reflows to a
    // smaller size, and the pane's grid verifiably follows.
    println!("self-test: phase 3 — resize (reflow to 60x20)...");
    let reflowed = resize_and_check(&session, &mut driver, 60, 20).await;

    // Phase 4 (windows, Part04 4.7.8): two windows, focus the second,
    // type, and the text lands in the focused pane alone.
    println!("self-test: phase 4 — windows (two windows, focused input)...");
    let windowed = window_scenario(&rmux).await;

    // Drop the session handle explicitly: keeping it alive past this point
    // has no effect (detach-on-quit is the app's rule, not the gate's).
    drop(session);
    report(cleanup, rmux, [rendered, echoed, reflowed, windowed]).await
}

/// Part04 4.7.8: open two windows through the reconciler (under the
/// gate's own session namespace), type into the focused second window,
/// prove the text reached only its pane, then close both — the daemon
/// must survive (the anchor rule).
async fn window_scenario(rmux: &Rmux) -> bool {
    let mut wm = WmState::new(Rect::new(0, 0, 60, 20));
    let mut panes = Panes::default();
    let argv = crate::platform::idle_shell_argv();
    let first = panes
        .launch(
            rmux,
            rmuxhost::SELFTEST_SESSION,
            &mut wm,
            "gate-w1",
            &argv,
            None,
        )
        .await;
    let second = panes
        .launch(
            rmux,
            rmuxhost::SELFTEST_SESSION,
            &mut wm,
            "gate-w2",
            &argv,
            None,
        )
        .await;
    let (w1, w2) = match (first, second) {
        (Ok(w1), Ok(w2)) => (w1, w2),
        _ => return false,
    };
    panes.refresh_all().await;
    let focused = wm.focus == Some(w2);
    if !focused {
        return false; // open focuses; the second window must hold focus
    }
    let Some(record) = panes.driver(w2) else {
        return false;
    };
    let keyboard = record.pane().keyboard();
    let typed = keyboard.type_text(WINDOW_TEXT.to_string()).await;
    let entered = keyboard.press("Enter".to_string()).await;
    if typed.is_err() || entered.is_err() {
        return false;
    }
    let landed = wait_for_pane_text(&mut panes, w2, WINDOW_NEEDLE, 10).await;
    panes.refresh_all().await;
    let w1_text = panes.driver(w1).map(pane_text).unwrap_or_default();
    let only_focused = landed && !w1_text.contains(WINDOW_NEEDLE);
    panes.close_pane(w1).await;
    panes.close_pane(w2).await;
    only_focused
}

/// Poll one pane's driver until `needle` appears in its snapshot.
async fn wait_for_pane_text(
    panes: &mut Panes,
    window: u64,
    needle: &str,
    timeout_secs: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let _ = panes.refresh_all().await;
        let found = panes
            .driver(window)
            .map(pane_text)
            .is_some_and(|text| text.contains(needle));
        if found {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// One pane's snapshot as text (the pane-side twin of `buffer_text`).
fn pane_text(driver: &PaneDriver) -> String {
    let snapshot = &driver.state().snapshot;
    let mut text = String::new();
    for row in 0..snapshot.rows {
        text.push_str(&snapshot.row_text(row));
        text.push('\n');
    }
    text
}

/// Poll the pane and render into the test buffer until `needle` shows up.
pub(crate) async fn wait_for_text(
    terminal: &mut Terminal<TestBackend>,
    driver: &mut PaneDriver,
    needle: &str,
    timeout_secs: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        let _ = driver.refresh().await;
        let _ = terminal.draw(|frame| {
            let widget = PaneWidget::new(driver.state()).base_style(pane_style());
            frame.render_widget(widget, frame.area());
        });
        if buffer_text(terminal).contains(needle) {
            return true;
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

/// The rendered text of a test terminal — shared by this gate and the
/// widget tests (one implementation of buffer reading).
pub(crate) fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol().to_string())
        .collect()
}

async fn type_line(pane: &Pane, text: &str) -> Result<(), SelftestError> {
    let keyboard = pane.keyboard();
    keyboard
        .type_text(text.to_string())
        .await
        .map_err(|e| SelftestError::Fail(format!("cannot type into pane: {e}")))?;
    keyboard
        .press("Enter".to_string())
        .await
        .map_err(|e| SelftestError::Fail(format!("cannot press Enter: {e}")))
}

async fn resize_and_check(
    session: &rmux_sdk::Session,
    driver: &mut PaneDriver,
    cols: u16,
    rows: u16,
) -> bool {
    // A one-pane window's grid changes through the window, not the pane
    // (rmux redistributes resize-pane only between siblings): the gate
    // proves the grid itself moved, not merely that content still draws
    // (the widget clips, so that alone would pass vacuously).
    let resized = session.window(0).resize(Some(cols), Some(rows)).await;
    if resized.is_err() {
        return false;
    }
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let _ = driver.refresh().await;
        let snapshot = &driver.state().snapshot;
        let grid_moved = snapshot.cols == cols && snapshot.rows == rows;
        if grid_moved {
            let mut smaller = match Terminal::new(TestBackend::new(cols, rows)) {
                Ok(terminal) => terminal,
                Err(_) => return false,
            };
            let drew = smaller.draw(|frame| {
                let widget = PaneWidget::new(driver.state()).base_style(pane_style());
                frame.render_widget(widget, frame.area());
            });
            return drew.is_ok() && !buffer_text(&smaller).trim().is_empty();
        }
        if Instant::now() > deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

fn pane_style() -> Style {
    Applier::fallback().style(Role::Text, Role::Background)
}

async fn report(cleanup: bool, rmux: Rmux, results: [bool; 4]) -> Result<(), SelftestError> {
    if cleanup {
        let _ = rmuxhost::teardown(rmux).await;
        println!("self-test: cleanup done (session killed, daemon shut down)");
    }
    match results {
        [true, true, true, true] => {
            println!(
                "self-test: PASS - render (child banner), input (typed keys echoed), \
                 resize (pane reflow) and windows (focused input lands in the focused \
                 pane) verified through rmux pane into ratatui buffer"
            );
            Ok(())
        }
        [false, _, _, _] => Err(SelftestError::Fail("child banner never rendered".into())),
        [_, false, _, _] => Err(SelftestError::Fail(
            "typed keys never reached the pane".into(),
        )),
        [_, _, false, _] => Err(SelftestError::Fail(
            "pane did not reflow after resize".into(),
        )),
        [_, _, _, false] => Err(SelftestError::Fail(
            "window scenario failed (open/focus/input/close through the reconciler)".into(),
        )),
    }
}
