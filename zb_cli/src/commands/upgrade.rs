use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use zb_io::{InstallProgress, ProgressCallback};

use crate::ui::StdUi;
use crate::utils::normalize_formula_name;

pub async fn execute(
    installer: &mut zb_io::Installer,
    formulas: Vec<String>,
    build_from_source: bool,
    no_link: bool,
    ui: &mut StdUi,
) -> Result<(), zb_core::Error> {
    let start = Instant::now();

    // Reported at the end so explicit-arg upgrades still run; we exit
    // non-zero afterwards rather than discard partial progress.
    let mut missing: Vec<String> = Vec::new();

    let outdated = if formulas.is_empty() {
        ui.heading("Checking for outdated packages...".to_string())
            .map_err(ui_error)?;
        let (outdated, warnings) = installer.check_outdated().await?;
        for warning in &warnings {
            eprintln!("{} {}", style("Warning:").yellow().bold(), warning);
        }
        if outdated.is_empty() {
            ui.info("All packages are up to date.".to_string())
                .map_err(ui_error)?;
            return Ok(());
        }
        outdated
    } else {
        let mut normalized = Vec::with_capacity(formulas.len());
        for formula in &formulas {
            normalized.push(normalize_formula_name(formula)?);
        }
        let mut outdated = Vec::new();
        for name in &normalized {
            match installer.is_outdated(name).await {
                Ok(Some(pkg)) => outdated.push(pkg),
                Ok(None) => {
                    ui.info(format!("{} is already up to date", name))
                        .map_err(ui_error)?;
                }
                Err(zb_core::Error::NotInstalled { .. }) => {
                    ui.error(format!("{} is not installed", name))
                        .map_err(ui_error)?;
                    missing.push(name.clone());
                }
                Err(e) => return Err(e),
            }
        }
        if outdated.is_empty() {
            if let Some(name) = missing.into_iter().next() {
                return Err(zb_core::Error::NotInstalled { name });
            }
            ui.info("All specified packages are up to date.".to_string())
                .map_err(ui_error)?;
            return Ok(());
        }
        outdated
    };

    ui.heading(format!("Upgrading {}...", style(outdated.len()).bold()))
        .map_err(ui_error)?;

    let multi = MultiProgress::new();
    let bars: Arc<Mutex<HashMap<String, ProgressBar>>> = Arc::new(Mutex::new(HashMap::new()));

    let spinner_style = ProgressStyle::default_spinner()
        .template("    {prefix:<16} {spinner:.cyan} {msg}")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");

    let done_style = ProgressStyle::default_spinner()
        .template("    {prefix:<16} {msg}")
        .unwrap();

    let bars_clone = bars.clone();
    let multi_clone = multi.clone();
    let spinner_style_clone = spinner_style.clone();
    let done_style_clone = done_style.clone();

    let progress_callback: Arc<ProgressCallback> = Arc::new(Box::new(move |event| {
        let mut bars = bars_clone.lock().unwrap();
        match event {
            InstallProgress::DownloadStarted { name, total_bytes } => {
                let pb = if let Some(total) = total_bytes {
                    let pb = multi_clone.add(ProgressBar::new(total));
                    pb.set_style(
                        ProgressStyle::default_bar()
                            .template("    {prefix:<16} {bar:25.cyan/dim} {bytes:>10}/{total_bytes:<10} {eta:>6}")
                            .unwrap()
                            .progress_chars("━━╸"),
                    );
                    pb
                } else {
                    let pb = multi_clone.add(ProgressBar::new_spinner());
                    pb.set_style(spinner_style_clone.clone());
                    pb.set_message("downloading...");
                    pb.enable_steady_tick(std::time::Duration::from_millis(80));
                    pb
                };
                pb.set_prefix(name.clone());
                bars.insert(name, pb);
            }
            InstallProgress::DownloadProgress {
                name,
                downloaded,
                total_bytes,
            } => {
                if let Some(pb) = bars.get(&name)
                    && total_bytes.is_some()
                {
                    pb.set_position(downloaded);
                }
            }
            InstallProgress::DownloadCompleted { name, total_bytes } => {
                if let Some(pb) = bars.get(&name) {
                    if total_bytes > 0 {
                        pb.set_position(total_bytes);
                    }
                    pb.set_style(spinner_style_clone.clone());
                    pb.set_message("unpacking...");
                    pb.enable_steady_tick(std::time::Duration::from_millis(80));
                }
            }
            InstallProgress::UnpackStarted { name } => {
                if let Some(pb) = bars.get(&name) {
                    pb.set_message("unpacking...");
                }
            }
            InstallProgress::UnpackCompleted { name } => {
                if let Some(pb) = bars.get(&name) {
                    pb.set_message("unpacked");
                }
            }
            InstallProgress::LinkStarted { name } => {
                if let Some(pb) = bars.get(&name) {
                    pb.set_message("linking...");
                }
            }
            InstallProgress::LinkCompleted { name } => {
                if let Some(pb) = bars.get(&name) {
                    pb.set_message("linked");
                }
            }
            InstallProgress::LinkSkipped { name, reason } => {
                if let Some(pb) = bars.get(&name) {
                    pb.set_message(format!("keg-only ({})", reason));
                }
            }
            InstallProgress::InstallCompleted { name } => {
                if let Some(pb) = bars.get(&name) {
                    pb.set_style(done_style_clone.clone());
                    pb.set_message(format!("{} upgraded", style("✓").green()));
                    pb.finish();
                }
            }
        }
    }));

    let mut upgraded = 0usize;
    let mut errors: Vec<(String, zb_core::Error)> = Vec::new();

    for pkg in &outdated {
        let name = &pkg.name;
        ui.step_start(name).map_err(ui_error)?;

        match installer
            .upgrade(
                name,
                build_from_source,
                !no_link,
                Some(progress_callback.clone()),
            )
            .await
        {
            Ok(()) => {
                ui.step_ok().map_err(ui_error)?;
                upgraded += 1;
            }
            Err(e) => {
                ui.step_fail().map_err(ui_error)?;
                errors.push((name.clone(), e));
            }
        }
    }

    {
        let bars = bars.lock().unwrap();
        for pb in bars.values() {
            if !pb.is_finished() {
                pb.finish();
            }
        }
    }

    let elapsed = start.elapsed();
    ui.blank_line().map_err(ui_error)?;

    for (name, err) in &errors {
        ui.error(format!("Failed to upgrade {}: {}", style(name).bold(), err))
            .map_err(ui_error)?;
    }

    if errors.is_empty() && missing.is_empty() {
        ui.heading(format!(
            "Upgraded {} packages in {:.2}s",
            style(upgraded).green().bold(),
            elapsed.as_secs_f64()
        ))
        .map_err(ui_error)?;
        Ok(())
    } else if !errors.is_empty() {
        Err(errors.remove(0).1)
    } else {
        Err(zb_core::Error::NotInstalled {
            name: missing.remove(0),
        })
    }
}

fn ui_error(err: std::io::Error) -> zb_core::Error {
    zb_core::Error::FileError {
        message: format!("failed to write CLI output: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use wiremock::MockServer;
    use zb_io::Installer;

    use crate::commands::test_support::{
        make_installer, mount_formula, mount_formula_up_to, mount_formula_with_failing_bottle,
    };
    use crate::ui::StdUi;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// Serve `name` at 1.0.0 exactly once — long enough for the install below
    /// to pick it up — then at 2.0.0 forever, so every later lookup reports it
    /// outdated.
    async fn mount_upgradable(server: &MockServer, name: &str) {
        mount_formula_up_to(server, name, "1.0.0", &[], 1).await;
        mount_formula(server, name, "2.0.0", &[]).await;
    }

    async fn install_at_1_0_0(installer: &mut Installer, names: &[&str]) {
        for name in names {
            installer.install(&[name.to_string()], true).await.unwrap();
            assert_eq!(installer.get_installed(name).unwrap().version, "1.0.0");
        }
    }

    /// `zb upgrade a b` walks the batch one package at a time; all of them
    /// have to land on the new version.
    #[tokio::test]
    async fn upgrades_every_requested_formula_in_one_invocation() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_upgradable(&server, "upa").await;
        mount_upgradable(&server, "upb").await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        install_at_1_0_0(&mut installer, &["upa", "upb"]).await;

        let mut ui = StdUi::new();
        super::execute(
            &mut installer,
            names(&["upa", "upb"]),
            false,
            false,
            &mut ui,
        )
        .await
        .unwrap();

        for name in ["upa", "upb"] {
            assert_eq!(
                installer.get_installed(name).unwrap().version,
                "2.0.0",
                "{name} was not upgraded"
            );
            assert!(root.join(format!("cellar/{name}/2.0.0")).exists());
            assert!(
                !root.join(format!("cellar/{name}/1.0.0")).exists(),
                "{name}'s old keg was left behind"
            );
        }
    }

    /// A name that was never installed is collected and reported at the end,
    /// so the packages listed after it still get upgraded. The command then
    /// exits non-zero naming the first one it could not find.
    #[tokio::test]
    async fn upgrade_reports_a_missing_formula_without_skipping_the_rest() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_upgradable(&server, "upc").await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        install_at_1_0_0(&mut installer, &["upc"]).await;

        let mut ui = StdUi::new();
        let err = super::execute(
            &mut installer,
            names(&["neverinstalled", "upc"]),
            false,
            false,
            &mut ui,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, zb_core::Error::NotInstalled { ref name } if name == "neverinstalled"),
            "expected the uninstalled formula to be named, got {err:?}"
        );
        assert_eq!(
            installer.get_installed("upc").unwrap().version,
            "2.0.0",
            "a missing formula must not stop the rest of the batch"
        );
    }

    /// An upgrade that fails mid-batch is recorded and the loop carries on.
    /// The failed package keeps its old version (its bottles are prefetched
    /// before the old keg is removed) and the command surfaces the first
    /// error.
    ///
    /// Note: only the *first* error is returned, and `missing` is dropped
    /// entirely when any upgrade also failed. Everything else is visible in
    /// the printed output only.
    #[tokio::test]
    async fn upgrade_continues_after_a_failure_and_reports_the_first_one() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula_up_to(&server, "upfail", "1.0.0", &[], 1).await;
        mount_formula_with_failing_bottle(&server, "upfail", "2.0.0").await;
        mount_upgradable(&server, "upok").await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        install_at_1_0_0(&mut installer, &["upfail", "upok"]).await;

        let mut ui = StdUi::new();
        let result = super::execute(
            &mut installer,
            names(&["upfail", "upok"]),
            false,
            false,
            &mut ui,
        )
        .await;

        assert!(result.is_err(), "the failed upgrade must be reported");
        assert_eq!(
            installer.get_installed("upfail").unwrap().version,
            "1.0.0",
            "a failed upgrade must leave the old version installed"
        );
        assert_eq!(
            installer.get_installed("upok").unwrap().version,
            "2.0.0",
            "a failure must not stop the rest of the batch"
        );
    }
}
