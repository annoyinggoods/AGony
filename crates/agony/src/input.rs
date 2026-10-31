//! Key forwarding: crossterm events in, the core's mapping table, the pane
//! keyboard out. Migrated from the spike's `forward_key` (lines 182-222);
//! the table lives in `agony_core::inputmap`, only the async call lives
//! here.
//!
//! Core functions: `forward_key` — one key press to the pane via the
//! core's `Action`; `combo_of` / `normalized_combo` — crossterm to core
//! key conversion; `matches_combo` — reserved-key matching (normalized, so
//! Ctrl-Q matches "ctrl-q").
//! Depends on: agony_core::inputmap (the table), rmuxhost (pane errors).
//! Called by: app (the input path).
//! Calls: rmux_sdk pane keyboard.

use agony_core::inputmap::{self, Action, Combo, Key, Mods};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rmux_sdk::Pane;

use crate::rmuxhost::RmuxHostError;

/// Send one key press to the pane, decided entirely by the core's table.
/// Unmappable keys (F-keys, media keys…) are dropped, never forwarded as a
/// NUL keystroke (audit F6).
pub async fn forward_key(pane: &Pane, key: KeyEvent) -> Result<(), RmuxHostError> {
    let Some(combo) = combo_of(key) else {
        return Ok(());
    };
    let action = inputmap::action_for(&combo);
    let keyboard = pane.keyboard();
    match action {
        Action::Type(text) => keyboard
            .type_text(text)
            .await
            .map_err(|e| RmuxHostError::Input(e.to_string())),
        Action::Press(name) => keyboard
            .press(name)
            .await
            .map_err(|e| RmuxHostError::Input(e.to_string())),
        Action::Ignore => Ok(()),
    }
}

/// The core combo for a crossterm key, case preserved (typing needs it).
/// None when the key has no core-table representation.
pub fn combo_of(key: KeyEvent) -> Option<Combo> {
    Some(Combo {
        key: key_of(key.code)?,
        mods: mods_of(key.modifiers),
    })
}

/// The normalized combo (chars lowercased) — what reserved keys compare
/// against.
pub fn normalized_combo(key: KeyEvent) -> Option<Combo> {
    let mut combo = combo_of(key)?;
    if let Key::Char(ch) = combo.key {
        combo.key = Key::Char(ch.to_ascii_lowercase());
    }
    Some(combo)
}

pub fn matches_combo(key: KeyEvent, combo: &Combo) -> bool {
    normalized_combo(key) == Some(*combo)
}

fn key_of(code: KeyCode) -> Option<Key> {
    let key = match code {
        KeyCode::Char(ch) => Key::Char(ch),
        KeyCode::Enter => Key::Enter,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Esc => Key::Escape,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        _ => return None,
    };
    Some(key)
}

fn mods_of(modifiers: KeyModifiers) -> Mods {
    Mods {
        ctrl: modifiers.contains(KeyModifiers::CONTROL),
        alt: modifiers.contains(KeyModifiers::ALT),
        shift: modifiers.contains(KeyModifiers::SHIFT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn named_keys_map_to_the_core_table() {
        assert_eq!(key_of(KeyCode::Enter), Some(Key::Enter));
        assert_eq!(key_of(KeyCode::BackTab), Some(Key::BackTab));
        assert_eq!(key_of(KeyCode::Left), Some(Key::Left));
    }

    #[test]
    fn unmapped_keys_are_dropped_not_nul() {
        // F-keys and unmapped codes have no core-table entry: they must not
        // become a NUL typed into the pane (audit F6).
        assert_eq!(key_of(KeyCode::F(5)), None);
        assert_eq!(key_of(KeyCode::Null), None);
        assert_eq!(combo_of(key(KeyCode::F(5), KeyModifiers::empty())), None);
    }

    #[test]
    fn combo_matching_is_case_insensitive() {
        let spec = inputmap::combo_from_str("ctrl-q").expect("default parses");
        let pressed = key(KeyCode::Char('Q'), KeyModifiers::CONTROL);
        assert!(matches_combo(pressed, &spec));
    }

    #[test]
    fn combo_matching_rejects_other_keys() {
        let spec = inputmap::combo_from_str("ctrl-q").expect("default parses");
        let plain = key(KeyCode::Char('q'), KeyModifiers::empty());
        assert!(!matches_combo(plain, &spec));
    }

    #[test]
    fn start_menu_default_matches_ctrl_space() {
        let spec = inputmap::combo_from_str(inputmap::DEFAULT_START_MENU).expect("parses");
        let pressed = key(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert!(matches_combo(pressed, &spec));
    }
}
