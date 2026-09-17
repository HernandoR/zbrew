use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use zb_io::{InstallProgress, ProgressCallback};

use crate::ui::StdUi;
use crate::utils::{normalize_formula_name, suggest_homebrew, suggest_missing_formula_matches};

pub async fn execute(
    installer: &mut zb_io::Installer,
    formulas: Vec<String>,
    no_link: bool,
    build_from_source: bool,
    ui: &mut StdUi,
) -> Result<(), zb_core::Error> {
    let start = Instant::now();
    ui.heading(format!(
        "Installing {}...",
        style(formulas.join(", ")).bold()
    ))
    .map_err(ui_error)?;

    let mut normalized_names = Vec::new();
    let mut cask_names = Vec::new();
    for formula in &formulas {
        match normalize_formula_name(formula) {
            Ok(name) => {
                if name.starts_with("cask:") {
                    cask_names.push(name);
                } else {
                    normalized_names.push(name);
                }
            }
            Err(e) => {
                suggest_homebrew(formula, &e);
                return Err(e);
            }
        }
    }

    let mut installed_count = 0usize;

    if !normalized_names.is_empty() {
        let plan = match installer
            .plan_with_options(&normalized_names, build_from_source)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                let handled_missing = suggest_missing_formula_matches(installer, &e).await;

                if !handled_missing {
                    for formula in &formulas {
                        suggest_homebrew(formula, &e);
                    }
                }
                return Err(e);
            }
        };

        installed_count += execute_formula_plan(installer, &formulas, plan, no_link, ui).await?;
    }

    if !cask_names.is_empty() {
        ui.heading(format!(
            "Installing casks ({} packages)...",
            cask_names.len()
        ))
        .map_err(ui_error)?;
        let result = installer.install_casks(&cask_names, !no_link).await?;
        installed_count += result.installed;
    }

    let elapsed = start.elapsed();
    ui.blank_line().map_err(ui_error)?;
    ui.heading(format!(
        "Installed {} packages in {:.2}s",
        style(installed_count).green().bold(),
        elapsed.as_secs_f64()
    ))
    .map_err(ui_error)?;

    Ok(())
}

pub async fn execute_formula_plan(
    installer: &mut zb_io::Installer,
    requested_formulas: &[String],
    plan: zb_io::InstallPlan,
    no_link: bool,
    ui: &mut StdUi,
) -> Result<usize, zb_core::Error> {
    ui.heading(format!(
        "Resolving dependencies ({} packages)...",
        plan.items.len()
    ))
    .map_err(ui_error)?;
    for item in &plan.items {
        ui.bullet(format!(
            "{} {}",
            style(&item.formula.name).green(),
            style(&item.formula.versions.stable).dim()
        ))
        .map_err(ui_error)?;
    }

    let multi = MultiProgress::new();
    let bars: Arc<Mutex<HashMap<String, ProgressBar>>> = Arc::new(Mutex::new(HashMap::new()));

    let download_style = ProgressStyle::default_bar()
        .template("    {prefix:<16} {bar:25.cyan/dim} {bytes:>10}/{total_bytes:<10} {eta:>6}")
        .unwrap()
        .progress_chars("━━╸");

    let spinner_style = ProgressStyle::default_spinner()
        .template("    {prefix:<16} {spinner:.cyan} {msg}")
        .unwrap()
        .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏");

    let done_style = ProgressStyle::default_spinner()
        .template("    {prefix:<16} {msg}")
        .unwrap();

    ui.heading("Downloading and installing formulas...")
        .map_err(ui_error)?;

    let bars_clone = bars.clone();
    let multi_clone = multi.clone();
    let download_style_clone = download_style.clone();
    let spinner_style_clone = spinner_style.clone();
    let done_style_clone = done_style.clone();

    let progress_callback: Arc<ProgressCallback> = Arc::new(Box::new(move |event| {
        let mut bars = bars_clone.lock().unwrap();
        match event {
            InstallProgress::DownloadStarted { name, total_bytes } => {
                let pb = if let Some(total) = total_bytes {
                    let pb = multi_clone.add(ProgressBar::new(total));
                    pb.set_style(download_style_clone.clone());
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
                    pb.set_message(format!("{} installed", style("✓").green()));
                    pb.finish();
                }
            }
        }
    }));

    let result_val = installer
        .execute_with_progress(plan, !no_link, Some(progress_callback))
        .await;

    {
        let bars = bars.lock().unwrap();
        for pb in bars.values() {
            if !pb.is_finished() {
                pb.finish();
            }
        }
    }

    match result_val {
        Ok(result) => Ok(result.installed),
        Err(ref e @ zb_core::Error::LinkConflict { ref conflicts }) => {
            ui.blank_line().map_err(ui_error)?;
            ui.error("The link step did not complete successfully.")
                .map_err(ui_error)?;
            ui.println("The formula was installed, but is not symlinked into the prefix.")
                .map_err(ui_error)?;
            ui.blank_line().map_err(ui_error)?;
            ui.println("Possible conflicting files:")
                .map_err(ui_error)?;
            for c in conflicts {
                if let Some(ref owner) = c.owned_by {
                    ui.println(format!(
                        "  {} (symlink belonging to {})",
                        c.path.display(),
                        style(owner).yellow()
                    ))
                    .map_err(ui_error)?;
                } else {
                    ui.println(format!("  {}", c.path.display()))
                        .map_err(ui_error)?;
                }
            }
            ui.blank_line().map_err(ui_error)?;
            Err(e.clone())
        }
        Err(e) => {
            let handled_missing = suggest_missing_formula_matches(installer, &e).await;

            if !handled_missing {
                for formula in requested_formulas {
                    suggest_homebrew(formula, &e);
                }
            }
            Err(e)
        }
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

    use crate::commands::test_support::{
        make_installer, mount_empty_formula_index, mount_formula,
        mount_formula_with_failing_bottle, mount_missing_formula,
    };
    use crate::ui::StdUi;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// `zb install a b c` plans the whole batch in one pass, so a dependency
    /// two of them share is fetched and installed once.
    #[tokio::test]
    async fn installs_every_requested_formula_in_one_invocation() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula(&server, "multidep", "1.0.0", &[]).await;
        mount_formula(&server, "multia", "1.0.0", &["multidep"]).await;
        mount_formula(&server, "multib", "1.0.0", &["multidep"]).await;
        mount_formula(&server, "multic", "1.0.0", &[]).await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        let mut ui = StdUi::new();

        super::execute(
            &mut installer,
            names(&["multia", "multib", "multic"]),
            false,
            false,
            &mut ui,
        )
        .await
        .unwrap();

        for name in ["multia", "multib", "multic", "multidep"] {
            assert!(
                installer.is_installed(name),
                "{name} was requested in the batch but is not installed"
            );
            assert!(
                prefix.join("bin").join(name).exists(),
                "{name} was not linked into the prefix"
            );
        }
        // The shared dependency is planned once, not once per dependent.
        assert!(root.join("cellar/multidep/1.0.0").exists());
    }

    /// Planning is all-or-nothing: one unknown name in the batch and none of
    /// its siblings are installed, because `plan_with_options` resolves every
    /// requested formula before a single byte is downloaded.
    #[tokio::test]
    async fn an_unknown_formula_aborts_the_whole_batch_before_anything_installs() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula(&server, "realpkg", "1.0.0", &[]).await;
        mount_missing_formula(&server, "ghostpkg").await;
        mount_empty_formula_index(&server).await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        let mut ui = StdUi::new();

        let err = super::execute(
            &mut installer,
            names(&["realpkg", "ghostpkg"]),
            false,
            false,
            &mut ui,
        )
        .await
        .unwrap_err();

        assert!(
            matches!(err, zb_core::Error::MissingFormula { ref name } if name == "ghostpkg"),
            "expected the unknown formula to be named, got {err:?}"
        );
        assert!(
            !installer.is_installed("realpkg"),
            "a plan failure must not install part of the batch"
        );
    }

    /// Once the batch is executing, failures stop being all-or-nothing: the
    /// packages that downloaded cleanly stay installed and only the broken one
    /// is rolled back.
    ///
    /// Two caveats this pins down rather than endorses, both worth their own
    /// issue:
    ///   * the command returns on the first `?` after `execute_formula_plan`,
    ///     so the "Installed N packages" summary never prints and the user is
    ///     never told which half of the batch succeeded;
    ///   * `suggest_homebrew` is then printed for *every* requested formula,
    ///     advising `brew install` even for the ones zbrew just installed.
    #[tokio::test]
    async fn keeps_the_rest_of_the_batch_when_one_download_fails() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula(&server, "okpkg1", "1.0.0", &[]).await;
        mount_formula(&server, "okpkg2", "1.0.0", &[]).await;
        mount_formula_with_failing_bottle(&server, "brokenpkg", "1.0.0").await;
        mount_empty_formula_index(&server).await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        let mut ui = StdUi::new();

        let result = super::execute(
            &mut installer,
            names(&["okpkg1", "brokenpkg", "okpkg2"]),
            false,
            false,
            &mut ui,
        )
        .await;

        assert!(result.is_err(), "a failed download must be reported");
        assert!(installer.is_installed("okpkg1"));
        assert!(installer.is_installed("okpkg2"));
        assert!(!installer.is_installed("brokenpkg"));
        assert!(!root.join("cellar/brokenpkg/1.0.0").exists());
    }
}
