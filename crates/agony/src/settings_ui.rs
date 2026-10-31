//! The settings window (Part08 §8.2): an internal WM window with a
//! section list (Appearance, Behavior, Files, Keys) and hand-laid rows
//! per section — ratatui 0.29's `Table` cells hold text only, so every
//! row is its own line widget. Every edit is a preview until committed:
//! theme and role choices re-preview the whole desktop through
//! `Event::Preview`, Apply persists (`Event::Apply`), Esc/Cancel reverts.
//! `open_at(group)` is the one programmatic entry — Part09's SETTINGS
//! section deep-links through it.
//!
//! Items: `State` — the draft (`config::Settings`), the section, the row
//! cursor, the theme list, the draft override deltas (`extends`-keyed),
//! the open color picker, the key-capture mode, the in-window note;
//! `Section` — the four groups (the SETTINGS menu's targets);
//! `Capture` — which reserved key is being re-bound; `Event` — what a
//! key or click asked the app to do; `Applied` — Apply's payload;
//! `Row` — one hand-laid row.
//! Core functions: `State::open` — draft from the live settings;
//! `open_at` — the deep-link seam; `handle_key` — the pure routing;
//! `activate_row` / `adjust_row` — Enter vs Left/Right on a row;
//! `commit_color` — the picker's commit: contrast on the effective
//! palette, refusal names the pair; `rows` / `row_at` — the display list
//! and the click lookup (one layout with `render`);
//! `preview_event` — the effective palette behind the current draft.
//! Depends on: agony_core::config + thememodel (override math,
//! contrast) + inputmap (combo spelling), crate::theme (base themes),
//! crate::picker, crate::settings (paths), crate::input (combo capture).
//!
//! Called by: app (lifecycle, keys, pointer, wheel, draw) and wm (render).
//! Calls: std::fs only for the custom theme dir listing.

use std::path::Path;

use agony_core::config::{FontMode, Settings, TaskbarPosition};
use agony_core::inputmap;
use agony_core::layout::Rect;
use agony_core::thememodel::{self, ColorValue, Palette, Role, ALL_ROLES};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::picker;
use crate::settings;
use crate::theme::{self, Applier};

/// The four settings groups, in display order — the config model's own
/// sections and the SETTINGS menu's deep-link targets (Part09 9.2.6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Appearance,
    Behavior,
    Files,
    Keys,
}

pub const ALL_SECTIONS: [Section; 4] = [
    Section::Appearance,
    Section::Behavior,
    Section::Files,
    Section::Keys,
];

impl Section {
    pub fn name(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Behavior => "Behavior",
            Section::Files => "Files",
            Section::Keys => "Keys",
        }
    }

    /// The one-line summary the SETTINGS menu shows under each group.
    pub fn summary(self) -> &'static str {
        match self {
            Section::Appearance => "themes, color roles, live preview",
            Section::Behavior => "suggestions, font mode, taskbar position",
            Section::Files => "the file panel's walk toggles",
            Section::Keys => "the reserved key combos",
        }
    }
}

/// Which reserved key capture mode is re-binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Capture {
    Quit,
    StartMenu,
    CycleFocus,
    CloseWindow,
    Minimize,
    Maximize,
    Bar,
}

impl Capture {
    fn all() -> [Capture; 7] {
        [
            Capture::Quit,
            Capture::StartMenu,
            Capture::CycleFocus,
            Capture::CloseWindow,
            Capture::Minimize,
            Capture::Maximize,
            Capture::Bar,
        ]
    }

    fn label(self) -> &'static str {
        match self {
            Capture::Quit => "quit",
            Capture::StartMenu => "start menu",
            Capture::CycleFocus => "cycle focus",
            Capture::CloseWindow => "close window",
            Capture::Minimize => "minimize",
            Capture::Maximize => "maximize",
            Capture::Bar => "command bar",
        }
    }

    /// Read/write the spec in a settings draft.
    fn spec_of(self, keys: &agony_core::config::ReservedKeys) -> String {
        match self {
            Capture::Quit => keys.quit.clone(),
            Capture::StartMenu => keys.start_menu.clone(),
            Capture::CycleFocus => keys.cycle_focus.clone(),
            Capture::CloseWindow => keys.close_window.clone(),
            Capture::Minimize => keys.minimize.clone(),
            Capture::Maximize => keys.maximize.clone(),
            Capture::Bar => keys.bar.clone(),
        }
    }

    fn set_spec(self, keys: &mut agony_core::config::ReservedKeys, spec: String) {
        match self {
            Capture::Quit => keys.quit = spec,
            Capture::StartMenu => keys.start_menu = spec,
            Capture::CycleFocus => keys.cycle_focus = spec,
            Capture::CloseWindow => keys.close_window = spec,
            Capture::Minimize => keys.minimize = spec,
            Capture::Maximize => keys.maximize = spec,
            Capture::Bar => keys.bar = spec,
        }
    }
}

/// What a key press or click asked the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Consumed,
    /// Cancel: the app reverts the preview and closes the window.
    Close,
    /// The draft's palette changed — the app swaps its applier.
    Preview {
        theme: String,
        palette: Palette,
    },
    /// Commit: the app writes settings.toml (+ the override pack).
    Apply(Box<Applied>),
}

/// Apply's payload: the whole draft plus the override deltas to persist.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    pub settings: Settings,
    pub overrides_extends: Option<String>,
    pub overrides: Vec<(Role, ColorValue)>,
}

/// One hand-laid row (Part08 8.2.1: text-only cells, each row its own
/// widget line).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Header(&'static str),
    Theme(usize),
    Role(usize),
    Suggestions,
    Font,
    Taskbar,
    ShowHidden,
    RespectIgnore,
    Depth,
    Entries,
    Key(Capture),
    Apply,
    Cancel,
}

/// Which half of the window holds the keyboard (the UI review's
/// dead-end finding: no key returned from the rows to the section
/// list — Tab is that key now).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Sections,
    Rows,
}

#[derive(Clone, Debug)]
pub struct State {
    pub section: Section,
    /// Which half holds the keyboard: the section list or the rows.
    pub focus: Focus,
    /// The section list's cursor (moves when Sections holds the keys).
    pub section_cursor: usize,
    pub selected: usize,
    pub scroll: usize,
    /// The visible row count, updated at draw.
    pub window: usize,
    /// The pending edits — nothing here is live until Apply.
    pub draft: Settings,
    /// Built-ins plus `<config>/agony/themes/*.json`, resolved at open.
    pub themes: Vec<String>,
    /// The draft override deltas (role-level only) and the theme they
    /// extend. Dormant while another theme is drafted; written on Apply.
    pub overrides: Vec<(Role, ColorValue)>,
    pub overrides_extends: Option<String>,
    pub picker: Option<picker::State>,
    /// Active while a reserved key is being re-bound.
    pub capture: Option<Capture>,
    /// The in-window refusal/note line.
    pub note: Option<String>,
}

impl State {
    /// The draft starts from the live settings; `theme` marks the current
    /// one in the Appearance list.
    pub fn open(live: &Settings) -> State {
        State {
            section: Section::Appearance,
            focus: Focus::Rows,
            section_cursor: 0,
            selected: 0,
            scroll: 0,
            window: 10,
            draft: live.clone(),
            themes: installed_themes(Some(Path::new(&settings::custom_theme_dir()))),
            overrides: Vec::new(),
            overrides_extends: None,
            picker: None,
            capture: None,
            note: None,
        }
    }

    pub fn rows(&self) -> Vec<Row> {
        match self.section {
            Section::Appearance => appearance_rows(self.themes.len()),
            Section::Behavior => vec![
                Row::Header("behavior"),
                Row::Suggestions,
                Row::Font,
                Row::Taskbar,
                Row::Apply,
                Row::Cancel,
            ],
            Section::Files => vec![
                Row::Header("files"),
                Row::ShowHidden,
                Row::RespectIgnore,
                Row::Depth,
                Row::Entries,
                Row::Apply,
                Row::Cancel,
            ],
            Section::Keys => {
                let mut rows = vec![Row::Header("reserved keys")];
                rows.extend(Capture::all().into_iter().map(Row::Key));
                rows.push(Row::Apply);
                rows.push(Row::Cancel);
                rows
            }
        }
    }

    fn selectable(&self) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|row| !matches!(row, Row::Header(_)))
            .collect()
    }
}

/// The window's default rect: roughly two thirds of the desktop, centered.
pub fn rect(desktop: Rect) -> Rect {
    let width = desktop.width.clamp(30, 64);
    let height = desktop.height.clamp(8, 22);
    let x = desktop.x + desktop.width.saturating_sub(width) / 2;
    let y = desktop.y + desktop.height.saturating_sub(height) / 2;
    Rect::new(x, y, width, height)
}

/// The Appearance display: the theme list, then the ten color roles,
/// then the two action rows.
fn appearance_rows(theme_count: usize) -> Vec<Row> {
    let mut rows = vec![Row::Header("themes")];
    rows.extend((0..theme_count).map(Row::Theme));
    rows.push(Row::Header("color roles"));
    rows.extend((0..ALL_ROLES.len()).map(Row::Role));
    rows.push(Row::Apply);
    rows.push(Row::Cancel);
    rows
}

/// The deep-link seam (Part08 8.2.4): the one programmatic entry — focus
/// the window on a named section. An already-open window re-targets.
pub fn open_at(state: &mut State, section: Section) {
    state.section = section;
    state.section_cursor = ALL_SECTIONS
        .iter()
        .position(|candidate| candidate == &section)
        .unwrap_or(0);
    state.selected = 0;
    state.scroll = 0;
    state.note = None;
}

/// The installed theme names: the built-ins plus `<dir>/*.json` (sorted,
/// deduplicated), the base for the Appearance list.
pub fn installed_themes(custom_dir: Option<&Path>) -> Vec<String> {
    let mut themes: Vec<String> = thememodel::BUILTIN_NAMES
        .iter()
        .map(|name| name.to_string())
        .collect();
    let Some(dir) = custom_dir else {
        return themes;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return themes;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_json = path.extension().map(|ext| ext == "json").unwrap_or(false);
        let is_override = path
            .file_name()
            .map(|name| name == theme::OVERRIDE_FILE)
            .unwrap_or(false);
        if !is_json || is_override {
            continue;
        }
        if let Some(stem) = path.file_stem() {
            themes.push(stem.to_string_lossy().into_owned());
        }
    }
    themes.sort();
    themes.dedup();
    themes
}

/// Route one key press. The picker, when open, is the modal's modal.
pub fn handle_key(state: &mut State, key: KeyEvent) -> Event {
    if state.picker.is_some() {
        let event = picker::handle_key(state.picker.as_mut().expect("checked"), key);
        return picker_event(state, event);
    }
    if let Some(capture) = state.capture {
        return capture_key(state, capture, key);
    }
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Esc => Event::Close,
        KeyCode::Tab if plain => {
            state.focus = match state.focus {
                Focus::Sections => Focus::Rows,
                Focus::Rows => Focus::Sections,
            };
            Event::Consumed
        }
        KeyCode::Enter if plain && state.focus == Focus::Sections => {
            state.focus = Focus::Rows;
            Event::Consumed
        }
        KeyCode::Up if plain && state.focus == Focus::Sections => {
            move_section(state, -1);
            Event::Consumed
        }
        KeyCode::Down if plain && state.focus == Focus::Sections => {
            move_section(state, 1);
            Event::Consumed
        }
        KeyCode::Enter if plain => activate_row(state),
        KeyCode::Left if plain && state.focus == Focus::Rows => adjust_row(state, -1),
        KeyCode::Right if plain && state.focus == Focus::Rows => adjust_row(state, 1),
        KeyCode::Up if plain => {
            move_selection(state, -1);
            Event::Consumed
        }
        KeyCode::Down if plain => {
            move_selection(state, 1);
            Event::Consumed
        }
        KeyCode::PageUp if plain => {
            scroll_by(state, -(state.window.max(1) as i32), state.window);
            Event::Consumed
        }
        KeyCode::PageDown if plain => {
            scroll_by(state, state.window.max(1) as i32, state.window);
            Event::Consumed
        }
        _ => Event::Consumed,
    }
}

/// The section list's keyboard walk: the cursor moves and the section
/// switches with it (open_at resets the row cursor — a real two-pane
/// settings window's behavior).
fn move_section(state: &mut State, delta: i32) {
    let last = ALL_SECTIONS.len().saturating_sub(1) as i32;
    let moved = (state.section_cursor as i32 + delta).clamp(0, last) as usize;
    state.section_cursor = moved;
    if let Some(section) = ALL_SECTIONS.get(moved) {
        open_at(state, *section);
    }
}

fn move_selection(state: &mut State, delta: i32) {
    let last = state.selectable().len().saturating_sub(1) as i32;
    state.selected = (state.selected as i32 + delta).clamp(0, last) as usize;
    ensure_visible(state);
}

/// The wheel's offset (the caller supplies the visible row count — the
/// section pane's height, the same number `render` stores).
pub fn scroll_by(state: &mut State, rows: i32, window: usize) {
    state.window = window;
    state.scroll = if rows >= 0 {
        state.scroll.saturating_add(rows as usize)
    } else {
        state.scroll.saturating_sub(rows.unsigned_abs() as usize)
    };
    clamp_scroll(state);
    ensure_visible(state);
}

fn clamp_scroll(state: &mut State) {
    let rows_len = state.rows().len();
    state.scroll = state
        .scroll
        .min(rows_len.saturating_sub(state.window.max(1)));
}

/// Keep the selection inside the scroll window (pull the offset, never
/// the selection).
fn ensure_visible(state: &mut State) {
    let rows = state.rows();
    let selected_row = state
        .selectable()
        .get(state.selected)
        .and_then(|row| rows.iter().position(|display| display == row));
    let Some(at) = selected_row else {
        return;
    };
    if at < state.scroll {
        state.scroll = at;
    }
    let bottom = state.scroll + state.window.max(1);
    if at >= bottom {
        state.scroll = at + 1 - state.window.max(1);
    }
    clamp_scroll(state);
}

/// Enter on a row: the row's own action. Theme rows preview; role rows
/// open the picker; toggles flip; enums cycle; Apply/Cancel act.
pub fn activate_row(state: &mut State) -> Event {
    let Some(row) = state.selectable().get(state.selected).copied() else {
        return Event::Consumed;
    };
    match row {
        Row::Theme(index) => {
            let Some(name) = state.themes.get(index) else {
                return Event::Consumed;
            };
            state.draft.theme = name.clone();
            state.note = Some(format!("previewing {name} — Apply persists it"));
            preview_event(state).unwrap_or(Event::Consumed)
        }
        Row::Role(index) => {
            let Some(role) = ALL_ROLES.get(index) else {
                return Event::Consumed;
            };
            open_picker(state, *role);
            Event::Consumed
        }
        Row::Suggestions => {
            state.draft.suggestions_enabled = !state.draft.suggestions_enabled;
            state.note = Some("applies on Apply".into());
            Event::Consumed
        }
        Row::Font => {
            state.draft.font_mode = cycle_font(state.draft.font_mode, 1);
            state.note = Some("applies on Apply".into());
            Event::Consumed
        }
        Row::Taskbar => {
            state.draft.taskbar_position = cycle_position(state.draft.taskbar_position, 1);
            state.note = Some("applies on Apply".into());
            Event::Consumed
        }
        Row::ShowHidden => {
            state.draft.files.show_hidden = !state.draft.files.show_hidden;
            state.note = Some("applies on Apply".into());
            Event::Consumed
        }
        Row::RespectIgnore => {
            state.draft.files.respect_ignore = !state.draft.files.respect_ignore;
            state.note = Some("applies on Apply".into());
            Event::Consumed
        }
        Row::Depth => {
            adjust_number(&mut state.draft.files.max_depth, 1, 1, 64);
            state.note = Some("applies on Apply".into());
            Event::Consumed
        }
        Row::Entries => {
            adjust_number(&mut state.draft.files.max_entries, 100, 100, 100_000);
            state.note = Some("applies on Apply".into());
            Event::Consumed
        }
        Row::Key(action) => {
            state.capture = Some(action);
            state.note = Some(format!("press the new key for '{}'", action.label()));
            Event::Consumed
        }
        Row::Apply => apply_event(state),
        Row::Cancel => Event::Close,
        Row::Header(_) => Event::Consumed,
    }
}

/// Left/Right on a row: the keyboard path for enums and numbers.
pub fn adjust_row(state: &mut State, direction: i32) -> Event {
    let Some(row) = state.selectable().get(state.selected).copied() else {
        return Event::Consumed;
    };
    match row {
        Row::Font => state.draft.font_mode = cycle_font(state.draft.font_mode, direction),
        Row::Taskbar => {
            state.draft.taskbar_position = cycle_position(state.draft.taskbar_position, direction)
        }
        Row::Depth => adjust_number(&mut state.draft.files.max_depth, direction, 1, 64),
        Row::Entries => adjust_number(&mut state.draft.files.max_entries, direction, 100, 100_000),
        _ => return Event::Consumed,
    }
    state.note = Some("applies on Apply".into());
    Event::Consumed
}

fn cycle_font(mode: FontMode, direction: i32) -> FontMode {
    let all = [FontMode::Auto, FontMode::Nerd, FontMode::Ascii];
    let at = all
        .iter()
        .position(|candidate| *candidate == mode)
        .unwrap_or(0);
    cycle_index(at, direction, &all)
}

fn cycle_position(position: TaskbarPosition, direction: i32) -> TaskbarPosition {
    let all = [TaskbarPosition::Bottom, TaskbarPosition::Top];
    let at = all
        .iter()
        .position(|candidate| *candidate == position)
        .unwrap_or(0);
    cycle_index(at, direction, &all)
}

fn cycle_index<T: Copy>(at: usize, direction: i32, all: &[T]) -> T {
    let last = all.len() as i32;
    let moved = (at as i32 + direction).rem_euclid(last) as usize;
    all[moved]
}

fn adjust_number(value: &mut usize, direction: i32, low: usize, high: usize) {
    let moved = *value as i64 + i64::from(direction);
    *value = moved.clamp(low as i64, high as i64) as usize;
}

/// The key capture: the next press becomes the binding (Esc cancels).
/// A combo that collides with another reserved spec is refused with the
/// conflicting action named.
pub fn capture_key(state: &mut State, action: Capture, key: KeyEvent) -> Event {
    state.capture = None;
    if key.code == KeyCode::Esc {
        state.note = Some("key binding kept".into());
        return Event::Consumed;
    }
    let Some(combo) = crate::input::combo_of(key) else {
        state.note = Some("that key has no name — try again".into());
        state.capture = Some(action);
        return Event::Consumed;
    };
    let spec = inputmap::combo_to_str(combo);
    if let Some(other) = conflicting_action(state, &spec, action) {
        state.note = Some(format!(
            "{spec} already runs '{}' — choose another",
            other.label()
        ));
        state.capture = Some(action);
        return Event::Consumed;
    }
    action.set_spec(&mut state.draft.reserved_keys, spec.clone());
    state.note = Some(format!(
        "'{}' is now {spec} — Apply persists it",
        action.label()
    ));
    Event::Consumed
}

/// Another reserved action whose draft spec already spells `spec`.
fn conflicting_action(state: &State, spec: &str, action: Capture) -> Option<Capture> {
    Capture::all()
        .into_iter()
        .find(|other| *other != action && other.spec_of(&state.draft.reserved_keys) == spec)
}

fn open_picker(state: &mut State, role: Role) {
    let current = current_color(state, role);
    state.picker = Some(picker::State::open(role, current));
    state.note = None;
}

/// The role's effective color under the current draft (base theme plus
/// the draft deltas that extend it).
fn current_color(state: &State, role: Role) -> ColorValue {
    preview_palette(state)
        .map(|(_, palette)| palette.get(role))
        .unwrap_or(ColorValue::Indexed(0))
}

/// The effective palette behind the draft: the drafted theme's base plus
/// the override deltas when they extend it. None when the theme cannot
/// be resolved (a deleted custom pack).
fn preview_palette(state: &State) -> Option<(String, Palette)> {
    let base = theme::base_theme(
        &state.draft.theme,
        Some(Path::new(&settings::custom_theme_dir())),
    )?;
    let overrides = match state.overrides_extends.as_deref() {
        Some(extends) if extends == state.draft.theme => state.overrides.as_slice(),
        _ => &[],
    };
    Some((
        state.draft.theme.clone(),
        thememodel::effective_palette(&base.palette, overrides),
    ))
}

/// The preview event the app acts on after a theme or color change.
fn preview_event(state: &State) -> Option<Event> {
    let (theme, palette) = preview_palette(state)?;
    Some(Event::Preview { theme, palette })
}

/// The picker's answer routed: previews flow through, commits run the
/// contrast check on the effective palette and join the draft deltas.
fn picker_event(state: &mut State, event: picker::Event) -> Event {
    match event {
        picker::Event::Consumed => Event::Consumed,
        picker::Event::Cancel => {
            state.picker = None;
            Event::Consumed
        }
        picker::Event::Preview(value) => {
            if let Some(picker_state) = state.picker.as_mut() {
                picker_state.current = value;
            }
            preview_event(state).unwrap_or(Event::Consumed)
        }
        picker::Event::Commit(value) => commit_color(state, value),
    }
}

/// Commit one color (8.3.2–8.3.3): the deltas stay role-level, extend
/// the drafted theme, and the contrast check runs on the effective
/// palette — a failing pair is refused with the pair named, the picker
/// stays open.
fn commit_color(state: &mut State, value: ColorValue) -> Event {
    let Some(role) = state.picker.as_ref().map(|picker_state| picker_state.role) else {
        return Event::Consumed;
    };
    let mut candidate = match state.overrides_extends.as_deref() {
        Some(extends) if extends == state.draft.theme => state.overrides.clone(),
        _ => Vec::new(),
    };
    replace_or_push(&mut candidate, role, value);
    let Some(effective) = effective_for(state, &candidate) else {
        state.note = Some("theme not found — commit refused".into());
        return Event::Consumed;
    };
    if let Err(error) = thememodel::validate(&effective) {
        if let Some(picker_state) = state.picker.as_mut() {
            picker_state.error = Some(error.to_string());
        }
        return Event::Consumed;
    }
    state.overrides = candidate;
    state.overrides_extends = Some(state.draft.theme.clone());
    state.picker = None;
    state.note = Some(format!(
        "{} committed — Apply writes the override pack",
        thememodel::role_name(role)
    ));
    preview_event(state).unwrap_or(Event::Consumed)
}

fn replace_or_push(pairs: &mut Vec<(Role, ColorValue)>, role: Role, value: ColorValue) {
    if let Some(slot) = pairs.iter_mut().find(|(existing, _)| *existing == role) {
        slot.1 = value;
        return;
    }
    pairs.push((role, value));
}

fn effective_for(state: &State, overrides: &[(Role, ColorValue)]) -> Option<Palette> {
    let base = theme::base_theme(&state.draft.theme, None)?;
    Some(thememodel::effective_palette(&base.palette, overrides))
}

/// Apply's payload: the draft plus the deltas (8.2's "no setting takes
/// effect through a path that bypasses the file" — the app writes both
/// files and only then activates).
fn apply_event(state: &State) -> Event {
    Event::Apply(Box::new(Applied {
        settings: state.draft.clone(),
        overrides_extends: state.overrides_extends.clone(),
        overrides: state.overrides.clone(),
    }))
}

/// A click's row inside the window's inner area — the same layout
/// `render` draws (8.2.5: the section list navigates by click).
pub fn row_at(state: &State, inner: Rect, point: (u16, u16)) -> Option<usize> {
    let (col, row) = point;
    if col < inner.x || col >= inner.x + inner.width || row < inner.y {
        return None;
    }
    let at = (row - inner.y) as usize + state.scroll;
    state.rows().get(at).map(|_| at)
}

/// The pane layout: section column left, section pane right, note line
/// at the bottom. `draw` and `row_at` derive from this one split.
pub fn panes(inner: Rect) -> (Rect, Rect, Rect) {
    let section_w = 14.min(inner.width);
    let note_h = 1.min(inner.height.saturating_sub(1));
    let body_h = inner.height - note_h;
    let list = Rect::new(inner.x, inner.y, section_w, body_h);
    let pane = Rect::new(
        inner.x + section_w,
        inner.y,
        inner.width.saturating_sub(section_w),
        body_h,
    );
    let note = Rect::new(inner.x, inner.y + body_h, inner.width, note_h);
    (list, pane, note)
}

/// A click inside the section pane: select the row and activate it
/// (clicks preview, Apply commits — the pointer included). A click in
/// the section list switches sections. Picker clicks route to the
/// picker.
pub fn click(state: &mut State, inner: Rect, pane: Rect, point: (u16, u16)) -> Event {
    if state.picker.is_some() {
        return picker_click(state, pane, point);
    }
    let (list, section_pane, _) = panes(inner);
    if point_in(point, section_pane) {
        return click_row(state, section_pane, point);
    }
    if point_in(point, list) {
        let index = (point.1 - list.y) as usize;
        if let Some(section) = ALL_SECTIONS.get(index) {
            open_at(state, *section);
            state.focus = Focus::Sections;
            state.section_cursor = index;
        }
    }
    Event::Consumed
}

/// Picker clicks: a cell selects and previews, the hex line focuses.
fn picker_click(state: &mut State, pane: Rect, point: (u16, u16)) -> Event {
    let Some(hit) = picker::click_at(pane, point) else {
        return Event::Consumed;
    };
    match hit {
        picker::Click::Cell(index) => {
            if let Some(picker_state) = state.picker.as_mut() {
                picker_state.cursor = index;
                picker_state.focus = picker::Focus::Grid;
                picker_state.current = ColorValue::Indexed(index);
            }
            preview_event(state).unwrap_or(Event::Consumed)
        }
        picker::Click::Hex => {
            if let Some(picker_state) = state.picker.as_mut() {
                picker_state.focus = picker::Focus::Hex;
            }
            Event::Consumed
        }
    }
}

fn click_row(state: &mut State, pane: Rect, point: (u16, u16)) -> Event {
    let Some(at) = row_at(state, pane, point) else {
        return Event::Consumed;
    };
    let rows = state.rows();
    let Some(row) = rows.get(at).copied() else {
        return Event::Consumed;
    };
    let selectable = state.selectable();
    if let Some(position) = selectable.iter().position(|candidate| candidate == &row) {
        state.selected = position;
        state.focus = Focus::Rows;
        ensure_visible(state);
    }
    if matches!(row, Row::Header(_)) {
        return Event::Consumed;
    }
    activate_row(state)
}

/// A picker double-click commits (8.3.4: click selects, double-click
/// commits the color).
pub fn double_click(state: &mut State, inner: Rect, pane: Rect, point: (u16, u16)) -> Event {
    if state.picker.is_some() {
        if let Some(picker::Click::Cell(index)) = picker::click_at(pane, point) {
            return commit_color(state, ColorValue::Indexed(index));
        }
        return Event::Consumed;
    }
    let (_, section_pane, _) = panes(inner);
    if point_in(point, section_pane) {
        return click_row(state, section_pane, point);
    }
    Event::Consumed
}

fn point_in(point: (u16, u16), rect: Rect) -> bool {
    let (col, row) = point;
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

// ---------------------------------------------------------------- render

pub struct View<'a> {
    pub state: &'a mut State,
    pub theme: &'a Applier,
}

pub fn render(frame: &mut Frame<'_>, inner: Rect, view: &mut View<'_>) {
    let state = &mut *view.state;
    state.window = pane_rows(inner);
    clamp_scroll(state);
    ensure_visible(state);
    let (list, pane, note) = panes(inner);
    frame.render_widget(section_list(state, view.theme), list);
    if state.picker.is_some() {
        if let Some(picker_state) = state.picker.as_ref() {
            picker::draw(frame, pane, picker_state, view.theme);
        }
        draw_note(frame, note, state, view.theme);
        return;
    }
    frame.render_widget(rows_paragraph(state, pane, view.theme), pane);
    draw_note(frame, note, state, view.theme);
}

/// The section pane's row capacity (the note line takes one row).
pub fn pane_rows(inner: Rect) -> usize {
    let (_, pane, _) = panes(inner);
    pane.height.max(1) as usize
}

fn section_list(state: &State, theme: &Applier) -> Paragraph<'static> {
    let lines: Vec<Line<'static>> = ALL_SECTIONS
        .iter()
        .enumerate()
        .map(|(at, section)| {
            let cursor = state.section_cursor == at;
            // When Sections holds the keyboard the cursor is loud; the
            // open section stays marked either way.
            let style = match (state.focus, cursor) {
                (Focus::Sections, true) => theme.style(Role::Background, Role::Selection),
                _ if *section == state.section => theme.style(Role::Accent, Role::Surface),
                _ => theme.style(Role::TextDim, Role::Surface),
            };
            Line::from(format!(" {name}", name = section.name())).style(style)
        })
        .collect();
    Paragraph::new(lines)
}

fn rows_paragraph(state: &State, pane: Rect, theme: &Applier) -> Paragraph<'static> {
    let visible = pane.height.max(1) as usize;
    let selected_row = state.selectable().get(state.selected).copied();
    let lines: Vec<Line<'static>> = state
        .rows()
        .into_iter()
        .skip(state.scroll)
        .take(visible)
        .map(|row| {
            row_line(
                row,
                state.focus == Focus::Rows && Some(row) == selected_row,
                state,
                theme,
            )
        })
        .collect();
    Paragraph::new(lines).style(theme.style(Role::Text, Role::Surface))
}

fn row_line(row: Row, selected: bool, state: &State, theme: &Applier) -> Line<'static> {
    if let Row::Header(name) = row {
        return Line::from(format!(" {name} ────"))
            .style(theme.style(Role::TextDim, Role::Surface));
    }
    let (label, value) = row_text(row, state);
    let style = if selected {
        theme.style(Role::Background, Role::Selection)
    } else {
        theme.style(Role::Text, Role::Surface)
    };
    let value_style = if selected {
        style
    } else {
        theme.style(Role::TextDim, Role::Surface)
    };
    // A color role's row carries a swatch: the actual color, not only
    // its hex spelling (the UI review's swatch finding). The shared
    // ColorValue→ratatui mapping supplies the color; theme.rs stays the
    // only module that names ratatui colors.
    let swatch = match row {
        Row::Role(index) => {
            let role = ALL_ROLES[index.min(ALL_ROLES.len() - 1)];
            Some(Span::styled(
                "██ ",
                ratatui::style::Style::new().fg(crate::theme::color_of(current_color(state, role))),
            ))
        }
        _ => None,
    };
    let mut spans = vec![Span::styled(format!(" {label:<24}"), style)];
    if let Some(swatch) = swatch {
        spans.push(swatch);
    }
    spans.push(Span::styled(value, value_style));
    Line::from(spans)
}

fn row_text(row: Row, state: &State) -> (String, String) {
    match row {
        Row::Theme(index) => {
            let name = state.themes.get(index).map(String::as_str).unwrap_or("?");
            let mark = if state.draft.theme == name {
                " ●"
            } else {
                ""
            };
            (name.to_string(), mark.to_string())
        }
        Row::Role(index) => {
            let role = ALL_ROLES[index.min(ALL_ROLES.len() - 1)];
            let value = current_color(state, role);
            (
                thememodel::role_name(role).to_string(),
                thememodel::hex_of(value),
            )
        }
        Row::Suggestions => (
            "suggestions".into(),
            on_off(state.draft.suggestions_enabled),
        ),
        Row::Font => (
            "font mode".into(),
            format!("{:?}", state.draft.font_mode).to_lowercase(),
        ),
        Row::Taskbar => (
            "taskbar position".into(),
            format!("{:?}", state.draft.taskbar_position).to_lowercase(),
        ),
        Row::ShowHidden => (
            "show hidden files".into(),
            on_off(state.draft.files.show_hidden),
        ),
        Row::RespectIgnore => (
            "respect ignore files".into(),
            on_off(state.draft.files.respect_ignore),
        ),
        Row::Depth => ("max depth".into(), state.draft.files.max_depth.to_string()),
        Row::Entries => (
            "max entries".into(),
            state.draft.files.max_entries.to_string(),
        ),
        Row::Key(action) => (
            action.label().to_string(),
            action.spec_of(&state.draft.reserved_keys),
        ),
        Row::Apply => ("Apply — write the files".into(), String::new()),
        Row::Cancel => ("Cancel — revert and close".into(), String::new()),
        Row::Header(name) => (name.to_string(), String::new()),
    }
}

fn on_off(value: bool) -> String {
    if value {
        "on".into()
    } else {
        "off".into()
    }
}

fn draw_note(frame: &mut Frame<'_>, note: Rect, state: &State, theme: &Applier) {
    let text = state
        .note
        .clone()
        .unwrap_or_else(|| "enter edit · esc cancel · clicks preview until Apply".to_string());
    frame.render_widget(
        Paragraph::new(text).style(theme.style(Role::TextDim, Role::Surface)),
        note,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use crossterm::event::KeyModifiers;
    use ratatui::widgets::Block;
    use ratatui::{backend::TestBackend, Terminal};

    fn state() -> State {
        State::open(&Settings::default())
    }

    #[test]
    fn sections_have_names_and_summaries_for_the_deep_link() {
        for section in ALL_SECTIONS {
            assert!(!section.name().is_empty());
            assert!(!section.summary().is_empty());
        }
        assert_eq!(Section::Files.name(), "Files");
    }

    #[test]
    fn appearance_lists_themes_then_roles_then_actions() {
        let state = state();
        let rows = state.rows();
        assert!(matches!(rows[1], Row::Theme(0)), "{rows:?}");
        let role_rows = rows
            .iter()
            .filter(|row| matches!(row, Row::Role(_)))
            .count();
        assert_eq!(role_rows, ALL_ROLES.len(), "every role is a row");
        assert_eq!(rows.last(), Some(&Row::Cancel), "cancel sits at the bottom");
    }

    #[test]
    fn choosing_a_theme_previews_and_apply_carries_the_draft() {
        let mut state = state();
        let paper = state
            .themes
            .iter()
            .position(|name| name == "paper")
            .expect("paper is builtin");
        state.selected = state
            .selectable()
            .iter()
            .position(|row| matches!(row, Row::Theme(index) if *index == paper))
            .expect("the paper row");
        let event = activate_row(&mut state);
        let Event::Preview { theme, palette } = event else {
            panic!("expected a preview");
        };
        assert_eq!(theme, "paper");
        assert_eq!(
            palette.get(Role::Background),
            agony_core::thememodel::builtin("paper")
                .expect("paper")
                .palette
                .get(Role::Background)
        );
        assert_eq!(state.draft.theme, "paper");
        let Event::Apply(applied) = activate_row_on(&mut state, Row::Apply) else {
            panic!("expected apply");
        };
        assert_eq!(applied.settings.theme, "paper");
        assert!(applied.overrides.is_empty());
    }

    fn activate_row_on(state: &mut State, target: Row) -> Event {
        state.selected = state
            .selectable()
            .iter()
            .position(|row| row == &target)
            .expect("row exists");
        activate_row(state)
    }

    #[test]
    fn toggles_and_cycles_change_only_the_draft() {
        let mut state = state();
        open_at(&mut state, Section::Behavior);
        let before = state.draft.clone();
        assert_eq!(
            activate_row_on(&mut state, Row::Suggestions),
            Event::Consumed
        );
        assert_ne!(state.draft.suggestions_enabled, before.suggestions_enabled);
        assert_eq!(activate_row_on(&mut state, Row::Font), Event::Consumed);
        assert_ne!(state.draft.font_mode, before.font_mode);
        assert_eq!(adjust_row(&mut state, -1), Event::Consumed);
        assert_eq!(state.draft.font_mode, before.font_mode, "left cycles back");
    }

    #[test]
    fn committing_a_color_joins_the_draft_and_previews() {
        let mut state = state();
        state.picker = Some(picker::State::open(Role::Accent, ColorValue::Indexed(196)));
        let event = picker_event(
            &mut state,
            picker::Event::Commit(ColorValue::Rgb(0x66, 0xd9, 0xff)),
        );
        let Event::Preview { palette, .. } = event else {
            panic!("expected a preview after commit");
        };
        assert_eq!(palette.get(Role::Accent), ColorValue::Rgb(0x66, 0xd9, 0xff));
        assert_eq!(state.overrides_extends.as_deref(), Some("graphite"));
        assert_eq!(
            state.overrides,
            vec![(Role::Accent, ColorValue::Rgb(0x66, 0xd9, 0xff))]
        );
        let Event::Apply(applied) = apply_event(&state) else {
            panic!("expected apply payload");
        };
        assert_eq!(
            applied.overrides_extends.as_deref(),
            Some("graphite"),
            "the deltas extend the drafted theme"
        );
    }

    #[test]
    fn a_contrast_failing_commit_is_refused_with_the_pair_named() {
        let mut state = state();
        state.picker = Some(picker::State::open(Role::Text, ColorValue::Indexed(231)));
        let event = picker_event(&mut state, picker::Event::Commit(ColorValue::Indexed(16)));
        assert_eq!(event, Event::Consumed, "refused");
        let error = state
            .picker
            .as_ref()
            .expect("picker stays open")
            .error
            .clone()
            .expect("the refusal is named");
        assert!(
            error.contains("text on background"),
            "the pair is named: {error}"
        );
        assert!(state.overrides.is_empty(), "nothing joined the draft");
    }

    #[test]
    fn key_capture_sets_the_spec_and_refuses_conflicts() {
        let mut state = state();
        open_at(&mut state, Section::Keys);
        assert_eq!(
            activate_row_on(&mut state, Row::Key(Capture::Minimize)),
            Event::Consumed
        );
        assert_eq!(state.capture, Some(Capture::Minimize));
        let ctrl_p = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(
            capture_key(&mut state, Capture::Minimize, ctrl_p),
            Event::Consumed
        );
        assert_eq!(state.draft.reserved_keys.minimize, "ctrl-p");
        // A binding that collides with another action is refused, named.
        let conflicting = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(
            capture_key(&mut state, Capture::StartMenu, conflicting),
            Event::Consumed
        );
        assert!(
            state
                .note
                .as_deref()
                .is_some_and(|note| note.contains("already runs")),
            "{:?}",
            state.note
        );
        assert_eq!(state.draft.reserved_keys.start_menu, "ctrl-space", "kept");
    }

    #[test]
    fn open_at_retargets_the_section_and_resets_the_cursor() {
        let mut state = state();
        state.selected = 3;
        open_at(&mut state, Section::Keys);
        assert_eq!(state.section, Section::Keys);
        assert_eq!(state.selected, 0);
        assert_eq!(state.scroll, 0);
    }

    #[test]
    fn tab_moves_between_the_section_list_and_the_rows() {
        let mut state = state();
        assert_eq!(state.focus, Focus::Rows, "rows hold the keys at open");
        let tab = KeyEvent::new(KeyCode::Tab, KeyModifiers::empty());
        assert_eq!(handle_key(&mut state, tab), Event::Consumed);
        assert_eq!(state.focus, Focus::Sections);
        // The section cursor walks and the section follows it.
        let down = KeyEvent::new(KeyCode::Down, KeyModifiers::empty());
        assert_eq!(handle_key(&mut state, down), Event::Consumed);
        assert_eq!(state.section, Section::Behavior);
        assert_eq!(state.section_cursor, 1);
        // Enter hands the keys back to the rows.
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::empty());
        assert_eq!(handle_key(&mut state, enter), Event::Consumed);
        assert_eq!(state.focus, Focus::Rows);
        assert_eq!(handle_key(&mut state, tab), Event::Consumed);
        assert_eq!(state.focus, Focus::Sections, "Tab returns to the list");
    }

    #[test]
    fn click_selects_and_activates_but_never_commits() {
        let mut state = state();
        let inner = Rect::new(0, 0, 60, 22);
        let (_, pane, _) = panes(inner);
        // The paper theme's row: click previews, does not apply.
        let paper = state
            .themes
            .iter()
            .position(|name| name == "paper")
            .expect("paper");
        let at = state
            .rows()
            .iter()
            .position(|row| matches!(row, Row::Theme(index) if *index == paper))
            .expect("display row");
        let event = click(&mut state, inner, pane, (pane.x + 2, pane.y + at as u16));
        assert!(matches!(event, Event::Preview { .. }), "a click previews");
        assert_eq!(state.draft.theme, "paper");
    }

    #[test]
    fn click_on_the_section_list_switches_sections() {
        let mut state = state();
        let inner = Rect::new(0, 0, 60, 22);
        let (list, _, _) = panes(inner);
        let event = click(
            &mut state,
            inner,
            Rect::new(0, 0, 0, 0),
            (list.x + 1, list.y + 2),
        );
        assert_eq!(event, Event::Consumed);
        assert_eq!(state.section, Section::Files, "the third section row");
    }

    #[test]
    fn double_click_on_a_palette_cell_commits() {
        let mut state = state();
        state.picker = Some(picker::State::open(Role::Accent, ColorValue::Indexed(0)));
        let inner = Rect::new(0, 0, 60, 22);
        let (_, pane, _) = panes(inner);
        // The segmented palette: three header rows, the base row, then
        // the cube. Cube hue column 35 (r=5,g=5), blue row 5 — the
        // cube's white, index 231 — passes the contrast check against
        // graphite's background.
        let event = double_click(&mut state, inner, pane, (pane.x + 35, pane.y + 3 + 1 + 5));
        assert!(matches!(event, Event::Preview { .. }), "commit previews");
        assert_eq!(
            state.overrides.last(),
            Some(&(Role::Accent, ColorValue::Indexed(231)))
        );
    }

    #[test]
    fn installed_themes_are_builtins_plus_custom_files() {
        let dir = std::env::temp_dir().join(format!("agony-setui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("midnight.json"), "{}").expect("theme file");
        std::fs::write(dir.join("notes.txt"), "nope").expect("distractor");
        let themes = installed_themes(Some(&dir));
        assert!(themes.contains(&"graphite".to_string()));
        assert!(themes.contains(&"midnight".to_string()));
        assert!(!themes.contains(&"notes".to_string()));
        std::fs::write(dir.join(theme::OVERRIDE_FILE), "{}").expect("override");
        assert!(
            !themes.contains(&"overrides".to_string()),
            "the override pack is not a theme"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drawn_window_shows_sections_rows_and_note() {
        let theme = Applier::fallback();
        let mut state = state();
        let backend = TestBackend::new(64, 22);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| {
                let block = Block::bordered();
                let inner = block.inner(Rect::new(0, 0, 64, 22));
                f.render_widget(block, Rect::new(0, 0, 64, 22));
                render(
                    f,
                    inner,
                    &mut View {
                        state: &mut state,
                        theme: &theme,
                    },
                );
            })
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Appearance"),
            "section list missing: {text:?}"
        );
        assert!(text.contains("graphite"), "theme rows missing: {text:?}");
        assert!(text.contains("text-dim"), "role rows missing: {text:?}");
        assert!(text.contains("Apply"), "the apply row missing: {text:?}");
    }
}
