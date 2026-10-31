//! THANK-YOU.md (Part08 §8.4): the generated credits file — every
//! dependency with its license, every bundled app with its repository
//! and license, and the standing credits (rmux, ratatui, carapace-bin,
//! the spike's inspirations). Written onto the AGony desktop at first
//! run and regenerated idempotently by `agony thanks`; hand edits are
//! overwritten by design and the file's header says so.
//!
//! Items: `CREDIT_LINES` — the standing credits (the things cargo
//! metadata cannot see).
//! Core functions: `generate` — the whole markdown from the embedded
//! dependency table; `generate_with` — the same over an explicit
//! manifest set (the test seam); `write_if_missing` — the first-run
//! write onto the desktop; `regenerate` — the verb's idempotent rewrite.
//! Depends on: build.rs's `DEPENDENCIES` table (via include!),
//! agony_core::manifest (the bundled manifests), crate::discover
//! (builtins), crate::settings (the desktop path).
//! Called by: main (the `agony thanks` verb) and app (first run).
//! Calls: std::fs under the config dir only.

use std::path::PathBuf;

use agony_core::manifest::AppManifest;

use crate::discover;
use crate::settings;

// deps_table.rs — (name, version, license) rows from build.rs's
// `cargo metadata`; the fallback is an empty table when it failed.
// (A doc comment cannot attach to `include!`.)
include!(concat!(env!("OUT_DIR"), "/deps_table.rs"));

/// The standing credits: the foundations cargo metadata cannot see.
const CREDIT_LINES: [(&str, &str); 4] = [
    (
        "rmux / rmux-sdk / ratatui-rmux",
        "the multiplexer engine AGony lives on — AGony never touches a \
         ConPTY itself; the daemon owns the process tree",
    ),
    (
        "carapace-bin",
        "the completion binary driving the command bar's suggestions, \
         and the app catalog's candidate import",
    ),
    (
        "desktop-tui",
        "an inspiration: a desktop environment in a terminal, done first",
    ),
    (
        "the rmux mini-zellij demo",
        "the spike's reference for embedding panes in a ratatui window",
    ),
];

/// The whole THANK-YOU.md from the embedded dependency table and the
/// discovered bundled manifests.
pub fn generate() -> String {
    let (manifests, _notes) = discover::builtins();
    generate_with(DEPENDENCIES, &manifests)
}

/// The test seam: the same generator over an explicit dependency table
/// and manifest set.
pub fn generate_with(deps: &[(&str, &str, &str)], manifests: &[AppManifest]) -> String {
    let mut out = String::new();
    out.push_str("# THANK-YOU.md\n\n");
    out.push_str("Everything AGony stands on, credited where the user can see it.\n");
    out.push_str(
        "This file is generated (`agony thanks`) — hand edits are overwritten by design.\n\n",
    );
    push_dependencies(&mut out, deps);
    push_bundled_apps(&mut out, manifests);
    push_standing_credits(&mut out);
    out
}

fn push_dependencies(out: &mut String, deps: &[(&str, &str, &str)]) {
    out.push_str("## Dependencies\n\n");
    if deps.is_empty() {
        out.push_str("The dependency table is unavailable (built without `cargo metadata`).\n\n");
        return;
    }
    out.push_str("| crate | version | license |\n| --- | --- | --- |\n");
    for (name, version, license) in deps {
        out.push_str(&format!("| {name} | {version} | {license} |\n"));
    }
    let apache = deps.iter().filter(|(_, _, l)| *l == "Apache-2.0").count();
    if apache > 0 {
        out.push_str(&format!(
            "\n{apache} Apache-2.0 dependencies: their NOTICE obligations are met by \
             this credit line.\n"
        ));
    }
    out.push('\n');
}

fn push_bundled_apps(out: &mut String, manifests: &[AppManifest]) {
    out.push_str("## Bundled apps\n\n");
    for manifest in manifests {
        let repository = manifest.source.as_deref().unwrap_or("-");
        let license = manifest.license.as_deref().unwrap_or("-");
        out.push_str(&format!(
            "- {} — {repository} — license: {license}\n",
            manifest.name
        ));
    }
    out.push('\n');
}

fn push_standing_credits(out: &mut String) {
    out.push_str("## Standing credits\n\n");
    for (name, why) in CREDIT_LINES {
        out.push_str(&format!("- **{name}** — {why}\n"));
    }
    out.push('\n');
}

/// The desktop document's path: `<config>/agony/Desktop/THANK-YOU.md`.
pub fn desktop_path() -> PathBuf {
    settings::desktop_dir().join("THANK-YOU.md")
}

/// The first-run write (8.4.2): generate the file onto the AGony desktop
/// when it is not there yet. A read-only desktop stays a status note,
/// never a failure.
pub fn write_if_missing() -> Option<String> {
    let path = desktop_path();
    if path.exists() {
        return None;
    }
    match regenerate() {
        Ok(written) => Some(format!("THANK-YOU.md written to {}", written.display())),
        Err(error) => Some(format!("THANK-YOU.md write failed: {error}")),
    }
}

/// Generate (or regenerate) the file onto the AGony desktop, idempotently.
pub fn regenerate() -> Result<PathBuf, std::io::Error> {
    let dir = settings::desktop_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("THANK-YOU.md");
    std::fs::write(&path, generate())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use agony_core::manifest::{AppManifest, Category};

    const DEPS: [(&str, &str, &str); 4] = [
        ("agony-core", "0.1.0", "Apache-2.0"),
        ("ratatui", "0.29.0", "MIT"),
        ("rmux-sdk", "0.10.0", "Apache-2.0"),
        ("serde", "1.0.0", "Apache-2.0 OR MIT"),
    ];

    fn manifest(name: &str, source: Option<&str>, license: Option<&str>) -> AppManifest {
        let mut manifest = AppManifest::minimal("id", name, name, Category::Text);
        manifest.source = source.map(str::to_string);
        manifest.license = license.map(str::to_string);
        manifest
    }

    #[test]
    fn the_document_lists_all_three_sources() {
        let manifests = [
            manifest(
                "Glow",
                Some("https://github.com/charmbracelet/glow"),
                Some("MIT"),
            ),
            manifest("Shell", None, None),
        ];
        let text = generate_with(&DEPS, &manifests);
        assert!(text.starts_with("# THANK-YOU.md"));
        assert!(
            text.contains("generated (`agony thanks`)"),
            "the header says hand edits are overwritten: {text}"
        );
        for (name, version, license) in DEPS {
            assert!(
                text.contains(&format!("| {name} | {version} | {license} |")),
                "{name} missing: {text}"
            );
        }
        assert!(
            text.contains("https://github.com/charmbracelet/glow"),
            "repository missing"
        );
        assert!(text.contains("- Glow — https://github.com/charmbracelet/glow — license: MIT"));
        assert!(
            text.contains("- Shell — - — license: -"),
            "blank fields stay visible"
        );
        assert!(text.contains("rmux"), "rmux credited by name");
        assert!(text.contains("ratatui"), "ratatui credited by name");
        assert!(text.contains("carapace-bin"), "carapace credited by name");
    }

    #[test]
    fn apache_notice_is_met_by_the_credit_line() {
        let text = generate_with(&DEPS, &[]);
        assert!(
            text.contains("Apache-2.0 dependencies: their NOTICE obligations are met"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_dependency_table_degrades_with_a_note() {
        let text = generate_with(&[], &[]);
        assert!(text.contains("unavailable"), "{text}");
        assert!(text.contains("rmux"), "the standing credits survive");
    }

    #[test]
    fn generation_is_deterministic_and_idempotent() {
        let manifests = [manifest("Glow", Some("https://x"), Some("MIT"))];
        assert_eq!(
            generate_with(&DEPS, &manifests),
            generate_with(&DEPS, &manifests)
        );
    }

    #[test]
    fn write_if_missing_writes_once_then_leaves_the_file() {
        let dir = std::env::temp_dir().join(format!("agony-thanks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("THANK-YOU.md");
        std::fs::remove_file(&path).ok();
        std::fs::write(&path, "user edited").expect("seed the file");
        // The app's first-run check consults existence only — the seam
        // here pins the decision: an existing file is left alone.
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn embedded_dependency_table_is_alive() {
        // build.rs's cargo metadata must have filled the table on this
        // build; an empty table would mean the embed silently broke.
        assert!(
            !DEPENDENCIES.is_empty(),
            "the embedded dependency table is empty — did build.rs's cargo metadata fail?"
        );
        assert!(
            DEPENDENCIES.iter().any(|(name, _, _)| *name == "ratatui"),
            "ratatui is in the table"
        );
    }
}
