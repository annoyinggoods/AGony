//! The installer (Part10 §10.3.2, generalized into Part11 §11.4.1):
//! recipe dispatch over the manifest's declared recipes, and the one
//! release-asset path — download → verify sha256 → extract → atomic
//! move into `<config>/agony/bin`. Built for the half-install rule: a
//! failure anywhere before the move leaves the bin dir untouched, the
//! temp dir cleaned, and the greyed entry greyed.
//!
//! Items: `Chosen` — the dispatch's pick (a package-manager recipe or
//! the release download); `Recipe` — the five kinds in dispatch order;
//! `InstallError` — every failure with the user-facing detail attached.
//! Core functions: `choose_recipe_with` — the pure dispatch (winget,
//! scoop, cargo, go — each only when its driver is on PATH — then
//! release); `install_release` — the download pipeline (curl + sha2 +
//! bsdtar, subprocesses, never the UI thread); `find_executable` — the
//! extracted tree walk matching the manifest's exec;
//! `run_package` — one package-manager command, output tail returned;
//! `output_tail` — the last lines of any command's output.
//! Depends on: agony_core::manifest (ReleaseSpec, AppManifest), discover
//! (exec_key), settings (the bin dir), rmuxhost (no-window spawn).
//! Called by: apps (the `bundle --install` and `install` verbs), the
//! directory window (Part11 §11.3.2).
//! Calls: std::fs under the config dir only; tokio process for curl /
//! tar / the package manager.

use std::path::{Path, PathBuf};

use agony_core::manifest::{AppManifest, ReleaseSpec};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::discover;
use crate::rmuxhost;
use crate::settings;

/// The package-manager recipe kinds, in dispatch order (Part11
/// §11.4.1). The release download is not here: it is `Chosen::Release`,
/// the fallback the dispatch reaches when no driver matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recipe {
    Winget,
    Scoop,
    Cargo,
    Go,
}

impl Recipe {
    /// The command's driver program, for the PATH check.
    fn driver(self) -> &'static str {
        match self {
            Recipe::Winget => "winget",
            Recipe::Scoop => "scoop",
            Recipe::Cargo => "cargo",
            Recipe::Go => "go",
        }
    }
}

/// What the dispatch picked for one manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chosen {
    /// A package-manager recipe, spelled as the user would type it.
    Package { recipe: Recipe, command: String },
    /// The hash-pinned release download (Part10's mechanics).
    Release(ReleaseSpec),
}

#[derive(Debug, Error)]
pub enum InstallError {
    #[error("io failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("download failed: {0}")]
    Download(String),
    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("extraction failed: {0}")]
    Extract(String),
    #[error("no {exec} found inside the downloaded archive")]
    ExecutableNotFound { exec: String },
    #[error("{command} failed:\n{tail}")]
    CommandFailed { command: String, tail: String },
}

/// The dispatch (pure): the first declared package recipe whose driver
/// exists on PATH; then the release download (self-contained, beats a
/// command that would fail on a missing tool); then — no driver, no
/// release — the first declared package recipe, whose own output names
/// the missing tool (Part11 §11.4.2's rule).
pub fn choose_recipe_with(
    manifest: &AppManifest,
    tool_on_path: &dyn Fn(&str) -> bool,
) -> Option<Chosen> {
    let install = manifest.install.as_ref()?;
    let declared = [
        (Recipe::Winget, install.winget.as_ref()),
        (Recipe::Scoop, install.scoop.as_ref()),
        (Recipe::Cargo, install.cargo.as_ref()),
        (Recipe::Go, install.go.as_ref()),
    ];
    for (recipe, id) in declared {
        let Some(id) = id else {
            continue;
        };
        if tool_on_path(recipe.driver()) {
            return Some(Chosen::Package {
                recipe,
                command: package_command(recipe, id.clone()),
            });
        }
    }
    if let Some(release) = release_or_none(manifest) {
        return Some(release);
    }
    declared.iter().find_map(|(recipe, id)| {
        id.map(|id| Chosen::Package {
            recipe: *recipe,
            command: package_command(*recipe, id.clone()),
        })
    })
}

fn release_or_none(manifest: &AppManifest) -> Option<Chosen> {
    manifest
        .install
        .as_ref()
        .and_then(|install| install.release.clone())
        .map(Chosen::Release)
}

fn package_command(recipe: Recipe, id: String) -> String {
    match recipe {
        Recipe::Winget => format!("winget install {id}"),
        Recipe::Scoop => format!("scoop install {id}"),
        Recipe::Cargo => format!("cargo install {id}"),
        Recipe::Go => format!("go install {id}@latest"),
    }
}

/// The dispatch over the real PATH.
pub fn choose_recipe(manifest: &AppManifest) -> Option<Chosen> {
    choose_recipe_with(manifest, &path_has)
}

fn path_has(tool: &str) -> bool {
    rmuxhost::find_command_path(tool).is_some()
}

/// The bin dir every release install lands in.
pub fn bin_dir() -> PathBuf {
    settings::bundle_bin_dir()
}

/// The release pipeline (10.3.2): temp dir under the config (so the
/// final move stays on one volume), download, verify the hash, extract,
/// locate the manifest's executable, move it in. Any earlier failure
/// cleans up and reports — the bin dir only ever sees a whole file.
pub async fn install_release(
    spec: &ReleaseSpec,
    manifest: &AppManifest,
    bin_dir: &Path,
) -> Result<PathBuf, InstallError> {
    let work = work_dir(manifest);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work)?;
    let outcome = download_verify_extract(spec, manifest, bin_dir, &work).await;
    let _ = std::fs::remove_dir_all(&work);
    outcome
}

async fn download_verify_extract(
    spec: &ReleaseSpec,
    manifest: &AppManifest,
    bin_dir: &Path,
    work: &Path,
) -> Result<PathBuf, InstallError> {
    let file_name = asset_name(&spec.url);
    let archive = work.join(&file_name);
    download(&spec.url, &archive).await?;
    let actual = sha256_file(&archive)?;
    if !actual.eq_ignore_ascii_case(&spec.sha256) {
        return Err(InstallError::HashMismatch {
            expected: spec.sha256.clone(),
            actual,
        });
    }
    let staged = if file_name.to_lowercase().ends_with(".exe") {
        archive.clone()
    } else {
        let extracted = work.join("extracted");
        std::fs::create_dir_all(&extracted)?;
        extract(&archive, &extracted).await?;
        find_executable(&extracted, &manifest.exec)?
    };
    std::fs::create_dir_all(bin_dir)?;
    let dest = bin_dir.join(
        staged
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| manifest.exec.clone()),
    );
    // std::fs::rename refuses to replace an existing file on Windows —
    // the previous install goes first, after the new one is whole.
    let _ = std::fs::remove_file(&dest);
    std::fs::rename(&staged, &dest)?;
    Ok(dest)
}

fn work_dir(manifest: &AppManifest) -> PathBuf {
    settings::config_dir().join(format!("tmp-install-{}", manifest.id))
}

/// The URL's last segment — the download's local file name.
fn asset_name(url: &str) -> String {
    url.rsplit('/').next().unwrap_or("download.bin").to_string()
}

async fn download(url: &str, dest: &Path) -> Result<(), InstallError> {
    let mut command = tokio::process::Command::new("curl");
    command
        .arg("-sSL")
        .arg("--fail")
        .arg("--max-time")
        .arg("600")
        .arg("-o")
        .arg(dest)
        .arg(url);
    rmuxhost::set_no_window(command.as_std_mut());
    let output = tokio::time::timeout(std::time::Duration::from_secs(660), command.output())
        .await
        .map_err(|_| InstallError::Download("timed out after 660s".into()))?
        .map_err(|error| InstallError::Download(error.to_string()))?;
    if !output.status.success() {
        return Err(InstallError::Download(format!(
            "curl exited with {:?} for {url}",
            output.status.code()
        )));
    }
    Ok(())
}

/// Extract any archive the platform tar reads — bsdtar on Windows reads
/// zip and tar.*, GNU tar the tar.* family. Subprocess, never in-process
/// unzip: no archive dependency enters the tree for one step.
async fn extract(archive: &Path, dest: &Path) -> Result<(), InstallError> {
    let mut command = tokio::process::Command::new("tar");
    command.arg("-xf").arg(archive).arg("-C").arg(dest);
    rmuxhost::set_no_window(command.as_std_mut());
    let output = tokio::time::timeout(std::time::Duration::from_secs(120), command.output())
        .await
        .map_err(|_| InstallError::Extract("timed out after 120s".into()))?
        .map_err(|error| InstallError::Extract(error.to_string()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(InstallError::Extract(format!(
            "tar exited with {:?}: {}",
            output.status.code(),
            detail.trim()
        )));
    }
    Ok(())
}

/// Walk the extracted tree for the manifest's executable — the one
/// exec-matching rule (discover's `exec_key`: case- and
/// extension-insensitive), depth-limited so a crafted archive cannot
/// loop us.
fn find_executable(dir: &Path, exec: &str) -> Result<PathBuf, InstallError> {
    let key = discover::exec_key(exec);
    let mut found = None;
    walk_for_exec(dir, &key, 0, &mut found);
    found.ok_or(InstallError::ExecutableNotFound {
        exec: exec.to_string(),
    })
}

fn walk_for_exec(dir: &Path, key: &str, depth: usize, found: &mut Option<PathBuf>) {
    if found.is_some() || depth > 6 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_for_exec(&path, key, depth + 1, found);
            continue;
        }
        let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if name.to_lowercase() == key {
            *found = Some(path);
            return;
        }
    }
}

/// One package-manager recipe: spawn, wait, and return the output tail
/// either way — the failure line is the recipe's own output (Part11
/// §11.4.2), never a paraphrase.
pub async fn run_package(command: &str) -> Result<String, InstallError> {
    let mut words = command.split_whitespace();
    let Some(program) = words.next() else {
        return Err(InstallError::CommandFailed {
            command: command.to_string(),
            tail: "empty recipe".into(),
        });
    };
    let args: Vec<&str> = words.collect();
    let mut process = tokio::process::Command::new(program);
    process.args(&args);
    rmuxhost::set_no_window(process.as_std_mut());
    let output = tokio::time::timeout(std::time::Duration::from_secs(900), process.output())
        .await
        .map_err(|_| InstallError::CommandFailed {
            command: command.to_string(),
            tail: "timed out after 900s".into(),
        })?
        .map_err(InstallError::Io)?;
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(InstallError::CommandFailed {
            command: command.to_string(),
            tail: output_tail(&combined, 6),
        });
    }
    Ok(output_tail(&combined, 4))
}

/// The last `keep` non-empty lines of a command's output, one string.
pub fn output_tail(text: &str, keep: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = lines.len().saturating_sub(keep);
    lines[start..].join("\n")
}

fn sha256_file(path: &Path) -> Result<String, InstallError> {
    let bytes = std::fs::read(path)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use agony_core::manifest::{AppManifest, Category, InstallSpec};

    fn manifest_with(install: InstallSpec) -> AppManifest {
        let mut manifest = AppManifest::minimal("x", "X", "x", Category::Dev);
        manifest.install = Some(install);
        manifest
    }

    fn any_tool(_tool: &str) -> bool {
        true
    }

    fn no_tool(tool: &str) -> bool {
        tool == "cargo" // only cargo exists, for the fallback tests
    }

    fn no_tools(_tool: &str) -> bool {
        false
    }

    #[test]
    fn dispatch_follows_winget_scoop_cargo_go_order() {
        let manifest = manifest_with(InstallSpec {
            scoop: Some("superfile".into()),
            winget: Some("yorukot.superfile".into()),
            ..InstallSpec::default()
        });
        let chosen = choose_recipe_with(&manifest, &any_tool).expect("a recipe");
        assert_eq!(
            chosen,
            Chosen::Package {
                recipe: Recipe::Winget,
                command: "winget install yorukot.superfile".into(),
            }
        );
        let manifest = manifest_with(InstallSpec {
            go: Some("github.com/x".into()),
            cargo: Some("x".into()),
            ..InstallSpec::default()
        });
        let chosen = choose_recipe_with(&manifest, &any_tool).expect("a recipe");
        assert_eq!(
            chosen,
            Chosen::Package {
                recipe: Recipe::Cargo,
                command: "cargo install x".into(),
            }
        );
    }

    #[test]
    fn dispatch_skips_missing_drivers_and_falls_back_to_release() {
        let manifest = manifest_with(InstallSpec {
            winget: Some("only-winget".into()),
            release: Some(ReleaseSpec {
                url: "https://example.com/x.zip".into(),
                sha256: "a".repeat(64),
            }),
            ..InstallSpec::default()
        });
        // No declared driver present: the release download wins over a
        // command that would only fail on its missing tool.
        let chosen = choose_recipe_with(&manifest, &no_tools).expect("release last");
        assert_eq!(
            chosen,
            Chosen::Release(ReleaseSpec {
                url: "https://example.com/x.zip".into(),
                sha256: "a".repeat(64),
            }),
            "with no driver at all, the release download is the pick"
        );
        let chosen = choose_recipe_with(&manifest, &no_tool);
        assert_eq!(
            chosen,
            Some(Chosen::Release(ReleaseSpec {
                url: "https://example.com/x.zip".into(),
                sha256: "a".repeat(64),
            })),
            "cargo on PATH does not help a manifest that declares no cargo recipe"
        );
        // No release either: the declared recipe shows its command, and
        // its own output will name the missing tool.
        let manifest = manifest_with(InstallSpec {
            winget: Some("only-winget".into()),
            ..InstallSpec::default()
        });
        let chosen = choose_recipe_with(&manifest, &no_tools).expect("declared recipe");
        assert_eq!(
            chosen,
            Chosen::Package {
                recipe: Recipe::Winget,
                command: "winget install only-winget".into(),
            },
        );
    }

    #[test]
    fn no_recipes_at_all_chooses_nothing() {
        let manifest = AppManifest::minimal("x", "X", "x", Category::Dev);
        assert_eq!(choose_recipe_with(&manifest, &any_tool), None);
    }

    #[test]
    fn output_tail_keeps_the_last_lines() {
        let text = "a\n\nb\nc\nd\ne\nf\n";
        assert_eq!(output_tail(text, 3), "d\ne\nf");
        assert_eq!(output_tail("   ", 4), "", "blank output stays blank");
    }

    #[test]
    fn find_executable_matches_the_exec_rule_inside_a_tree() {
        let dir = std::env::temp_dir().join(format!("agony-installer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let nested = dir.join("dist").join("superfile-1.0");
        std::fs::create_dir_all(&nested).expect("tree");
        std::fs::write(nested.join("Spf.EXE"), b"bin").expect("binary");
        std::fs::write(nested.join("LICENSE"), b"text").expect("doc");
        let found = find_executable(&dir, "spf").expect("the binary found");
        assert_eq!(
            found.file_name().map(|n| n.to_string_lossy().into_owned()),
            Some("Spf.EXE".into()),
            "case- and extension-insensitive, like discovery"
        );
        assert!(matches!(
            find_executable(&dir, "nothere"),
            Err(InstallError::ExecutableNotFound { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sha256_matches_the_known_digests() {
        let dir = std::env::temp_dir().join(format!("agony-sha-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp");
        let path = dir.join("abc.bin");
        std::fs::write(&path, b"abc").expect("bytes");
        assert_eq!(
            sha256_file(&path).expect("hash"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::write(&path, b"").expect("empty");
        assert_eq!(
            sha256_file(&path).expect("hash"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn asset_name_is_the_urls_last_segment() {
        assert_eq!(
            asset_name("https://github.com/x/y/releases/download/v1/x.zip"),
            "x.zip"
        );
        assert_eq!(asset_name("https://example.com"), "example.com");
    }
}
