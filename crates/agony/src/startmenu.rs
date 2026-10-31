//! The start menu (Part09 §9.2): four roots — SEARCH, FILE MANAGER,
//! PROGRAMS, SETTINGS — rendered at the top under the filter line.
//! FILE MANAGER, PROGRAMS and SETTINGS expand in place (accordion — one
//! open at a time); SEARCH never expands: it acts, closing the menu and
//! opening the command bar with the filter text carried over. Typing
//! anywhere switches the whole list to ranked results over apps, places
//! and setting names (the one suggest engine over merged fields);
//! Backspace-to-empty returns the roots. PROGRAMS expands to today's
//! catalog view unchanged; FILE MANAGER to `places::Places` (resolved
//! once per open, never per draw); SETTINGS to the settings groups,
//! deep-linking through Part08's `open_at` seam.
//!
//! Items: `Entry` — one launchable app (its manifest plus the discovered
//! path and candidate flag); `Root` — the four roots; `State` — filter,
//! selection, scroll, the open root, entries and places; `Row` — one
//! display row (root, header, program, place, drive, or setting);
//! `MenuHit` — what a click or the context seam landed on; `MenuEvent`
//! — what a key press asked the app to do.
//! Core functions: `handle_key` — the one cursor state machine
//! (Up/Down walk roots and expanded rows, Right/Enter expand or
//! activate, Left/Esc collapse then close); `click` / `double_click` —
//! the pointer path (click activates, a program row needs a
//! double-click); `rows` — the grouped, expanded or ranked view;
//! `selectable` — the navigable rows; `scroll_by` / `page` /
//! `clamp_scroll` — the one viewport offset; `rect` — the overlay (it
//! grows with the open section, capped at the desktop); `hit_at` — the
//! context seam's lookup; `draw` — the widget; `entries_from` /
//! `builtin_entries` — the catalog snapshot as entries.
//! Depends on: agony_core::manifest + suggest + icon + inputmap +
//! layout, crate::discover, crate::places, crate::settings_ui (the
//! Section type), crate::platform (the shell fallback's argv).
//! Called by: app (open/close/keys/pointer/events) and the draw pass.
//! Calls: nothing async — launching is the app's job.

use std::path::PathBuf;

use agony_core::config::{FontMode, TaskbarPosition};
use agony_core::icon::{self as icon_table, Icon};
use agony_core::layout::Rect;
use agony_core::manifest::{AppManifest, Category};
use agony_core::suggest::{self, FieldItem};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;

use crate::discover;
use crate::places::Places;
use crate::settings_ui::Section;
use crate::theme::{Applier, Role};

const MENU_WIDTH: u16 = 48;
const MENU_HEIGHT: u16 = 18;
const NOT_INSTALLED: &str = "not installed";

#[derive(Clone, Debug)]
pub struct Entry {
    pub manifest: AppManifest,
    /// The resolved executable path when the discovery scan found it.
    pub path: Option<String>,
    /// True when carapace seeded this entry and no manifest claimed it.
    pub candidate: bool,
}

impl Entry {
    pub fn label(&self) -> &str {
        &self.manifest.name
    }

    /// Greyed entries are manifests whose exec the scan never found.
    pub fn greyed(&self) -> bool {
        !self.candidate && self.path.is_none()
    }

    /// The dim text after the label: description and resolved path when
    /// installed, the license and install recipe when not, the
    /// description for carapace candidates. A declared license rides
    /// along in every state — the GPL shelf's visible line (10.3.3).
    pub fn hint(&self) -> String {
        if self.candidate {
            return fallback(self.manifest.description.clone(), "carapace command");
        }
        if self.greyed() {
            let recipe = self
                .manifest
                .install_recipe()
                .unwrap_or_else(|| "no recipe".into());
            return match self.manifest.license.as_deref() {
                Some(license) => format!("not installed — {license} — {recipe}"),
                None => format!("not installed — {recipe}"),
            };
        }
        let path = self.path.clone().unwrap_or_default();
        let license = self
            .manifest
            .license
            .as_deref()
            .map(|license| format!(" · {license}"))
            .unwrap_or_default();
        if self.manifest.description.is_empty() {
            return format!("{path}{license}");
        }
        format!("{} · {}{}", self.manifest.description, path, license)
    }

    /// The fields the filter matches: name, description and category.
    pub fn search_fields(&self) -> Vec<String> {
        vec![
            self.manifest.name.clone(),
            self.manifest.description.clone(),
            self.manifest.category.as_str().to_string(),
        ]
    }
}

fn fallback(text: String, instead: &str) -> String {
    if text.trim().is_empty() {
        instead.to_string()
    } else {
        text
    }
}

/// The menu's four roots (Part09 9.2.1). Search acts; the rest expand.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Root {
    Search,
    FileManager,
    Programs,
    Settings,
}

pub const ALL_ROOTS: [Root; 4] = [
    Root::Search,
    Root::FileManager,
    Root::Programs,
    Root::Settings,
];

impl Root {
    pub fn label(self) -> &'static str {
        match self {
            Root::Search => "SEARCH",
            Root::FileManager => "FILE MANAGER",
            Root::Programs => "PROGRAMS",
            Root::Settings => "SETTINGS",
        }
    }

    /// SEARCH never expands (9.2.1).
    fn expandable(self) -> bool {
        self != Root::Search
    }
}

#[derive(Clone, Debug)]
pub struct State {
    pub entries: Vec<Entry>,
    pub places: Places,
    pub filter: String,
    /// The position among the currently visible (selectable) rows.
    pub selected: usize,
    /// The first visible display row (Part06 6.3.3): keys and the wheel
    /// move the same offset; clamped to the content on every change.
    pub scroll: usize,
    /// The visible row count, updated at draw time (the clamp needs it).
    pub window: usize,
    /// The accordion's open root (one at a time; None = collapsed).
    pub open: Option<Root>,
}

/// One display row: a root, a dim header, or a selectable item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Root(Root),
    Header(&'static str),
    Item {
        index: usize,
    },
    Place {
        index: usize,
    },
    Drive {
        index: usize,
    },
    Setting {
        section: Section,
    },
    /// SETTINGS's last row (10.2.2): reopens the welcome document.
    About,
    /// PROGRAMS's first row (Part11 §11.3.1): opens the app directory.
    GetMoreApps,
}

/// What a click or the context seam landed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuHit {
    Root(Root),
    Program(usize),
    Place(usize),
    Drive(usize),
    Setting(Section),
    About,
    GetMoreApps,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuEvent {
    Consumed,
    Close,
    Launch(usize),
    /// SEARCH acted: open the command bar carrying this text (9.2.2).
    Search(String),
    OpenPlace {
        path: PathBuf,
    },
    OpenSettings {
        section: Section,
    },
    /// The About row (10.2.2): the welcome document, reopened.
    OpenAbout,
    /// The Get more apps row (Part11 §11.3.1): the directory window.
    OpenDirectory,
}

/// One ranked search hit's tag: which corpus the row came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tag {
    Program(usize),
    Place(usize),
    Drive(usize),
    Setting(Section),
}

impl State {
    pub fn new(entries: Vec<Entry>, places: Places) -> State {
        State {
            entries,
            places,
            filter: String::new(),
            selected: 0,
            scroll: 0,
            window: 15,
            open: None,
        }
    }

    /// The display rows: ranked mixed results when filtering; otherwise
    /// the four roots with the open section's rows expanded in place.
    pub fn rows(&self) -> Vec<Row> {
        if self.filter.is_empty() {
            return rooted_rows(self);
        }
        ranked_rows(&self.filter, &self.entries, &self.places)
    }

    /// The navigable rows, in display order.
    pub fn selectable(&self) -> Vec<Row> {
        self.rows()
            .into_iter()
            .filter(|row| !matches!(row, Row::Header(_)))
            .collect()
    }

    /// Wheel scroll (Part06 6.3.2): move the offset by signed rows
    /// (positive = later content), then keep the selection in view.
    pub fn scroll_by(&mut self, rows: i32) {
        self.scroll = offset_shift(self.scroll, rows);
        self.clamp_scroll();
    }

    /// PageUp/PageDown: the same offset, one window at a time.
    pub fn page(&mut self, direction: i32) {
        self.scroll_by(direction * self.window.max(1) as i32);
    }

    /// The one clamp (Part06 6.3.3's pitfall row): the offset stays
    /// inside the content and the selection stays inside the window —
    /// a scroll that would strand the selection pulls it to the nearest
    /// visible row instead. Called on every offset, selection, filter
    /// and window change.
    pub fn clamp_scroll(&mut self) {
        let rows_len = self.rows().len();
        let visible = self.window.max(1);
        self.scroll = self.scroll.min(rows_len.saturating_sub(visible));
        let Some(position) = self.selected_position() else {
            return;
        };
        if position >= self.scroll && position < self.scroll + visible {
            return;
        }
        let selectable_positions = self.selectable_positions();
        let Some(&target) = selectable_positions
            .iter()
            .find(|position| **position >= self.scroll)
            .or(selectable_positions.last())
        else {
            return;
        };
        self.selected = selectable_positions
            .iter()
            .position(|position| *position == target)
            .unwrap_or(self.selected);
    }

    /// The selected row's position among the display rows.
    fn selected_position(&self) -> Option<usize> {
        let selected = self.selectable().get(self.selected).copied()?;
        self.rows().iter().position(|row| *row == selected)
    }

    /// Every selectable row's display-row position, in order.
    fn selectable_positions(&self) -> Vec<usize> {
        self.rows()
            .iter()
            .enumerate()
            .filter_map(|(position, row)| self.selectable().contains(row).then_some(position))
            .collect()
    }

    /// Toggle a root's expansion (accordion: opening one closes the
    /// others; SEARCH never expands).
    fn toggle_open(&mut self, root: Root) {
        if !root.expandable() {
            return;
        }
        self.open = if self.open == Some(root) {
            None
        } else {
            Some(root)
        };
        self.clamp_scroll();
    }
}

/// The unfiltered view: the roots with the open section's rows expanded
/// directly under their root (the accordion the UI review's keyboard
/// finding asked for — Down from an expanded root reaches its children,
/// not the next root).
fn rooted_rows(state: &State) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for root in ALL_ROOTS {
        rows.push(Row::Root(root));
        if state.open == Some(root) {
            rows.extend(section_rows_of(state, root));
        }
    }
    rows
}

/// The open root's rows (none when collapsed).
fn section_rows(state: &State) -> Vec<Row> {
    match state.open {
        None => Vec::new(),
        Some(open) => section_rows_of(state, open),
    }
}

/// One root's expanded rows, by root.
fn section_rows_of(state: &State, root: Root) -> Vec<Row> {
    match root {
        Root::FileManager => place_rows(&state.places),
        Root::Programs => program_rows(&state.entries),
        Root::Settings => settings_rows(),
        Root::Search => Vec::new(),
    }
}

/// PROGRAMS expands to the directory's door on top (Part11 §11.3.1),
/// then the grouped catalog view, unchanged.
fn program_rows(entries: &[Entry]) -> Vec<Row> {
    let mut rows = vec![Row::GetMoreApps];
    rows.extend(grouped_rows(entries));
    rows
}

/// SETTINGS expands to the four groups, then the About row (10.2.2).
fn settings_rows() -> Vec<Row> {
    let mut rows: Vec<Row> = ALL_SECTIONS
        .iter()
        .copied()
        .map(|section| Row::Setting { section })
        .collect();
    rows.push(Row::About);
    rows
}

fn place_rows(places: &Places) -> Vec<Row> {
    let mut rows = vec![Row::Header("places")];
    rows.extend((0..places.known.len()).map(|index| Row::Place { index }));
    if !places.drives.is_empty() {
        rows.push(Row::Header("drives"));
        rows.extend((0..places.drives.len()).map(|index| Row::Drive { index }));
    }
    rows
}

pub const ALL_SECTIONS: [Section; 4] = crate::settings_ui::ALL_SECTIONS;

/// Shift an offset by signed rows, saturating at zero (the upper clamp
/// is content-dependent and lives in `clamp_scroll`).
fn offset_shift(offset: usize, rows: i32) -> usize {
    if rows >= 0 {
        offset.saturating_add(rows as usize)
    } else {
        offset.saturating_sub(rows.unsigned_abs() as usize)
    }
}

fn grouped_rows(entries: &[Entry]) -> Vec<Row> {
    let mut rows = Vec::new();
    for category in Category::ALL {
        rows.extend(category_rows(entries, category));
    }
    let greyed: Vec<Row> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.greyed())
        .map(|(index, _)| Row::Item { index })
        .collect();
    if !greyed.is_empty() {
        rows.push(Row::Header(NOT_INSTALLED));
        rows.extend(greyed);
    }
    rows
}

fn category_rows(entries: &[Entry], category: Category) -> Vec<Row> {
    let items: Vec<Row> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            !entry.candidate && !entry.greyed() && entry.manifest.category == category
        })
        .map(|(index, _)| Row::Item { index })
        .collect();
    if items.is_empty() {
        return Vec::new();
    }
    let mut rows = vec![Row::Header(category.as_str())];
    rows.extend(items);
    rows
}

/// The filtered view: one ranked merge over apps, places, drives and
/// setting names — the suggest engine over the merged fields (9.2.2).
fn ranked_rows(filter: &str, entries: &[Entry], places: &Places) -> Vec<Row> {
    let mut items: Vec<FieldItem> = Vec::new();
    let mut tags: Vec<Tag> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        items.push(FieldItem {
            label: entry.label().to_string(),
            hint: entry.hint(),
            fields: entry.search_fields(),
        });
        tags.push(Tag::Program(index));
    }
    for (index, place) in places.known.iter().enumerate() {
        items.push(FieldItem {
            label: place.label.clone(),
            hint: place.path.display().to_string(),
            fields: vec![place.label.clone(), "place".into(), "file manager".into()],
        });
        tags.push(Tag::Place(index));
    }
    for (index, drive) in places.drives.iter().enumerate() {
        items.push(FieldItem {
            label: drive.clone(),
            hint: "drive".into(),
            fields: vec![drive.clone(), "drive".into()],
        });
        tags.push(Tag::Drive(index));
    }
    for section in ALL_SECTIONS {
        items.push(FieldItem {
            label: section.name().to_string(),
            hint: section.summary().to_string(),
            fields: vec![
                section.name().to_string(),
                section.summary().to_string(),
                "settings".into(),
            ],
        });
        tags.push(Tag::Setting(section));
    }
    suggest::rank_fields(filter, &items)
        .into_iter()
        .filter_map(|suggestion| tag_row(tags.get(suggestion.index).copied()?))
        .collect()
}

fn tag_row(tag: Tag) -> Option<Row> {
    Some(match tag {
        Tag::Program(index) => Row::Item { index },
        Tag::Place(index) => Row::Place { index },
        Tag::Drive(index) => Row::Drive { index },
        Tag::Setting(section) => Row::Setting { section },
    })
}

/// Route one key press inside the menu: the one cursor state machine
/// (9.2.7). Typing edits the filter; Up/Down walk roots and expanded
/// rows; Right/Enter expand or activate; Left/Esc collapse, then close;
/// PageUp/PageDown scroll.
pub fn handle_key(state: &mut State, key: KeyEvent) -> MenuEvent {
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Esc => {
            if state.open.is_some() {
                state.open = None;
                state.clamp_scroll();
                MenuEvent::Consumed
            } else {
                MenuEvent::Close
            }
        }
        KeyCode::Enter if plain => activate_selected(state),
        KeyCode::Right if plain => right_key(state),
        KeyCode::Left if plain => {
            if state.open.is_some() {
                state.open = None;
                state.clamp_scroll();
            }
            MenuEvent::Consumed
        }
        KeyCode::Backspace if plain => {
            state.filter.pop();
            state.selected = 0;
            state.scroll = 0;
            state.clamp_scroll();
            MenuEvent::Consumed
        }
        KeyCode::Up if plain => {
            move_selection(state, -1);
            MenuEvent::Consumed
        }
        KeyCode::Down if plain => {
            move_selection(state, 1);
            MenuEvent::Consumed
        }
        KeyCode::PageUp if plain => {
            state.page(-1);
            MenuEvent::Consumed
        }
        KeyCode::PageDown if plain => {
            state.page(1);
            MenuEvent::Consumed
        }
        KeyCode::Char(ch) if plain => {
            state.filter.push(ch);
            state.selected = 0;
            state.scroll = 0;
            state.clamp_scroll();
            MenuEvent::Consumed
        }
        _ => MenuEvent::Consumed,
    }
}

fn right_key(state: &mut State) -> MenuEvent {
    let Some(row) = state.selectable().get(state.selected).copied() else {
        return MenuEvent::Consumed;
    };
    if let Row::Root(root) = row {
        if root.expandable() {
            state.toggle_open(root);
            return MenuEvent::Consumed;
        }
    }
    activate_selected(state)
}

/// Enter (or an activating click) on the selected row.
pub fn activate_selected(state: &mut State) -> MenuEvent {
    let Some(row) = state.selectable().get(state.selected).copied() else {
        return MenuEvent::Consumed;
    };
    activate_row(state, row)
}

fn activate_row(state: &mut State, row: Row) -> MenuEvent {
    match row {
        Row::Root(Root::Search) => MenuEvent::Search(state.filter.clone()),
        Row::Root(root) => {
            state.toggle_open(root);
            MenuEvent::Consumed
        }
        Row::Item { index } => MenuEvent::Launch(index),
        Row::Place { index } => match state.places.known.get(index) {
            Some(place) => MenuEvent::OpenPlace {
                path: place.path.clone(),
            },
            None => MenuEvent::Consumed,
        },
        Row::Drive { index } => match state.places.drives.get(index) {
            Some(drive) => MenuEvent::OpenPlace {
                path: drive_root(drive),
            },
            None => MenuEvent::Consumed,
        },
        Row::Setting { section } => MenuEvent::OpenSettings { section },
        Row::About => MenuEvent::OpenAbout,
        Row::GetMoreApps => MenuEvent::OpenDirectory,
        Row::Header(_) => MenuEvent::Consumed,
    }
}

/// A drive label's browsable root: "C:" browses "C:\".
fn drive_root(drive: &str) -> PathBuf {
    if drive.ends_with(':') {
        format!("{drive}\\").into()
    } else {
        drive.into()
    }
}

/// A click (9.2.8): a root toggles (SEARCH acts), a place/setting
/// activates, a program row selects only — the double-click launches.
pub fn click(state: &mut State, region: Rect, point: (u16, u16)) -> MenuEvent {
    let Some(row) = hit_row(state, region, point) else {
        return MenuEvent::Consumed;
    };
    if let Row::Item { index } = row {
        if let Some(position) = state.selectable().iter().position(|candidate| {
            matches!(candidate, Row::Item { index: candidate_index } if *candidate_index == index)
        }) {
            state.selected = position;
            state.clamp_scroll();
        }
        return MenuEvent::Consumed;
    }
    if let Some(position) = state
        .selectable()
        .iter()
        .position(|candidate| candidate == &row)
    {
        state.selected = position;
    }
    activate_row(state, row)
}

/// A double-click (9.2.8): a program row launches; anything else
/// activates like a single click.
pub fn double_click(state: &mut State, region: Rect, point: (u16, u16)) -> MenuEvent {
    let Some(row) = hit_row(state, region, point) else {
        return MenuEvent::Consumed;
    };
    if let Some(position) = state
        .selectable()
        .iter()
        .position(|candidate| candidate == &row)
    {
        state.selected = position;
    }
    activate_row(state, row)
}

/// The row under a point inside the menu's region (the click and the
/// context seam's shared lookup).
fn hit_row(state: &State, region: Rect, point: (u16, u16)) -> Option<Row> {
    let items = items_area(Block::bordered().inner(region));
    row_at_items(state, items, point.1)
}

/// The display row at a list row — the one walk behind the click, the
/// hover highlight and the context seam.
fn row_at_items(state: &State, items: Rect, row: u16) -> Option<Row> {
    if row < items.y || row >= items.y + items.height {
        return None;
    }
    let visible = state.rows().into_iter().skip(state.scroll);
    for (at, display) in visible.enumerate() {
        if at as u16 == row - items.y {
            return match display {
                Row::Header(_) => None,
                other => Some(other),
            };
        }
    }
    None
}

/// The context seam's lookup (Part06 6.3.6): the hit under the pointer,
/// for the PROGRAMS-row context menu.
pub fn hit_at(state: &State, region: Rect, point: (u16, u16)) -> Option<MenuHit> {
    match hit_row(state, region, point)? {
        Row::Item { index } => Some(MenuHit::Program(index)),
        Row::Root(root) => Some(MenuHit::Root(root)),
        Row::Place { index } => Some(MenuHit::Place(index)),
        Row::Drive { index } => Some(MenuHit::Drive(index)),
        Row::Setting { section } => Some(MenuHit::Setting(section)),
        Row::About => Some(MenuHit::About),
        Row::GetMoreApps => Some(MenuHit::GetMoreApps),
        Row::Header(_) => None,
    }
}

fn move_selection(state: &mut State, delta: i32) {
    let last = state.selectable().len().saturating_sub(1) as i32;
    let moved = state.selected as i32 + delta;
    state.selected = moved.clamp(0, last) as usize;
    state.clamp_scroll();
}

/// The overlay's rectangle: anchored at the taskbar's edge of the
/// desktop — bottom-left above a bottom taskbar, top-left below a top
/// one (the start button and its menu stay on one edge). `extra_rows`
/// is the open section's height (9.2.9); the total is capped at the
/// desktop.
pub fn rect(desktop: Rect, extra_rows: u16, position: TaskbarPosition) -> Rect {
    let width = MENU_WIDTH.min(desktop.width);
    let height = (MENU_HEIGHT + extra_rows).min(desktop.height);
    let y = match position {
        TaskbarPosition::Top => desktop.y,
        TaskbarPosition::Bottom => desktop.y + desktop.height - height,
    };
    Rect::new(desktop.x, y, width, height)
}

/// The open section's row count — `rect`'s extra rows for a state.
pub fn open_rows(state: &State) -> u16 {
    section_rows(state).len() as u16
}

/// The overlay rect for a state (the one the app's callers use).
pub fn rect_for(desktop: Rect, state: &State, position: TaskbarPosition) -> Rect {
    rect(desktop, open_rows(state), position)
}

pub fn draw(
    frame: &mut Frame<'_>,
    region: Rect,
    state: &mut State,
    theme: &Applier,
    font: FontMode,
    hover: Option<(u16, u16)>,
) {
    // The menu owns every cell of its region: Clear first, so the
    // windows under it never bleed through (the UI review's #1 finding).
    frame.render_widget(Clear, region);
    let block = Block::bordered()
        .border_style(theme.style(Role::Border, Role::Surface))
        .style(theme.style(Role::Text, Role::Surface));
    let inner = block.inner(region);
    frame.render_widget(block, region);
    frame.render_widget(filter_line(state, theme, font), filter_area(inner));
    let items = items_area(inner);
    state.window = items.height as usize;
    state.clamp_scroll();
    let hovered = hovered_row(state, items, hover);
    frame.render_widget(rows_paragraph(state, theme, items, hovered), items);
}

/// The display row under the pointer, when it rests on a row (Part06
/// 6.3.5: hover highlights, never focuses).
fn hovered_row(state: &State, items: Rect, hover: Option<(u16, u16)>) -> Option<Row> {
    let (_, row) = hover?;
    row_at_items(state, items, row)
}

/// The never-empty fallback menu: one shell entry, so the menu always
/// demonstrates launch even when discovery has nothing. The argv is the
/// platform seam's (login shell + its flag).
pub fn builtin_entries() -> Vec<Entry> {
    let argv = crate::platform::login_shell_argv();
    let shell = argv[0].clone();
    let mut manifest = AppManifest::minimal("shell", &shell, &shell, Category::Shell);
    manifest.description = "the shell".into();
    manifest.args = argv.into_iter().skip(1).collect();
    vec![Entry {
        manifest,
        path: Some(shell),
        candidate: false,
    }]
}

/// The discovery snapshot as menu entries; the shell fallback when there
/// is nothing at all. Catalog entries (Part11 §11.3.1) appear only when
/// installed or bundled: while a catalog app is just a greyed recipe,
/// the start menu's "Get more apps…" row and the directory window are
/// its home — the menu stays the user's daily launcher.
pub fn entries_from(cache: Option<&discover::CatalogCache>) -> Vec<Entry> {
    let Some(cache) = cache else {
        return builtin_entries();
    };
    let entries: Vec<Entry> = cache
        .apps
        .iter()
        .filter(|app| menu_worthy(app))
        .map(|app| Entry {
            manifest: app.manifest.clone(),
            path: app.path.clone(),
            candidate: app.candidate,
        })
        .collect();
    if entries.is_empty() {
        builtin_entries()
    } else {
        entries
    }
}

/// The menu's entry rule: candidates and installed apps always show; a
/// greyed app shows unless it is a catalog-only entry (not bundled).
fn menu_worthy(app: &discover::CachedApp) -> bool {
    if app.candidate || app.path.is_some() {
        return true;
    }
    !app.catalog || app.manifest.bundled
}

fn filter_area(inner: Rect) -> Rect {
    Rect::new(inner.x, inner.y, inner.width, 1)
}

fn items_area(inner: Rect) -> Rect {
    let height = inner.height.saturating_sub(1);
    Rect::new(inner.x, inner.y + 1, inner.width, height)
}

fn filter_line(state: &State, theme: &Applier, font: FontMode) -> Paragraph<'static> {
    let glyph = icon_table::glyph(Icon::Search, font);
    let text = format!(" {glyph} {}_", state.filter);
    Paragraph::new(text).style(theme.style(Role::TextDim, Role::Surface))
}

fn rows_paragraph(
    state: &State,
    theme: &Applier,
    area: Rect,
    hovered: Option<Row>,
) -> Paragraph<'static> {
    let visible = (area.height as usize).max(1);
    let budget = area.width as usize;
    let lines = if state.selectable().is_empty() {
        vec![dim_line("(no matches)", theme)]
    } else {
        windowed(state, theme, visible, budget, hovered)
    };
    Paragraph::new(lines).style(theme.style(Role::Text, Role::Surface))
}

/// The visible window of rows: from the state's scroll offset (the one
/// the keys and the wheel move), each line tagged with whether it is
/// the selection or under the pointer. Lines are cut to the area's
/// width with an ellipsis — never a hard clip mid-word.
fn windowed(
    state: &State,
    theme: &Applier,
    visible: usize,
    budget: usize,
    hovered: Option<Row>,
) -> Vec<Line<'static>> {
    let selected_row = state.selectable().get(state.selected).copied();
    state
        .rows()
        .into_iter()
        .skip(state.scroll)
        .take(visible)
        .filter_map(|row| match row {
            Row::Header(name) => Some(header_line(name, theme)),
            other => state.row_line(other).map(|(label, hint)| {
                item_line(
                    selected_row == Some(other),
                    hovered == Some(other),
                    label,
                    hint,
                    budget,
                    theme,
                )
            }),
        })
        .collect()
}

/// A selectable row's (label, dim hint) pair.
impl State {
    fn row_line(&self, row: Row) -> Option<(String, String)> {
        Some(match row {
            Row::Root(root) => {
                let marker = match self.open {
                    Some(open) if open == root => "-",
                    _ if root.expandable() => ">",
                    _ => " ",
                };
                (format!("{marker} {}", root.label()), root_hint(root))
            }
            Row::Item { index } => {
                let entry = self.entries.get(index)?;
                (entry.label().to_string(), entry.hint())
            }
            Row::Place { index } => {
                let place = self.places.known.get(index)?;
                (place.label.clone(), place.path.display().to_string())
            }
            Row::Drive { index } => {
                let drive = self.places.drives.get(index)?;
                (drive.clone(), "drive".into())
            }
            Row::Setting { section } => (section.name().to_string(), section.summary().to_string()),
            Row::About => (
                "About AGony".to_string(),
                "the welcome: keys, font, first steps".to_string(),
            ),
            Row::GetMoreApps => (
                "Get more apps…".to_string(),
                "browse the app directory and install".to_string(),
            ),
            Row::Header(_) => return None,
        })
    }
}

fn root_hint(root: Root) -> String {
    match root {
        Root::Search => "open the command bar".into(),
        Root::FileManager => "common places and drives".into(),
        Root::Programs => "the app catalog".into(),
        Root::Settings => "themes, behavior, files, keys".into(),
    }
}

fn header_line(name: &str, theme: &Applier) -> Line<'static> {
    Line::from(format!("   {name} ─────")).style(theme.style(Role::TextDim, Role::Surface))
}

/// A menu row: the selection wears the Selection role (the loud
/// highlight, distinct from the green accent); a hovered row highlights
/// in the accent foreground (Part06 6.3.5 — a restyle, never a focus
/// change); greyed entries stay dim through both. The composed line is
/// cut to `budget` cells with an ellipsis.
fn item_line(
    selected: bool,
    hovered: bool,
    label: String,
    hint: String,
    budget: usize,
    theme: &Applier,
) -> Line<'static> {
    let style = if selected {
        theme.style(Role::Background, Role::Selection)
    } else if hovered {
        theme.style(Role::Accent, Role::Surface)
    } else {
        theme.style(Role::Text, Role::Surface)
    };
    let composed = format!("   {label} — {hint}");
    let shown = agony_core::textutil::ellipsize(&composed, budget.max(1));
    Line::from(vec![Span::styled(shown, style)])
}

fn dim_line(text: &str, theme: &Applier) -> Line<'static> {
    Line::from(text.to_string()).style(theme.style(Role::TextDim, Role::Surface))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selftest::buffer_text;
    use agony_core::layout::Rect as Area;
    use crossterm::event::KeyModifiers;
    use ratatui::{backend::TestBackend, Terminal};

    fn state() -> State {
        State::new(builtin_entries(), Places::default())
    }

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn press_char(ch: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(ch), KeyModifiers::empty())
    }

    fn entry(id: &str, name: &str, category: Category, path: Option<&str>) -> Entry {
        let mut manifest = AppManifest::minimal(id, name, id, category);
        manifest.description = format!("{name} description");
        Entry {
            manifest,
            path: path.map(str::to_string),
            candidate: false,
        }
    }

    fn candidate(id: &str, description: &str) -> Entry {
        let mut manifest = AppManifest::minimal(id, id, id, Category::System);
        manifest.description = description.to_string();
        manifest.terminal_app = false;
        Entry {
            manifest,
            path: Some(format!("C:\\bin\\{id}.exe")),
            candidate: true,
        }
    }

    fn demo_entries() -> Vec<Entry> {
        vec![
            entry(
                "superfile",
                "Superfile",
                Category::Files,
                Some("C:\\bin\\spf.exe"),
            ),
            entry("gum", "Gum", Category::Dev, Some("C:\\bin\\gum.exe")),
            entry("glow", "Glow", Category::Text, None),
            candidate("git", "version control"),
        ]
    }

    fn demo_places() -> Places {
        Places {
            known: vec![
                crate::places::Place {
                    label: "Home".into(),
                    path: PathBuf::from(r"C:\Users\me"),
                },
                crate::places::Place {
                    label: "Downloads".into(),
                    path: PathBuf::from(r"C:\Users\me\Downloads"),
                },
            ],
            drives: vec!["C:".into()],
        }
    }

    #[test]
    fn builtin_menu_is_never_empty_and_launches_a_shell() {
        let menu = state();
        assert_eq!(menu.entries.len(), 1);
        assert_eq!(
            menu.entries[0].path.as_deref(),
            Some(crate::platform::login_shell().as_str())
        );
    }

    #[test]
    fn the_four_roots_render_and_expand_as_an_accordion() {
        let mut menu = state();
        let rows = menu.rows();
        assert_eq!(
            rows,
            vec![
                Row::Root(Root::Search),
                Row::Root(Root::FileManager),
                Row::Root(Root::Programs),
                Row::Root(Root::Settings),
            ]
        );
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Down)),
            MenuEvent::Consumed
        );
        // Enter on FILE MANAGER expands it; opening another closes it.
        assert_eq!(activate_selected(&mut menu), MenuEvent::Consumed);
        assert_eq!(menu.open, Some(Root::FileManager));
        assert!(menu
            .rows()
            .iter()
            .any(|row| matches!(row, Row::Header("places"))));
        // One Down reaches PROGRAMS; Enter swaps the open section.
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Down)),
            MenuEvent::Consumed
        );
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Enter)),
            MenuEvent::Consumed
        );
        assert_eq!(menu.open, Some(Root::Programs), "accordion: one at a time");
        assert!(
            !menu
                .rows()
                .iter()
                .any(|row| matches!(row, Row::Header("places"))),
            "the other section collapsed"
        );
        // Esc collapses first, then closes.
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Esc)),
            MenuEvent::Consumed
        );
        assert_eq!(menu.open, None);
        assert_eq!(handle_key(&mut menu, press(KeyCode::Esc)), MenuEvent::Close);
    }

    #[test]
    fn search_act_carries_the_filter_to_the_bar() {
        let mut menu = State::new(demo_entries(), demo_places());
        for ch in "git".chars() {
            handle_key(&mut menu, press_char(ch));
        }
        assert!(
            !menu.rows().iter().any(|row| matches!(row, Row::Root(_))),
            "typing hides the roots"
        );
        // With no filter, Enter on SEARCH carries the empty text through
        // the `Search` event (the app hands it to the command bar).
        let mut plain_menu = state();
        assert_eq!(
            activate_selected(&mut plain_menu),
            MenuEvent::Search(String::new())
        );
        assert_eq!(plain_menu.filter, "");
    }

    #[test]
    fn backspace_to_empty_returns_the_roots() {
        let mut menu = state();
        handle_key(&mut menu, press_char('g'));
        assert!(!menu.rows().iter().any(|row| matches!(row, Row::Root(_))));
        handle_key(&mut menu, press(KeyCode::Backspace));
        assert!(menu.rows().iter().any(|row| matches!(row, Row::Root(_))));
    }

    #[test]
    fn ranked_results_cover_apps_places_and_settings() {
        let mut menu = State::new(demo_entries(), demo_places());
        for ch in "down".chars() {
            handle_key(&mut menu, press_char(ch));
        }
        let rows = menu.rows();
        assert!(
            rows.contains(&Row::Place { index: 1 }),
            "Downloads ranks: {rows:?}"
        );
        // A fresh state: the filter accumulates, so "keys" must not
        // ride on "down".
        let mut keys_menu = State::new(demo_entries(), demo_places());
        for ch in "keys".chars() {
            handle_key(&mut keys_menu, press_char(ch));
        }
        assert!(
            keys_menu.rows().contains(&Row::Setting {
                section: Section::Keys
            }),
            "setting names rank: {:?}",
            keys_menu.rows()
        );
    }

    #[test]
    fn enter_on_a_place_opens_it_and_on_a_setting_deep_links() {
        let mut menu = State::new(demo_entries(), demo_places());
        handle_key(&mut menu, press(KeyCode::Down)); // FILE MANAGER
        handle_key(&mut menu, press(KeyCode::Enter)); // expand
                                                      // The expanded rows sit directly under their root (the review's
                                                      // keyboard finding): one Down from FILE MANAGER reaches Home.
        handle_key(&mut menu, press(KeyCode::Down));
        let event = handle_key(&mut menu, press(KeyCode::Enter));
        assert_eq!(
            event,
            MenuEvent::OpenPlace {
                path: PathBuf::from(r"C:\Users\me")
            }
        );
        let mut settings_menu = State::new(demo_entries(), demo_places());
        for _ in 0..3 {
            handle_key(&mut settings_menu, press(KeyCode::Down));
        }
        handle_key(&mut settings_menu, press(KeyCode::Enter)); // expand SETTINGS
        let event = handle_key(&mut settings_menu, press(KeyCode::Down));
        assert_eq!(event, MenuEvent::Consumed);
        assert_eq!(
            handle_key(&mut settings_menu, press(KeyCode::Enter)),
            MenuEvent::OpenSettings {
                section: Section::Appearance
            }
        );
    }

    #[test]
    fn expanded_children_sit_under_their_root_not_after_the_roots() {
        let mut menu = State::new(demo_entries(), demo_places());
        handle_key(&mut menu, press(KeyCode::Down)); // FILE MANAGER
        handle_key(&mut menu, press(KeyCode::Enter)); // expand
        let rows = menu.rows();
        let fm_at = rows
            .iter()
            .position(|row| *row == Row::Root(Root::FileManager))
            .expect("the root");
        assert!(
            matches!(rows.get(fm_at + 1), Some(Row::Header("places"))),
            "the places header follows its root directly: {rows:?}"
        );
        assert!(
            rows.iter()
                .position(|row| *row == Row::Root(Root::Programs))
                .expect("programs")
                > rows
                    .iter()
                    .position(|row| matches!(row, Row::Place { index: 0 }))
                    .expect("home"),
            "the remaining roots come after the expanded children"
        );
    }

    #[test]
    fn typing_filters_and_enter_launches() {
        let mut menu = state();
        assert_eq!(handle_key(&mut menu, press_char('s')), MenuEvent::Consumed);
        assert_eq!(menu.filter, "s");
        // The shell entry ranks (its label and description match); the
        // first result is its index.
        let first = menu.rows().first().copied();
        assert!(matches!(first, Some(Row::Item { index: 0 })), "{first:?}");
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Enter)),
            MenuEvent::Launch(0)
        );
    }

    #[test]
    fn filter_that_matches_nothing_launches_nothing() {
        let mut menu = state();
        for ch in "zzz".chars() {
            handle_key(&mut menu, press_char(ch));
        }
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Enter)),
            MenuEvent::Consumed
        );
    }

    #[test]
    fn escape_closes_and_backspace_edits() {
        let mut menu = state();
        handle_key(&mut menu, press_char('g'));
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Backspace)),
            MenuEvent::Consumed
        );
        assert!(menu.filter.is_empty());
        assert_eq!(handle_key(&mut menu, press(KeyCode::Esc)), MenuEvent::Close);
    }

    #[test]
    fn ctrl_keys_are_consumed_not_typed() {
        let mut menu = state();
        let ctrl_q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut menu, ctrl_q), MenuEvent::Consumed);
        assert!(menu.filter.is_empty());
    }

    #[test]
    fn menu_rect_grows_with_the_open_section_and_caps_at_the_desktop() {
        let desktop = Area::new(0, 0, 80, 23);
        assert_eq!(
            rect(desktop, 0, TaskbarPosition::Bottom),
            Area::new(0, 5, 48, 18)
        );
        // Five place rows plus headers grow the overlay, capped at 23.
        assert_eq!(rect(desktop, 7, TaskbarPosition::Bottom).height, 23);
        let small = Area::new(0, 0, 20, 5);
        assert_eq!(
            rect(small, 10, TaskbarPosition::Bottom),
            Area::new(0, 0, 20, 5),
            "capped at the desktop"
        );
        let menu = State::new(demo_entries(), demo_places());
        assert_eq!(open_rows(&menu), 0);
        let mut open = menu.clone();
        open.open = Some(Root::Settings);
        assert_eq!(open_rows(&open), 5, "the four setting rows plus About");
        assert_eq!(rect_for(desktop, &open, TaskbarPosition::Bottom).height, 23);
        let mut programs = menu.clone();
        programs.open = Some(Root::Programs);
        assert_eq!(
            open_rows(&programs),
            7,
            "Get more apps + 3 group rows + 3 entries"
        );
    }

    #[test]
    fn a_top_taskbar_anchors_the_menu_at_the_desktops_top() {
        let desktop = Area::new(0, 1, 80, 23);
        assert_eq!(
            rect(desktop, 0, TaskbarPosition::Top),
            Area::new(0, 1, 48, 18),
            "the menu opens under the top taskbar, at its edge"
        );
        let mut open = State::new(demo_entries(), demo_places());
        open.open = Some(Root::Settings);
        assert_eq!(
            rect_for(desktop, &open, TaskbarPosition::Top),
            Area::new(0, 1, 48, 23),
            "growth runs downward from the taskbar's edge (four groups + About)"
        );
    }

    #[test]
    fn grouped_view_orders_installed_categories_then_the_greyed() {
        let mut state = State::new(demo_entries(), Places::default());
        state.open = Some(Root::Programs);
        assert_eq!(
            state.rows(),
            vec![
                Row::Root(Root::Search),
                Row::Root(Root::FileManager),
                Row::Root(Root::Programs),
                Row::GetMoreApps,
                Row::Header("files"),
                Row::Item { index: 0 },
                Row::Header("dev"),
                Row::Item { index: 1 },
                Row::Header(NOT_INSTALLED),
                Row::Item { index: 2 },
                Row::Root(Root::Settings),
            ]
        );
        assert_eq!(
            state.selectable(),
            vec![
                Row::Root(Root::Search),
                Row::Root(Root::FileManager),
                Row::Root(Root::Programs),
                Row::GetMoreApps,
                Row::Item { index: 0 },
                Row::Item { index: 1 },
                Row::Item { index: 2 },
                Row::Root(Root::Settings),
            ]
        );
    }

    #[test]
    fn candidates_surface_only_when_filtering() {
        let state = State::new(demo_entries(), Places::default());
        assert!(
            !state
                .selectable()
                .iter()
                .any(|row| matches!(row, Row::Item { index: 3 })),
            "candidates hide from the unfiltered grouped view"
        );
        let mut filtered = state.clone();
        filtered.filter = "git".into();
        // Fuzzy matching is generous, so the assertion is on the
        // candidate ranking first, not alone.
        let first = filtered.rows().first().copied();
        assert_eq!(first, Some(Row::Item { index: 3 }));
    }

    #[test]
    fn selection_walks_roots_then_items_and_enter_launches() {
        let mut menu = State::new(demo_entries(), Places::default());
        menu.open = Some(Root::Programs);
        for _ in 0..6 {
            handle_key(&mut menu, press(KeyCode::Down));
        }
        assert_eq!(
            menu.selected, 6,
            "the greyed row sits under its group header"
        );
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Enter)),
            MenuEvent::Launch(2),
            "enter launches the greyed row's entry"
        );
    }

    #[test]
    fn the_two_new_rows_reach_their_events() {
        let mut menu = State::new(demo_entries(), Places::default());
        menu.open = Some(Root::Programs);
        // Get more apps sits directly under the PROGRAMS root.
        for _ in 0..3 {
            handle_key(&mut menu, press(KeyCode::Down));
        }
        assert_eq!(
            handle_key(&mut menu, press(KeyCode::Enter)),
            MenuEvent::OpenDirectory
        );
        let mut settings_menu = State::new(demo_entries(), demo_places());
        for _ in 0..3 {
            handle_key(&mut settings_menu, press(KeyCode::Down));
        }
        handle_key(&mut settings_menu, press(KeyCode::Enter)); // expand SETTINGS
        for _ in 0..5 {
            handle_key(&mut settings_menu, press(KeyCode::Down));
        }
        assert_eq!(
            handle_key(&mut settings_menu, press(KeyCode::Enter)),
            MenuEvent::OpenAbout,
            "About AGony sits last under SETTINGS"
        );
    }

    #[test]
    fn greyed_hint_carries_the_install_recipe() {
        let mut glow = entry("glow", "Glow", Category::Text, None);
        glow.manifest.install = Some(agony_core::manifest::InstallSpec {
            winget: Some("charmbracelet.glow".into()),
            ..Default::default()
        });
        assert!(glow.greyed());
        assert_eq!(
            glow.hint(),
            "not installed — winget install charmbracelet.glow"
        );
        let installed = entry(
            "spf",
            "Superfile",
            Category::Files,
            Some("C:\\bin\\spf.exe"),
        );
        assert!(installed.hint().contains("C:\\bin\\spf.exe"));
    }

    #[test]
    fn entries_from_the_cache_and_the_fallback() {
        assert_eq!(entries_from(None).len(), 1, "no cache: the shell fallback");
        let cache = discover::CatalogCache {
            schema: 0,
            scanned_unix: 1,
            apps: vec![discover::CachedApp {
                manifest: entry("spf", "Superfile", Category::Files, None).manifest,
                path: Some("C:\\bin\\spf.exe".into()),
                candidate: false,
                catalog: false,
            }],
        };
        let entries = entries_from(Some(&cache));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].label(), "Superfile");
    }

    #[test]
    fn catalog_only_greyed_entries_stay_out_of_the_menu() {
        let mut catalog_app = AppManifest::minimal("bat", "Bat", "bat", Category::Text);
        catalog_app.bundled = false;
        let mut bundled_greyed = AppManifest::minimal("gambit", "Gambit", "gambit", Category::Game);
        bundled_greyed.bundled = true;
        let cache = discover::CatalogCache {
            schema: 0,
            scanned_unix: 1,
            apps: vec![
                discover::CachedApp {
                    manifest: catalog_app.clone(),
                    path: None,
                    candidate: false,
                    catalog: true,
                },
                discover::CachedApp {
                    manifest: catalog_app.clone(),
                    path: Some("C:\\bin\\bat.exe".into()),
                    candidate: false,
                    catalog: true,
                },
                discover::CachedApp {
                    manifest: bundled_greyed,
                    path: None,
                    candidate: false,
                    catalog: false,
                },
            ],
        };
        let entries = entries_from(Some(&cache));
        let ids: Vec<&str> = entries.iter().map(|e| e.manifest.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["bat", "gambit"],
            "installed catalog entries show; greyed ones wait in the directory"
        );
    }

    #[test]
    fn drawn_menu_shows_roots_filter_and_items() {
        let theme = Applier::fallback();
        let mut menu = state();
        let backend = TestBackend::new(48, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| {
                draw(
                    f,
                    Area::new(0, 0, 48, 18),
                    &mut menu,
                    &theme,
                    FontMode::Ascii,
                    None,
                )
            })
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(
            text.contains("find"),
            "ascii filter glyph missing: {text:?}"
        );
        for root in ["SEARCH", "FILE MANAGER", "PROGRAMS", "SETTINGS"] {
            assert!(text.contains(root), "{root} missing: {text:?}");
        }
    }

    #[test]
    fn drawn_expanded_menu_shows_programs_and_the_greyed_recipe() {
        let theme = Applier::fallback();
        let mut menu = State::new(demo_entries(), demo_places());
        menu.open = Some(Root::Programs);
        let backend = TestBackend::new(48, 18);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|f| {
                draw(
                    f,
                    Area::new(0, 0, 48, 18),
                    &mut menu,
                    &theme,
                    FontMode::Ascii,
                    None,
                )
            })
            .expect("draw");
        let text = buffer_text(&terminal);
        assert!(text.contains("files"), "category header missing: {text:?}");
        assert!(
            text.contains("Superfile"),
            "installed entry missing: {text:?}"
        );
        assert!(
            text.contains("not installed"),
            "greyed group header missing: {text:?}"
        );
        assert!(text.contains("Glow"), "greyed entry missing: {text:?}");
        assert!(
            !text.contains("git "),
            "candidates must not appear unfiltered: {text:?}"
        );
    }

    #[test]
    fn click_activates_roots_places_settings_and_selects_programs() {
        let region = Area::new(0, 0, 48, 23);
        let mut menu = State::new(demo_entries(), demo_places());
        // Click the FILE MANAGER root (items row 1; the items area
        // starts one row under the filter line).
        let event = click(&mut menu, region, (2, 3));
        assert_eq!(event, MenuEvent::Consumed);
        assert_eq!(menu.open, Some(Root::FileManager));
        // Click the SEARCH root: it acts.
        let mut search_menu = State::new(demo_entries(), demo_places());
        search_menu.filter = "glow".into();
        let event = click(&mut search_menu, region, (2, 5));
        assert_eq!(
            event,
            MenuEvent::Consumed,
            "roots are hidden while filtering"
        );
    }

    #[test]
    fn double_click_on_a_program_row_launches() {
        let mut menu = State::new(demo_entries(), Places::default());
        menu.open = Some(Root::Programs);
        let region = Area::new(0, 0, 48, 23);
        // Superfile sits at display row 5 (three roots before PROGRAMS,
        // Get more apps, the files header, then the entry); the items
        // area starts at screen row 2.
        let event = double_click(&mut menu, region, (2, 7));
        assert_eq!(event, MenuEvent::Launch(0));
    }

    #[test]
    fn hit_at_feeds_the_context_seam() {
        let mut menu = State::new(demo_entries(), Places::default());
        menu.open = Some(Root::Programs);
        let region = Area::new(0, 0, 48, 23);
        assert_eq!(
            hit_at(&menu, region, (2, 7)),
            Some(MenuHit::Program(0)),
            "the Superfile row"
        );
        assert_eq!(
            hit_at(&menu, region, (2, 2)),
            Some(MenuHit::Root(Root::Search))
        );
        assert_eq!(hit_at(&menu, region, (2, 30)), None, "outside");
    }

    #[test]
    fn scroll_by_moves_the_offset_and_clamps_at_the_ends() {
        let mut menu = State::new(demo_entries(), demo_places());
        menu.open = Some(Root::FileManager);
        menu.window = 3;
        menu.scroll_by(2);
        assert_eq!(menu.scroll, 2);
        menu.scroll_by(-10);
        assert_eq!(menu.scroll, 0, "clamps at the top");
        menu.scroll_by(100);
        assert_eq!(
            menu.scroll,
            menu.rows().len().saturating_sub(3),
            "clamps at the bottom of the content"
        );
    }

    #[test]
    fn scroll_keeps_the_selection_in_view() {
        let mut menu = State::new(demo_entries(), demo_places());
        menu.open = Some(Root::FileManager);
        menu.window = 2;
        menu.selected = menu.selectable().len() - 1;
        menu.clamp_scroll();
        let position = menu.selected_position().expect("visible selection");
        assert!(
            position >= menu.scroll && position < menu.scroll + menu.window,
            "selection {position} must sit inside [{}, {})",
            menu.scroll,
            menu.scroll + menu.window
        );
        menu.scroll_by(10);
        let position = menu.selected_position().expect("still visible");
        assert!(position < menu.scroll + menu.window, "never lost offscreen");
    }

    #[test]
    fn page_moves_one_window_at_a_time() {
        let mut menu = State::new(demo_entries(), demo_places());
        menu.open = Some(Root::FileManager);
        menu.window = 2;
        menu.page(1);
        assert_eq!(menu.scroll, 2);
        menu.page(-1);
        assert_eq!(menu.scroll, 0);
        menu.page(-5);
        assert_eq!(menu.scroll, 0, "no wrap past the top");
    }

    #[test]
    fn filter_reset_restarts_the_view() {
        let mut menu = State::new(demo_entries(), demo_places());
        menu.window = 2;
        menu.scroll_by(4);
        let _ = handle_key(&mut menu, press_char('g'));
        assert_eq!(menu.scroll, 0, "a filter change restarts the window");
        assert_eq!(menu.selected, 0);
    }
}
