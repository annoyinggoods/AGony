//! The `agony apps` verbs: `add` writes a user manifest from flags,
//! `list` prints the effective catalog with each app's install state,
//! `bundle` shows or drives the bundled set (Part10 §10.3.2), and
//! `install` runs any manifest's recipe (Part11 §11.4). The verbs are
//! the manual path into the catalog (Part03 3.3.4) — no daemon, no UI.
//!
//! Items: `AddSpec` — the flags of one `apps add`; `AppsError` — the
//! verbs' failures.
//! Core functions: `add` — validate + write `<config>/agony/apps/<id>.toml`
//! (refuses to overwrite); `add_in` — the same against an injected dir (the
//! tests' seam); `list` — a synchronous (throttled) refresh, then one line
//! per app; `find_app` — the merged catalog lookup every install verb
//! shares; `bundle_install` — the bundled set's hash-checked release
//! download (refuses non-bundled apps — the GPL shelf's mechanical rule);
//! `install` — Part11's recipe dispatch over the same installer.
//! Depends on: agony_core::manifest (schema, validation, the merge),
//! crate::discover (dirs, refresh), crate::installer (the one install
//! path), crate::settings (config dir).
//! Called by: main (the `apps` subcommand).
//! Calls: std::fs under the config dir only; the installer's
//! subprocesses.

use std::fs;
use std::path::{Path, PathBuf};

use agony_core::manifest::{category_from_name, AppManifest, ManifestError, ManifestFile};
use thiserror::Error;

use crate::discover;
use crate::installer;
use crate::settings;

#[derive(Debug, Error)]
pub enum AppsError {
    #[error("apps io failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Manifest(#[from] ManifestError),
    #[error("unknown category: {0}")]
    UnknownCategory(String),
    #[error("user manifest already exists: {0} (edit or delete it first)")]
    Exists(String),
    #[error("no such app in the catalog: {0}")]
    UnknownApp(String),
    #[error("{0}")]
    Refusal(String),
    #[error("{0}")]
    Install(#[from] installer::InstallError),
}

#[derive(Clone, Debug)]
pub struct AddSpec {
    pub id: String,
    pub name: String,
    pub exec: String,
    pub description: String,
    pub category: String,
    pub icon: String,
    pub args: Vec<String>,
    pub one_shot: bool,
}

/// Write a user manifest into the real apps dir and invalidate the cached
/// scan, so the change reaches the menu at the next startup.
pub fn add(spec: AddSpec) -> Result<PathBuf, AppsError> {
    let path = add_in(&discover::apps_dir(), spec)?;
    discover::invalidate_cache();
    Ok(path)
}

/// The same, against an injected directory. Refuses to overwrite an
/// existing file — the user's edits always win.
pub fn add_in(dir: &Path, spec: AddSpec) -> Result<PathBuf, AppsError> {
    let manifest = build_manifest(spec)?;
    fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.toml", manifest.id));
    if path.exists() {
        return Err(AppsError::Exists(path.display().to_string()));
    }
    let text = ManifestFile { app: manifest }.to_toml()?;
    fs::write(&path, text)?;
    Ok(path)
}

fn build_manifest(spec: AddSpec) -> Result<AppManifest, AppsError> {
    let category = category_from_name(&spec.category)
        .ok_or_else(|| AppsError::UnknownCategory(spec.category.clone()))?;
    let manifest = AppManifest {
        id: spec.id,
        name: spec.name,
        description: spec.description,
        exec: spec.exec,
        args: spec.args,
        category,
        icon: spec.icon,
        source: None,
        license: None,
        install: None,
        bundled: false,
        open_with: Vec::new(),
        terminal_app: !spec.one_shot,
    };
    manifest.validate()?;
    Ok(manifest)
}

/// One catalog line per app, after a synchronous (throttled) refresh.
pub async fn list() -> Result<(), AppsError> {
    let config_dir = settings::config_dir();
    let (cache, notes) = discover::refresh(&config_dir)
        .await
        .map_err(|error| AppsError::Io(std::io::Error::other(error.to_string())))?;
    for note in notes {
        println!("note    {note}");
    }
    println!(
        "catalog scanned {} — {} app(s), cache: {}",
        cache.scanned_unix,
        cache.apps.len(),
        discover::cache_path(&config_dir).display()
    );
    for app in &cache.apps {
        println!("{}", list_line(app));
    }
    Ok(())
}

fn list_line(app: &discover::CachedApp) -> String {
    let manifest = &app.manifest;
    let state = if app.candidate {
        "candidate (carapace)".to_string()
    } else {
        match &app.path {
            Some(path) => format!("installed: {path}"),
            None => match manifest.install_recipe() {
                Some(recipe) => format!("not installed ({recipe})"),
                None => "not installed".to_string(),
            },
        }
    };
    let origin = if app.catalog { " [catalog]" } else { "" };
    format!(
        "{:<14} {:<20} {:<7} {}{}",
        manifest.id,
        truncate(&manifest.name, 20),
        manifest.category.as_str(),
        state,
        origin
    )
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max - 1).collect();
    format!("{cut}…")
}

// ------------------------------------------------------------- installs

/// The merged catalog lookup every install verb shares: a throttled
/// refresh, then one app by id.
async fn find_app(id: &str) -> Result<discover::CachedApp, AppsError> {
    let config_dir = settings::config_dir();
    let (cache, _notes) = discover::refresh(&config_dir)
        .await
        .map_err(|error| AppsError::Io(std::io::Error::other(error.to_string())))?;
    cache
        .apps
        .into_iter()
        .find(|app| app.manifest.id == id)
        .ok_or_else(|| AppsError::UnknownApp(id.to_string()))
}

/// `agony apps bundle` (10.3.1): the bundled set, one line per app.
pub async fn bundle_list() -> Result<(), AppsError> {
    let config_dir = settings::config_dir();
    let (cache, _notes) = discover::refresh(&config_dir)
        .await
        .map_err(|error| AppsError::Io(std::io::Error::other(error.to_string())))?;
    let bundled: Vec<_> = cache
        .apps
        .iter()
        .filter(|app| app.manifest.bundled)
        .collect();
    if bundled.is_empty() {
        println!("no bundled apps in the catalog");
        return Ok(());
    }
    println!("the bundled set (`agony apps bundle --install <id>` installs):");
    for app in bundled {
        println!("  {}", list_line(app));
    }
    Ok(())
}

/// `agony apps bundle --install <id>` (10.3.2): the hash-checked
/// release download. Only the bundled set rides this path — a refusal
/// for anything else is the GPL shelf's mechanical rule, made here.
pub async fn bundle_install(id: &str) -> Result<(), AppsError> {
    let app = find_app(id).await?;
    let manifest = &app.manifest;
    if !manifest.bundled {
        let license = manifest.license.as_deref().unwrap_or("unknown license");
        let recipe = manifest
            .install_recipe()
            .unwrap_or_else(|| format!("see {}", manifest.source.as_deref().unwrap_or("-")));
        return Err(AppsError::Refusal(format!(
            "{id} is not a bundled app (license: {license}) — AGony never auto-downloads \
             it; install it yourself: {recipe}"
        )));
    }
    let Some(release) = manifest.install.as_ref().and_then(|i| i.release.clone()) else {
        let recipe = manifest
            .install_recipe()
            .unwrap_or_else(|| format!("see {}", manifest.source.as_deref().unwrap_or("-")));
        return Err(AppsError::Refusal(format!(
            "{id} has no hash-checked Windows asset to fetch — install it directly: {recipe}"
        )));
    };
    println!("{id}: downloading {}", release.url);
    let installed = installer::install_release(&release, manifest, &installer::bin_dir()).await?;
    discover::invalidate_cache();
    println!("{id}: hash verified — installed to {}", installed.display());
    println!(
        "note: {} is scanned by AGony's discovery; add it to PATH to run {id} from any shell",
        installer::bin_dir().display()
    );
    Ok(())
}

/// `agony apps install <id>` (Part11 §11.4.1): the recipe dispatch —
/// winget, scoop, cargo/go when their driver exists, the release
/// download last. One progress line, one result line; a failure prints
/// the recipe's own output tail.
pub async fn install(id: &str) -> Result<(), AppsError> {
    let app = find_app(id).await?;
    let manifest = app.manifest.clone();
    let chosen = installer::choose_recipe(&manifest).ok_or_else(|| {
        AppsError::Refusal(format!(
            "{id} has no install recipe — see {}",
            manifest.source.as_deref().unwrap_or("its source")
        ))
    })?;
    match chosen {
        installer::Chosen::Package { command, .. } => {
            println!("{id}: {command}");
            let tail = installer::run_package(&command).await?;
            discover::invalidate_cache();
            println!("{id}: done{}", tail_tail(&tail));
        }
        installer::Chosen::Release(release) => {
            println!("{id}: downloading {}", release.url);
            let installed =
                installer::install_release(&release, &manifest, &installer::bin_dir()).await?;
            discover::invalidate_cache();
            println!("{id}: hash verified — installed to {}", installed.display());
            println!(
                "note: {} is scanned by AGony's discovery; add it to PATH to run {id} from any shell",
                installer::bin_dir().display()
            );
        }
    }
    Ok(())
}

fn tail_tail(tail: &str) -> String {
    if tail.is_empty() {
        String::new()
    } else {
        format!("\n{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agony_core::manifest::{Category, InstallSpec};

    fn spec(id: &str, exec: &str) -> AddSpec {
        AddSpec {
            id: id.into(),
            name: "My Tool".into(),
            exec: exec.into(),
            description: "a manual app".into(),
            category: "dev".into(),
            icon: "app".into(),
            args: vec!["--verbose".into()],
            one_shot: false,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let unique = format!("agony-apps-{tag}-{}", std::process::id());
        let dir = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn add_writes_a_valid_manifest_file() {
        let dir = temp_dir("add");
        let path = add_in(&dir, spec("mytool", "mytool")).expect("add works");
        let text = fs::read_to_string(&path).expect("manifest readable");
        let manifest = AppManifest::from_toml(&text).expect("written file parses");
        assert_eq!(manifest.id, "mytool");
        assert_eq!(manifest.args, vec!["--verbose".to_string()]);
        assert!(manifest.terminal_app);
        manifest.validate().expect("written manifest validates");
    }

    #[test]
    fn add_refuses_to_overwrite_and_rejects_bad_values() {
        let dir = temp_dir("add-twice");
        add_in(&dir, spec("mytool", "mytool")).expect("first add works");
        let again = add_in(&dir, spec("mytool", "mytool"));
        assert!(matches!(again, Err(AppsError::Exists(_))));
        let mut bad = spec("mytool2", "x");
        bad.category = "archives".into();
        assert!(matches!(
            add_in(&dir, bad),
            Err(AppsError::UnknownCategory(_))
        ));
        let mut invalid = spec("Bad Id", "x");
        invalid.category = "dev".into();
        assert!(matches!(add_in(&dir, invalid), Err(AppsError::Manifest(_))));
    }

    #[test]
    fn list_line_shows_state_per_app() {
        let mut installed = discover::CachedApp {
            manifest: AppManifest::minimal("spf", "Superfile", "spf", Category::Files),
            path: Some("C:\\bin\\spf.exe".into()),
            candidate: false,
            catalog: false,
        };
        assert!(list_line(&installed).contains("installed: C:\\bin\\spf.exe"));
        installed.path = None;
        assert!(list_line(&installed).contains("not installed"));
        installed.manifest.install = Some(InstallSpec {
            scoop: Some("superfile".into()),
            ..InstallSpec::default()
        });
        assert!(list_line(&installed).contains("scoop install superfile"));
        installed.candidate = true;
        assert!(list_line(&installed).contains("candidate"));
    }
}
