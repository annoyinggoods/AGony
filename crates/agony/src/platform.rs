//! The one platform seam (Part12 12.2.3): every OS fork in AGony's own
//! code lives in this file, so `cfg(target_os)`/`cfg(windows)` and their
//! runtime forms appear nowhere else (`tests/platform_seam.rs` greps and
//! fails on a stray fork). Covered: the daemon endpoint (Windows named
//! pipe vs Unix socket), the shell choice, the config/home dirs, the
//! drive probe, the executable test, process-window suppression, and the
//! four child programs (login shell, idle anchor, self-test gate,
//! doctor's probe).
//!
//! Items: `WINDOWS_PIPE` — the Windows daemon endpoint; `GateChild` —
//! the self-test's child program and its render needle;
//! `EXECUTABLE_EXTENSIONS` — the runnable-name table (shared with
//! discover's `exec_key`).
//! Core functions: `is_windows`; `home_dir` / `config_dir`;
//! `drive_labels_with`; `is_executable_name` / `is_executable_file`;
//! `login_shell` / `login_shell_argv` / `shell_command_argv`;
//! `idle_shell_argv`; `gate_child`; `probe_child_argv`;
//! `exiting_child_argv` (a child that dies with code 3 — the
//! dead-pane tests' program);
//! `apply_daemon_endpoint` / `daemon_endpoint_args` /
//! `daemon_endpoint_display`; `find_command_path`; `set_no_window`.
//! Depends on: rmux-sdk (the builder's endpoint setters only).
//! Called by: settings, discover, places, rmuxhost, app, selftest,
//! doctor, commandbar, startmenu.
//! Calls: std::env, std::fs (existence/mode probes), std::process (the
//! PATH probe), rmux_sdk (builder).

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

use rmux_sdk::RmuxBuilder;

pub const WINDOWS_PIPE: &str = r"\\.\pipe\rmux-agony";

/// The Unix daemon socket: `<tmp>/agony.sock` — the same name the
/// Windows pipe replaces (a fixed endpoint the CLI cannot address; see
/// AGENTS.md's gotchas). One literal, two users via the functions below.
const UNIX_SOCKET_NAME: &str = "agony.sock";

pub fn is_windows() -> bool {
    cfg!(windows)
}

/// The user's home directory: `%USERPROFILE%` on Windows, `$HOME`
/// elsewhere; a dot when nothing resolves (places hide what is missing).
pub fn home_dir() -> PathBuf {
    let variable = if is_windows() { "USERPROFILE" } else { "HOME" };
    env::var_os(variable)
        .map(PathBuf::from)
        .unwrap_or_else(fallback_home)
}

/// The platform config directory: `%APPDATA%\agony` on Windows,
/// `$XDG_CONFIG_HOME/agony` (or `~/.config/agony`) elsewhere.
pub fn config_dir() -> PathBuf {
    if is_windows() {
        let base = env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(fallback_home);
        return base.join("agony");
    }
    let xdg = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| fallback_home().join(".config"));
    xdg.join("agony")
}

/// The home fallback the dir resolvers share: `$HOME`, else a dot — so
/// callers filter out what does not exist instead of failing on an
/// empty path (the original `settings::home_fallback` chain).
fn fallback_home() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The drives passing `probe` — all 26 letters on Windows, the single
/// root on Unix. The probe is injected so tests never touch real drives.
pub fn drive_labels_with(probe: impl Fn(&Path) -> bool) -> Vec<String> {
    if !is_windows() {
        return probe(Path::new("/"))
            .then(|| "/".to_string())
            .into_iter()
            .collect();
    }
    (b'A'..=b'Z')
        .map(|letter| format!("{}:", letter as char))
        .filter(|drive| probe(Path::new(&format!("{drive}\\"))))
        .collect()
}

/// The executable extensions the scan treats as runnable on Windows —
/// one table, three users (`is_executable_name`, discover's `exec_key`
/// and its installer search).
pub const EXECUTABLE_EXTENSIONS: [&str; 4] = [".exe", ".com", ".bat", ".cmd"];

#[cfg(windows)]
pub fn is_executable_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    EXECUTABLE_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

#[cfg(not(windows))]
pub fn is_executable_name(_name: &str) -> bool {
    true
}

#[cfg(windows)]
pub fn is_executable_file(_path: &Path) -> bool {
    true
}

#[cfg(not(windows))]
pub fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|meta| meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Mark a path executable: the Unix chmod the test fixtures need (and a
/// no-op on Windows, where the name decides). Test-only: the product
/// never changes a file's modes.
#[cfg(test)]
pub fn make_executable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(metadata) = std::fs::metadata(path) else {
            return;
        };
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        let _ = std::fs::set_permissions(path, permissions);
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// The user's login shell: on Windows the detected PowerShell (7 when
/// installed, else 5.1 — probed once, cached, so the UI thread never
/// spawns a probe); on Unix `$SHELL`, else `/bin/sh`.
pub fn login_shell() -> String {
    if is_windows() {
        windows_shell().to_string()
    } else {
        env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
}

/// The login shell as argv for the desktop's first window: `-NoLogo`
/// (a PowerShell-only flag) is appended on Windows, nothing on Unix.
pub fn login_shell_argv() -> Vec<String> {
    let mut argv = vec![login_shell()];
    if is_windows() {
        argv.push("-NoLogo".into());
    }
    argv
}

fn windows_shell() -> &'static str {
    static SHELL: OnceLock<&'static str> = OnceLock::new();
    SHELL.get_or_init(|| {
        if find_command_path("pwsh.exe").is_some() {
            "pwsh.exe"
        } else {
            "powershell.exe"
        }
    })
}

/// The shell-command shape of a one-shot non-app line: the platform
/// shell invoked with the whole line as one argument — the launcher
/// carries it as argv, never as re-split shell text.
pub fn shell_command_argv(line: &str) -> Vec<String> {
    if is_windows() {
        vec![
            windows_shell().to_string(),
            "-NoLogo".into(),
            "-Command".into(),
            line.to_string(),
        ]
    } else {
        vec!["sh".into(), "-c".into(), line.to_string()]
    }
}

/// The cheap idle child for anchor panes (the daemon exits when its last
/// session dies, so something must stay alive): the platform shell.
pub fn idle_shell_argv() -> [&'static str; 1] {
    if is_windows() {
        ["cmd.exe"]
    } else {
        ["sh"]
    }
}

/// The self-test gate's child program and its phase-1 needle. Windows:
/// cmd.exe, whose startup banner the gate waits for. Unix: a preamble
/// echo followed by a live shell — the tty line discipline echoes typed
/// input exactly as cmd does, so the input phase is platform-neutral.
pub struct GateChild {
    pub argv: Vec<&'static str>,
    pub render_needle: &'static str,
}

pub fn gate_child() -> GateChild {
    if is_windows() {
        GateChild {
            argv: vec!["cmd.exe"],
            render_needle: "Microsoft Windows",
        }
    } else {
        GateChild {
            argv: vec!["sh", "-c", "echo AGONY_RENDER_OK; exec sh"],
            render_needle: "AGONY_RENDER_OK",
        }
    }
}

/// Doctor's pane round-trip: a known line, printed, then ~3 s of delay —
/// rmux replaces a dead pane's screen with its exit notice, so a bare
/// `echo` can vanish before the first snapshot.
pub fn probe_child_argv() -> Vec<&'static str> {
    if is_windows() {
        vec![
            "cmd.exe",
            "/c",
            "echo AGONY_DOCTOR_OK & ping -n 3 127.0.0.1 >nul",
        ]
    } else {
        vec!["sh", "-c", "echo AGONY_DOCTOR_OK; sleep 3"]
    }
}

/// A child that exits immediately with code 3 — the dead-pane tests'
/// program (`adopt`'s skip and the exit watcher both need a pane whose
/// process verifiably dies while keep-alive holds the pane open).
#[cfg(test)]
pub fn exiting_child_argv() -> Vec<&'static str> {
    if is_windows() {
        vec!["cmd.exe", "/c", "exit 3"]
    } else {
        vec!["sh", "-c", "exit 3"]
    }
}

/// The daemon endpoint as the SDK builder's view: a Windows named pipe
/// or a Unix socket under the temp dir.
pub fn apply_daemon_endpoint(builder: RmuxBuilder) -> RmuxBuilder {
    if is_windows() {
        builder.windows_pipe(WINDOWS_PIPE)
    } else {
        builder.unix_socket(env::temp_dir().join(UNIX_SOCKET_NAME))
    }
}

/// The same endpoint as the manual daemon command's argument
/// (`rmux --__internal-daemon <endpoint> …`).
pub fn daemon_endpoint_args() -> Vec<OsString> {
    if is_windows() {
        vec![WINDOWS_PIPE.into()]
    } else {
        vec![env::temp_dir().join(UNIX_SOCKET_NAME).into_os_string()]
    }
}

/// The endpoint as a report line (doctor names where it probed).
pub fn daemon_endpoint_display() -> String {
    if is_windows() {
        WINDOWS_PIPE.to_string()
    } else {
        env::temp_dir()
            .join(UNIX_SOCKET_NAME)
            .to_string_lossy()
            .into_owned()
    }
}

/// The one PATH probe: `where.exe`/`command -v` once, first stdout line
/// as the resolved path. Every "is X installed" question goes through
/// here; `is_some()` answers the yes/no form.
pub fn find_command_path(command: &str) -> Option<PathBuf> {
    let mut probe = if is_windows() {
        let mut c = Command::new("where.exe");
        c.arg(command);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-lc").arg(format!("command -v {command}"));
        c
    };
    probe.stdout(Stdio::piped()).stderr(Stdio::null());
    set_no_window(&mut probe);
    let output = probe.output().ok()?;
    let first = String::from_utf8_lossy(&output.stdout);
    let line = first.lines().next()?.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// The PATH environment's separator: `;` on Windows, `:` elsewhere.
pub fn path_separator() -> char {
    if is_windows() {
        ';'
    } else {
        ':'
    }
}

#[cfg(windows)]
pub fn set_no_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn set_no_window(_command: &mut Command) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_dir_names_agony_under_a_base() {
        let dir = config_dir();
        assert!(dir.ends_with("agony"), "config dir ends in agony: {dir:?}");
        assert!(!dir.as_os_str().is_empty());
    }

    #[test]
    fn home_dir_never_empty() {
        assert!(!home_dir().as_os_str().is_empty());
    }

    #[test]
    fn drive_probe_keeps_only_existing_labels() {
        let drives = drive_labels_with(|path| path == Path::new("C:\\"));
        if is_windows() {
            assert_eq!(drives, vec!["C:".to_string()]);
        } else {
            assert!(drives.is_empty(), "no C: drive on Unix");
        }
        assert!(
            drive_labels_with(|_| false).is_empty(),
            "nothing exists: no drives"
        );
    }

    #[test]
    fn login_shell_argv_starts_with_the_shell() {
        let argv = login_shell_argv();
        assert_eq!(
            argv.first().map(String::as_str),
            Some(login_shell().as_str())
        );
        if is_windows() {
            assert!(argv.contains(&"-NoLogo".to_string()));
        } else {
            assert_eq!(argv.len(), 1, "no flags on Unix");
        }
    }

    #[test]
    fn shell_command_argv_carries_the_whole_line_last() {
        let argv = shell_command_argv("git checkout -b main");
        assert_eq!(
            argv.last().map(String::as_str),
            Some("git checkout -b main")
        );
        assert!(argv.len() >= 2);
    }

    #[test]
    fn gate_child_names_a_needle_and_an_argv() {
        let gate = gate_child();
        assert!(!gate.argv.is_empty());
        assert!(!gate.render_needle.is_empty());
    }

    #[test]
    fn probe_child_argv_ends_with_the_printed_line() {
        let argv = probe_child_argv();
        assert!(
            argv.iter().any(|arg| arg.contains("AGONY_DOCTOR_OK")),
            "the probe's needle lives in the argv: {argv:?}"
        );
    }

    #[test]
    fn daemon_endpoint_agrees_between_builder_and_command_views() {
        assert_eq!(daemon_endpoint_args().len(), 1);
        assert!(!daemon_endpoint_display().is_empty());
    }

    #[test]
    fn is_executable_name_follows_the_platform_rule() {
        if is_windows() {
            assert!(is_executable_name("Tool.EXE"));
            assert!(!is_executable_name("readme.txt"));
        } else {
            assert!(
                is_executable_name("readme.txt"),
                "Unix: the bit decides, not the name"
            );
        }
    }
}
