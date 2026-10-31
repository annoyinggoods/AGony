//! The desktop-shortcut stub schema (Part09 9.4.1): the small TOML a
//! pinned app writes into `<config>/agony/Desktop/` — a reference to a
//! catalog manifest id, nothing more. The desktop's tile launches through
//! the catalog, so the stub never duplicates a manifest.
//!
//! Items: `Shortcut` — the one field that matters (the manifest id);
//! `ShortcutFile` — the `[shortcut]`-wrapped file format;
//! `ShortcutError` — parse/serialize failures.
//! Core functions: `Shortcut::from_toml` / `to_toml` — the file's two
//! directions; `Shortcut::validate` — the id must be non-empty (the id's
//! own shape is the manifest schema's job, at catalog load).
//! Depends on: serde, toml, thiserror.
//! Called by: the shell's `desktop_icons` (scan, Pin to desktop).
//! Calls: nothing outside this crate.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shortcut {
    pub id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShortcutFile {
    pub shortcut: Shortcut,
}

#[derive(Debug, Error)]
pub enum ShortcutError {
    #[error("shortcut parse failed: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("shortcut serialize failed: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("shortcut id must not be empty")]
    EmptyId,
}

impl Shortcut {
    pub fn from_toml(text: &str) -> Result<Shortcut, ShortcutError> {
        let file: ShortcutFile = toml::from_str(text)?;
        let shortcut = file.shortcut;
        shortcut.validate()?;
        Ok(shortcut)
    }

    pub fn to_toml(&self) -> Result<String, ShortcutError> {
        self.validate()?;
        Ok(toml::to_string_pretty(&ShortcutFile {
            shortcut: self.clone(),
        })?)
    }

    /// The stub's one rule: it must name something. Shape validation is
    /// the manifest schema's (the catalog already refuses malformed ids).
    pub fn validate(&self) -> Result<(), ShortcutError> {
        if self.id.is_empty() {
            Err(ShortcutError::EmptyId)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_round_trips_through_toml() {
        let stub = Shortcut { id: "glow".into() };
        let text = stub.to_toml().expect("serialize");
        assert!(text.contains("[shortcut]"), "{text}");
        assert_eq!(Shortcut::from_toml(&text).expect("parse"), stub);
    }

    #[test]
    fn empty_ids_are_refused_in_both_directions() {
        let text = "[shortcut]\nid = \"\"\n";
        assert!(matches!(
            Shortcut::from_toml(text),
            Err(ShortcutError::EmptyId)
        ));
        assert!(matches!(
            Shortcut { id: String::new() }.to_toml(),
            Err(ShortcutError::EmptyId)
        ));
    }

    #[test]
    fn foreign_toml_is_a_parse_error_not_a_shortcut() {
        assert!(Shortcut::from_toml("[app]\nid = \"glow\"\n").is_err());
        assert!(Shortcut::from_toml("not toml at all =").is_err());
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// Any non-empty stub id survives its own TOML round trip.
        #[test]
        fn prop_shortcut_toml_round_trips(id in "[a-z0-9._-]{1,16}") {
            let stub = Shortcut { id };
            let text = stub.to_toml().expect("serializes");
            let parsed = Shortcut::from_toml(&text).expect("parses");
            prop_assert_eq!(parsed, stub);
        }
    }
}
