//! `agony doctor`: five checks, one line each, OK or the problem and the
//! command that would fix it — plus the bundled set's one informational
//! line (Part10 §10.3.4). Never repairs anything — a doctor that
//! fixes what it finds is a doctor whose findings cannot be trusted.
//!
//! Items: `DoctorError` — the findings count as an error.
//! Checks, in order: `rmux` on PATH and its version; the daemon reachable
//! on the configured endpoint (with the hosted-box remedy when it is
//! not); a pane spawn round-trip through the daemon (`platform::
//! probe_child_argv` — an echoed line, read back); carapace found
//! (env `AGONY_CARAPACE`, the repo's `bin/`, or PATH) and its version; a
//! Nerd Font test pattern for the human to eyeball; the bundled apps'
//! missing list (a note, never a failure).
//! Depends on: rmuxhost (probe/launch/PATH probe), platform (the probe
//! child + the endpoint display), discover (the carapace path),
//! selftest (`wait_for_text`), the icon table (the pattern).
//! Called by: `agony doctor` (main).
//! Calls: rmux-sdk (probe only — never a start), std::process for
//! version queries, std::fs for existence checks.

use std::path::{Path, PathBuf};
use std::process::Command;

use agony_core::config::FontMode;
use agony_core::icon::{self as icon_table, Icon};
use ratatui::{backend::TestBackend, Terminal};
use ratatui_rmux::PaneDriver;
use thiserror::Error;

use crate::rmuxhost;
use crate::selftest::wait_for_text;

#[derive(Debug, Error)]
pub enum DoctorError {
    #[error("{0} doctor check(s) failed — see the report above")]
    Findings(usize),
}

pub async fn run() -> Result<(), DoctorError> {
    let mut failed = 0usize;
    failed += check_rmux();
    failed += check_daemon().await;
    failed += check_carapace();
    check_font();
    check_bundled();
    if failed > 0 {
        return Err(DoctorError::Findings(failed));
    }
    Ok(())
}

fn check_rmux() -> usize {
    let Some(found) = find_command("rmux") else {
        report_fail("rmux is not on PATH", "install rmux (cargo install rmux)");
        return 1;
    };
    report_ok(&format!("rmux on PATH at {}", found.display()));
    report_ok(&format!(
        "rmux version: {}",
        command_output(&found, &["-V"])
    ));
    0
}

async fn check_daemon() -> usize {
    let endpoint = crate::platform::daemon_endpoint_display();
    match rmuxhost::probe_connect().await {
        Ok(rmux) => {
            report_ok(&format!("daemon reachable on {endpoint}"));
            let roundtrip = check_pane_roundtrip(&rmux).await;
            drop(rmux);
            roundtrip
        }
        Err(error) => {
            report_fail(
                &format!("daemon not reachable on {endpoint} ({error})"),
                "run `agony` once (it self-heals and starts the daemon) or run it manually: \
                 rmux --__internal-daemon <endpoint> --config-default --config-quiet",
            );
            1
        }
    }
}

async fn check_pane_roundtrip(rmux: &rmux_sdk::Rmux) -> usize {
    // The child must outlive the spawn settle time: rmux replaces a dead
    // pane's screen with its exit notice, so a bare `echo` can vanish
    // before the first snapshot (the seam's argv delays ~3 s on every
    // platform).
    let argv = crate::platform::probe_child_argv();
    let spawned = rmuxhost::fresh_named_session(rmux, "agony-doctor", 80, 10, &argv, None).await;
    let (session, pane) = match spawned {
        Ok(pair) => pair,
        Err(error) => {
            report_fail(
                &format!("pane round-trip failed: {error}"),
                "check the daemon log and `agony selftest`",
            );
            return 1;
        }
    };
    let mut driver = PaneDriver::new(pane);
    let Ok(mut terminal) = Terminal::new(TestBackend::new(80, 10)) else {
        report_fail(
            "pane round-trip failed: no test terminal",
            "this is an agony bug",
        );
        return 1;
    };
    let echoed = wait_for_text(&mut terminal, &mut driver, "AGONY_DOCTOR_OK", 10).await;
    if !echoed {
        let peek: String = crate::selftest::buffer_text(&terminal)
            .chars()
            .filter(|c| !c.is_whitespace())
            .take(80)
            .collect();
        println!("        pane content seen: {peek:?}");
    }
    let _ = session.kill().await;
    let detail = if echoed {
        "pane round-trip: the spawned program's output read back through the daemon"
    } else {
        "pane round-trip: spawn worked but the output never came back"
    };
    report_condition(echoed, detail, "re-run `agony selftest` for detail");
    usize::from(!echoed)
}

fn check_carapace() -> usize {
    let Some(path) = find_carapace() else {
        report_fail(
            "carapace not found (env AGONY_CARAPACE, <repo>/bin/carapace.exe, PATH)",
            "run: powershell -ExecutionPolicy Bypass -File scripts/get-carapace.ps1 \
             (downloads the release zip, hash-checked, unpacks into bin/) — or set AGONY_CARAPACE",
        );
        return 1;
    };
    report_ok(&format!(
        "carapace at {} (version {})",
        path.display(),
        command_output(&path, &["--version"])
    ));
    0
}

fn find_carapace() -> Option<PathBuf> {
    crate::discover::carapace_path()
}

/// A human-judged pattern, not a pass/fail: terminals do not report their
/// font, so the runtime `auto` mode cannot decide — the pattern decides
/// whether to pin `nerd`.
fn check_font() {
    let powerline = "\u{e0b0}\u{e0b1}";
    let glyphs: Vec<String> = [Icon::Start, Icon::Search, Icon::Folder, Icon::App]
        .iter()
        .map(|icon| icon_table::glyph(*icon, FontMode::Nerd).to_string())
        .collect();
    println!(
        "SEE     font pattern: {powerline} {} — boxes mean no Nerd Font:",
        glyphs.join(" ")
    );
    println!(
        "        then keep font_mode = \"ascii\", else set font_mode = \"nerd\" in settings.toml"
    );
}

/// The bundled set's state (10.3.4): one line, informational — a
/// bundled-but-missing app is a greyed menu entry, never a failure.
fn check_bundled() {
    let (manifests, _) = crate::discover::builtins();
    let path_env = std::env::var("PATH").unwrap_or_default();
    let mut map = crate::discover::scan_path_dirs(&path_env);
    for (key, path) in
        crate::discover::scan_path_dirs(&crate::settings::bundle_bin_dir().to_string_lossy())
    {
        map.entry(key).or_insert(path);
    }
    let missing: Vec<&str> = manifests
        .iter()
        .filter(|m| m.bundled)
        .filter(|m| crate::discover::lookup_path(&map, &m.exec).is_none())
        .map(|m| m.id.as_str())
        .collect();
    if missing.is_empty() {
        report_ok("bundled apps: all installed");
        return;
    }
    println!(
        "NOTE    bundled apps not installed: {} — they stay greyed; \
         `agony apps bundle --install <id>` installs one",
        missing.join(", ")
    );
}

fn find_command(name: &str) -> Option<PathBuf> {
    rmuxhost::find_command_path(name)
}

fn command_output(path: &Path, args: &[&str]) -> String {
    Command::new(path)
        .args(args)
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn report_ok(detail: &str) {
    println!("OK      {detail}");
}

fn report_fail(problem: &str, fix: &str) {
    println!("FAIL    {problem}");
    println!("        fix: {fix}");
}

fn report_condition(passed: bool, ok_text: &str, fix: &str) {
    if passed {
        report_ok(ok_text);
    } else {
        report_fail(ok_text, fix);
    }
}
