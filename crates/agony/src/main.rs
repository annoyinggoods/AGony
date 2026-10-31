//! The agony binary's entry: runtime setup and verb dispatch (clap).
//!
//! Verbs: `agony` — the desktop (app::run); `agony selftest [--cleanup]`
//! — the end-to-end gate (the spike's `--self-test` and `cleanup` modes
//! folded in); `agony doctor` — the environment report (the spike's
//! `check` mode folded in); `agony apps add|list|bundle|install` — the
//! app-catalog verbs (Part03, Part10's bundle, Part11's installer);
//! `agony history [--clear]` — the run history (Part07 7.4.3);
//! `agony thanks` — regenerate THANK-YOU.md onto the desktop (Part08).
//! Depends on: every shell module.
//! Called by: the user.
//! Calls: app / selftest / doctor / apps / installer / commandbar / thanks.

mod app;
mod apps;
mod catalog;
mod commandbar;
mod complete;
mod contextmenu;
mod desktop;
mod desktop_icons;
mod directory;
mod discover;
mod doctor;
mod frame;
mod fsprobe;
mod input;
mod installer;
mod panel;
mod pastemenu;
mod picker;
mod places;
mod platform;
mod reconcile;
mod rmuxhost;
mod selftest;
mod settings;
mod settings_ui;
mod startmenu;
mod taskbar;
mod thanks;
mod theme;
mod welcome;
mod wm;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "agony",
    version,
    about = "AGony — a desktop environment that lives in a terminal"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Verb>,
}

#[derive(Subcommand)]
enum Verb {
    /// Run the end-to-end gate: spawn a child through rmux, render, type, resize.
    Selftest {
        /// Kill the session and shut the daemon down after the test.
        #[arg(long)]
        cleanup: bool,
    },
    /// Diagnose the environment. Reports; never repairs.
    Doctor,
    /// Inspect and extend the app catalog.
    Apps {
        #[command(subcommand)]
        command: AppsCommand,
    },
    /// Print the command bar's run history (newest last); `--clear` empties it.
    History {
        /// Empty the history file.
        #[arg(long)]
        clear: bool,
    },
    /// Regenerate THANK-YOU.md onto the AGony desktop (idempotent).
    Thanks,
}

#[derive(Subcommand)]
enum AppsCommand {
    /// Write a user manifest from flags; it appears in the menu after restart.
    Add {
        /// Stable lowercase id (also the manifest's file name).
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: String,
        /// The executable, resolved through PATH at launch.
        #[arg(long)]
        exec: String,
        #[arg(long, default_value = "")]
        description: String,
        /// files | shell | text | dev | game | net | system
        #[arg(long, default_value = "system")]
        category: String,
        /// An Icon variant name (see `agony-core::icon`).
        #[arg(long, default_value = "app")]
        icon: String,
        /// A fixed argument; repeat for several (--arg x --arg y).
        #[arg(long = "arg")]
        args: Vec<String>,
        /// The app is a one-shot command, not a full-screen TUI.
        #[arg(long)]
        one_shot: bool,
    },
    /// Print the effective catalog with install state.
    List,
    /// The bundled set (Part10): list it, or `--install <id>` one of it.
    Bundle {
        /// Install one bundled app from its hash-checked release asset.
        #[arg(long)]
        install: Option<String>,
    },
    /// Install any catalog app through its recipes (Part11):
    /// winget, then scoop, then cargo/go, then the release download.
    Install {
        /// The app's catalog id.
        id: String,
    },
}

fn main() {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("agony: cannot start the async runtime: {error}");
            std::process::exit(2);
        }
    };
    let code = runtime.block_on(dispatch(Cli::parse()));
    std::process::exit(code);
}

async fn dispatch(cli: Cli) -> i32 {
    rmuxhost::ensure_daemon_env();
    let result: Result<(), Box<dyn std::error::Error + Send + Sync>> = match cli.command {
        None => app::run().await.map_err(Into::into),
        Some(Verb::Selftest { cleanup }) => selftest::run(cleanup).await.map_err(Into::into),
        Some(Verb::Doctor) => doctor::run().await.map_err(Into::into),
        Some(Verb::Apps {
            command:
                AppsCommand::Add {
                    id,
                    name,
                    exec,
                    description,
                    category,
                    icon,
                    args,
                    one_shot,
                },
        }) => {
            let spec = apps::AddSpec {
                id,
                name,
                exec,
                description,
                category,
                icon,
                args,
                one_shot,
            };
            apps::add(spec)
                .map(|path| {
                    println!(
                        "wrote {} — it appears in the menu after restart",
                        path.display()
                    )
                })
                .map_err(Into::into)
        }
        Some(Verb::Apps {
            command: AppsCommand::List,
        }) => apps::list().await.map_err(Into::into),
        Some(Verb::Apps {
            command: AppsCommand::Bundle { install },
        }) => match install {
            Some(id) => apps::bundle_install(&id).await.map_err(Into::into),
            None => apps::bundle_list().await.map_err(Into::into),
        },
        Some(Verb::Apps {
            command: AppsCommand::Install { id },
        }) => apps::install(&id).await.map_err(Into::into),
        Some(Verb::History { clear }) => history(clear).map_err(Into::into),
        Some(Verb::Thanks) => thanks_verb().map_err(Into::into),
    };
    report(result)
}

/// `agony history` (Part07 7.4.3): print the recorded lines (the cwd and
/// a `✗` marker for recorded failures), or empty the file with `--clear`.
fn history(clear: bool) -> Result<(), std::io::Error> {
    let dir = settings::config_dir();
    let store = commandbar::HistoryStore::load(&dir);
    if clear {
        store.replace(agony_core::history::History::default());
        commandbar::save(&dir, &store.snapshot())?;
        println!(
            "history cleared ({})",
            commandbar::history_path(&dir).display()
        );
        return Ok(());
    }
    let history = store.snapshot();
    if history.entries.is_empty() {
        println!("no history yet — run lines in the command bar (Ctrl-K)");
        return Ok(());
    }
    for entry in &history.entries {
        let marker = if entry.status.is_some() { "✗ " } else { "  " };
        println!("{marker}{}  [{}]", entry.line, entry.cwd);
    }
    Ok(())
}

/// `agony thanks` (Part08 8.4.1): regenerate THANK-YOU.md onto the AGony
/// desktop and print where it went — idempotent by design.
fn thanks_verb() -> Result<(), std::io::Error> {
    let path = thanks::regenerate()?;
    println!("THANK-YOU.md written to {}", path.display());
    Ok(())
}

fn report(result: Result<(), Box<dyn std::error::Error + Send + Sync>>) -> i32 {
    let Some(error) = result.err() else {
        return 0;
    };
    eprintln!("agony: {error}");
    let mut source = error.source();
    while let Some(cause) = source {
        eprintln!("  caused by: {cause}");
        source = cause.source();
    }
    1
}
