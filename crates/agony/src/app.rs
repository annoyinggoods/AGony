//! The application loop: settings → theme → the rmux anchor → the
//! desktop's windows → the draw and input cycle (Part04 rebuilt the loop
//! around the window manager; Part06 rebuilt the mouse half around the
//! core's gesture machine; Part07 added the modal command bar; Part08
//! added the settings window and the live reload; Part09 added the
//! desktop's icon grid and the first context menus).
//! The loop owns only orchestration — every drawn thing is a widget call
//! into a module that owns it.
//!
//! Items: `AppError` — the loop's failures; `App` — the whole desktop
//! (wm state, panes, panel, menu, bar, history, discovery, drag, the
//! gesture machine, the pointer's cell, the settings window, the
//! context menu and the icon grid); `Drag` — an active titlebar move or
//! edge resize; `TaskbarHit` / `WheelTarget` / `ContextTarget` — the
//! pure routing tables the pointer dispatches through.
//! Core functions: `run` — the startup chain; `drive_events` — refresh,
//! bar tick, settings watch, wheel drain, draw, then one batched event
//! drain per draw tick (a pointer flood folds before the next draw);
//! `handle_key` — capture, quit, the modal bar, the context menu, then
//! reserved keys, menu, settings, panel, icons, focused pane;
//! `activate_settings` — the one apply path Apply and the live reload
//! share; `handle_mouse` — translate → fold into `Gestures` → dispatch
//! one `Gesture`; `handle_wheel` / `handle_double_click` /
//! `context_target` — the routed tables; `execute_line` — the bar's one
//! launch path; `cell_at` — the one mouse-position-to-cell translation.
//! Depends on: every shell module; chrono for the clock string (time is
//! injected into the taskbar; the gesture machine gets `now_ms` from
//! the loop's epoch).
//! Called by: main (the plain `agony` run).
//! Calls: rmuxhost/reconcile (panes), discover, input, commandbar,
//! complete, the draw modules.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use agony_core::config::{FilesSettings, FontMode, Settings, TaskbarPosition};
use agony_core::history::Entry;
use agony_core::inputmap;
use agony_core::layout::{self, hit_test, Edge, FrameRegions, Hit, Rect, TitleButton, WmState};
use agony_core::pathpick::Picked;
use agony_core::pointer::{self, Gesture, PointerButton, PointerEvent, PointerKind};
use agony_core::thememodel::{ColorValue, Role};
use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{
        self, disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};
use rmux_sdk::Rmux;
use thiserror::Error;

use crate::commandbar::{self, BarEvent};
use crate::complete;
use crate::contextmenu::{self, Action as CtxAction};
use crate::desktop;
use crate::desktop_icons;
use crate::directory::{self, Outcome};
use crate::discover;
use crate::frame;
use crate::input;
use crate::installer;
use crate::panel;
use crate::places;
use crate::reconcile::{self, LaunchError};
use crate::rmuxhost::{self, RmuxHostError};
use crate::settings::{self, Combos};
use crate::settings_ui;
use crate::startmenu::{self, MenuEvent};
use crate::taskbar::{self, TaskWindow};
use crate::thanks;
use crate::theme::{self, Applier};
use crate::welcome;
use crate::wm;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Host(#[from] RmuxHostError),
    #[error("terminal error: {0}")]
    Terminal(#[from] io::Error),
}

/// An in-progress titlebar drag: the core's state moves freely under the
/// pointer; the pane resize is issued once, at drag end (coalescing).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Drag {
    Move {
        window: u64,
        last: (u16, u16),
    },
    Resize {
        window: u64,
        edge: Edge,
        last: (u16, u16),
    },
}

/// The panel refresh idle threshold (Part05 5.2.4).
const PANEL_REFRESH_IDLE: Duration = Duration::from_secs(5);
/// Pointer events handled per draw tick at most — a flood folds into
/// the gesture accumulator instead of starving the draw (Part06).
const EVENT_BATCH: usize = 64;
/// How long a status line keeps the taskbar's middle zone before the
/// window-keys hint returns (the UI review's pinned-error finding: a
/// launch-time diagnosis must not squat on the bar forever).
const STATUS_TTL: Duration = Duration::from_secs(10);

struct App {
    theme: Applier,
    /// The last applied theme — what Cancel/Close restores (Part08's
    /// preview-until-commit rule).
    committed_theme: Applier,
    font: FontMode,
    combos: Combos,
    /// The drop key's combo (Part07: bar-local, not a reserved key).
    paste_combo: inputmap::Combo,
    rmux: Rmux,
    wm: WmState,
    panes: reconcile::Panes,
    panel: Option<panel::State>,
    panel_window: Option<u64>,
    picked: Option<Picked>,
    menu: Option<startmenu::State>,
    bar: Option<commandbar::State>,
    /// The settings window (Part08): its wm id and its draft state.
    settings_win: Option<u64>,
    settings_state: Option<settings_ui::State>,
    /// The welcome document (Part10 §10.2): its wm id and its content.
    welcome_win: Option<u64>,
    welcome_content: welcome::Content,
    /// The app directory window (Part11 §11.3): its wm id and its state.
    directory_win: Option<u64>,
    directory: Option<directory::State>,
    /// The running install's report channel (Part11 §11.3.2) and its
    /// busy flag — one install at a time.
    install_rx: Option<tokio::sync::mpsc::UnboundedReceiver<Outcome>>,
    install_busy: bool,
    /// The live settings as last applied — the draft's seed (Part08).
    settings_live: Settings,
    /// The settings file's stamp at the last read/apply — the watcher's
    /// ignore-my-own-writes guard (Part08 8.2.6).
    settings_stamp: Option<settings::FileStamp>,
    settings_watch_at: Instant,
    /// The open context menu (Part09 §9.3), if any — the menu plus what
    /// it anchors to (the widget itself stays app-agnostic).
    context: Option<ContextState>,
    /// The desktop's icon grid (Part09 §9.4).
    icons: desktop_icons::State,
    history: commandbar::HistoryStore,
    history_dirty_at: Option<Instant>,
    complete: complete::Shared,
    suggestions_enabled: bool,
    status: Option<String>,
    /// The status as last drawn — the change detector behind the TTL
    /// (a status ages out; the hint returns — the review's pinned-error
    /// finding), so no writer needs to remember a timestamp.
    shown_status: Option<String>,
    status_at: Instant,
    layout: FrameRegions,
    taskbar_pos: TaskbarPosition,
    entries: Vec<startmenu::Entry>,
    discovery: discover::Shared,
    discovery_reported: bool,
    drag: Option<Drag>,
    /// The gesture machine (Part06): events in, one gesture per draw
    /// tick out, time injected from the loop's epoch.
    gestures: pointer::Gestures,
    pointer_cell: (u16, u16),
    hover: Option<(u16, u16)>,
    epoch: Instant,
    files: FilesSettings,
    panel_walked_at: Option<Instant>,
    panel_had_focus: bool,
}

pub async fn run() -> Result<(), AppError> {
    // The first-run question must come before `load` writes the defaults
    // file out (10.2.1): a fresh config is the welcome's trigger.
    let settings_existed = settings::settings_path().exists();
    let loaded = settings::load();
    let (font, font_note) = settings::resolve_font_mode(loaded.settings.font_mode);
    let (theme, mut notes) =
        Applier::load(&loaded.settings.theme, Some(&settings::custom_theme_dir()));
    let (combos, combo_notes) = settings::reserved_combos(&loaded.settings);
    notes.extend(loaded.notes);
    notes.extend(combo_notes);
    notes.extend(font_note);
    let suggestions_enabled = loaded.settings.suggestions_enabled;
    let (complete, complete_note) = complete::Shared::start(suggestions_enabled);
    if let Some(note) = complete_note {
        notes.push(note);
    }
    let history = commandbar::HistoryStore::load(&settings::config_dir());

    let discovery = discover::start_worker(settings::config_dir());
    let entries = menu_entries(&discovery);

    let (cols, rows) = terminal::size().unwrap_or((120, 40));
    let taskbar_pos = loaded.settings.taskbar_position;
    let rmux = rmuxhost::connect().await?;
    // The anchor keeps the daemon alive whatever the windows do (the
    // daemon exits when its last session dies): a cheap idle shell pane
    // in the app session, never rendered, reused across runs.
    let _anchor =
        rmuxhost::fresh_session(&rmux, 80, 24, &crate::platform::idle_shell_argv()).await?;

    let regions = frame::regions(Rect::new(0, 0, cols, rows), taskbar_pos);
    let mut wm = WmState::new(regions.desktop);
    let mut panes = reconcile::Panes::default();
    let adopted = panes.adopt(&rmux, rmuxhost::SESSION, &mut wm).await;
    if adopted == 0 {
        open_shell_window(&rmux, &mut wm, &mut panes).await?;
    }

    // THANK-YOU.md lands before the first grid scan, so the tile is
    // there from the very first paint (10.2.3).
    let thanks_note = thanks::write_if_missing();
    let mut icons = desktop_icons::State::default();
    icons.refresh();
    let mut app = App {
        committed_theme: theme.clone(),
        theme,
        font,
        combos,
        paste_combo: inputmap::combo_from_str(inputmap::BAR_PASTE)
            .unwrap_or_else(inputmap::default_quit_combo),
        rmux,
        wm,
        panes,
        panel: None,
        panel_window: None,
        picked: None,
        menu: None,
        bar: None,
        settings_win: None,
        settings_state: None,
        welcome_win: None,
        welcome_content: welcome::Content::default(),
        directory_win: None,
        directory: None,
        install_rx: None,
        install_busy: false,
        settings_live: loaded.settings.clone(),
        settings_stamp: settings::file_stamp(&settings::settings_path()),
        settings_watch_at: Instant::now(),
        context: None,
        icons,
        history,
        history_dirty_at: None,
        complete,
        suggestions_enabled,
        status: first_status(&notes),
        shown_status: None,
        status_at: Instant::now(),
        layout: regions,
        taskbar_pos,
        entries,
        discovery,
        discovery_reported: false,
        drag: None,
        gestures: pointer::Gestures::new(),
        pointer_cell: (0, 0),
        hover: None,
        epoch: Instant::now(),
        files: loaded.settings.files,
        panel_walked_at: None,
        panel_had_focus: false,
    };
    open_panel(&mut app);
    if let Some(note) = thanks_note {
        app.status = Some(note);
    }
    // The first run opens the welcome document (10.2.1) — after the
    // panel, so it starts focused on top; skipping it once (10.2.2)
    // writes the marker that keeps it away next time.
    if !settings_existed && !welcome::seen() {
        open_welcome(&mut app);
    }
    let outcome = run_loop(&mut app).await;
    save_history(&mut app);
    outcome
}

/// The first window of a fresh desktop: the detected shell.
/// The first window of a fresh desktop: the detected login shell (the
/// platform seam's argv).
async fn open_shell_window(
    rmux: &Rmux,
    wm: &mut WmState,
    panes: &mut reconcile::Panes,
) -> Result<(), AppError> {
    let shell_argv = crate::platform::login_shell_argv();
    let argv: Vec<&str> = shell_argv.iter().map(String::as_str).collect();
    let launched = panes
        .launch(rmux, rmuxhost::SESSION, wm, "shell", &argv, None)
        .await;
    match launched {
        Ok(_) => Ok(()),
        Err(LaunchError::Host(error)) => Err(error.into()),
        Err(LaunchError::Spawn(message)) => Err(AppError::Host(RmuxHostError::Spawn(message))),
    }
}

/// The docked file panel: an internal window (no pane) plus its first
/// walk, already running on the worker. The top-level dir starts
/// expanded so the panel never looks empty on a slow first walk.
fn open_panel(app: &mut App) {
    let rect = panel::docked_rect(app.wm.desktop);
    let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut state = panel::open(root, fsprobe_options(app));
    state.cursor.move_right(&state.roots_snapshot());
    let id = app.wm.open_at("files", None, rect);
    app.panel_window = Some(id);
    app.panel_walked_at = Some(Instant::now());
    app.wm.set_title(id, &panel::window_title(&state, None));
    app.panel = Some(state);
}

/// The loop: raw mode, alternate screen, a restore guard, then draw +
/// event batches until the reserved quit key. Quitting detaches — the
/// windows' sessions and the daemon stay alive for the next run (which
/// adopts them back); `agony selftest --cleanup` is the full teardown.
async fn run_loop(app: &mut App) -> Result<(), AppError> {
    enable_raw_mode()?;
    let _restore = TerminalRestore;
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        cursor::Hide
    )?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    drive_events(app, &mut terminal).await
}

async fn drive_events(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> Result<(), AppError> {
    loop {
        sync_layout(app).await;
        app.panes.refresh_all().await;
        refresh_panel_on_focus(app);
        bar_tick(app).await;
        watch_settings(app).await;
        drain_installs(app);
        // The wheel drains once per draw tick: a trackpad flood between
        // two draws is one scroll (Part06 6.2.3).
        if let Some(wheel) = app.gestures.drain_wheel() {
            dispatch_gesture(app, wheel).await;
        }
        let clock = clock_text();
        terminal.draw(|f| draw_frame(f, app, &clock))?;
        if drain_events(app).await? {
            return Ok(());
        }
        save_history_if_due(app);
    }
}

/// Poll the event queue without drawing between events (capped): a
/// pointer flood folds in the gesture machine instead of starving the
/// draw. The first poll keeps the loop's 50 ms cadence; the rest are
/// immediate.
async fn drain_events(app: &mut App) -> Result<bool, AppError> {
    let mut handled = 0;
    loop {
        let Some(event) = poll_event(handled == 0)? else {
            return Ok(false);
        };
        if handle_event(app, event).await? {
            return Ok(true);
        }
        handled += 1;
        if handled >= EVENT_BATCH {
            return Ok(false);
        }
    }
}

/// Keep the core's desktop (and every pane) aligned with the terminal's
/// size: the core clamps, the reconciler follows (Part04 4.4.4).
async fn sync_layout(app: &mut App) {
    let regions = frame::regions(
        Rect::new(0, 0, terminal_width(), terminal_height()),
        app.taskbar_pos,
    );
    if regions.desktop == app.layout.desktop {
        app.layout = regions;
        return;
    }
    app.layout = regions;
    app.wm.set_desktop(regions.desktop);
    let errors = app.panes.resize_all(&app.wm).await;
    app.status = first_error(errors).or(app.status.take());
}

/// Part05 5.2.4: when the panel regains focus after 5+ idle seconds,
/// refresh its walk — never on a timer while the user reads. The icon
/// grid shares the cadence (9.4.6): a panel focus regain refreshes it.
fn refresh_panel_on_focus(app: &mut App) {
    let focused = panel_is_focused(app);
    let regained = focused && !app.panel_had_focus;
    app.panel_had_focus = focused;
    if !regained {
        return;
    }
    app.icons.refresh();
    let stale = panel_is_stale(app);
    if stale {
        refresh_panel(app);
    }
}

fn panel_is_stale(app: &App) -> bool {
    app.panel_walked_at
        .is_none_or(|at| at.elapsed() >= PANEL_REFRESH_IDLE)
}

fn refresh_panel(app: &mut App) {
    let options = fsprobe_options(app);
    let refreshed = app.panel.as_mut().is_some_and(|state| {
        state.refresh(options);
        true
    });
    if refreshed {
        app.panel_walked_at = Some(Instant::now());
    }
}

fn fsprobe_options(app: &App) -> crate::fsprobe::Options {
    crate::fsprobe::Options::from_settings(&app.files)
}

async fn handle_event(app: &mut App, event: Event) -> Result<bool, AppError> {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(handle_key(app, key).await),
        Event::Resize(cols, rows) => {
            handle_resize(app, cols, rows).await;
            Ok(false)
        }
        Event::Mouse(mouse) => {
            handle_mouse(app, mouse).await;
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Input routing: a key capture first (the settings window is re-binding
/// a reserved key, so even the quit combo is a candidate binding — Esc
/// cancels the capture), then quit, then the modal command bar, the open
/// context menu, the bar's reserved open key, the drop key with a file
/// held, the open menu, the focused settings/panel window, the icon
/// grid when nothing is focused, then the reserved window keys and the
/// focused window's pane.
async fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    app.status = None;
    if settings_is_capturing(app) {
        let event = capture_key(app, key);
        apply_settings_event(app, event).await;
        return false;
    }
    if input::matches_combo(key, &app.combos.quit) {
        return true;
    }
    if app.bar.is_some() {
        let picked = app.picked.as_ref().map(|picked| picked.path.clone());
        let event = app
            .bar
            .as_mut()
            .map(|bar| commandbar::handle_key(bar, key, picked.as_deref()))
            .unwrap_or(BarEvent::Consumed);
        apply_bar_event(app, event).await;
        return false;
    }
    if context_key(app, key).await {
        return false;
    }
    if input::matches_combo(key, &app.combos.bar) {
        open_bar(app, None, false);
        return false;
    }
    if app.picked.is_some() && input::matches_combo(key, &app.paste_combo) {
        // The drop works whether the bar is open or not (7.5.3).
        open_bar(app, None, true);
        return false;
    }
    if input::matches_combo(key, &app.combos.start_menu) {
        app.toggle_menu();
        return false;
    }
    if input::matches_combo(key, &app.combos.cycle_focus) {
        app.wm.focus_next();
        return false;
    }
    if input::matches_combo(key, &app.combos.close_window) {
        close_focused_window(app).await;
        return false;
    }
    if input::matches_combo(key, &app.combos.minimize) {
        minimize_focused(app);
        return false;
    }
    if input::matches_combo(key, &app.combos.maximize) {
        maximize_focused(app).await;
        return false;
    }
    route_unreserved_key(app, key).await;
    false
}

/// The open context menu takes keys first: Up/Down/Enter/Esc only; any
/// other key closes it (a click elsewhere closes — keys should not
/// sneak past a modal). True when the key was consumed.
async fn context_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(state) = app.context.as_mut() else {
        return false;
    };
    match state.menu.handle_key(key) {
        contextmenu::Event::Consumed => true,
        contextmenu::Event::Close => {
            app.context = None;
            true
        }
        contextmenu::Event::Chosen(action) => {
            let Some(state) = app.context.take() else {
                return true;
            };
            run_context_action(app, state, action).await;
            true
        }
    }
}

fn settings_is_capturing(app: &App) -> bool {
    app.settings_state
        .as_ref()
        .is_some_and(|state| state.capture.is_some())
}

/// The captured key reaches the settings state directly (the capture
/// flag is already set; the state's own capture handler decides).
fn capture_key(app: &mut App, key: KeyEvent) -> settings_ui::Event {
    let Some(state) = app.settings_state.as_mut() else {
        return settings_ui::Event::Consumed;
    };
    let Some(capture) = state.capture else {
        return settings_ui::Event::Consumed;
    };
    settings_ui::capture_key(state, capture, key)
}

async fn route_unreserved_key(app: &mut App, key: KeyEvent) {
    if let Some(menu) = app.menu.as_mut() {
        let menu_event = startmenu::handle_key(menu, key);
        apply_menu_event(app, menu_event).await;
        return;
    }
    if welcome_is_focused(app) && key.code == KeyCode::Esc {
        close_welcome(app);
        return;
    }
    if directory_is_focused(app) {
        let event = app
            .directory
            .as_mut()
            .map(|state| directory::handle_key(state, key))
            .unwrap_or(directory::Event::Consumed);
        apply_directory_event(app, event).await;
        return;
    }
    if settings_is_focused(app) {
        let event = settings_key(app, key);
        apply_settings_event(app, event).await;
        return;
    }
    if panel_is_focused(app) {
        let event = panel_key(app, key);
        apply_panel_event(app, event).await;
        return;
    }
    if app.wm.focus.is_none() {
        match icons_key(app, key) {
            IconsKey::Ignored => {}
            IconsKey::Moved => return,
            IconsKey::Activate(index) => {
                activate_tile(app, index).await;
                return;
            }
        }
    }
    forward_to_focused(app, key).await;
}

fn settings_is_focused(app: &App) -> bool {
    app.settings_win.is_some_and(|id| app.wm.focus == Some(id))
}

fn settings_key(app: &mut App, key: KeyEvent) -> settings_ui::Event {
    let Some(state) = app.settings_state.as_mut() else {
        return settings_ui::Event::Consumed;
    };
    settings_ui::handle_key(state, key)
}

/// The icon grid's keyboard outcome.
enum IconsKey {
    Ignored,
    Moved,
    Activate(usize),
}

/// The icon grid's keyboard path (Part06's parity rule): arrows and
/// Enter, only when no window holds focus.
fn icons_key(app: &mut App, key: KeyEvent) -> IconsKey {
    let desktop = app.wm.desktop;
    let plain = key.modifiers.is_empty();
    match key.code {
        KeyCode::Up if plain => {
            desktop_icons::move_cursor(&mut app.icons, desktop, 0, -1);
            IconsKey::Moved
        }
        KeyCode::Down if plain => {
            desktop_icons::move_cursor(&mut app.icons, desktop, 0, 1);
            IconsKey::Moved
        }
        KeyCode::Left if plain => {
            desktop_icons::move_cursor(&mut app.icons, desktop, -1, 0);
            IconsKey::Moved
        }
        KeyCode::Right if plain => {
            desktop_icons::move_cursor(&mut app.icons, desktop, 1, 0);
            IconsKey::Moved
        }
        KeyCode::Enter if plain && !app.icons.entries.is_empty() => {
            IconsKey::Activate(app.icons.cursor.selected)
        }
        _ => IconsKey::Ignored,
    }
}

fn panel_is_focused(app: &App) -> bool {
    app.panel_window.is_some_and(|id| app.wm.focus == Some(id))
}

fn panel_key(app: &mut App, key: KeyEvent) -> panel::Event {
    let Some(state) = app.panel.as_mut() else {
        return panel::Event::Consumed;
    };
    let picked = app.picked.clone();
    let event = panel::handle_key(state, key, picked.as_ref());
    if event == panel::Event::OpenSearch {
        state.search = Some(panel::Search::default());
    }
    event
}

async fn apply_panel_event(app: &mut App, event: panel::Event) {
    match event {
        panel::Event::Consumed => {}
        panel::Event::OpenFile { path } => open_file(app, &path).await,
        panel::Event::Grab { path } => grab_file(app, path),
        panel::Event::OpenSearch => {
            if panel_is_stale(app) {
                refresh_panel(app);
            }
        }
        panel::Event::Refresh => refresh_panel(app),
        panel::Event::DropPick => drop_pick(app),
    }
}

/// `g` on a file: hold it (Part05 5.5) — one `Picked`, replacing any
/// previous one, shown in the panel's title.
fn grab_file(app: &mut App, path: PathBuf) {
    app.picked = Some(Picked {
        path: path.display().to_string(),
    });
    rename_panel_title(app);
}

fn drop_pick(app: &mut App) {
    app.picked = None;
    rename_panel_title(app);
}

fn rename_panel_title(app: &mut App) {
    let Some(id) = app.panel_window else {
        return;
    };
    let title = app
        .panel
        .as_ref()
        .map(|state| panel::window_title(state, app.picked.as_ref()))
        .unwrap_or_else(|| "files".into());
    app.wm.set_title(id, &title);
}

/// Enter on a file (Part05 5.3.2): the first installed manifest whose
/// `open_with` covers the extension launches the file as its argument,
/// in a new window whose cwd is the file's directory.
async fn open_file(app: &mut App, path: &std::path::Path) {
    let Some(entry) = opener_for(app, path) else {
        app.status = Some(no_opener_message(path));
        return;
    };
    let mut argv = rmuxhost::manifest_argv(&entry.manifest, entry.path.as_deref());
    argv.push(path.display().to_string());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let cwd = path.parent().map(PathBuf::from);
    let launched = app
        .panes
        .launch(
            &app.rmux,
            rmuxhost::SESSION,
            &mut app.wm,
            &entry.manifest.name,
            &argv,
            cwd.as_deref(),
        )
        .await;
    report_launch(app, launched, &entry.manifest.name);
}

fn opener_for(app: &App, path: &std::path::Path) -> Option<startmenu::Entry> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    app.entries
        .iter()
        .find(|entry| entry.path.is_some() && entry.manifest.opens_extension(&ext))
        .cloned()
}

fn no_opener_message(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_else(|| "none".into());
    format!("no installed app opens .{ext} files — set open_with in a manifest")
}

async fn close_focused_window(app: &mut App) {
    let Some(id) = app.wm.focus else {
        return;
    };
    close_window(app, id).await;
}

async fn close_window(app: &mut App, id: u64) {
    if app.panel_window == Some(id) {
        app.panel = None;
        app.panel_window = None;
        app.wm.close(id);
        return;
    }
    if app.welcome_win == Some(id) {
        close_welcome(app);
        return;
    }
    if app.directory_win == Some(id) {
        close_directory(app);
        return;
    }
    if app.settings_win == Some(id) {
        // Closing the settings window is Cancel: the preview reverts.
        close_settings(app);
        return;
    }
    app.panes.close_pane(id).await;
    app.wm.close(id);
    let errors = app.panes.resize_all(&app.wm).await;
    app.status = first_error(errors).or(app.status.take());
}

fn minimize_focused(app: &mut App) {
    let Some(id) = app.wm.focus else {
        return;
    };
    app.wm.minimize(id);
}

async fn maximize_focused(app: &mut App) {
    let Some(id) = app.wm.focus else {
        return;
    };
    toggle_maximize_window(app, id).await;
}

/// The one maximize toggle (the reserved key, the titlebar button and
/// Part06's double-click all land here): the core flips the state, the
/// reconciler brings the panes along.
async fn toggle_maximize_window(app: &mut App, id: u64) {
    app.wm.toggle_maximize(id);
    let errors = app.panes.resize_all(&app.wm).await;
    app.status = first_error(errors).or(app.status.take());
}

async fn forward_to_focused(app: &mut App, key: KeyEvent) {
    let Some(id) = app.wm.focus else {
        return;
    };
    let forwarded = app.panes.forward_key(id, key).await;
    if let Err(error) = forwarded {
        app.status = Some(input_error_message(app, id, "input", error).await);
    }
}

/// A failed input becomes a human status. An exited program is the one
/// expected cause — every key into its pane fails the same way — so say
/// that and the way out (checked once, on the error path only); anything
/// else reports the raw error.
async fn input_error_message(app: &App, id: u64, op: &str, error: RmuxHostError) -> String {
    let Some(pane) = app.panes.pane_handle(id) else {
        return format!("{op}: {error}");
    };
    if !reconcile::pane_has_exited(&pane).await {
        return format!("{op}: {error}");
    }
    let title = app
        .wm
        .window(id)
        .map(|window| window.title.clone())
        .unwrap_or_else(|| format!("window {id}"));
    exited_window_message(&title, &inputmap::combo_to_str(app.combos.close_window))
}

/// The status for a window whose program is gone (the pane reports
/// `Exited`): what happened and the one way out.
fn exited_window_message(title: &str, close: &str) -> String {
    format!(
        "{title}: its program has exited — this window takes no more input; close it with {close}"
    )
}

/// The resize path: the core clamps every window into the new desktop;
/// the reconciler brings the panes along.
async fn handle_resize(app: &mut App, cols: u16, rows: u16) {
    app.layout = frame::regions(Rect::new(0, 0, cols, rows), app.taskbar_pos);
    app.wm.set_desktop(app.layout.desktop);
    app.panes.refresh_all().await;
    let errors = app.panes.resize_all(&app.wm).await;
    app.status = first_error(errors).or(app.status.take());
}

async fn apply_menu_event(app: &mut App, event: MenuEvent) {
    match event {
        MenuEvent::Consumed => {}
        MenuEvent::Close => app.menu = None,
        MenuEvent::Launch(index) => launch_entry(app, index).await,
        MenuEvent::Search(text) => {
            // SEARCH acted (9.2.2): the menu closes and the command bar
            // opens with the filter carried through `open_with`.
            app.menu = None;
            let carried = if text.trim().is_empty() {
                None
            } else {
                Some(text)
            };
            open_bar(app, carried.as_deref(), false);
        }
        MenuEvent::OpenPlace { path } => {
            app.menu = None;
            open_place(app, &path);
        }
        MenuEvent::OpenSettings { section } => {
            app.menu = None;
            open_settings_at(app, section);
        }
        MenuEvent::OpenAbout => {
            app.menu = None;
            open_welcome(app);
        }
        MenuEvent::OpenDirectory => {
            app.menu = None;
            open_directory(app);
        }
    }
}

// --------------------------------------------- places and the panel (Part09)

/// Enter/click on a place (9.2.5): re-root the docked file panel at that
/// directory and focus it — reopening the panel window if it was closed
/// (the standing panel-reopen gap, closed here).
fn open_place(app: &mut App, path: &std::path::Path) {
    if app.panel_window.is_none() {
        open_panel_at(app, path);
        return;
    }
    let id = app.panel_window.expect("checked");
    let mut state = panel::open(path.to_path_buf(), fsprobe_options(app));
    state.cursor.move_right(&state.roots_snapshot());
    app.wm.set_title(id, &panel::window_title(&state, None));
    app.panel = Some(state);
    app.panel_walked_at = Some(Instant::now());
    app.wm.focus(id);
}

/// The docked panel over an explicit root (the open_place reopen path).
fn open_panel_at(app: &mut App, root: &std::path::Path) {
    let rect = panel::docked_rect(app.wm.desktop);
    let mut state = panel::open(root.to_path_buf(), fsprobe_options(app));
    state.cursor.move_right(&state.roots_snapshot());
    let id = app.wm.open_at("files", None, rect);
    app.panel_window = Some(id);
    app.panel_walked_at = Some(Instant::now());
    app.wm.set_title(id, &panel::window_title(&state, None));
    app.panel = Some(state);
}

// ------------------------------------------------- the welcome window (Part10)

/// Open (or refocus) the welcome document. Never marks the seen marker —
/// only a dismissal does (10.2.2).
fn open_welcome(app: &mut App) {
    if let Some(id) = app.welcome_win {
        app.wm.focus(id);
        return;
    }
    app.welcome_content = welcome::build(app.font, font_note_of(app), &app.combos, &app.theme);
    let rect = welcome::rect(app.wm.desktop);
    let id = app.wm.open_at("Welcome to AGony", None, rect);
    app.welcome_win = Some(id);
    app.wm.focus(id);
}

/// The startup font note, as the welcome restates it (the note lives in
/// the first status line; rebuild it from the live mode instead).
fn font_note_of(app: &App) -> Option<&'static str> {
    match app.font {
        FontMode::Nerd => None,
        _ => Some("run `agony doctor` to check the Nerd Font pattern"),
    }
}

/// Esc or the close button dismisses it — and that one dismissal is what
/// the marker remembers.
fn close_welcome(app: &mut App) {
    if let Some(id) = app.welcome_win.take() {
        app.wm.close(id);
    }
    welcome::mark_seen();
}

fn welcome_is_focused(app: &App) -> bool {
    app.welcome_win.is_some_and(|id| app.wm.focus == Some(id))
}

// ------------------------------------------------- the directory window (Part11)

/// Open (or refocus + refresh) the directory over the discovery
/// snapshot (11.3.1).
fn open_directory(app: &mut App) {
    if let Some(id) = app.directory_win {
        rebuild_directory(app);
        app.wm.focus(id);
        return;
    }
    let mut state = directory::State::default();
    let cache = discover::lock(&app.discovery).cache.clone();
    if let Some(cache) = cache {
        state.set_entries(directory::entries_from(&cache));
    }
    let rect = directory::rect(app.wm.desktop);
    let id = app.wm.open_at("App directory", None, rect);
    app.directory_win = Some(id);
    app.directory = Some(state);
    app.wm.focus(id);
}

fn close_directory(app: &mut App) {
    if let Some(id) = app.directory_win.take() {
        app.wm.close(id);
    }
    app.directory = None;
}

fn directory_is_focused(app: &App) -> bool {
    app.directory_win.is_some_and(|id| app.wm.focus == Some(id))
}

/// Rebuild the entries from the latest snapshot (open, post-install).
fn rebuild_directory(app: &mut App) {
    let cache = discover::lock(&app.discovery).cache.clone();
    let Some(state) = app.directory.as_mut() else {
        return;
    };
    let Some(cache) = cache else {
        return;
    };
    state.set_entries(directory::entries_from(&cache));
}

async fn apply_directory_event(app: &mut App, event: directory::Event) {
    match event {
        directory::Event::Consumed => {}
        directory::Event::Close => close_directory(app),
        directory::Event::Install(pending) => run_install(app, *pending).await,
    }
}

/// The confirmed install (11.3.2's action): one task runs it through
/// the one installer; its Outcome lands on the channel the loop drains.
/// A success refreshes discovery first, so the entry is live by the
/// time the result line shows (11.4.1).
async fn run_install(app: &mut App, pending: directory::Pending) {
    if app.install_busy {
        if let Some(state) = app.directory.as_mut() {
            state.result = Some("failed: an install is already running".into());
            state.pending = None;
        }
        return;
    }
    let Some(state) = app.directory.as_mut() else {
        return;
    };
    state.pending = None;
    let what = match &pending.action {
        directory::Action::Package(command) => format!("running: {command}"),
        directory::Action::Release(boxed) => format!("downloading {}", boxed.0.url),
    };
    state.progress = Some(what);
    state.result = None;
    app.install_busy = true;
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    app.install_rx = Some(rx);
    let shared = app.discovery.clone();
    let config_dir = settings::config_dir();
    let id = pending.id.clone();
    tokio::spawn(async move {
        let (ok, message) = run_install_action(pending).await;
        if ok {
            discover::refresh_into(shared, config_dir).await;
        }
        let _ = tx.send(Outcome { id, ok, message });
    });
}

/// The one install execution, off the UI thread (the result message is
/// the one line 11.4.2 shows; a failure carries the recipe's output).
async fn run_install_action(pending: directory::Pending) -> (bool, String) {
    match pending.action {
        directory::Action::Package(command) => match installer::run_package(&command).await {
            Ok(tail) => (true, format!("done {}", tail).trim().to_string()),
            Err(error) => (false, format!("failed: {error}")),
        },
        directory::Action::Release(boxed) => {
            let (spec, manifest) = *boxed;
            match installer::install_release(&spec, &manifest, &installer::bin_dir()).await {
                Ok(path) => (
                    true,
                    format!("hash verified — installed to {}", path.display()),
                ),
                Err(error) => (false, format!("failed: {error}")),
            }
        }
    }
}

/// Drain finished installs (one loop tick): the result line lands, the
/// entries rebuild from the refreshed snapshot, the busy flag clears.
fn drain_installs(app: &mut App) {
    let mut outcomes = Vec::new();
    if let Some(rx) = app.install_rx.as_mut() {
        while let Ok(outcome) = rx.try_recv() {
            outcomes.push(outcome);
        }
    }
    if outcomes.is_empty() {
        return;
    }
    app.install_busy = false;
    let Some(state) = app.directory.as_mut() else {
        return;
    };
    state.progress = None;
    if let Some(last) = outcomes.last() {
        state.result = Some(format!("{}: {}", last.id, last.message));
    }
}

// ------------------------------------------------- the settings window (Part08)

/// The one programmatic entry (8.2.4): open the settings window focused
/// on a named section — the seam Part09's SETTINGS section deep-links
/// through. An already-open window re-targets instead of reopening.
fn open_settings_at(app: &mut App, section: settings_ui::Section) {
    let Some(id) = app.settings_win else {
        let mut state = settings_ui::State::open(&app.settings_live);
        settings_ui::open_at(&mut state, section);
        let rect = settings_ui::rect(app.wm.desktop);
        let id = app.wm.open_at("settings", None, rect);
        app.settings_win = Some(id);
        app.settings_state = Some(state);
        app.wm.focus(id);
        return;
    };
    if let Some(state) = app.settings_state.as_mut() {
        settings_ui::open_at(state, section);
    }
    app.wm.focus(id);
}

/// Cancel/Close (8.2's pattern): revert the preview, drop the draft.
fn close_settings(app: &mut App) {
    app.theme = app.committed_theme.clone();
    if let Some(id) = app.settings_win.take() {
        app.wm.close(id);
    }
    app.settings_state = None;
}

/// The settings events the window emits (Part08's preview-until-commit):
/// previews swap the live applier, Apply persists both files and then
/// activates, Close reverts.
async fn apply_settings_event(app: &mut App, event: settings_ui::Event) {
    match event {
        settings_ui::Event::Consumed => {}
        settings_ui::Event::Close => close_settings(app),
        settings_ui::Event::Preview { theme, palette } => {
            app.theme = Applier::with_parts(&theme, palette);
        }
        settings_ui::Event::Apply(applied) => apply_settings(app, *applied).await,
    }
}

/// Apply (8.2's pattern): write settings.toml and the override pack —
/// no setting takes effect through a path that bypasses the file — then
/// activate. The watcher's stamp is refreshed so the watcher ignores
/// this write (its own-writes guard).
async fn apply_settings(app: &mut App, applied: settings_ui::Applied) {
    let mut notes = Vec::new();
    if let Err(error) = settings::save(&applied.settings) {
        app.status = Some(format!("settings save failed: {error}"));
        return;
    }
    if !applied.overrides.is_empty() {
        if let Some(extends) = applied.overrides_extends.as_deref() {
            if let Err(message) = write_overrides(extends, &applied.overrides) {
                notes.push(message);
            }
        }
    }
    let (theme_applier, theme_notes) =
        Applier::load(&applied.settings.theme, Some(&settings::custom_theme_dir()));
    notes.extend(theme_notes);
    activate(app, applied.settings, theme_applier, notes);
    app.settings_stamp = settings::file_stamp(&settings::settings_path());
    refresh_panel(app);
}

/// The override pack's atomic write (temp + rename, the watcher rule).
fn write_overrides(extends: &str, overrides: &[(Role, ColorValue)]) -> Result<(), String> {
    let dir = settings::custom_theme_dir();
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let json = agony_core::thememodel::override_to_json("user overrides", extends, overrides);
    let path = dir.join(theme::OVERRIDE_FILE);
    let temp = dir.join("overrides.json.tmp");
    std::fs::write(&temp, json)
        .and_then(|_| std::fs::rename(&temp, &path))
        .map_err(|error| format!("override pack write failed: {error}"))
}

/// The one activation (Apply and the live reload converge here): font,
/// combos, suggestions, files, taskbar position, the applier.
fn activate(app: &mut App, new_settings: Settings, applier: Applier, mut notes: Vec<String>) {
    let (font, font_note) = settings::resolve_font_mode(new_settings.font_mode);
    let (combos, combo_notes) = settings::reserved_combos(&new_settings);
    app.font = font;
    app.combos = combos;
    app.suggestions_enabled = new_settings.suggestions_enabled;
    app.complete.set_enabled(new_settings.suggestions_enabled);
    app.files = new_settings.files;
    app.taskbar_pos = new_settings.taskbar_position;
    app.theme = applier;
    app.committed_theme = app.theme.clone();
    app.settings_live = new_settings;
    if let Some(note) = font_note {
        notes.push(note);
    }
    notes.extend(combo_notes);
    if !notes.is_empty() {
        app.status = Some(notes.join(" | "));
    }
}

/// The live reload (8.2.6): the settings file is watched through its
/// parent directory's stamp (atomic-rename-safe: a rename replaces the
/// parent's entry). Our own writes never trigger a reload — `apply`
/// refreshes the stamp after saving, the classic mtime guard, stated
/// here. A malformed edit changes nothing and says so; the stamp still
/// updates so the note is not repeated every tick.
async fn watch_settings(app: &mut App) {
    if app.settings_watch_at.elapsed() < std::time::Duration::from_millis(500) {
        return;
    }
    app.settings_watch_at = Instant::now();
    let stamp = settings::file_stamp(&settings::settings_path());
    if stamp == app.settings_stamp {
        return;
    }
    app.settings_stamp = stamp;
    let loaded = settings::load();
    let (theme_applier, mut notes) =
        Applier::load(&loaded.settings.theme, Some(&settings::custom_theme_dir()));
    notes.extend(loaded.notes);
    if notes.is_empty() {
        notes.push("settings reloaded".into());
    }
    activate(app, loaded.settings, theme_applier, notes);
}

// ------------------------------------------------- the icon grid (Part09)

/// Activate a tile (9.4.4): a shortcut launches through the one
/// launcher (a vanished manifest refuses with the recipe status, the
/// menu's exact rule); a folder re-roots the docked panel (the 9.2.5
/// seam); a document opens through the `open_with` lookup.
async fn activate_tile(app: &mut App, index: usize) {
    let Some(tile) = app.icons.entries.get(index) else {
        return;
    };
    match &tile.kind {
        desktop_icons::Kind::Shortcut { id } => {
            let id = id.clone();
            let entry = app
                .entries
                .iter()
                .find(|entry| entry.manifest.id == id)
                .cloned();
            let Some(entry) = entry else {
                app.status = Some(format!("shortcut {id}: the app left the catalog"));
                return;
            };
            launch_tile_shortcut(app, &entry, &id).await;
        }
        desktop_icons::Kind::Folder => {
            let path = tile.path.clone();
            open_place(app, &path);
        }
        desktop_icons::Kind::Document => {
            let path = tile.path.clone();
            open_file(app, &path).await;
        }
    }
}

/// A shortcut's launch (the one launcher; greyed refuses with recipe).
async fn launch_tile_shortcut(app: &mut App, entry: &startmenu::Entry, id: &str) {
    if let Err(message) = check_launchable(entry) {
        app.status = Some(format!("shortcut {id}: {message}"));
        return;
    }
    let name = entry.manifest.name.clone();
    let argv = rmuxhost::manifest_argv(&entry.manifest, entry.path.as_deref());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let cwd = entry.path.as_deref().map(PathBuf::from);
    let launched = app
        .panes
        .launch(
            &app.rmux,
            rmuxhost::SESSION,
            &mut app.wm,
            &name,
            &argv,
            cwd.as_deref(),
        )
        .await;
    report_launch(app, launched, &name);
}

// ------------------------------------------------- context menus (Part09)

/// What an open context menu acts on (the app's anchor; the widget
/// carries plain items).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextAnchor {
    Program(usize),
    Tile(usize),
}

/// The open context menu plus its anchor.
struct ContextState {
    menu: contextmenu::Menu,
    anchor: ContextAnchor,
}

/// Build and open the context menu for a target (9.3); inert elsewhere.
fn open_context_menu(app: &mut App, target: ContextTarget, at: (u16, u16)) {
    let Some(items) = context_items(app, target) else {
        return; // the seam routes; nothing renders (9.3.4)
    };
    let Some(anchor) = context_anchor(target) else {
        return;
    };
    app.context = Some(ContextState {
        menu: contextmenu::Menu::open_at(items, at),
        anchor,
    });
}

/// The anchor behind a menu-bearing target; None stays inert.
fn context_anchor(target: ContextTarget) -> Option<ContextAnchor> {
    match target {
        ContextTarget::MenuItem {
            hit: startmenu::MenuHit::Program(index),
        } => Some(ContextAnchor::Program(index)),
        ContextTarget::Tile(index) => Some(ContextAnchor::Tile(index)),
        _ => None,
    }
}

/// The menu items per target; None stays inert (9.3.4).
fn context_items(app: &App, target: ContextTarget) -> Option<Vec<contextmenu::Item>> {
    match target {
        ContextTarget::MenuItem {
            hit: startmenu::MenuHit::Program(_),
        } => Some(vec![
            contextmenu::Item::new("Open", CtxAction::Open),
            contextmenu::Item::new("Pin to desktop", CtxAction::Pin),
            contextmenu::Item::new("Copy launch command", CtxAction::CopyCommand),
        ]),
        ContextTarget::Tile(index) => {
            let tile = app.icons.entries.get(index)?;
            Some(match &tile.kind {
                desktop_icons::Kind::Shortcut { id } => vec![
                    contextmenu::Item::new("Open", CtxAction::Open),
                    contextmenu::Item::new(format!("Unpin {id}"), CtxAction::Unpin),
                    contextmenu::Item::new("Copy path", CtxAction::CopyPath),
                ],
                desktop_icons::Kind::Folder => vec![
                    contextmenu::Item::new("Open", CtxAction::Open),
                    contextmenu::Item::new("Copy path", CtxAction::CopyPath),
                ],
                desktop_icons::Kind::Document => vec![
                    contextmenu::Item::new("Open", CtxAction::Open),
                    contextmenu::Item::new("Delete document", CtxAction::DeleteDocument),
                    contextmenu::Item::new("Copy path", CtxAction::CopyPath),
                ],
            })
        }
        _ => None,
    }
}

/// Execute a context action (9.3.2–9.3.3). The menu is already closed;
/// the anchor remembers what it acted on.
async fn run_context_action(app: &mut App, state: ContextState, action: CtxAction) {
    let anchor = state.anchor;
    match action {
        CtxAction::Open => open_context_target(app, anchor).await,
        CtxAction::Pin => pin_context_target(app, anchor),
        CtxAction::CopyCommand => copy_command_target(app, anchor),
        CtxAction::Unpin => unpin_context_target(app, anchor),
        CtxAction::DeleteDocument => {
            app.status =
                Some("no trash yet — document deletion is refused (the file moves nowhere)".into());
        }
        CtxAction::CopyPath => {
            if let Some(path) = context_tile_path(app, anchor) {
                app.status = Some(path.display().to_string());
            }
        }
    }
}

fn context_tile_path(app: &App, anchor: ContextAnchor) -> Option<std::path::PathBuf> {
    let ContextAnchor::Tile(index) = anchor else {
        return None;
    };
    app.icons.entries.get(index).map(|tile| tile.path.clone())
}

async fn open_context_target(app: &mut App, anchor: ContextAnchor) {
    match anchor {
        ContextAnchor::Program(index) => launch_entry(app, index).await,
        ContextAnchor::Tile(index) => activate_tile(app, index).await,
    }
}

fn pin_context_target(app: &mut App, anchor: ContextAnchor) {
    let ContextAnchor::Program(index) = anchor else {
        return;
    };
    let Some(id) = app
        .entries
        .get(index)
        .map(|entry| entry.manifest.id.clone())
    else {
        return;
    };
    match desktop_icons::pin(&settings::desktop_dir(), &id) {
        Ok(_) => {
            app.icons.refresh();
            app.status = Some(format!("{id} pinned to the desktop"));
        }
        Err(error) => app.status = Some(format!("pin failed: {error}")),
    }
}

fn unpin_context_target(app: &mut App, anchor: ContextAnchor) {
    let ContextAnchor::Tile(index) = anchor else {
        return;
    };
    let kind = app.icons.entries.get(index).map(|tile| tile.kind.clone());
    let Some(desktop_icons::Kind::Shortcut { id }) = kind else {
        app.status = Some("only shortcuts unpin — files and folders wait for a trash".into());
        return;
    };
    match desktop_icons::unpin(&settings::desktop_dir(), &id) {
        Ok(_) => {
            app.icons.refresh();
            app.status = Some(format!("{id} unpinned"));
        }
        Err(error) => app.status = Some(format!("unpin failed: {error}")),
    }
}

fn copy_command_target(app: &mut App, anchor: ContextAnchor) {
    let ContextAnchor::Program(index) = anchor else {
        return;
    };
    let Some(entry) = app.entries.get(index) else {
        return;
    };
    let argv = rmuxhost::manifest_argv(&entry.manifest, entry.path.as_deref());
    app.status = Some(argv.join(" "));
}

/// Launch through the one seam into a new window (Part04 4.4.2): the
/// overlay closes, the program becomes a window on the desktop. A
/// not-installed entry or a failed spawn surfaces as a status line
/// naming the app.
async fn launch_entry(app: &mut App, index: usize) {
    app.menu = None;
    let Some(entry) = app.entries.get(index) else {
        return;
    };
    if let Err(message) = check_launchable(entry) {
        app.status = Some(message);
        return;
    }
    let name = entry.manifest.name.clone();
    let argv = rmuxhost::manifest_argv(&entry.manifest, entry.path.as_deref());
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let cwd = entry.path.as_deref().map(PathBuf::from);
    let launched = app
        .panes
        .launch(
            &app.rmux,
            rmuxhost::SESSION,
            &mut app.wm,
            &name,
            &argv,
            cwd.as_deref(),
        )
        .await;
    report_launch(app, launched, &name);
}

fn report_launch(app: &mut App, launched: Result<u64, LaunchError>, name: &str) {
    if let Err(error) = launched {
        app.status = Some(format!("launch failed: {name}: {error}"));
    }
}

fn first_error(errors: Vec<String>) -> Option<String> {
    errors.into_iter().next()
}

/// The pure launch gate: candidates and installed entries launch; a greyed
/// manifest refuses with its install recipe.
fn check_launchable(entry: &startmenu::Entry) -> Result<(), String> {
    if entry.candidate || entry.path.is_some() {
        return Ok(());
    }
    let recipe = entry
        .manifest
        .install_recipe()
        .unwrap_or_else(|| "no recipe".into());
    Err(format!(
        "{} is not installed — {recipe}",
        entry.manifest.name
    ))
}

// ------------------------------------------------------- the command bar

/// Open the bar (7.2.1): the reserved key, the taskbar's prompt click,
/// or the drop with a file held. `initial` is `open_with`'s text
/// (Part09's SEARCH carries its filter through it).
fn open_bar(app: &mut App, initial: Option<&str>, with_paste: bool) {
    let cwd = std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let mut bar = match initial {
        Some(text) => commandbar::State::open_with(&cwd, text, app.suggestions_enabled),
        None => commandbar::State::open(&cwd, app.suggestions_enabled),
    };
    if let Some(picked) = app.picked.as_ref().map(|picked| picked.path.clone()) {
        if with_paste {
            bar.drop_picked(&picked);
        }
    }
    app.menu = None;
    app.bar = Some(bar);
}

async fn apply_bar_event(app: &mut App, event: BarEvent) {
    match event {
        BarEvent::Consumed => {}
        BarEvent::Close => app.bar = None,
        BarEvent::Execute => execute_line(app).await,
    }
}

/// The bar's per-draw-tick work: fresh candidates, the debounced
/// carapace request, and the context re-read when one was asked for.
async fn bar_tick(app: &mut App) {
    if app.bar.is_none() {
        return;
    }
    refresh_bar(app);
    maybe_request_carapace(app);
    refresh_bar_context(app).await;
}

/// The suggestion pass: one merge of the four sources per tick.
fn refresh_bar(app: &mut App) {
    let files = app
        .panel
        .as_ref()
        .map(|state| (state.root.clone(), state.roots_snapshot()));
    let Some(bar) = app.bar.as_mut() else {
        return;
    };
    let sources = commandbar::Sources {
        entries: &app.entries,
        history: &app.history,
        files,
        complete: &app.complete,
    };
    commandbar::refresh(bar, &sources);
}

/// The debounce (7.3.7): one carapace subprocess per 150 ms of quiet
/// typing, only when the answer is not cached or already running.
fn maybe_request_carapace(app: &mut App) {
    if !app.suggestions_enabled || !app.complete.enabled() {
        return;
    }
    let Some(bar) = app.bar.as_ref() else {
        return;
    };
    if bar.edit_at.elapsed().as_millis() < complete::QUIET_MS {
        return;
    }
    let parsed = agony_core::cmdline::parse(&bar.line.text, bar.line.cursor);
    let values = agony_core::cmdline::prefix_values(&bar.line.text, bar.line.cursor);
    let Some(command) = values.first().cloned() else {
        return;
    };
    if app.complete.cached(&command, &parsed.active_text).is_some()
        || app.complete.in_flight(&command, &parsed.active_text)
    {
        return;
    }
    // carapace wants the words after the command — `carapace <cmd>
    // nushell -- <args...>`, the spike's proven shape (including the
    // command makes it answer `[]`).
    let mut args: Vec<String> = values.into_iter().skip(1).collect();
    args.push(parsed.active_text.clone());
    app.complete.request(command, args);
}

/// The context re-read (7.2.2): the focused window's pane cwd, falling
/// back to the desktop's. Asked for at open and on a context click.
async fn refresh_bar_context(app: &mut App) {
    let dirty = app.bar.as_ref().is_some_and(|bar| bar.context_dirty);
    if !dirty {
        return;
    }
    let focused = app.wm.focus;
    let from_pane = match focused {
        Some(id) => app.panes.pane_cwd(id).await,
        None => None,
    };
    let resolved = from_pane.or_else(|| {
        std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string())
    });
    let Some(bar) = app.bar.as_mut() else {
        return;
    };
    bar.context_dirty = false;
    if let Some(cwd) = resolved {
        bar.context = cwd;
    }
}

/// Enter on the bare line (7.2.4): a manifest app launches with the
/// remaining words as argv through the same launcher as the start menu;
/// anything else runs as one shell command line in a new window. The
/// line lands in history either way; shell lines get an exit watcher
/// (7.4.2's failure memory).
async fn execute_line(app: &mut App) {
    let Some(bar) = app.bar.take() else {
        return;
    };
    let line = bar.line.text.clone();
    if line.trim().is_empty() {
        app.bar = Some(bar);
        return;
    }
    let cwd = bar.context.clone();
    let values = agony_core::cmdline::values_of(&line);
    let title;
    let argv;
    let mut is_shell = false;
    match commandbar::resolve_launch(&app.entries, &values) {
        Some((entry, rest)) => {
            if let Err(message) = check_launchable(entry) {
                app.status = Some(message);
                app.bar = Some(bar);
                return;
            }
            let mut manifest_argv = rmuxhost::manifest_argv(&entry.manifest, entry.path.as_deref());
            manifest_argv.extend(rest);
            title = entry.manifest.name.clone();
            argv = manifest_argv;
        }
        None => {
            title = values.first().cloned().unwrap_or_else(|| "shell".into());
            argv = commandbar::shell_argv(&line);
            is_shell = true;
        }
    }
    let timestamp = now_ms(app);
    app.history.record(Entry {
        line: line.clone(),
        cwd: cwd.clone(),
        timestamp_ms: timestamp,
        status: None,
    });
    mark_history_dirty(app);
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    let cwd_path = PathBuf::from(&cwd);
    let launched = app
        .panes
        .launch(
            &app.rmux,
            rmuxhost::SESSION,
            &mut app.wm,
            &title,
            &argv_ref,
            Some(&cwd_path),
        )
        .await;
    match launched {
        Ok(window) if is_shell => spawn_exit_watcher(app, window, &line, timestamp),
        Ok(_) => {}
        Err(error) => app.status = Some(format!("launch failed: {title}: {error}")),
    }
}

/// The failure recorder (7.4.2): poll the pane off-thread; when the
/// command's process exits nonzero, write the status onto its history
/// entry.
fn spawn_exit_watcher(app: &App, window: u64, line: &str, timestamp: u64) {
    let (Some(pane), Some(key)) = (app.panes.pane_handle(window), app.panes.pane_key(window))
    else {
        return;
    };
    let store = app.history.clone();
    let line = line.to_string();
    tokio::spawn(async move {
        if let Some(code) = commandbar::pane_exit_status(&pane, key).await {
            store.record_failure(&line, timestamp, code);
            mark_dirty_from_task(&store);
        }
    });
}

/// The watcher cannot reach the app's dirty flag; it leaves the store
/// consistent and the next record's save sweeps it. (The quiet timer
/// saves on any subsequent activity; a hard kill loses only the tail.)
fn mark_dirty_from_task(_store: &commandbar::HistoryStore) {}

fn mark_history_dirty(app: &mut App) {
    app.history_dirty_at = Some(Instant::now());
}

/// The quiet-timer flush (7.4.1): at most one write per idle window,
/// never per keystroke.
fn save_history_if_due(app: &mut App) {
    let due = app
        .history_dirty_at
        .is_some_and(|at| at.elapsed() >= commandbar::SAVE_IDLE);
    if due {
        flush_history(app);
    }
}

/// Write the history file when the loop's dirty flag is set — the
/// quiet timer's flush.
fn flush_history(app: &mut App) {
    if app.history_dirty_at.is_none() {
        return;
    }
    app.history_dirty_at = None;
    save_history(app);
}

/// The unconditional write (quit path): the exit watcher records
/// failures straight into the shared store without the loop's dirty
/// flag, so quitting always persists whatever the store holds.
fn save_history(app: &mut App) {
    if let Err(error) = commandbar::save(&settings::config_dir(), &app.history.snapshot()) {
        app.status = Some(format!("history save failed: {error}"));
    }
}

// -------------------------------------------------------- pointer (Part06)

/// Mouse routing (Part06 6.3.1): translate once, fold into the gesture
/// machine, dispatch one gesture. The drag plumbing (Part04) watches
/// the raw Drag/Up events beneath the fold, unchanged.
async fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    let point = cell_at(mouse.column, mouse.row);
    app.pointer_cell = point;
    match mouse.kind {
        MouseEventKind::Drag(MouseButton::Left) => handle_drag(app, point),
        MouseEventKind::Up(MouseButton::Left) => end_drag(app).await,
        _ => {}
    }
    let Some(event) = translate_mouse(mouse, point) else {
        return;
    };
    let now = now_ms(app);
    if let Some(gesture) = app.gestures.on_event(event, now) {
        dispatch_gesture(app, gesture).await;
    }
}

/// The one mouse-position-to-cell translation (Part04 4.5.3): on this
/// host crossterm's mouse arrives with 0-based rows but 1-based columns
/// (verified live — with the identity mapping every titlebar button
/// fired its right neighbour), so the column is the only adjustment.
/// Kept as the one place so any future host quirk has exactly one knob.
/// No pixel math exists in AGony.
fn cell_at(column: u16, row: u16) -> (u16, u16) {
    (column.saturating_sub(1), row)
}

/// The crossterm → core translation (Part06 6.2.4): the cells are
/// already translated (`cell_at` above is the one place); the wheel's
/// signs are fixed here (up = toward the top of the list); horizontal
/// wheels are dropped. Nothing is injected into panes.
fn translate_mouse(mouse: MouseEvent, point: (u16, u16)) -> Option<PointerEvent> {
    let (kind, button) = match mouse.kind {
        MouseEventKind::Down(button) => (PointerKind::Down, Some(button)),
        MouseEventKind::Up(button) => (PointerKind::Up, Some(button)),
        MouseEventKind::Drag(button) => (PointerKind::Drag, Some(button)),
        MouseEventKind::Moved => (PointerKind::Move, None),
        MouseEventKind::ScrollDown => (
            PointerKind::Wheel {
                rows: pointer::WHEEL_ROWS_PER_NOTCH,
            },
            None,
        ),
        MouseEventKind::ScrollUp => (
            PointerKind::Wheel {
                rows: -pointer::WHEEL_ROWS_PER_NOTCH,
            },
            None,
        ),
        // crossterm 0.28 has no pointer-left event: Leave stays a core
        // gesture with no producer until the crossterm bump.
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => return None,
    };
    let button = button.map(core_button).unwrap_or(PointerButton::Left);
    Some(PointerEvent {
        kind,
        button,
        col: point.0,
        row: point.1,
    })
}

fn core_button(button: MouseButton) -> PointerButton {
    match button {
        MouseButton::Left => PointerButton::Left,
        MouseButton::Middle => PointerButton::Middle,
        MouseButton::Right => PointerButton::Right,
    }
}

fn now_ms(app: &App) -> u64 {
    app.epoch.elapsed().as_millis() as u64
}

/// The one dispatcher (Part06 6.3.1): every pointer action lands here
/// exactly once.
async fn dispatch_gesture(app: &mut App, gesture: Gesture) {
    match gesture {
        Gesture::Click {
            button: PointerButton::Left,
            col,
            row,
        } => handle_click(app, (col, row)).await,
        Gesture::Click {
            button: PointerButton::Right,
            col,
            row,
        } => {
            // The right-click seam (Part06 6.3.6): Part09 hangs the
            // first context menus on it; other targets stay inert.
            let target = context_target(app, (col, row));
            let Some(target) = target else {
                return;
            };
            open_context_menu(app, target, (col, row));
        }
        Gesture::Click { .. } => {}
        Gesture::DoubleClick {
            button: PointerButton::Left,
            col,
            row,
        } => handle_double_click(app, (col, row)).await,
        Gesture::DoubleClick { .. } => {}
        Gesture::Wheel { rows } => handle_wheel(app, rows).await,
        Gesture::Hover { col, row } => app.hover = Some((col, row)),
        Gesture::Leave => app.hover = None,
    }
}

async fn handle_click(app: &mut App, point: (u16, u16)) {
    if app.context.is_some() {
        // The context menu is the modal's modal: a row activates,
        // outside closes (9.3.1).
        context_click(app, point).await;
        return;
    }
    if app.bar.is_some() {
        if point_in(point, bar_rect(app)) {
            bar_click(app, point);
            return;
        }
        app.bar = None; // outside closes; the click continues below
    }
    if click_taskbar(app, point).await {
        return;
    }
    let menu_open = app.menu.is_some();
    let menu_rect = app
        .menu
        .as_ref()
        .map(|menu| startmenu::rect_for(app.layout.overlay, menu, app.taskbar_pos));
    let outside_menu = menu_rect.is_none_or(|rect| !point_in(point, rect));
    if menu_open && outside_menu {
        app.menu = None;
    } else if let Some(menu) = app.menu.as_mut() {
        let region = menu_rect.unwrap_or_default();
        let event = startmenu::click(menu, region, point);
        apply_menu_event(app, event).await;
        return;
    }
    match hit_test(&app.wm, point.0, point.1) {
        Hit::Button { window, button } => titlebar_button(app, window, button).await,
        Hit::TitleBar { window } => {
            app.wm.focus(window);
            app.drag = Some(Drag::Move {
                window,
                last: point,
            });
        }
        Hit::Edge { window, edge } => {
            app.wm.focus(window);
            app.drag = Some(Drag::Resize {
                window,
                edge,
                last: point,
            });
        }
        Hit::Interior { window } if app.settings_win == Some(window) => {
            app.wm.focus(window);
            settings_click(app, point).await;
        }
        Hit::Interior { window } if app.directory_win == Some(window) => {
            app.wm.focus(window);
            let inner = directory_inner(app, window);
            let event = app
                .directory
                .as_mut()
                .map(|state| directory::click(state, inner, point))
                .unwrap_or(directory::Event::Consumed);
            apply_directory_event(app, event).await;
        }
        Hit::Interior { window } => {
            app.wm.focus(window);
            forward_click(app, window, point).await;
        }
        Hit::None => {
            // The bare desktop: a tile click selects (9.4.4).
            if let Some(index) = desktop_icons::tile_at(app.wm.desktop, &app.icons.entries, point) {
                app.icons.cursor.selected = index;
            }
        }
    }
}

/// The directory window's chrome inner rect — the same area its render
/// and its hit test use.
fn directory_inner(app: &App, id: u64) -> Rect {
    let Some(window) = app.wm.window(id) else {
        return Rect::default();
    };
    agony_core::layout::inner_rect(agony_core::layout::effective_rect(window, app.wm.desktop))
}

/// A click with the context menu open (9.3.1): a row activates it at
/// once (the menu rule — the mouse half of Enter), any click outside
/// closes. Taking the state closes the menu either way.
async fn context_click(app: &mut App, point: (u16, u16)) {
    let bounds = app.wm.desktop;
    let Some(state) = app.context.take() else {
        return;
    };
    let Some(action) = state.menu.action_at(bounds, point) else {
        return;
    };
    run_context_action(app, state, action).await;
}

/// The settings window's pointer path (8.2.5): section list and rows
/// click through the same `settings_ui::click` the keyboard routes to.
async fn settings_click(app: &mut App, point: (u16, u16)) {
    let Some(id) = app.settings_win else {
        return;
    };
    let Some(window) = app.wm.window(id) else {
        return;
    };
    let inner =
        agony_core::layout::inner_rect(agony_core::layout::effective_rect(window, app.wm.desktop));
    let Some(state) = app.settings_state.as_mut() else {
        return;
    };
    let (_, pane, _) = settings_ui::panes(inner);
    let event = settings_ui::click(state, inner, pane, point);
    apply_settings_event(app, event).await;
}

/// A click inside the open bar (7.3.5): a row selects it; the context
/// line asks for a cwd re-read.
fn bar_click(app: &mut App, point: (u16, u16)) {
    let region = bar_rect(app);
    let target = app
        .bar
        .as_ref()
        .and_then(|bar| commandbar::click_target(region, bar, point));
    let Some(target) = target else {
        return;
    };
    match target {
        commandbar::Click::Context => {
            if let Some(bar) = app.bar.as_mut() {
                bar.context_dirty = true;
            }
        }
        commandbar::Click::Row(index) => {
            if let Some(bar) = app.bar.as_mut() {
                bar.selected = Some(index);
            }
        }
    }
}

/// The bar's area, from the same row count the draw uses.
fn bar_rect(app: &App) -> Rect {
    let rows = app
        .bar
        .as_ref()
        .map(commandbar::visible_rows)
        .unwrap_or(0)
        .max(1);
    commandbar::rect(app.layout.desktop, rows, app.taskbar_pos)
}

/// What a taskbar click landed on — the shared table for clicks,
/// double-clicks, hover and the context seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskbarHit {
    Start,
    Window(u64),
    Prompt,
    Other,
}

fn taskbar_hit(
    taskbar: Rect,
    font: agony_core::config::FontMode,
    windows: &[TaskWindow],
    point: (u16, u16),
) -> Option<TaskbarHit> {
    if !point_in(point, taskbar) {
        return None;
    }
    if point_in(point, taskbar::start_button_rect(taskbar, font)) {
        return Some(TaskbarHit::Start);
    }
    let rects = taskbar::window_button_rects(taskbar, font, windows);
    if let Some((id, _)) = rects.iter().find(|(_, rect)| point_in(point, *rect)) {
        return Some(TaskbarHit::Window(*id));
    }
    if point_in(point, taskbar::prompt_rect(taskbar, font, windows)) {
        return Some(TaskbarHit::Prompt);
    }
    Some(TaskbarHit::Other)
}

async fn click_taskbar(app: &mut App, point: (u16, u16)) -> bool {
    let Some(hit) = taskbar_hit(app.layout.taskbar, app.font, &taskbar_windows(app), point) else {
        return false;
    };
    match hit {
        TaskbarHit::Start => app.toggle_menu(),
        TaskbarHit::Window(id) => apply_taskbar_click(&mut app.wm, id),
        TaskbarHit::Prompt => open_bar(app, None, false),
        TaskbarHit::Other => {}
    }
    true
}

/// The Windows behavior, one function: the focused entry minimizes; any
/// other focuses (and restores).
fn apply_taskbar_click(wm: &mut WmState, id: u64) {
    let minimized = wm
        .window(id)
        .is_some_and(|w| w.state == layout::WinState::Minimized);
    if wm.focus == Some(id) && !minimized {
        wm.minimize(id);
    } else {
        wm.focus(id);
    }
}

/// Part06 6.3.4's collapse rule: a pair of clicks is focus-and-restore
/// — never a minimize (focus() restores a minimized window).
fn apply_taskbar_double_click(wm: &mut WmState, id: u64) {
    wm.focus(id);
}

async fn titlebar_button(app: &mut App, window: u64, button: TitleButton) {
    match button {
        TitleButton::Minimize => app.wm.minimize(window),
        TitleButton::Maximize => toggle_maximize_window(app, window).await,
        TitleButton::Close => close_window(app, window).await,
    }
}

/// Double-click (Part06 6.3.4): a taskbar pair collapses to
/// focus-and-restore, a titlebar toggles maximize, and a pane interior
/// forwards its second click (two clicks, as before).
async fn handle_double_click(app: &mut App, point: (u16, u16)) {
    if app.bar.is_some() {
        if point_in(point, bar_rect(app)) {
            bar_double_click(app, point);
        }
        return;
    }
    if point_in(point, app.layout.taskbar) {
        let hit = taskbar_hit(app.layout.taskbar, app.font, &taskbar_windows(app), point);
        match hit {
            Some(TaskbarHit::Window(id)) => apply_taskbar_double_click(&mut app.wm, id),
            Some(TaskbarHit::Start) => app.toggle_menu(),
            Some(TaskbarHit::Prompt) => open_bar(app, None, false),
            _ => {}
        }
        return;
    }
    if let Some(menu) = app.menu.as_mut() {
        let region = startmenu::rect_for(app.layout.overlay, menu, app.taskbar_pos);
        let event = startmenu::double_click(menu, region, point);
        apply_menu_event(app, event).await;
        return;
    }
    match hit_test(&app.wm, point.0, point.1) {
        Hit::TitleBar { window } => {
            app.wm.focus(window);
            toggle_maximize_window(app, window).await;
        }
        Hit::Interior { window } if app.settings_win == Some(window) => {
            app.wm.focus(window);
            settings_double_click(app, point).await;
        }
        Hit::Interior { window } if app.directory_win == Some(window) => {
            app.wm.focus(window);
            let inner = directory_inner(app, window);
            let event = app
                .directory
                .as_mut()
                .map(|state| directory::double_click(state, inner, point))
                .unwrap_or(directory::Event::Consumed);
            apply_directory_event(app, event).await;
        }
        Hit::Interior { window } => {
            app.wm.focus(window);
            forward_click(app, window, point).await;
        }
        Hit::None => {
            // The bare desktop: a tile double-click activates (9.4.4).
            if let Some(index) = desktop_icons::tile_at(app.wm.desktop, &app.icons.entries, point) {
                activate_tile(app, index).await;
            }
        }
        _ => {}
    }
}

/// The picker's double-click commits the color (8.3.4).
async fn settings_double_click(app: &mut App, point: (u16, u16)) {
    let Some(id) = app.settings_win else {
        return;
    };
    let Some(window) = app.wm.window(id) else {
        return;
    };
    let inner =
        agony_core::layout::inner_rect(agony_core::layout::effective_rect(window, app.wm.desktop));
    let Some(state) = app.settings_state.as_mut() else {
        return;
    };
    let (_, pane, _) = settings_ui::panes(inner);
    let event = settings_ui::double_click(state, inner, pane, point);
    apply_settings_event(app, event).await;
}

/// A double-click on a suggestion row accepts it (7.3.5).
fn bar_double_click(app: &mut App, point: (u16, u16)) {
    let region = bar_rect(app);
    let target = app
        .bar
        .as_ref()
        .and_then(|bar| commandbar::click_target(region, bar, point));
    let Some(commandbar::Click::Row(index)) = target else {
        return;
    };
    if let Some(bar) = app.bar.as_mut() {
        bar.selected = Some(index);
        commandbar::accept_selected(bar);
    }
}

/// Where the wheel goes (Part06 6.3.2), in the overlay-first order the
/// click router uses: the bar, the menu, the panel's tree — window
/// interiors, titlebars, the taskbar and the bare desktop take no wheel
/// action in this part.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WheelTarget {
    Bar,
    Menu,
    Settings(u64),
    Directory(u64),
    Panel(u64),
    None,
}

fn wheel_target(
    bar: Option<Rect>,
    menu: Option<Rect>,
    settings_window: Option<u64>,
    directory_window: Option<u64>,
    panel_window: Option<u64>,
    wm: &WmState,
    point: (u16, u16),
) -> WheelTarget {
    if bar.is_some_and(|rect| point_in(point, rect)) {
        return WheelTarget::Bar;
    }
    if menu.is_some_and(|rect| point_in(point, rect)) {
        return WheelTarget::Menu;
    }
    for window in [settings_window, directory_window, panel_window] {
        let Some(id) = window else {
            continue;
        };
        if matches!(
            hit_test(wm, point.0, point.1),
            Hit::Interior { window } if window == id
        ) {
            if settings_window == Some(id) {
                return WheelTarget::Settings(id);
            }
            if directory_window == Some(id) {
                return WheelTarget::Directory(id);
            }
            return WheelTarget::Panel(id);
        }
    }
    WheelTarget::None
}

async fn handle_wheel(app: &mut App, rows: i32) {
    if rows == 0 {
        return;
    }
    let point = app.pointer_cell;
    let bar_open = app.bar.is_some();
    let bar = bar_open.then(|| bar_rect(app));
    let menu_open = app.menu.is_some();
    let menu = menu_open.then(|| {
        app.menu
            .as_ref()
            .map(|menu| startmenu::rect_for(app.layout.overlay, menu, app.taskbar_pos))
            .unwrap_or_default()
    });
    let settings_window = app.settings_win;
    let directory_window = app.directory_win;
    let panel_window = app.panel_window;
    match wheel_target(
        bar,
        menu,
        settings_window,
        directory_window,
        panel_window,
        &app.wm,
        point,
    ) {
        WheelTarget::Bar => {
            if let Some(bar) = app.bar.as_mut() {
                commandbar::scroll(bar, rows);
            }
        }
        WheelTarget::Menu => {
            if let Some(menu) = app.menu.as_mut() {
                menu.scroll_by(rows);
            }
        }
        WheelTarget::Settings(_) => {
            let desktop = app.wm.desktop;
            let Some(id) = app.settings_win else {
                return;
            };
            let Some(window) = app.wm.window(id) else {
                return;
            };
            let inner =
                agony_core::layout::inner_rect(agony_core::layout::effective_rect(window, desktop));
            if let Some(state) = app.settings_state.as_mut() {
                settings_ui::scroll_by(state, rows, settings_ui::pane_rows(inner));
            }
        }
        WheelTarget::Directory(_) => {
            if let Some(state) = app.directory.as_mut() {
                state.scroll_by(rows);
            }
        }
        WheelTarget::Panel(_) => {
            if let Some(panel) = app.panel.as_mut() {
                panel.scroll_by(rows);
            }
        }
        WheelTarget::None => {}
    }
}

/// The right-click seam (Part06 6.3.6): which surface and which item is
/// under the pointer. Part09 hangs the first context menus on the menu
/// row and the desktop tile targets; everything else stays inert.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextTarget {
    TaskbarStart,
    TaskbarWindow(u64),
    TaskbarPrompt,
    Bar,
    MenuItem {
        hit: startmenu::MenuHit,
    },
    TitleBar {
        window: u64,
    },
    TitleButton {
        window: u64,
        button: TitleButton,
    },
    WindowEdge {
        window: u64,
        edge: Edge,
    },
    WindowInterior {
        window: u64,
    },
    /// A desktop tile (the icon grid, Part09 §9.4).
    Tile(usize),
    Desktop,
}

fn context_target(app: &App, point: (u16, u16)) -> Option<ContextTarget> {
    let bar_open = app.bar.is_some();
    let bar = bar_open.then(|| bar_rect(app));
    let menu_hit = app.menu.as_ref().and_then(|menu| {
        let region = startmenu::rect_for(app.layout.overlay, menu, app.taskbar_pos);
        startmenu::hit_at(menu, region, point)
    });
    let surfaces = ContextSurfaces {
        taskbar: app.layout.taskbar,
        font: app.font,
        windows: &taskbar_windows(app),
        bar,
        menu_hit,
        tiles: &app.icons.entries,
        desktop: app.wm.desktop,
    };
    classify_context(&surfaces, &app.wm, point)
}

/// The surfaces the context classifier looks at — a bundle, not nine
/// parameters.
struct ContextSurfaces<'a> {
    taskbar: Rect,
    font: agony_core::config::FontMode,
    windows: &'a [TaskWindow],
    bar: Option<Rect>,
    menu_hit: Option<startmenu::MenuHit>,
    tiles: &'a [desktop_icons::Tile],
    desktop: Rect,
}

/// The pure classification behind `context_target` (fixtures over a
/// real `WmState` pin it; the loop feeds it the live parts). `menu_hit`
/// carries the open menu's hit under the pointer; the bare desktop is
/// first a tile question, then the desktop.
fn classify_context(
    surfaces: &ContextSurfaces<'_>,
    wm: &WmState,
    point: (u16, u16),
) -> Option<ContextTarget> {
    let ContextSurfaces {
        taskbar,
        font,
        windows,
        bar,
        menu_hit,
        tiles,
        desktop,
    } = *surfaces;
    if let Some(hit) = taskbar_hit(taskbar, font, windows, point) {
        return match hit {
            TaskbarHit::Start => Some(ContextTarget::TaskbarStart),
            TaskbarHit::Window(id) => Some(ContextTarget::TaskbarWindow(id)),
            TaskbarHit::Prompt => Some(ContextTarget::TaskbarPrompt),
            TaskbarHit::Other => None,
        };
    }
    if bar.is_some_and(|rect| point_in(point, rect)) {
        return Some(ContextTarget::Bar);
    }
    if let Some(hit) = menu_hit {
        return Some(ContextTarget::MenuItem { hit });
    }
    match hit_test(wm, point.0, point.1) {
        Hit::TitleBar { window } => Some(ContextTarget::TitleBar { window }),
        Hit::Button { window, button } => Some(ContextTarget::TitleButton { window, button }),
        Hit::Edge { window, edge } => Some(ContextTarget::WindowEdge { window, edge }),
        Hit::Interior { window } => Some(ContextTarget::WindowInterior { window }),
        Hit::None => {
            let tile = desktop_icons::tile_at(desktop, tiles, point);
            tile.map(ContextTarget::Tile)
                .or(Some(ContextTarget::Desktop))
        }
    }
}

/// A drag step: the core's state moves freely under the pointer; no pane
/// resize is issued until the drag ends.
fn handle_drag(app: &mut App, point: (u16, u16)) {
    let Some(drag) = app.drag else {
        return;
    };
    let (dx, dy) = (
        point.0 as i32 - drag.last().0 as i32,
        point.1 as i32 - drag.last().1 as i32,
    );
    match drag {
        Drag::Move { window, .. } => app.wm.move_by(window, dx, dy),
        Drag::Resize { window, edge, .. } => {
            let delta = match edge {
                Edge::Left | Edge::Right => dx,
                Edge::Top | Edge::Bottom => dy,
            };
            app.wm.resize_edge(window, edge, delta);
        }
    }
    app.drag = Some(drag.moved(point));
}

/// Drag end: the accumulated geometry reaches the panes — one resize
/// pass per drag (the coalescing rule).
async fn end_drag(app: &mut App) {
    if matches!(app.drag, Some(Drag::Resize { .. })) {
        app.panes.refresh_all().await;
        let errors = app.panes.resize_all(&app.wm).await;
        app.status = first_error(errors).or(app.status.take());
    }
    app.drag = None;
}

async fn forward_click(app: &mut App, window: u64, point: (u16, u16)) {
    let Some(record_window) = app.wm.window(window) else {
        return;
    };
    if record_window.pane_id.is_none() {
        return; // internal windows (the panel) take no pane clicks
    }
    let rect = layout::effective_rect(record_window, app.wm.desktop);
    let inner = layout::inner_rect(rect);
    let row = point.1.saturating_sub(inner.y);
    let col = point.0.saturating_sub(inner.x);
    let clicked = app.panes.click(window, row, col).await;
    if let Err(error) = clicked {
        app.status = Some(input_error_message(app, window, "click", error).await);
    }
}

impl Drag {
    fn last(&self) -> (u16, u16) {
        match *self {
            Drag::Move { last, .. } | Drag::Resize { last, .. } => last,
        }
    }

    fn moved(&self, point: (u16, u16)) -> Drag {
        match *self {
            Drag::Move { window, .. } => Drag::Move {
                window,
                last: point,
            },
            Drag::Resize { window, edge, .. } => Drag::Resize {
                window,
                edge,
                last: point,
            },
        }
    }
}

fn taskbar_windows(app: &App) -> Vec<TaskWindow> {
    app.wm
        .windows
        .iter()
        .map(|window| TaskWindow {
            id: window.id,
            title: window.title.clone(),
            active: app.wm.focus == Some(window.id) && window.state != layout::WinState::Minimized,
            minimized: window.state == layout::WinState::Minimized,
        })
        .collect()
}

fn draw_frame(frame: &mut ratatui::Frame<'_>, app: &mut App, clock: &str) {
    let status = effective_status(app);
    desktop::draw(frame, app.layout.desktop, &app.theme);
    // The icon grid paints on the bare desktop, under every window
    // (9.4.5 — z-order is the WM's; a maximized window simply covers it).
    desktop_icons::draw(frame, app.layout.desktop, &app.icons, &app.theme);
    let panel_shown = panel_visible(app);
    let panel_view = if panel_shown {
        app.panel.as_mut().map(|state| panel::View {
            state,
            picked: app.picked.as_ref(),
        })
    } else {
        None
    };
    let settings_view = match (app.settings_win, app.settings_state.as_mut()) {
        (Some(id), Some(state)) => Some((
            id,
            settings_ui::View {
                state,
                theme: &app.theme,
            },
        )),
        _ => None,
    };
    let mut view = wm::View {
        theme: &app.theme,
        font: app.font,
        hover: app.hover,
        panel: panel_view,
        settings: settings_view,
        welcome: app.welcome_win.map(|id| (id, &app.welcome_content)),
        directory: None,
    };
    if let (Some(id), Some(state)) = (app.directory_win, app.directory.as_mut()) {
        view.directory = Some((id, state));
    }
    wm::draw(frame, &app.wm, &app.panes, &mut view);
    let windows = taskbar_windows(app);
    let taskbar_view = taskbar::View {
        theme: &app.theme,
        font: app.font,
        clock,
        status: status.as_deref(),
        hint: HINT_LINE,
        menu_open: app.menu.is_some(),
        hover: app.hover,
        windows: &windows,
    };
    taskbar::draw(frame, app.layout.taskbar, &taskbar_view);
    if let Some(menu) = app.menu.as_mut() {
        let region = startmenu::rect_for(app.layout.overlay, menu, app.taskbar_pos);
        startmenu::draw(frame, region, menu, &app.theme, app.font, app.hover);
    }
    if let Some(bar) = app.bar.as_mut() {
        let list_rows = commandbar::visible_rows(bar);
        let region = commandbar::rect(app.layout.desktop, list_rows, app.taskbar_pos);
        commandbar::draw(frame, region, bar, &app.theme, app.font);
    }
    if let Some(state) = app.context.as_ref() {
        contextmenu::draw(frame, app.wm.desktop, &state.menu, &app.theme);
    }
}

fn panel_visible(app: &App) -> bool {
    app.panel_window.is_some_and(|id| {
        app.wm
            .window(id)
            .is_some_and(|w| w.state != layout::WinState::Minimized)
    })
}

/// The status the taskbar shows this tick: the live status while it is
/// fresh, None once the TTL passes (the hint returns). The change
/// detector re-arms the clock whenever the message changes, so writers
/// stay plain `app.status = Some(…)`.
fn effective_status(app: &mut App) -> Option<String> {
    status_for_bar(
        app.status.as_deref(),
        &mut app.shown_status,
        &mut app.status_at,
        Instant::now(),
    )
}

/// The TTL decision with the clock injected (tests pin the ageing).
fn status_for_bar(
    status: Option<&str>,
    shown: &mut Option<String>,
    armed: &mut Instant,
    now: Instant,
) -> Option<String> {
    if shown.as_deref() != status {
        *shown = status.map(str::to_string);
        *armed = now;
    }
    match status {
        Some(_) if now.duration_since(*armed) < STATUS_TTL => status.map(str::to_string),
        _ => None,
    }
}

/// The window keys, one line (Part04 4.5.1's documentation rule; ^K is
/// the command bar's, Part07).
const HINT_LINE: &str = "^E cycle · ^W close · ^M min · ^F max · ^K bar · ^Space menu";

impl App {
    /// The one toggle both entry points use: the start key and the start
    /// button. Each open rebuilds the entries from the discovery snapshot
    /// (the worker may have refreshed since the last open) and resolves
    /// the places once (9.2.4 — never per draw); closing refreshes the
    /// icon grid (9.4.6's cadence). The bar is modal over the menu —
    /// opening one closes the other.
    fn toggle_menu(&mut self) {
        self.bar = None;
        self.entries = menu_entries(&self.discovery);
        self.report_discovery_once();
        self.menu = match &self.menu {
            None => {
                let places = places::resolve();
                Some(startmenu::State::new(self.entries.clone(), places))
            }
            Some(_) => {
                self.icons.refresh();
                None
            }
        };
    }

    /// The first discovery problem (a failed refresh, a broken user
    /// manifest) shows once as a status line — never silently swallowed.
    fn report_discovery_once(&mut self) {
        if self.discovery_reported {
            return;
        }
        let messages = discovery_messages(&self.discovery);
        if messages.is_empty() {
            return;
        }
        self.discovery_reported = true;
        self.status = Some(messages.join(" | "));
    }
}

/// The snapshot under the shared discovery state, as menu entries.
fn menu_entries(discovery: &discover::Shared) -> Vec<startmenu::Entry> {
    let state = discover::lock(discovery);
    startmenu::entries_from(state.cache.as_ref())
}

/// The discovery's error and notes, if any.
fn discovery_messages(discovery: &discover::Shared) -> Vec<String> {
    let state = discover::lock(discovery);
    let mut messages = Vec::new();
    if let Some(error) = &state.error {
        messages.push(error.clone());
    }
    messages.extend(state.notes.iter().cloned());
    messages
}

fn terminal_width() -> u16 {
    terminal::size().map(|(cols, _)| cols).unwrap_or(120)
}

fn terminal_height() -> u16 {
    terminal::size().map(|(_, rows)| rows).unwrap_or(40)
}

fn poll_event(immediate: bool) -> io::Result<Option<Event>> {
    let timeout = if immediate {
        Duration::ZERO
    } else {
        Duration::from_millis(50)
    };
    if !event::poll(timeout)? {
        return Ok(None);
    }
    Ok(Some(event::read()?))
}

fn point_in(point: (u16, u16), rect: Rect) -> bool {
    let (x, y) = point;
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn clock_text() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

fn first_status(notes: &[String]) -> Option<String> {
    if notes.is_empty() {
        None
    } else {
        Some(notes.join(" | "))
    }
}

/// Restore the terminal exactly, however the loop ends (spike's guard).
struct TerminalRestore;

impl Drop for TerminalRestore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            cursor::Show
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agony_core::manifest::{AppManifest, Category, InstallSpec};
    use crossterm::event::KeyModifiers;

    fn entry(path: Option<&str>, candidate: bool) -> startmenu::Entry {
        let mut manifest = AppManifest::minimal("glow", "Glow", "glow", Category::Text);
        if path.is_none() && !candidate {
            manifest.install = Some(InstallSpec {
                winget: Some("charmbracelet.glow".into()),
                ..InstallSpec::default()
            });
        }
        startmenu::Entry {
            manifest,
            path: path.map(str::to_string),
            candidate,
        }
    }

    #[test]
    fn point_in_follows_rect_edges() {
        let rect = Rect::new(2, 3, 4, 1);
        assert!(point_in((2, 3), rect));
        assert!(point_in((5, 3), rect));
        assert!(!point_in((6, 3), rect));
        assert!(!point_in((2, 4), rect));
    }

    #[test]
    fn an_exited_window_tells_what_happened_and_the_way_out() {
        let message = exited_window_message("winget", "ctrl-w");
        assert!(
            message.contains("winget"),
            "the message names the window: {message:?}"
        );
        assert!(message.contains("exited"), "it says what happened");
        assert!(message.contains("ctrl-w"), "it names the close key");
    }

    #[test]
    fn cell_at_is_the_one_translation() {
        assert_eq!(
            cell_at(3, 4),
            (2, 4),
            "rows pass through (0-based), columns drop one (1-based on this host)"
        );
        assert_eq!(cell_at(0, 0), (0, 0), "saturates, never wraps");
    }

    #[test]
    fn launch_gate_refuses_greyed_entries_with_the_recipe() {
        let error = check_launchable(&entry(None, false)).expect_err("greyed refuses");
        assert_eq!(
            error,
            "Glow is not installed — winget install charmbracelet.glow"
        );
        check_launchable(&entry(Some("C:\\bin\\glow.exe"), false)).expect("installed launches");
        check_launchable(&entry(Some("C:\\bin\\git.exe"), true)).expect("candidates launch");
    }

    #[test]
    fn first_status_joins_notes_or_hides() {
        assert_eq!(first_status(&[]), None);
        assert_eq!(
            first_status(&["a".into(), "b".into()]),
            Some("a | b".to_string())
        );
    }

    #[test]
    fn a_status_ages_out_and_a_change_re_arms_the_clock() {
        let mut shown = None;
        let mut armed = Instant::now();
        let now = Instant::now();
        assert_eq!(
            status_for_bar(Some("hello"), &mut shown, &mut armed, now),
            Some("hello".into()),
            "a fresh status shows"
        );
        let stale = now + STATUS_TTL + Duration::from_secs(1);
        assert_eq!(
            status_for_bar(Some("hello"), &mut shown, &mut armed, stale),
            None,
            "the TTL returns the hint (None status)"
        );
        // A different message re-arms: it shows again even when late.
        assert_eq!(
            status_for_bar(Some("next"), &mut shown, &mut armed, stale),
            Some("next".into())
        );
        assert_eq!(
            status_for_bar(None, &mut shown, &mut armed, stale),
            None,
            "an empty status is never shown"
        );
    }

    #[test]
    fn unused_roles_keep_the_theme_applier_authoritative() {
        // The applier (not a literal) decides every color the loop draws.
        let (applier, _) = Applier::load("paper", None);
        assert_eq!(applier.name(), "paper");
        assert!(applier
            .style(crate::theme::Role::Text, crate::theme::Role::Background)
            .fg
            .is_some());
    }

    // ------------------------------------------------- translation (6.2.4)

    fn mouse(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 5,
            row: 7,
            modifiers: KeyModifiers::empty(),
        }
    }

    #[test]
    fn translation_applies_cell_at_exactly_once() {
        let event = translate_mouse(
            mouse(MouseEventKind::Down(MouseButton::Left)),
            cell_at(5, 7),
        )
        .expect("translated");
        assert_eq!(
            (event.col, event.row),
            (4, 7),
            "the column drops one, the row passes through"
        );
        assert_eq!(event.kind, PointerKind::Down);
        assert_eq!(event.button, PointerButton::Left);
    }

    #[test]
    fn translation_fixes_the_wheel_signs_and_drops_horizontal() {
        let up = translate_mouse(mouse(MouseEventKind::ScrollUp), (0, 0)).expect("up");
        assert_eq!(
            up.kind,
            PointerKind::Wheel {
                rows: -pointer::WHEEL_ROWS_PER_NOTCH
            }
        );
        let down = translate_mouse(mouse(MouseEventKind::ScrollDown), (0, 0)).expect("down");
        assert_eq!(
            down.kind,
            PointerKind::Wheel {
                rows: pointer::WHEEL_ROWS_PER_NOTCH
            }
        );
        assert!(translate_mouse(mouse(MouseEventKind::ScrollLeft), (0, 0)).is_none());
        assert!(translate_mouse(mouse(MouseEventKind::ScrollRight), (0, 0)).is_none());
    }

    #[test]
    fn translation_covers_move() {
        let moved = translate_mouse(mouse(MouseEventKind::Moved), (1, 2)).expect("moved");
        assert_eq!(moved.kind, PointerKind::Move);
    }

    // --------------------------------------------------- taskbar routing

    fn task_windows() -> Vec<TaskWindow> {
        vec![TaskWindow {
            id: 1,
            title: "shell".into(),
            active: true,
            minimized: false,
        }]
    }

    #[test]
    fn taskbar_hit_classifies_start_windows_prompt_and_other() {
        let region = Rect::new(0, 24, 80, 1);
        let font = agony_core::config::FontMode::Ascii;
        let windows = task_windows();
        let start = taskbar::start_button_rect(region, font);
        assert_eq!(
            taskbar_hit(region, font, &windows, (start.x, region.y)),
            Some(TaskbarHit::Start)
        );
        let rects = taskbar::window_button_rects(region, font, &windows);
        assert_eq!(
            taskbar_hit(region, font, &windows, (rects[0].1.x, region.y)),
            Some(TaskbarHit::Window(1))
        );
        let prompt = taskbar::prompt_rect(region, font, &windows);
        assert_eq!(
            taskbar_hit(region, font, &windows, (prompt.x, region.y)),
            Some(TaskbarHit::Prompt)
        );
        assert_eq!(
            taskbar_hit(region, font, &windows, (region.right() - 1, region.y)),
            Some(TaskbarHit::Other),
            "the clock's cells are the taskbar's dead space"
        );
        assert_eq!(taskbar_hit(region, font, &windows, (40, 10)), None);
    }

    #[test]
    fn a_click_pair_on_a_focused_button_never_stays_minimized() {
        let mut wm = WmState::new(Rect::new(0, 0, 80, 24));
        let id = wm.open("shell", Some(0));
        // First click: the Windows rule minimizes the focused entry...
        apply_taskbar_click(&mut wm, id);
        assert_eq!(wm.window(id).expect("w").state, layout::WinState::Minimized);
        // ...and the pair's collapse rule restores it, focused.
        apply_taskbar_double_click(&mut wm, id);
        assert_eq!(wm.window(id).expect("w").state, layout::WinState::Normal);
        assert_eq!(wm.focus, Some(id));
    }

    #[test]
    fn a_click_pair_on_an_unfocused_button_just_focuses() {
        let mut wm = WmState::new(Rect::new(0, 0, 80, 24));
        let first = wm.open("a", None);
        let second = wm.open("b", None);
        apply_taskbar_double_click(&mut wm, first);
        assert_eq!(wm.focus, Some(first));
        assert_ne!(wm.focus, Some(second));
        assert_eq!(wm.window(first).expect("w").state, layout::WinState::Normal);
    }

    // ------------------------------------------------------ wheel routing

    fn wm_with_panel() -> WmState {
        let mut wm = WmState::new(Rect::new(0, 0, 80, 24));
        wm.open_at("files", None, Rect::new(0, 0, 20, 24));
        wm.focus(wm.windows[0].id);
        wm
    }

    #[test]
    fn wheel_routes_overlay_first_then_the_panel() {
        let wm = wm_with_panel();
        let panel_id = wm.windows[0].id;
        let menu = Some(Rect::new(0, 6, 48, 18));
        // The bar wins over everything under it.
        assert_eq!(
            wheel_target(
                Some(Rect::new(0, 16, 80, 7)),
                menu,
                None,
                None,
                Some(panel_id),
                &wm,
                (10, 20)
            ),
            WheelTarget::Bar
        );
        // The menu wins where the bar is not.
        assert_eq!(
            wheel_target(None, menu, None, None, Some(panel_id), &wm, (10, 10)),
            WheelTarget::Menu
        );
        // The panel's interior takes the wheel (menu closed).
        assert_eq!(
            wheel_target(None, None, None, None, Some(panel_id), &wm, (10, 10)),
            WheelTarget::Panel(panel_id)
        );
        // The settings window's interior takes the wheel over the panel's.
        assert_eq!(
            wheel_target(
                None,
                None,
                Some(panel_id),
                None,
                Some(panel_id),
                &wm,
                (10, 10)
            ),
            WheelTarget::Settings(panel_id)
        );
        // A pane interior, the taskbar and the desktop take none.
        assert_eq!(
            wheel_target(None, None, None, None, Some(panel_id), &wm, (40, 12)),
            WheelTarget::None,
            "no wheel action on the bare desktop"
        );
        assert_eq!(
            wheel_target(None, None, None, None, Some(panel_id), &wm, (40, 24)),
            WheelTarget::None,
            "no wheel action on the taskbar"
        );
    }

    // ----------------------------------------------------- context seam

    /// The classifier's fixtures: bare surfaces, no bar, no open menu.
    fn plain_surfaces<'a>(
        taskbar: Rect,
        font: agony_core::config::FontMode,
        windows: &'a [TaskWindow],
        tiles: &'a [desktop_icons::Tile],
        desktop: Rect,
    ) -> ContextSurfaces<'a> {
        ContextSurfaces {
            taskbar,
            font,
            windows,
            bar: None,
            menu_hit: None,
            tiles,
            desktop,
        }
    }

    #[test]
    fn context_target_classifies_every_surface() {
        let mut wm = WmState::new(Rect::new(0, 0, 80, 23));
        let panel = wm.open_at("files", None, Rect::new(0, 0, 20, 23));
        let window = wm.open_at("shell", Some(0), Rect::new(30, 5, 30, 10));
        let font = agony_core::config::FontMode::Ascii;
        let taskbar = Rect::new(0, 23, 80, 1);
        let windows = vec![
            TaskWindow {
                id: panel,
                title: "files".into(),
                active: false,
                minimized: false,
            },
            TaskWindow {
                id: window,
                title: "shell".into(),
                active: true,
                minimized: false,
            },
        ];
        let desktop = Rect::new(0, 0, 80, 23);
        let no_tiles: Vec<desktop_icons::Tile> = Vec::new();
        let classify = |tiles: &[desktop_icons::Tile], point: (u16, u16)| {
            classify_context(
                &plain_surfaces(taskbar, font, &windows, tiles, desktop),
                &wm,
                point,
            )
        };
        let start = taskbar::start_button_rect(taskbar, font);
        assert_eq!(
            classify(&no_tiles, (start.x, 23)),
            Some(ContextTarget::TaskbarStart)
        );
        let rects = taskbar::window_button_rects(taskbar, font, &windows);
        assert_eq!(
            classify(&no_tiles, (rects[1].1.x, 23)),
            Some(ContextTarget::TaskbarWindow(window))
        );
        let prompt = taskbar::prompt_rect(taskbar, font, &windows);
        assert_eq!(
            classify(&no_tiles, (prompt.x, 23)),
            Some(ContextTarget::TaskbarPrompt)
        );
        let with_bar = ContextSurfaces {
            bar: Some(Rect::new(0, 15, 80, 8)),
            ..plain_surfaces(taskbar, font, &windows, &no_tiles, desktop)
        };
        assert_eq!(
            classify_context(&with_bar, &wm, (40, 20)),
            Some(ContextTarget::Bar)
        );
        assert_eq!(
            classify(&no_tiles, (31, 5)),
            Some(ContextTarget::TitleBar { window })
        );
        assert_eq!(
            classify(&no_tiles, (35, 8)),
            Some(ContextTarget::WindowInterior { window })
        );
        assert_eq!(
            classify(&no_tiles, (30, 8)),
            Some(ContextTarget::WindowEdge {
                window,
                edge: Edge::Left
            })
        );
        assert_eq!(
            classify(&no_tiles, (10, 10)),
            Some(ContextTarget::WindowInterior { window: panel })
        );
        assert_eq!(classify(&no_tiles, (25, 20)), Some(ContextTarget::Desktop));
        // A tile under the bare desktop becomes the Tile target (9.3.3);
        // tiles never win under a window (9.4.5), so the point sits in
        // the bare strip right of the docked panel. (25, 18) is grid
        // column 5, row 4 — column-major index 4 + 5*5 = 29.
        let tile = |n: usize| desktop_icons::Tile {
            name: format!("t{n}"),
            path: std::path::PathBuf::new(),
            kind: desktop_icons::Kind::Document,
        };
        let tiles: Vec<desktop_icons::Tile> = (0..30).map(tile).collect();
        assert_eq!(classify(&tiles, (25, 18)), Some(ContextTarget::Tile(29)));
    }
}
