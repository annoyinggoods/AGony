//! The app-manifest schema: how AGony describes a launchable program.
//!
//! One TOML file per app under an `[app]` table; the same struct serves
//! built-ins, the app directory's catalog entries, user manifests, and
//! the store fields (`source`/`license`/`install`/`bundled`).
//! Items: `AppManifest` — one app; `Category` — the seven fixed groups;
//! `InstallSpec` — the per-source install recipes; `ReleaseSpec` — one
//! hash-pinned release-asset download; `ManifestFile` — the `[app]`-
//! wrapped file format; `CatalogFile` — the `[[app]]`-array catalog
//! format (Part11); `Catalog` — the merged list; `LoadIssue` — one
//! rejected file; `ManifestError` — parse/validate failures.
//! Core functions: `AppManifest::validate` — id/icon/source/license
//! shape plus the licensing rule (a `bundled` manifest never carries a
//! GPL license — 10.3.3, made mechanical); `AppManifest::minimal` — the
//! one synthetic-app constructor; `AppManifest::install_recipe` — the
//! first available install command; `license_is_copyleft` — the GPL
//! shelf's test; `load_catalog` — the one merge (built-ins, then the
//! catalog, then user files; last writer wins on id collision);
//! `catalog_from_toml` — the catalog file format; `category_from_name`
//! — the CLI's category spelling.
//! Depends on: serde, toml, thiserror, crate::icon (icon-name validation).
//! Called by: the shell's discovery (it resolves the directories; parsing
//! and merging happen here, in the pure core).
//! Calls: nothing outside this crate.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::icon;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("manifest parse failed: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("manifest serialize failed: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("manifest id must not be empty")]
    EmptyId,
    #[error("manifest id must be lowercase ascii (letters, digits, -_.): {0}")]
    IdShape(String),
    #[error("unknown icon name: {0}")]
    UnknownIcon(String),
    #[error("source must be an http(s) URL: {0}")]
    SourceShape(String),
    #[error("license must be a short SPDX-style string: {0}")]
    LicenseShape(String),
    #[error(
        "a bundled manifest never carries a copyleft license ({0}) — \
             GPL apps ship as external downloads only (10.3.3)"
    )]
    BundledCopyleft(String),
    #[error("release recipe needs a 64-hex sha256: {0}")]
    ReleaseHash(String),
}

/// The seven fixed start-menu groups, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Files,
    Shell,
    Text,
    Dev,
    Game,
    Net,
    System,
}

impl Category {
    pub const ALL: [Category; 7] = [
        Category::Files,
        Category::Shell,
        Category::Text,
        Category::Dev,
        Category::Game,
        Category::Net,
        Category::System,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Files => "files",
            Category::Shell => "shell",
            Category::Text => "text",
            Category::Dev => "dev",
            Category::Game => "game",
            Category::Net => "net",
            Category::System => "system",
        }
    }
}

/// The category behind a CLI-spelled name, or None when unknown.
pub fn category_from_name(name: &str) -> Option<Category> {
    Category::ALL.iter().copied().find(|c| c.as_str() == name)
}

/// Install recipes per source, in dispatch order (Part11 §11.4.1:
/// winget, then scoop, then cargo/go; `release` is the hash-pinned
/// release-asset download Part10's bundle installer runs).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub winget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoop: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub go: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release: Option<ReleaseSpec>,
}

/// One hash-checked release-asset download: the bundle installer's
/// whole input (10.3.2). The URL pins the version; the sha256 pins the
/// bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseSpec {
    pub url: String,
    pub sha256: String,
}

/// One launchable program as every surface sees it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub exec: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub category: Category,
    #[serde(default = "default_icon")]
    pub icon: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install: Option<InstallSpec>,
    /// Part of the bundled set (10.3.1): the bundle installer may fetch
    /// it. The validator refuses `bundled` with a copyleft license —
    /// GPL apps are external downloads only, never a convenience bundle.
    #[serde(default, skip_serializing_if = "is_false")]
    pub bundled: bool,
    /// The extensions this app opens (Part05 5.6.8), lowercase, no dots
    /// — glow declares `["md", "markdown"]`. Enter on a matching file in
    /// the panel launches the app with the file as its argument.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_with: Vec<String>,
    #[serde(default = "default_true")]
    pub terminal_app: bool,
}

fn default_icon() -> String {
    "app".into()
}

fn default_true() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// The `[app]`-wrapped file format: one manifest per TOML file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ManifestFile {
    pub app: AppManifest,
}

/// The app directory's catalog file format (Part11 §11.2): one TOML
/// file, many `[[app]]` entries — the checked-in seed and the remote
/// source share it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CatalogFile {
    #[serde(default)]
    pub app: Vec<AppManifest>,
}

impl ManifestFile {
    pub fn from_toml(text: &str) -> Result<ManifestFile, ManifestError> {
        Ok(toml::from_str(text)?)
    }

    pub fn to_toml(&self) -> Result<String, ManifestError> {
        Ok(toml::to_string_pretty(self)?)
    }
}

impl CatalogFile {
    /// Parse a catalog file and validate every entry; the first invalid
    /// entry fails the whole file (a catalog is curated, not user data).
    pub fn from_toml(text: &str) -> Result<CatalogFile, ManifestError> {
        let file: CatalogFile = toml::from_str(text)?;
        for manifest in &file.app {
            manifest.validate()?;
        }
        Ok(file)
    }

    pub fn to_toml(&self) -> Result<String, ManifestError> {
        Ok(toml::to_string_pretty(self)?)
    }
}

impl AppManifest {
    pub fn from_toml(text: &str) -> Result<AppManifest, ManifestError> {
        Ok(ManifestFile::from_toml(text)?.app)
    }

    /// The minimal three-field manifest: everything optional defaults, the
    /// description echoes the name. The one constructor every caller with a
    /// synthetic app uses (discovery's candidates, the shell's tests) — so
    /// the twelve-field literal is written once (audit F5).
    pub fn minimal(id: &str, name: &str, exec: &str, category: Category) -> AppManifest {
        AppManifest {
            id: id.into(),
            name: name.into(),
            description: name.into(),
            exec: exec.into(),
            args: Vec::new(),
            category,
            icon: "app".into(),
            source: None,
            license: None,
            install: None,
            bundled: false,
            open_with: Vec::new(),
            terminal_app: true,
        }
    }

    /// Whether this app declares itself an opener for `ext` (lowercase,
    /// no dot — the panel's Enter-on-file lookup).
    pub fn opens_extension(&self, ext: &str) -> bool {
        self.open_with.iter().any(|declared| declared == ext)
    }

    /// The schema's own rules: known id shape, known icon, URL-shaped
    /// source, SPDX-shaped license, hash-shaped release recipe — and the
    /// licensing rule: a `bundled` manifest never carries a copyleft
    /// license (10.3.3, the standing decision made mechanical).
    pub fn validate(&self) -> Result<(), ManifestError> {
        id_shape(&self.id)?;
        if icon::from_name(&self.icon).is_none() {
            return Err(ManifestError::UnknownIcon(self.icon.clone()));
        }
        check_source(&self.source)?;
        check_license(&self.license)?;
        check_release(&self.install)?;
        if self.bundled && license_is_copyleft(&self.license) {
            return Err(ManifestError::BundledCopyleft(
                self.license.clone().unwrap_or_default(),
            ));
        }
        Ok(())
    }

    /// The first available install command, or None when the manifest
    /// carries no recipe at all. `release` sorts last: the package
    /// managers come first, the bundle download is the AGony-native
    /// fallback (Part11 §11.4.1's dispatch order).
    pub fn install_recipe(&self) -> Option<String> {
        let from_install = self.install.as_ref().and_then(|install| {
            [
                install
                    .winget
                    .as_ref()
                    .map(|id| format!("winget install {id}")),
                install
                    .scoop
                    .as_ref()
                    .map(|id| format!("scoop install {id}")),
                install
                    .cargo
                    .as_ref()
                    .map(|id| format!("cargo install {id}")),
                install
                    .go
                    .as_ref()
                    .map(|id| format!("go install {id}@latest")),
                install
                    .release
                    .as_ref()
                    .map(|_| format!("agony apps bundle --install {}", self.id)),
            ]
            .into_iter()
            .flatten()
            .next()
        });
        from_install.or_else(|| self.source.as_ref().map(|url| format!("see {url}")))
    }
}

fn id_shape(id: &str) -> Result<(), ManifestError> {
    if id.is_empty() {
        return Err(ManifestError::EmptyId);
    }
    let ok = id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
    if ok {
        Ok(())
    } else {
        Err(ManifestError::IdShape(id.to_string()))
    }
}

fn check_source(source: &Option<String>) -> Result<(), ManifestError> {
    let Some(url) = source else {
        return Ok(());
    };
    let shaped = url.starts_with("https://") || url.starts_with("http://");
    if shaped {
        Ok(())
    } else {
        Err(ManifestError::SourceShape(url.clone()))
    }
}

fn check_license(license: &Option<String>) -> Result<(), ManifestError> {
    let Some(text) = license else {
        return Ok(());
    };
    let ok = !text.is_empty()
        && text.len() <= 64
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '+' | '(' | ')' | ' '));
    if ok {
        Ok(())
    } else {
        Err(ManifestError::LicenseShape(text.clone()))
    }
}

/// Whether a license string marks a copyleft (GPL-family) app — the GPL
/// shelf's test (10.3.3). Unknown or absent licenses are not copyleft.
pub fn license_is_copyleft(license: &Option<String>) -> bool {
    license
        .as_deref()
        .map(|text| {
            let upper = text.to_uppercase();
            upper.starts_with("GPL") || upper.starts_with("AGPL") || upper.starts_with("LGPL")
        })
        .unwrap_or(false)
}

fn check_release(install: &Option<InstallSpec>) -> Result<(), ManifestError> {
    let Some(spec) = install
        .as_ref()
        .and_then(|install| install.release.as_ref())
    else {
        return Ok(());
    };
    let shaped = spec.url.starts_with("https://") || spec.url.starts_with("http://");
    if !shaped {
        return Err(ManifestError::SourceShape(spec.url.clone()));
    }
    let hex = spec.sha256.len() == 64 && spec.sha256.chars().all(|c| c.is_ascii_hexdigit());
    if !hex {
        return Err(ManifestError::ReleaseHash(spec.sha256.clone()));
    }
    Ok(())
}

/// The merged app list: built-ins first (in their given order), then the
/// user's own apps sorted by id; a user manifest with a built-in's id
/// replaces it — override, never duplicate.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub apps: Vec<AppManifest>,
}

/// One user file that failed to parse or validate; skipped, not fatal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadIssue {
    pub file: String,
    pub error: String,
}

/// The one loader and the one merge (Part03 3.2.4, grown a middle layer
/// by Part11 §11.2.3): built-ins, then the app directory's catalog, then
/// the user's manifests — each layer wins on id collision with the one
/// before it. The caller reads the files; this function parses, validates
/// and merges — the merge logic lives here and nowhere else.
pub fn load_catalog(
    builtins: &[AppManifest],
    catalog: &[AppManifest],
    user_files: &[(&str, &str)],
) -> (Catalog, Vec<LoadIssue>) {
    let (user, issues) = parse_user_files(user_files);
    (
        Catalog {
            apps: merge_layers(&[builtins, catalog, &user]),
        },
        issues,
    )
}

fn parse_user_files(user_files: &[(&str, &str)]) -> (Vec<AppManifest>, Vec<LoadIssue>) {
    let mut user = Vec::new();
    let mut issues = Vec::new();
    for (file, text) in user_files {
        match AppManifest::from_toml(text).and_then(|manifest| {
            manifest.validate()?;
            Ok(manifest)
        }) {
            Ok(manifest) => user.push(manifest),
            Err(error) => issues.push(LoadIssue {
                file: (*file).to_string(),
                error: error.to_string(),
            }),
        }
    }
    user.sort_by(|a, b| a.id.cmp(&b.id));
    (user, issues)
}

/// The one merge: layers in order, a later layer's id replaces an
/// earlier one's — override, never duplicate.
pub fn merge_layers(layers: &[&[AppManifest]]) -> Vec<AppManifest> {
    let mut index_of: HashMap<String, usize> = HashMap::new();
    let mut apps: Vec<AppManifest> = Vec::new();
    for layer in layers {
        for manifest in *layer {
            match index_of.get(&manifest.id) {
                Some(&at) => apps[at] = manifest.clone(),
                None => {
                    index_of.insert(manifest.id.clone(), apps.len());
                    apps.push(manifest.clone());
                }
            }
        }
    }
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, name: &str, category: Category) -> AppManifest {
        let mut m = AppManifest::minimal(id, name, id, category);
        m.description = format!("{name} description");
        m
    }

    const SAMPLE: &str = r#"
[app]
id = "superfile"
name = "Superfile"
description = "A pretty and modern terminal file manager"
exec = "spf"
args = []
category = "files"
icon = "folder"
source = "https://github.com/yorukot/superfile"
license = "MIT"
terminal_app = true
"#;

    #[test]
    fn sample_manifest_parses_and_validates() {
        let manifest = AppManifest::from_toml(SAMPLE).expect("sample parses");
        manifest.validate().expect("sample validates");
        assert_eq!(manifest.id, "superfile");
        assert_eq!(manifest.category, Category::Files);
        assert_eq!(manifest.icon, "folder");
        assert!(manifest.terminal_app);
        assert!(manifest.install.is_none());
    }

    #[test]
    fn toml_round_trips_with_optional_fields() {
        let manifest = AppManifest::from_toml(SAMPLE).expect("sample parses");
        let text = ManifestFile {
            app: manifest.clone(),
        }
        .to_toml()
        .expect("serialize");
        let reparsed = AppManifest::from_toml(&text).expect("own output parses");
        assert_eq!(reparsed, manifest);
    }

    #[test]
    fn unknown_category_is_rejected() {
        let text = SAMPLE.replace("category = \"files\"", "category = \"archives\"");
        assert!(AppManifest::from_toml(&text).is_err());
    }

    #[test]
    fn missing_category_is_rejected() {
        let text = SAMPLE.replace("category = \"files\"\n", "");
        assert!(AppManifest::from_toml(&text).is_err());
    }

    #[test]
    fn empty_id_is_rejected() {
        let manifest = manifest("", "Nope", Category::System);
        assert!(matches!(manifest.validate(), Err(ManifestError::EmptyId)));
    }

    #[test]
    fn uppercase_id_is_rejected() {
        let manifest = manifest("MyTool", "MyTool", Category::System);
        assert!(matches!(
            manifest.validate(),
            Err(ManifestError::IdShape(_))
        ));
    }

    #[test]
    fn unknown_icon_is_rejected() {
        let mut bad = manifest("x", "X", Category::System);
        bad.icon = "sparkles".into();
        assert!(matches!(bad.validate(), Err(ManifestError::UnknownIcon(_))));
    }

    #[test]
    fn bad_source_and_license_are_rejected() {
        let mut bad = manifest("x", "X", Category::System);
        bad.source = Some("github.com/yorukot/superfile".into());
        assert!(matches!(bad.validate(), Err(ManifestError::SourceShape(_))));
        bad.source = Some("https://github.com/yorukot/superfile".into());
        bad.license = Some("MIT\tOR\tlater with tabs".into());
        assert!(matches!(
            bad.validate(),
            Err(ManifestError::LicenseShape(_))
        ));
        bad.license = Some("Apache-2.0 OR MIT".into());
        bad.validate().expect("fixed fields validate");
    }

    #[test]
    fn install_recipe_prefers_winget_then_down_the_list() {
        let mut m = manifest("x", "X", Category::System);
        assert_eq!(m.install_recipe(), None);
        m.install = Some(InstallSpec {
            go: Some("github.com/yorukot/superfile".into()),
            scoop: Some("superfile".into()),
            ..InstallSpec::default()
        });
        assert_eq!(
            m.install_recipe().as_deref(),
            Some("scoop install superfile")
        );
        m.install = None;
        m.source = Some("https://example.com/x".into());
        assert_eq!(
            m.install_recipe().as_deref(),
            Some("see https://example.com/x")
        );
    }

    #[test]
    fn open_with_matches_extensions_caselessly_by_declaration() {
        let mut opener = manifest("glow", "Glow", Category::Text);
        assert!(!opener.opens_extension("md"));
        opener.open_with = vec!["md".into(), "markdown".into()];
        assert!(opener.opens_extension("md"));
        assert!(opener.opens_extension("markdown"));
        assert!(!opener.opens_extension("mdx"));
        // The lookup lowercases the file's extension before asking.
        assert!(!opener.opens_extension("MD"), "callers lowercase first");
    }

    #[test]
    fn category_names_round_trip() {
        for category in Category::ALL {
            assert_eq!(category_from_name(category.as_str()), Some(category));
        }
        assert_eq!(category_from_name("archives"), None);
    }

    #[test]
    fn user_manifest_with_a_builtin_id_replaces_it() {
        let builtins = [manifest("glow", "Glow", Category::Text)];
        let mut replacement = manifest("glow", "My Glow", Category::Text);
        replacement.exec = "myglow".into();
        let (catalog, issues) = load_catalog(
            &builtins,
            &[],
            &[("glow.toml", "[app]\nid = \"glow\"\nname = \"My Glow\"\nexec = \"myglow\"\ncategory = \"text\"\n")],
        );
        assert!(issues.is_empty());
        assert_eq!(catalog.apps.len(), 1);
        assert_eq!(catalog.apps[0].name, "My Glow");
        assert_eq!(catalog.apps[0].exec, "myglow");
        let _ = replacement;
    }

    #[test]
    fn user_only_manifests_are_appended_sorted_by_id() {
        let builtins = [manifest("shell", "Shell", Category::Shell)];
        let mine_b = "[app]\nid = \"b-tool\"\nname = \"B\"\nexec = \"b\"\ncategory = \"dev\"\n";
        let mine_a = "[app]\nid = \"a-tool\"\nname = \"A\"\nexec = \"a\"\ncategory = \"dev\"\n";
        let (catalog, issues) =
            load_catalog(&builtins, &[], &[("b.toml", mine_b), ("a.toml", mine_a)]);
        assert!(issues.is_empty());
        let ids: Vec<&str> = catalog.apps.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["shell", "a-tool", "b-tool"]);
    }

    #[test]
    fn invalid_user_files_become_issues_not_failures() {
        let builtins = [];
        let (catalog, issues) = load_catalog(&builtins, &[], &[("bad.toml", "[app]\nid = \"\"\n")]);
        assert!(catalog.apps.is_empty());
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].file, "bad.toml");
    }

    #[test]
    fn merge_keeps_builtin_order_when_no_user_files() {
        let builtins = [
            manifest("shell", "Shell", Category::Shell),
            manifest("glow", "Glow", Category::Text),
        ];
        let (catalog, issues) = load_catalog(&builtins, &[], &[]);
        assert!(issues.is_empty());
        assert_eq!(catalog.apps.len(), 2);
        assert_eq!(catalog.apps[0].id, "shell");
    }

    #[test]
    fn invariant_bundled_refuses_copyleft_license() {
        let mut gpl = manifest("flapioca", "Flapioca", Category::Game);
        gpl.license = Some("GPL-3.0".into());
        gpl.validate().expect("gpl manifest alone validates");
        gpl.bundled = true;
        assert_eq!(
            gpl.validate(),
            Err(ManifestError::BundledCopyleft("GPL-3.0".into()))
        );
        // The whole copyleft family is refused, MIT is not.
        for license in ["AGPL-3.0", "LGPL-2.1", "GPL-2.0-only"] {
            gpl.license = Some(license.into());
            assert!(matches!(
                gpl.validate(),
                Err(ManifestError::BundledCopyleft(_))
            ));
        }
        gpl.license = Some("MIT".into());
        gpl.validate().expect("bundled MIT validates");
    }

    #[test]
    fn license_is_copyleft_covers_the_gpl_family_only() {
        assert!(!license_is_copyleft(&None));
        assert!(!license_is_copyleft(&Some("MIT".into())));
        assert!(!license_is_copyleft(&Some("Apache-2.0".into())));
        assert!(license_is_copyleft(&Some("GPL-3.0".into())));
        assert!(license_is_copyleft(&Some("gpl-3.0-or-later".into())));
        assert!(license_is_copyleft(&Some("LGPL-2.1".into())));
        assert!(license_is_copyleft(&Some("AGPL-3.0".into())));
    }

    #[test]
    fn release_recipe_needs_an_https_url_and_a_hex_hash() {
        let mut m = manifest("gambit", "Gambit", Category::Game);
        m.install = Some(InstallSpec {
            release: Some(ReleaseSpec {
                url: "ftp://example.com/x.zip".into(),
                sha256: "a".repeat(64),
            }),
            ..InstallSpec::default()
        });
        assert!(matches!(m.validate(), Err(ManifestError::SourceShape(_))));
        m.install = Some(InstallSpec {
            release: Some(ReleaseSpec {
                url: "https://example.com/x.zip".into(),
                sha256: "nothex".into(),
            }),
            ..InstallSpec::default()
        });
        assert!(matches!(m.validate(), Err(ManifestError::ReleaseHash(_))));
        m.install = Some(InstallSpec {
            release: Some(ReleaseSpec {
                url: "https://example.com/x.zip".into(),
                sha256: "ABCDEF".into(),
            }),
            ..InstallSpec::default()
        });
        assert!(matches!(m.validate(), Err(ManifestError::ReleaseHash(_))));
        m.install = Some(InstallSpec {
            release: Some(ReleaseSpec {
                url: "https://example.com/x.zip".into(),
                sha256: "a".repeat(64),
            }),
            ..InstallSpec::default()
        });
        m.validate().expect("a well-shaped release validates");
    }

    #[test]
    fn install_recipe_shows_the_bundle_command_last() {
        let mut m = manifest("gambit", "Gambit", Category::Game);
        m.install = Some(InstallSpec {
            release: Some(ReleaseSpec {
                url: "https://example.com/x.zip".into(),
                sha256: "a".repeat(64),
            }),
            ..InstallSpec::default()
        });
        assert_eq!(
            m.install_recipe().as_deref(),
            Some("agony apps bundle --install gambit")
        );
    }

    #[test]
    fn catalog_file_parses_and_validates_every_entry() {
        let text = r#"
[[app]]
id = "bat"
name = "Bat"
exec = "bat"
category = "text"
license = "MIT OR Apache-2.0"

[[app]]
id = "flapioca"
name = "Flapioca"
exec = "flapioca"
category = "game"
license = "GPL-3.0"
"#;
        let file = CatalogFile::from_toml(text).expect("catalog parses");
        assert_eq!(file.app.len(), 2);
        assert_eq!(file.app[0].id, "bat");
        assert_eq!(file.app[1].license.as_deref(), Some("GPL-3.0"));
        let bad = text.replace("category = \"game\"", "category = \"archives\"");
        assert!(CatalogFile::from_toml(&bad).is_err(), "invalid entry fails");
    }

    #[test]
    fn merge_layers_last_layer_wins_per_id() {
        let builtin = manifest("glow", "Glow", Category::Text);
        let mut catalog_entry = manifest("glow", "Catalog Glow", Category::Text);
        catalog_entry.exec = "glow-catalog".into();
        let user_file = manifest("glow", "My Glow", Category::Text);
        let merged = merge_layers(&[&[builtin], &[catalog_entry], &[user_file]]);
        assert_eq!(merged.len(), 1, "one id, one entry");
        assert_eq!(
            merged[0].name, "My Glow",
            "user beats catalog beats builtin"
        );
        let layers = [
            [manifest("a", "A", Category::Dev)],
            [manifest("b", "B", Category::Dev)],
        ];
        let merged = merge_layers(&[&layers[0], &layers[1]]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].id, "a");
    }

    // ------------------------------------------- property tests (audit)
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(crate::proptest_cfg::config())]

        /// The minimal constructor's output survives its own TOML round
        /// trip and validates, for every category.
        #[test]
        fn prop_minimal_manifest_toml_round_trips(
            id in "[a-z0-9._-]{1,12}",
            name in "[a-zA-Z ]{1,12}",
            category in proptest::sample::select(Category::ALL.to_vec()),
        ) {
            let manifest = AppManifest::minimal(&id, &name, "exec-x", category);
            manifest.validate().expect("minimal validates");
            let text = ManifestFile { app: manifest.clone() }
                .to_toml()
                .expect("serializes");
            let parsed = AppManifest::from_toml(&text).expect("own output parses");
            prop_assert_eq!(parsed, manifest);
        }

        /// validate() on a minimal manifest fails on the id shape and
        /// nothing else — the documented charset, empty refused.
        #[test]
        fn prop_minimal_validate_only_checks_the_id(id in any::<String>()) {
            let manifest = AppManifest::minimal(&id, "n", "e", Category::Dev);
            let shaped = !id.is_empty()
                && id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'));
            match manifest.validate() {
                Ok(()) => prop_assert!(shaped, "bad id {:?} accepted", id),
                Err(ManifestError::IdShape(_) | ManifestError::EmptyId) => {
                    prop_assert!(!shaped, "good id {:?} refused", id);
                }
                Err(other) => panic!("minimal failed on {}", other),
            }
        }

        /// license_is_copyleft is exactly the GPL/AGPL/LGPL prefix rule.
        #[test]
        fn prop_license_is_copyleft_is_the_prefix_rule(text in "[a-zA-Z0-9 .+-]{0,20}") {
            let upper = text.to_uppercase();
            let expected = upper.starts_with("GPL")
                || upper.starts_with("AGPL")
                || upper.starts_with("LGPL");
            prop_assert_eq!(license_is_copyleft(&Some(text)), expected);
        }

        /// merge_layers keeps one entry per id, later layers winning.
        #[test]
        fn prop_merge_layers_last_wins_per_id(
            layers in proptest::collection::vec(
                proptest::collection::vec(("[a-z]{1,3}", 0u8..3), 0..5).prop_map(|specs| {
                    specs
                        .into_iter()
                        .map(|(id, pick)| {
                            manifest(&id, &format!("n{pick}"), Category::ALL[pick as usize])
                        })
                        .collect::<Vec<_>>()
                }),
                0..4,
            ),
        ) {
            let refs: Vec<&[AppManifest]> = layers.iter().map(|layer| layer.as_slice()).collect();
            let merged = merge_layers(&refs);
            let ids: std::collections::HashSet<&str> =
                merged.iter().map(|m| m.id.as_str()).collect();
            prop_assert_eq!(ids.len(), merged.len(), "duplicate ids survived");
            for app in &merged {
                let last = layers.iter().flatten().rev().find(|m| m.id == app.id);
                prop_assert_eq!(last, Some(app), "layer order broke for {}", app.id);
            }
        }
    }

    // -------------------------------- mutation follow-ups (audit §2)

    #[test]
    fn terminal_app_defaults_to_true_when_omitted() {
        let text = "[app]\nid = \"x\"\nname = \"X\"\nexec = \"x\"\ncategory = \"dev\"\n";
        let parsed = AppManifest::from_toml(text).expect("parses");
        assert!(
            parsed.terminal_app,
            "omitted terminal_app must default true"
        );
        assert!(!parsed.bundled, "omitted bundled must default false");
    }

    #[test]
    fn bundled_false_is_skipped_in_toml_and_true_round_trips() {
        let plain = manifest("plain", "Plain", Category::Dev);
        let text = ManifestFile { app: plain.clone() }
            .to_toml()
            .expect("serializes");
        assert!(
            !text.contains("bundled"),
            "a false bundled is skipped:\n{text}"
        );
        let parsed = AppManifest::from_toml(&text).expect("reparses");
        assert_eq!(parsed, plain);
        let mut bundled = manifest("bundled", "B", Category::Dev);
        bundled.bundled = true;
        let text = ManifestFile {
            app: bundled.clone(),
        }
        .to_toml()
        .expect("serializes");
        let parsed = AppManifest::from_toml(&text).expect("reparses");
        assert_eq!(parsed, bundled, "a true bundled survives the round trip");
    }

    #[test]
    fn catalog_file_to_toml_round_trips() {
        let file = CatalogFile {
            app: vec![
                manifest("a", "A", Category::Dev),
                manifest("b", "B", Category::Text),
            ],
        };
        let text = file.to_toml().expect("serializes");
        let parsed = CatalogFile::from_toml(&text).expect("reparses");
        assert_eq!(parsed.app, file.app);
    }
}
