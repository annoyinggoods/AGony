//! agony-core: the pure data-and-math half of AGony.
//!
//! By construction this crate touches no platform: no OS conditionals, no
//! multiplexer, no filesystem, no clock — `tests/freedom.rs` greps `src/`
//! so that stays true.
//! Modules: `layout` — frame and window-manager math (Part04), the icon
//! grid and its cursor (Part09); `inputmap` — the neutral key table and
//! reserved-key combos; `thememodel` — palette schema, built-ins,
//! contrast validation and override packs (Part08); `config` — the
//! settings model; `icon` — the glyph-or-ASCII table and tile initials;
//! `manifest` — the app-manifest schema and the catalog merge; `suggest`
//! — fuzzy ranking, dedup and path search; `tree` — the labeled tree and
//! its cursor (Part05); `pathpick` — the picked-file forms (Part05);
//! `pointer` — the gesture machine (Part06); `cmdline` — the
//! command-line tokenizer and editor (Part07); `history` — the
//! run-history model (Part07); `shortcut` — the desktop-shortcut stub
//! schema (Part09).
//! Depends on: ratatui (types only, no backend features), serde/serde_json,
//! toml, thiserror.
//! Called by: the `agony` shell crate, which resolves all platform facts.
//! Calls: nothing outside this crate.

pub mod cmdline;
pub mod config;
pub mod history;
pub mod icon;
pub mod inputmap;
pub mod layout;
pub mod manifest;
pub mod pathpick;
pub mod pointer;
pub mod shortcut;
pub mod suggest;
pub mod textutil;
pub mod thememodel;
pub mod tree;

/// Test-only: the one proptest configuration every property test
/// shares — a fixed RNG seed so runs are reproducible, and persistence
/// fully off, so a failing input never writes into the repository (the
/// seed and input print to the test's stdout instead).
#[cfg(test)]
pub(crate) mod proptest_cfg {
    use proptest::prelude::*;

    pub(crate) fn config() -> ProptestConfig {
        ProptestConfig {
            cases: 256,
            source_file: None,
            failure_persistence: None,
            rng_seed: proptest::test_runner::RngSeed::Fixed(20261026),
            ..ProptestConfig::default()
        }
    }
}
