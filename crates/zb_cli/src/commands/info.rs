use chrono::{DateTime, Local};
use console::style;
use std::path::Path;
use walkdir::WalkDir;
use zb_store::InstalledKeg;

use crate::utils::normalize_formula_name;

pub async fn execute(
    installer: &mut zb_installer::Installer,
    formula: String,
) -> Result<(), zb_core::Error> {
    let name = normalize_formula_name(&formula)?;
    let installed = installer.get_installed(&name);

    // Metadata is nice to have, not required: `zb info` must still describe a
    // package that is on disk when the network is down or the formula has
    // since been dropped from the API. A cask has no formula to describe it,
    // so its installed record is all there is.
    let metadata = if name.starts_with("cask:") {
        None
    } else {
        match installer.get_formula(&name).await {
            Ok(formula) => Some(formula),
            Err(e) if installed.is_some() => {
                tracing::debug!("no formula metadata for {name}: {e}");
                None
            }
            Err(e) => return Err(e),
        }
    };

    if metadata.is_none() && installed.is_none() {
        return Err(zb_core::Error::NotInstalled { name });
    }

    print_header(&name, installed.as_ref(), metadata.as_ref());
    print_installation(installer, installed.as_ref());
    if let Some(formula) = metadata.as_ref() {
        print_dependencies(formula);
    }

    Ok(())
}

fn print_header(name: &str, installed: Option<&InstalledKeg>, metadata: Option<&zb_core::Formula>) {
    let version = metadata
        .map(|f| f.effective_version())
        .or_else(|| installed.map(|keg| keg.version.clone()));

    match version {
        Some(version) => println!("{}: stable {}", style(name).bold(), style(version).dim()),
        None => println!("{}", style(name).bold()),
    }

    let Some(formula) = metadata else {
        return;
    };

    if let Some(desc) = &formula.desc {
        println!("{desc}");
    }
    if let Some(homepage) = &formula.homepage {
        println!("{}", style(homepage).cyan());
    }
    if let Some(license) = &formula.license {
        print_field("License:", license);
    }
    if formula.is_keg_only() {
        let reason = formula
            .keg_only_reason
            .as_ref()
            .map(|r| r.explanation.as_str())
            .filter(|explanation| !explanation.is_empty())
            .unwrap_or("this formula is not symlinked into the prefix");
        print_field("Keg-only:", reason);
    }
}

fn print_installation(installer: &zb_installer::Installer, installed: Option<&InstalledKeg>) {
    let Some(keg) = installed else {
        println!("{}", style("Not installed").dim());
        return;
    };

    println!("{}", style("Installed").green().bold());

    let path = installer.keg_path(&keg.name, &keg.version);
    match DiskUsage::of(&path) {
        Some(usage) => println!("  {} ({usage})", path.display()),
        None => println!("  {}", path.display()),
    }

    print_field("  Installed:", format_timestamp(keg.installed_at));
    print_field(
        "  Store key:",
        &keg.store_key[..12.min(keg.store_key.len())],
    );
    if keg.reason.is_transient() {
        print_field("  Kind:", "temporary install (removed by zb gc)");
    }
}

fn print_dependencies(formula: &zb_core::Formula) {
    if formula.dependencies.is_empty() && formula.build_dependencies.is_empty() {
        return;
    }

    println!("{}", style("Dependencies").bold());
    if !formula.dependencies.is_empty() {
        println!("  required: {}", formula.dependencies.join(", "));
    }
    if !formula.build_dependencies.is_empty() {
        println!("  build: {}", formula.build_dependencies.join(", "));
    }
}

/// What a keg occupies on disk.
///
/// Counted by walking the keg rather than read from the bottle's size,
/// because zbrew clones files out of a shared store: the number worth showing
/// is what the directory holds now.
struct DiskUsage {
    files: usize,
    bytes: u64,
}

impl DiskUsage {
    fn of(path: &Path) -> Option<Self> {
        if !path.exists() {
            return None;
        }

        let mut usage = Self { files: 0, bytes: 0 };
        for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
            // `metadata` follows symlinks; a keg is full of links into the
            // store and into other kegs, and counting their targets would
            // report a size the directory does not occupy.
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                continue;
            }
            usage.files += 1;
            usage.bytes += meta.len();
        }
        Some(usage)
    }
}

impl std::fmt::Display for DiskUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}, {}",
            self.files,
            if self.files == 1 { "file" } else { "files" },
            format_bytes(self.bytes)
        )
    }
}

/// Render a byte count the way `brew info` does: three significant figures in
/// the largest unit that keeps the number above 1.
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn print_field(label: &str, value: impl std::fmt::Display) {
    println!("{:<12}  {}", style(label).dim(), value);
}

fn format_timestamp(timestamp: i64) -> String {
    match DateTime::from_timestamp(timestamp, 0) {
        Some(dt) => {
            let local_dt = dt.with_timezone(&Local);
            let now = Local::now();
            let duration = now.signed_duration_since(local_dt);

            if duration.num_days() > 0 {
                format!(
                    "{} ({} days ago)",
                    local_dt.format("%Y-%m-%d"),
                    duration.num_days()
                )
            } else if duration.num_hours() > 0 {
                format!(
                    "{} ({} hours ago)",
                    local_dt.format("%Y-%m-%d %H:%M"),
                    duration.num_hours()
                )
            } else {
                format!(
                    "{} ({} minutes ago)",
                    local_dt.format("%H:%M"),
                    duration.num_minutes()
                )
            }
        }
        None => "invalid timestamp".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use wiremock::MockServer;

    use crate::commands::test_support::{
        make_installer, mount_empty_formula_index, mount_formula, mount_missing_formula,
    };

    /// `brew info` describes a package you have not installed. `zb info` used
    /// to answer "not installed" and stop, which made it useless for deciding
    /// whether to install something.
    #[tokio::test]
    async fn describes_a_formula_that_is_not_installed() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let mut installer = make_installer(
            &tmp.path().join("zbrew"),
            &tmp.path().join("homebrew"),
            &server.uri(),
        );

        mount_formula(&server, "infodep", "1.0.0", &[]).await;
        mount_formula(&server, "infopkg", "2.0.0", &["infodep"]).await;

        super::execute(&mut installer, "infopkg".to_string())
            .await
            .expect("an uninstalled formula still has metadata to show");
        assert!(!installer.is_installed("infopkg"));
    }

    #[tokio::test]
    async fn reports_a_formula_that_exists_nowhere() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let mut installer = make_installer(
            &tmp.path().join("zbrew"),
            &tmp.path().join("homebrew"),
            &server.uri(),
        );

        mount_missing_formula(&server, "nosuchpkg").await;
        mount_empty_formula_index(&server).await;

        let err = super::execute(&mut installer, "nosuchpkg".to_string())
            .await
            .unwrap_err();

        assert!(
            matches!(err, zb_core::Error::MissingFormula { ref name } if name == "nosuchpkg"),
            "expected the unknown formula to be named, got {err:?}"
        );
    }

    /// An installed package must still be describable when the API cannot be
    /// reached -- that is exactly when a user is trying to find out what they
    /// have on disk.
    #[tokio::test]
    async fn describes_an_installed_formula_without_metadata() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        let mut installer = make_installer(&root, &prefix, &server.uri());
        let mut ui = crate::ui::StdUi::new();

        mount_formula(&server, "infogone", "1.0.0", &[]).await;
        crate::commands::install::execute(
            &mut installer,
            vec!["infogone".to_string()],
            false,
            false,
            &mut ui,
        )
        .await
        .unwrap();

        // The formula disappears from the API, as `fasd` did upstream.
        server.reset().await;
        mount_missing_formula(&server, "infogone").await;
        mount_empty_formula_index(&server).await;
        installer.clear_api_cache().unwrap();

        super::execute(&mut installer, "infogone".to_string())
            .await
            .expect("an installed keg is describable without the API");
    }

    #[test]
    fn bytes_render_in_the_largest_unit_that_keeps_the_number_above_one() {
        assert_eq!(format_bytes(0), "0B");
        assert_eq!(format_bytes(512), "512B");
        assert_eq!(format_bytes(1024), "1.0KB");
        assert_eq!(format_bytes(1536), "1.5KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0GB");
    }

    /// A keg is mostly symlinks into the store, so following them would report
    /// a size the directory does not occupy.
    #[test]
    fn disk_usage_counts_links_rather_than_what_they_point_at() {
        let tmp = tempfile::tempdir().unwrap();
        let keg = tmp.path().join("keg");
        std::fs::create_dir_all(keg.join("bin")).unwrap();

        let target = tmp.path().join("big");
        std::fs::write(&target, vec![0u8; 4096]).unwrap();
        std::fs::write(keg.join("bin").join("real"), b"hello").unwrap();
        std::os::unix::fs::symlink(&target, keg.join("bin").join("linked")).unwrap();

        let usage = DiskUsage::of(&keg).unwrap();

        assert_eq!(usage.files, 2);
        assert!(
            usage.bytes < 4096,
            "the symlink's 4KB target was counted: {} bytes",
            usage.bytes
        );
    }

    #[test]
    fn disk_usage_is_absent_for_a_keg_that_is_not_on_disk() {
        let tmp = tempfile::tempdir().unwrap();

        assert!(DiskUsage::of(&tmp.path().join("missing")).is_none());
    }
}
