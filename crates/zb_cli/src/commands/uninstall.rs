use crate::ui::StdUi;
use crate::utils::normalize_formula_name;
use console::style;

pub fn execute(
    installer: &mut zb_installer::Installer,
    formulas: Vec<String>,
    all: bool,
    ui: &mut StdUi,
) -> Result<(), zb_core::Error> {
    let formulas = if all {
        let installed = installer.list_installed()?;
        if installed.is_empty() {
            ui.info("No formulas installed.").map_err(ui_error)?;
            return Ok(());
        }
        installed.into_iter().map(|k| k.name).collect()
    } else {
        let mut normalized = Vec::with_capacity(formulas.len());
        for formula in formulas {
            normalized.push(normalize_formula_name(&formula)?);
        }
        normalized
    };

    if formulas.is_empty() {
        ui.info("No formulas to uninstall.").map_err(ui_error)?;
        return Ok(());
    }

    ui.heading(format!(
        "Uninstalling {}...",
        style(formulas.join(", ")).bold()
    ))
    .map_err(ui_error)?;

    let mut removed: Vec<String> = Vec::new();
    let mut failures: Vec<zb_core::PackageFailure> = Vec::new();

    if formulas.len() > 1 {
        for name in &formulas {
            ui.step_start(name).map_err(ui_error)?;
            match installer.uninstall(name) {
                Ok(()) => {
                    ui.step_ok().map_err(ui_error)?;
                    removed.push(name.clone());
                }
                Err(e) => {
                    ui.step_fail().map_err(ui_error)?;
                    failures.push(zb_core::PackageFailure::new(name.clone(), e));
                }
            }
        }
    } else if let Some(name) = formulas.first() {
        match installer.uninstall(name) {
            Ok(()) => removed.push(name.clone()),
            Err(e) => failures.push(zb_core::PackageFailure::new(name.clone(), e)),
        }
    }

    if !failures.is_empty() && !removed.is_empty() {
        ui.bullet(format!(
            "uninstalled: {}",
            style(removed.join(", ")).green()
        ))
        .map_err(ui_error)?;
    }

    for failure in &failures {
        ui.error(format!(
            "Failed to uninstall {}: {}",
            style(&failure.name).bold(),
            failure.error
        ))
        .map_err(ui_error)?;
    }

    // Every failure travels up, not just the first one.
    match zb_core::collapse_failures(failures) {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn ui_error(err: std::io::Error) -> zb_core::Error {
    zb_core::Error::StoreCorruption {
        message: format!("failed to write CLI output: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;
    use wiremock::MockServer;
    use zb_installer::Installer;

    use crate::commands::test_support::{make_installer, mount_formula};
    use crate::ui::StdUi;

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    async fn install_all(installer: &mut Installer, names: &[&str]) {
        for name in names {
            installer.install(&[name.to_string()], true).await.unwrap();
            assert!(installer.is_installed(name));
        }
    }

    /// `zb uninstall a b c` removes every name it was handed, kegs and prefix
    /// links included.
    #[tokio::test]
    async fn uninstalls_every_requested_formula_in_one_invocation() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        for name in ["rma", "rmb", "rmc"] {
            mount_formula(&server, name, "1.0.0", &[]).await;
        }

        let mut installer = make_installer(&root, &prefix, &server.uri());
        install_all(&mut installer, &["rma", "rmb", "rmc"]).await;

        let mut ui = StdUi::new();
        super::execute(
            &mut installer,
            names(&["rma", "rmb", "rmc"]),
            false,
            &mut ui,
        )
        .unwrap();

        for name in ["rma", "rmb", "rmc"] {
            assert!(!installer.is_installed(name), "{name} is still installed");
            assert!(!root.join(format!("cellar/{name}/1.0.0")).exists());
            assert!(!prefix.join("bin").join(name).exists());
        }
    }

    /// A name that is not installed does not abort the batch: the loop records
    /// the error, keeps going, and the command exits non-zero afterwards. One
    /// failure keeps its own error variant.
    #[tokio::test]
    async fn uninstall_continues_past_a_formula_that_is_not_installed() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        for name in ["rmx", "rmz"] {
            mount_formula(&server, name, "1.0.0", &[]).await;
        }

        let mut installer = make_installer(&root, &prefix, &server.uri());
        install_all(&mut installer, &["rmx", "rmz"]).await;

        let mut ui = StdUi::new();
        let err = super::execute(
            &mut installer,
            names(&["rmx", "neverinstalled", "rmz"]),
            false,
            &mut ui,
        )
        .unwrap_err();

        assert!(
            matches!(err, zb_core::Error::NotInstalled { ref name } if name == "neverinstalled"),
            "expected the uninstalled formula to be named, got {err:?}"
        );
        assert!(!installer.is_installed("rmx"));
        assert!(
            !installer.is_installed("rmz"),
            "a name after the failing one must still be uninstalled"
        );
    }

    /// Several names that are not installed are all reported, not just the
    /// first one the loop tripped over (issue #102).
    #[tokio::test]
    async fn uninstall_reports_every_name_it_could_not_remove() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        mount_formula(&server, "rmkeep", "1.0.0", &[]).await;

        let mut installer = make_installer(&root, &prefix, &server.uri());
        install_all(&mut installer, &["rmkeep"]).await;

        let mut ui = StdUi::new();
        let err = super::execute(
            &mut installer,
            names(&["ghostone", "rmkeep", "ghosttwo"]),
            false,
            &mut ui,
        )
        .unwrap_err();

        let zb_core::Error::BatchFailure { failures } = err else {
            panic!("expected both missing names to be reported, got {err:?}");
        };
        let mut failed: Vec<&str> = failures.iter().map(|f| f.name.as_str()).collect();
        failed.sort();
        assert_eq!(failed, ["ghostone", "ghosttwo"]);

        assert!(
            !installer.is_installed("rmkeep"),
            "the name that was installed must still be removed"
        );
    }

    /// `--all` is the other way a single invocation covers many formulas: the
    /// list comes from the database instead of the argv.
    #[tokio::test]
    async fn uninstall_all_removes_every_installed_formula() {
        let server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        for name in ["alla", "allb"] {
            mount_formula(&server, name, "1.0.0", &[]).await;
        }

        let mut installer = make_installer(&root, &prefix, &server.uri());
        install_all(&mut installer, &["alla", "allb"]).await;

        let mut ui = StdUi::new();
        super::execute(&mut installer, Vec::new(), true, &mut ui).unwrap();

        assert!(installer.list_installed().unwrap().is_empty());
        assert!(!prefix.join("bin/alla").exists());
        assert!(!prefix.join("bin/allb").exists());
    }
}
