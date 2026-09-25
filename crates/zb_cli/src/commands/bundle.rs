use console::style;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::install;
use crate::cli::BundleCommands;
use crate::ui::StdUi;
use crate::utils::normalize_formula_name;

pub async fn execute(
    installer: &mut zb_installer::Installer,
    command: Option<BundleCommands>,
    ui: &mut StdUi,
) -> Result<(), zb_core::Error> {
    match command.unwrap_or(BundleCommands::Install {
        file: PathBuf::from("Brewfile"),
        no_link: false,
    }) {
        BundleCommands::Install { file, no_link } => {
            install_from_file(installer, &file, no_link, ui).await
        }
        BundleCommands::Check { file } => check_file(installer, &file),
        BundleCommands::Dump { file, force } => dump_to_file(installer, &file, force),
    }
}

/// The packages a Brewfile asks for.
struct Manifest {
    /// Install names, normalised the way `zb install` normalises them, so a
    /// `homebrew/cask/docker` line and a `cask "docker"` line are one package.
    entries: Vec<String>,
}

impl Manifest {
    fn load(path: &Path) -> Result<Self, zb_core::Error> {
        let contents = std::fs::read_to_string(path).map_err(|e| zb_core::Error::FileError {
            message: format!("failed to read manifest {}: {}", path.display(), e),
        })?;

        let mut entries = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for line in contents.lines() {
            // Handle inline comments by splitting on '#' and taking the first part
            let entry = line.split('#').next().unwrap_or("").trim();
            if entry.is_empty() {
                continue;
            }

            if let Some(parsed) = parse_brewfile_entry(entry)
                && seen.insert(parsed.clone())
            {
                entries.push(parsed);
            }
        }

        if entries.is_empty() {
            return Err(zb_core::Error::FileError {
                message: format!("manifest {} did not contain any formulas", path.display()),
            });
        }

        Ok(Self { entries })
    }

    /// Split the manifest into what is already installed and what is not.
    ///
    /// A keg `zb run` left behind counts as missing: it is not something the
    /// user installed, and `zb gc` will delete it, so a Brewfile that names it
    /// is not satisfied.
    fn partition_installed(&self, installer: &zb_installer::Installer) -> Partitioned<'_> {
        let mut satisfied = Vec::new();
        let mut missing = Vec::new();

        for entry in &self.entries {
            let name = normalize_formula_name(entry).unwrap_or_else(|_| entry.clone());
            match installer.get_installed(&name) {
                Some(keg) if !keg.reason.is_transient() => satisfied.push(entry.as_str()),
                _ => missing.push(entry.as_str()),
            }
        }

        Partitioned { satisfied, missing }
    }
}

struct Partitioned<'a> {
    satisfied: Vec<&'a str>,
    missing: Vec<&'a str>,
}

/// `brew bundle check`: say whether the Brewfile is satisfied and let the exit
/// code carry the answer, so it can gate a `.envrc` or a CI step.
fn check_file(
    installer: &zb_installer::Installer,
    manifest_path: &Path,
) -> Result<(), zb_core::Error> {
    let manifest = Manifest::load(manifest_path)?;
    let Partitioned { missing, .. } = manifest.partition_installed(installer);

    if missing.is_empty() {
        println!(
            "{} The Brewfile's dependencies are satisfied.",
            style("==>").cyan().bold()
        );
        return Ok(());
    }

    println!(
        "{} {} missing from {}:",
        style("==>").cyan().bold(),
        style(missing.len()).yellow().bold(),
        manifest_path.display()
    );
    for entry in &missing {
        println!("  {}", style(entry).bold());
    }

    Err(zb_core::Error::InvalidArgument {
        message: format!(
            "{} of the Brewfile's dependencies are not installed; run zb bundle install",
            missing.len()
        ),
    })
}

async fn install_from_file(
    installer: &mut zb_installer::Installer,
    manifest_path: &Path,
    no_link: bool,
    ui: &mut StdUi,
) -> Result<(), zb_core::Error> {
    let manifest = Manifest::load(manifest_path)?;
    // Installing what is already there costs an unpack and a relink per
    // package, which is what made `zb bundle` in an `.envrc` redo its work on
    // every directory change. `brew bundle install` skips them too.
    let Partitioned { satisfied, missing } = manifest.partition_installed(installer);

    if !satisfied.is_empty() {
        println!(
            "{} {} already installed, skipping",
            style("==>").cyan().bold(),
            style(satisfied.len()).green().bold()
        );
    }

    if missing.is_empty() {
        println!(
            "{} {} is already satisfied.",
            style("==>").cyan().bold(),
            manifest_path.display()
        );
        return Ok(());
    }

    println!(
        "{} Installing {} formulas from {}...",
        style("==>").cyan().bold(),
        style(missing.len()).green().bold(),
        manifest_path.display()
    );

    let start = Instant::now();
    for formula in missing {
        install::execute(installer, vec![formula.to_string()], no_link, false, ui).await?;
    }

    println!(
        "{} Finished installing manifest in {:.2}s",
        style("==>").cyan().bold(),
        start.elapsed().as_secs_f64()
    );
    Ok(())
}

fn dump_to_file(
    installer: &mut zb_installer::Installer,
    file_path: &Path,
    force: bool,
) -> Result<(), zb_core::Error> {
    if file_path.exists() && !force {
        return Err(zb_core::Error::FileError {
            message: format!(
                "file {} already exists (use --force to overwrite)",
                file_path.display()
            ),
        });
    }

    // Temporary installs are excluded: a Brewfile records what the user
    // wants reproduced, and a keg `zb run` created for one command is not
    // that.
    let installed: Vec<_> = installer
        .list_installed()?
        .into_iter()
        .filter(|keg| !keg.reason.is_transient())
        .collect();
    let mut content = String::new();
    for keg in &installed {
        content.push_str(&format!("brew \"{}\"\n", keg.name));
    }

    std::fs::write(file_path, content).map_err(|e| zb_core::Error::FileError {
        message: format!("failed to write {}: {}", file_path.display(), e),
    })?;

    println!(
        "{} Dumped {} packages to {}",
        style("==>").cyan().bold(),
        style(installed.len()).green().bold(),
        file_path.display()
    );

    Ok(())
}

fn parse_brewfile_entry(line: &str) -> Option<String> {
    if line.starts_with("tap ") {
        return None;
    }

    if let Some(token) = parse_quoted_directive(line, "cask") {
        return Some(format!("cask:{token}"));
    }

    if let Some(formula) = parse_quoted_directive(line, "brew") {
        return Some(formula.to_string());
    }

    Some(line.to_string())
}

fn parse_quoted_directive<'a>(line: &'a str, directive: &str) -> Option<&'a str> {
    if !line.starts_with(directive) {
        return None;
    }

    let rest = line[directive.len()..].trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }

    let tail = &rest[1..];
    let end = tail.find(quote)?;
    Some(&tail[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;
    use wiremock::MockServer;

    use crate::commands::test_support::{make_installer, mount_formula};

    fn load_manifest(path: &Path) -> Result<Vec<String>, zb_core::Error> {
        Manifest::load(path).map(|manifest| manifest.entries)
    }

    fn brewfile(dir: &TempDir, contents: &str) -> PathBuf {
        let path = dir.path().join("Brewfile");
        std::fs::write(&path, contents).unwrap();
        path
    }

    /// `brew bundle check` is meant to gate a script, so the exit code has to
    /// carry the answer: success when satisfied, failure when not.
    #[tokio::test]
    async fn check_succeeds_only_once_every_entry_is_installed() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula(&server, "checkone", "1.0.0", &[]).await;
        mount_formula(&server, "checktwo", "1.0.0", &[]).await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        let mut ui = StdUi::new();
        let manifest = brewfile(&tmp, "brew \"checkone\"\nbrew \"checktwo\"\n");

        let err = check_file(&installer, &manifest).unwrap_err();
        assert!(
            err.to_string().contains("2 of the Brewfile"),
            "the failure should count what is missing, got: {err}"
        );

        install::execute(
            &mut installer,
            vec!["checkone".to_string(), "checktwo".to_string()],
            false,
            false,
            &mut ui,
        )
        .await
        .unwrap();

        check_file(&installer, &manifest).expect("every entry is installed");
    }

    /// A dependency pulled in by another package still satisfies a Brewfile
    /// line naming it -- `brew bundle check` asks whether the package is
    /// present, not why.
    #[tokio::test]
    async fn check_counts_a_dependency_as_satisfied() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula(&server, "checkdep", "1.0.0", &[]).await;
        mount_formula(&server, "checkroot", "1.0.0", &["checkdep"]).await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        let mut ui = StdUi::new();

        install::execute(
            &mut installer,
            vec!["checkroot".to_string()],
            false,
            false,
            &mut ui,
        )
        .await
        .unwrap();

        let manifest = brewfile(&tmp, "brew \"checkdep\"\n");
        check_file(&installer, &manifest).expect("checkdep is installed as a dependency");
    }

    /// The reason `zb bundle` was unusable in an `.envrc`: every invocation
    /// reinstalled the whole manifest. A second run must reach the installer
    /// for nothing, which is observable here because the mock serves each
    /// formula's metadata only while the test still has it mounted.
    #[tokio::test]
    async fn a_second_install_skips_what_is_already_installed() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula(&server, "bundleskip", "1.0.0", &[]).await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        let mut ui = StdUi::new();
        let manifest = brewfile(&tmp, "brew \"bundleskip\"\n");

        install_from_file(&mut installer, &manifest, false, &mut ui)
            .await
            .unwrap();
        assert!(installer.is_installed("bundleskip"));

        let requests_after_first = server.received_requests().await.unwrap().len();

        install_from_file(&mut installer, &manifest, false, &mut ui)
            .await
            .unwrap();

        assert_eq!(
            server.received_requests().await.unwrap().len(),
            requests_after_first,
            "a satisfied Brewfile must not send the API or the bottle server anything"
        );
    }

    #[test]
    fn load_manifest_parses_entries_ignoring_whitespace_and_comments() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            "# comment\n\njq\nwget\njq\n   git  \n# another comment"
        )
        .unwrap();

        let entries = load_manifest(file.path()).unwrap();
        assert_eq!(entries, vec!["jq", "wget", "git"]);
    }

    #[test]
    fn load_manifest_handles_inline_comments() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            "jq # inline comment\nwget# no space\n  git  # with spaces  "
        )
        .unwrap();

        let entries = load_manifest(file.path()).unwrap();
        assert_eq!(entries, vec!["jq", "wget", "git"]);
    }

    #[test]
    fn load_manifest_errors_when_only_comments() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(file, "# nothing here\n   # still nothing").unwrap();

        let err = load_manifest(file.path()).unwrap_err();
        match err {
            zb_core::Error::FileError { message } => {
                assert!(message.contains("did not contain any formulas"))
            }
            other => panic!("expected file error, got {other:?}"),
        }
    }

    #[test]
    fn load_manifest_errors_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");

        let err = load_manifest(&missing).unwrap_err();
        match err {
            zb_core::Error::FileError { message } => {
                assert!(message.contains("failed to read manifest"))
            }
            other => panic!("expected file error, got {other:?}"),
        }
    }

    #[test]
    fn load_manifest_parses_brewfile_cask_and_brew_entries() {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            file,
            "tap \"homebrew/cask\"\nbrew \"wget\"\ncask \"docker-desktop\"\n"
        )
        .unwrap();

        let entries = load_manifest(file.path()).unwrap();
        assert_eq!(entries, vec!["wget", "cask:docker-desktop"]);
    }

    #[test]
    fn parse_brewfile_entry_handles_brew_directive() {
        assert_eq!(parse_brewfile_entry("brew \"jq\""), Some("jq".to_string()));
        assert_eq!(
            parse_brewfile_entry("brew 'wget'"),
            Some("wget".to_string())
        );
    }

    #[test]
    fn parse_brewfile_entry_handles_cask_directive() {
        assert_eq!(
            parse_brewfile_entry("cask \"docker\""),
            Some("cask:docker".to_string())
        );
    }

    #[test]
    fn parse_brewfile_entry_skips_tap_directive() {
        assert_eq!(parse_brewfile_entry("tap \"homebrew/core\""), None);
    }
}
