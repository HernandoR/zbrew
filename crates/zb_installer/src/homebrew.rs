use std::path::{Path, PathBuf};
use std::process::Command;

use tracing::debug;
use zb_core::{Error, Formula};

/// The Homebrew installation on this machine, as a source of formula
/// metadata the JSON API no longer serves.
///
/// Homebrew keeps the formula it installed at
/// `<prefix>/Cellar/<name>/<version>/.brew/<name>.rb`. When a formula is
/// dropped from homebrew-core -- `fasd` is the reported case -- that file is
/// the only description of it left anywhere, and without it `zb migrate`
/// silently skips a package the user still has installed.
pub struct HomebrewCellar {
    cellar: PathBuf,
}

impl HomebrewCellar {
    /// Locate Homebrew, or `None` when this machine has none.
    pub fn discover() -> Option<Self> {
        Self::prefix_from_env()
            .or_else(Self::prefix_from_brew)
            .or_else(Self::prefix_from_defaults)
            .map(|prefix| Self::at(&prefix))
    }

    pub fn at(prefix: &Path) -> Self {
        Self {
            cellar: prefix.join("Cellar"),
        }
    }

    /// The formula Homebrew installed for `name`, parsed from the copy it
    /// keeps beside the keg.
    pub fn local_formula(&self, name: &str) -> Option<Formula> {
        let source_path = self.formula_source_path(name)?;
        let source = std::fs::read_to_string(&source_path).ok()?;

        match zb_net::parse_core_formula_ruby(name, &source) {
            Ok(formula) => Some(formula),
            Err(e) => {
                debug!("failed to parse {}: {e}", source_path.display());
                None
            }
        }
    }

    /// The newest installed version's `.brew/<name>.rb`.
    ///
    /// Homebrew can keep several versions of a formula side by side; the
    /// highest version is the one migration should reproduce.
    fn formula_source_path(&self, name: &str) -> Option<PathBuf> {
        let mut versions: Vec<String> = std::fs::read_dir(self.cellar.join(name))
            .ok()?
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        versions.sort_by_cached_key(|version| version_order(version));

        versions
            .into_iter()
            .rev()
            .map(|version| {
                self.cellar
                    .join(name)
                    .join(version)
                    .join(".brew")
                    .join(format!("{name}.rb"))
            })
            .find(|path| path.is_file())
    }

    fn prefix_from_env() -> Option<PathBuf> {
        let prefix = PathBuf::from(std::env::var_os("HOMEBREW_PREFIX")?);
        prefix.join("Cellar").is_dir().then_some(prefix)
    }

    fn prefix_from_brew() -> Option<PathBuf> {
        let output = Command::new("brew").arg("--prefix").output().ok()?;
        if !output.status.success() {
            return None;
        }
        let prefix = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        prefix.join("Cellar").is_dir().then_some(prefix)
    }

    fn prefix_from_defaults() -> Option<PathBuf> {
        ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"]
            .into_iter()
            .map(PathBuf::from)
            .find(|prefix| prefix.join("Cellar").is_dir())
    }
}

/// Represents a Homebrew package that can be migrated
#[derive(Debug, Clone)]
pub struct HomebrewPackage {
    pub name: String,
    pub tap: String,
    pub is_cask: bool,
}

/// Result of collecting Homebrew packages for migration
pub struct HomebrewMigrationPackages {
    /// Formulas from homebrew/core that can be migrated
    pub formulas: Vec<HomebrewPackage>,
    /// Formulas from non-core taps that cannot be migrated
    pub non_core_formulas: Vec<HomebrewPackage>,
    /// Cask packages that cannot be migrated
    pub casks: Vec<HomebrewPackage>,
}

/// Parse Homebrew formulas from JSON output of `brew info --json=v1 --installed`
pub(crate) fn parse_formulas_from_json(json: &serde_json::Value) -> Vec<HomebrewPackage> {
    let mut packages = Vec::new();

    if let Some(formulas) = json.as_array() {
        for formula in formulas {
            if let Some(name) = formula.get("name").and_then(|n| n.as_str()) {
                let tap = formula
                    .get("tap")
                    .and_then(|t| t.as_str())
                    .unwrap_or("homebrew/core")
                    .to_string();

                packages.push(HomebrewPackage {
                    name: name.to_string(),
                    tap,
                    is_cask: false,
                });
            }
        }
    }

    packages
}

/// Parse Homebrew leaves from plain text output of `brew leaves`
pub(crate) fn parse_leaves_from_plain_text(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|s| s.to_string())
        .collect()
}

/// Parse Homebrew casks from plain text output of `brew list --cask`
pub(crate) fn parse_casks_from_plain_text(output: &str) -> Vec<HomebrewPackage> {
    output
        .lines()
        .filter(|line| !line.is_empty())
        .map(|name| HomebrewPackage {
            name: name.to_string(),
            tap: "homebrew/cask".to_string(),
            is_cask: true,
        })
        .collect()
}

/// Categorize Homebrew packages for migration
///
/// Returns a struct with separate lists for:
/// - Formulas from homebrew/core (migratable)
/// - Formulas from other taps (not migratable)
/// - Cask packages (not migratable)
pub(crate) fn categorize_packages(packages: Vec<HomebrewPackage>) -> HomebrewMigrationPackages {
    let mut formulas = Vec::new();
    let mut non_core_formulas = Vec::new();
    let mut casks = Vec::new();

    for pkg in packages {
        if pkg.is_cask {
            casks.push(pkg);
        } else if pkg.tap == "homebrew/core" {
            formulas.push(pkg);
        } else {
            non_core_formulas.push(pkg);
        }
    }

    HomebrewMigrationPackages {
        formulas,
        non_core_formulas,
        casks,
    }
}

/// Get all installed Homebrew packages, categorized for migration
///
/// Only formulas from `homebrew/core` can be migrated to zbrew.
/// Formulas from other taps and all casks are collected separately.
/// Only leaves are migrated, as there's no use to reinstalling dependencies.
pub fn get_homebrew_packages() -> Result<HomebrewMigrationPackages, Error> {
    let leaves_output = Command::new("brew")
        .args(["leaves"])
        .output()
        .map_err(Error::exec("failed to run 'brew leaves'"))?;

    if !leaves_output.status.success() {
        return Err((Error::exec("brew leaves failed"))(
            String::from_utf8_lossy(&leaves_output.stderr),
        ));
    }

    let leaves = parse_leaves_from_plain_text(&String::from_utf8_lossy(&leaves_output.stdout));

    let formulas = if leaves.is_empty() {
        Vec::new()
    } else {
        let formulas_output = Command::new("brew")
            .args(["info", "--json=v1"])
            .args(&leaves)
            .output()
            .map_err(Error::exec("failed to run 'brew info'"))?;

        if !formulas_output.status.success() {
            return Err((Error::exec("brew info failed"))(String::from_utf8_lossy(
                &formulas_output.stderr,
            )));
        }

        let formulas_json: serde_json::Value = serde_json::from_slice(&formulas_output.stdout)
            .map_err(Error::exec("failed to parse brew info JSON"))?;

        parse_formulas_from_json(&formulas_json)
    };

    let casks_output = Command::new("brew")
        .args(["list", "--cask"])
        .output()
        .map_err(Error::exec("failed to run 'brew list --cask'"))?;

    if !casks_output.status.success() {
        return Err((Error::exec("brew list --cask failed"))(
            String::from_utf8_lossy(&casks_output.stderr),
        ));
    }

    let casks = parse_casks_from_plain_text(&String::from_utf8_lossy(&casks_output.stdout));

    let all_packages: Vec<HomebrewPackage> = formulas.into_iter().chain(casks).collect();
    Ok(categorize_packages(all_packages))
}
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum VersionPart {
    Number(u64),
    Text(String),
}

/// Sort key that orders version directory names the way a human reads them,
/// so `1.10.0` comes after `1.9.0`. Plain string order would not, and
/// Homebrew keeps several versions of a formula side by side.
fn version_order(version: &str) -> Vec<VersionPart> {
    let mut parts = Vec::new();
    let mut rest = version;

    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits > 0 {
            match rest[..digits].parse::<u64>() {
                Ok(number) => parts.push(VersionPart::Number(number)),
                Err(_) => parts.push(VersionPart::Text(rest[..digits].to_string())),
            }
            rest = &rest[digits..];
            continue;
        }

        let text = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        parts.push(VersionPart::Text(rest[..text].to_string()));
        rest = &rest[text..];
    }

    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{get_test_bottle_tag, write_homebrew_keg};

    const SHA: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    /// Homebrew can keep several versions of a formula side by side. The one
    /// migration should reproduce is the newest.
    #[test]
    fn the_newest_installed_version_supplies_the_formula() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path().join("homebrew");
        write_homebrew_keg(&prefix, "multi", "1.9.0", &[], SHA);
        write_homebrew_keg(&prefix, "multi", "1.10.0", &[], SHA);

        let formula = HomebrewCellar::at(&prefix).local_formula("multi").unwrap();

        assert_eq!(formula.versions.stable, "1.10.0");
    }

    /// The recovered formula has to be installable, not merely parseable: the
    /// bottle it names must point at where homebrew-core's bottles live.
    #[test]
    fn the_recovered_formula_points_at_the_core_bottle_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path().join("homebrew");
        write_homebrew_keg(&prefix, "recovered", "1.0.0", &["somedep"], SHA);

        let formula = HomebrewCellar::at(&prefix)
            .local_formula("recovered")
            .unwrap();

        assert_eq!(formula.dependencies, ["somedep"]);
        let bottle = formula
            .bottle
            .stable
            .files
            .get(get_test_bottle_tag())
            .expect("a bottle for this platform");
        assert_eq!(
            bottle.url,
            format!("https://ghcr.io/v2/homebrew/core/recovered/blobs/sha256:{SHA}")
        );
        assert_eq!(bottle.sha256, SHA);
    }

    #[test]
    fn versions_order_by_number_rather_than_by_string() {
        let mut versions = ["1.9.0", "1.10.0", "1.0.0", "1.10.0_1"];
        versions.sort_by_cached_key(|v| version_order(v));

        assert_eq!(versions, ["1.0.0", "1.9.0", "1.10.0", "1.10.0_1"]);
    }

    #[test]
    fn a_formula_homebrew_never_installed_has_no_local_copy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path().join("homebrew");
        write_homebrew_keg(&prefix, "present", "1.0.0", &[], SHA);

        assert!(
            HomebrewCellar::at(&prefix)
                .local_formula("absent")
                .is_none()
        );
    }

    /// A keg left behind without its `.brew` directory -- an old Homebrew, or
    /// a hand-edited Cellar -- must not be mistaken for a usable formula.
    #[test]
    fn a_keg_without_the_saved_formula_has_no_local_copy() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prefix = tmp.path().join("homebrew");
        std::fs::create_dir_all(prefix.join("Cellar/bare/1.0.0/bin")).unwrap();

        assert!(HomebrewCellar::at(&prefix).local_formula("bare").is_none());
    }

    #[test]
    fn test_parse_formulas_from_json() {
        let brew_output = r#"[
            {
                "name": "git",
                "tap": "homebrew/core",
                "versions": { "stable": "2.40.0" }
            },
            {
                "name": "neovim",
                "tap": "homebrew/core",
                "versions": { "stable": "0.9.0" }
            }
        ]"#;

        let formulas_json: serde_json::Value = serde_json::from_str(brew_output).unwrap();
        let packages = parse_formulas_from_json(&formulas_json);

        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "git");
        assert_eq!(packages[0].tap, "homebrew/core");
        assert!(!packages[0].is_cask);
        assert_eq!(packages[1].name, "neovim");
        assert!(!packages[1].is_cask);
    }

    #[test]
    fn test_parse_formulas_handles_missing_tap() {
        let brew_output = r#"[
            {"name": "no-tap-formula"}
        ]"#;

        let formulas_json: serde_json::Value = serde_json::from_str(brew_output).unwrap();
        let packages = parse_formulas_from_json(&formulas_json);

        assert_eq!(packages.len(), 1);
        assert_eq!(packages[0].name, "no-tap-formula");
        assert_eq!(packages[0].tap, "homebrew/core");
    }

    #[test]
    fn test_parse_casks_from_plain_text() {
        // Simulate brew list --cask output
        let brew_output = "visual-studio-code\nfirefox\n";

        let packages = parse_casks_from_plain_text(brew_output);

        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "visual-studio-code");
        assert_eq!(packages[0].tap, "homebrew/cask");
        assert!(packages[0].is_cask);
        assert_eq!(packages[1].name, "firefox");
        assert!(packages[1].is_cask);
    }

    #[test]
    fn test_parse_leaves_from_plain_text() {
        let brew_output = "git\nneovim\nfirefox\n\n";

        let leaves = parse_leaves_from_plain_text(brew_output);

        assert_eq!(leaves, vec!["git", "neovim", "firefox"]);
    }

    #[test]
    fn test_parse_casks_handles_empty_output() {
        let brew_output = "";

        let packages = parse_casks_from_plain_text(brew_output);

        assert!(packages.is_empty());
    }

    #[test]
    fn test_parse_casks_handles_multiple_lines() {
        let brew_output = "visual-studio-code\nfirefox\ndocker\niterm2\n";

        let packages = parse_casks_from_plain_text(brew_output);

        assert_eq!(packages.len(), 4);
        assert_eq!(
            packages.iter().map(|p| &p.name).collect::<Vec<_>>(),
            vec!["visual-studio-code", "firefox", "docker", "iterm2"]
        );
    }

    #[test]
    fn test_categorize_packages_filters_core_formulas() {
        let packages = vec![
            HomebrewPackage {
                name: "git".to_string(),
                tap: "homebrew/core".to_string(),
                is_cask: false,
            },
            HomebrewPackage {
                name: "curl".to_string(),
                tap: "homebrew/core".to_string(),
                is_cask: false,
            },
        ];

        let result = categorize_packages(packages);

        assert_eq!(result.formulas.len(), 2);
        assert!(result.non_core_formulas.is_empty());
        assert!(result.casks.is_empty());
    }

    #[test]
    fn test_categorize_packages_filters_non_core_formulas() {
        let packages = vec![
            HomebrewPackage {
                name: "php".to_string(),
                tap: "shivammathur/php".to_string(),
                is_cask: false,
            },
            HomebrewPackage {
                name: "mysql".to_string(),
                tap: "homebrew/mysql".to_string(),
                is_cask: false,
            },
        ];

        let result = categorize_packages(packages);

        assert!(result.formulas.is_empty());
        assert_eq!(result.non_core_formulas.len(), 2);
        assert!(result.casks.is_empty());
    }

    #[test]
    fn test_categorize_packages_filters_casks() {
        let packages = vec![
            HomebrewPackage {
                name: "visual-studio-code".to_string(),
                tap: "homebrew/cask".to_string(),
                is_cask: true,
            },
            HomebrewPackage {
                name: "firefox".to_string(),
                tap: "homebrew/cask".to_string(),
                is_cask: true,
            },
        ];

        let result = categorize_packages(packages);

        assert!(result.formulas.is_empty());
        assert!(result.non_core_formulas.is_empty());
        assert_eq!(result.casks.len(), 2);
    }

    #[test]
    fn test_categorize_packages_mixed_packages() {
        let packages = vec![
            HomebrewPackage {
                name: "git".to_string(),
                tap: "homebrew/core".to_string(),
                is_cask: false,
            },
            HomebrewPackage {
                name: "php".to_string(),
                tap: "homebrew/php".to_string(),
                is_cask: false,
            },
            HomebrewPackage {
                name: "visual-studio-code".to_string(),
                tap: "homebrew/cask".to_string(),
                is_cask: true,
            },
        ];

        let result = categorize_packages(packages);

        assert_eq!(result.formulas.len(), 1);
        assert_eq!(result.formulas[0].name, "git");

        assert_eq!(result.non_core_formulas.len(), 1);
        assert_eq!(result.non_core_formulas[0].name, "php");

        assert_eq!(result.casks.len(), 1);
        assert_eq!(result.casks[0].name, "visual-studio-code");
    }

    #[test]
    fn test_homebrew_package_struct() {
        let pkg = HomebrewPackage {
            name: "test-formula".to_string(),
            tap: "homebrew/core".to_string(),
            is_cask: false,
        };

        assert_eq!(pkg.name, "test-formula");
        assert_eq!(pkg.tap, "homebrew/core");
        assert!(!pkg.is_cask);

        let cask = HomebrewPackage {
            name: "test-cask".to_string(),
            tap: "homebrew/cask".to_string(),
            is_cask: true,
        };

        assert!(cask.is_cask);
    }
}
