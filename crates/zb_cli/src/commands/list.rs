use console::style;
use zb_store::InstalledKeg;

pub fn execute(
    installer: &mut zb_installer::Installer,
    all: bool,
    json: bool,
) -> Result<(), zb_core::Error> {
    let installed = installer.list_installed()?;
    let (shown, hidden) = partition_for_display(&installed, all);

    if json {
        return print_json(&shown);
    }

    if shown.is_empty() {
        println!("No formulas installed.");
    } else {
        print_grouped(&shown);
    }

    if hidden > 0 {
        println!(
            "{} {} temporary {} hidden; show with --all, remove with zb gc",
            style("==>").cyan().bold(),
            hidden,
            if hidden == 1 { "install" } else { "installs" }
        );
    }

    Ok(())
}

/// A cask is not a formula, and a flat alphabetical list buries the handful of
/// casks among the dependencies. `brew list` separates them the same way.
fn print_grouped(shown: &[&InstalledKeg]) {
    let (casks, formulas): (Vec<&InstalledKeg>, Vec<&InstalledKeg>) = shown
        .iter()
        .copied()
        .partition(|keg| cask_token(&keg.name).is_some());
    // With nothing to separate from, a heading is noise.
    let headed = !formulas.is_empty() && !casks.is_empty();

    for (heading, kegs) in [("Formulae", &formulas), ("Casks", &casks)] {
        if kegs.is_empty() {
            continue;
        }
        if headed {
            println!("{} {}", style("==>").cyan().bold(), style(heading).bold());
        }
        for keg in kegs {
            print_keg(keg);
        }
    }
}

fn print_keg(keg: &InstalledKeg) {
    let name = cask_token(&keg.name).unwrap_or(&keg.name);
    if keg.reason.is_transient() {
        println!(
            "{} {} {}",
            style(name).bold(),
            style(&keg.version).dim(),
            style("(temporary)").yellow()
        );
    } else {
        println!("{} {}", style(name).bold(), style(&keg.version).dim());
    }
}

/// Machine-readable output, for the callers that were parsing the coloured
/// human listing because there was nothing else to read.
fn print_json(shown: &[&InstalledKeg]) -> Result<(), zb_core::Error> {
    let entries: Vec<_> = shown
        .iter()
        .map(|keg| {
            let (kind, name) = match cask_token(&keg.name) {
                Some(token) => ("cask", token),
                None => ("formula", keg.name.as_str()),
            };
            serde_json::json!({
                "name": name,
                "kind": kind,
                "version": keg.version,
                "store_key": keg.store_key,
                "installed_at": keg.installed_at,
                "install_reason": keg.reason.as_str(),
            })
        })
        .collect();

    let rendered =
        serde_json::to_string_pretty(&entries).map_err(|e| zb_core::Error::FileError {
            message: format!("failed to render list as JSON: {e}"),
        })?;
    println!("{rendered}");
    Ok(())
}

/// Casks are recorded under a `cask:` install name so they cannot collide with
/// a formula of the same name.
fn cask_token(install_name: &str) -> Option<&str> {
    install_name.strip_prefix("cask:")
}

/// Split installed kegs into the ones to print and a count of the ones
/// withheld. Kegs `zb run`/`zbx` created are withheld unless `all`: they are
/// not something the user installed, and `zb gc` deletes them. See issue #36.
fn partition_for_display(installed: &[InstalledKeg], all: bool) -> (Vec<&InstalledKeg>, usize) {
    if all {
        return (installed.iter().collect(), 0);
    }

    let (shown, hidden): (Vec<_>, Vec<_>) =
        installed.iter().partition(|keg| !keg.reason.is_transient());

    (shown, hidden.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use zb_store::InstallReason;

    fn keg(name: &str, reason: InstallReason) -> InstalledKeg {
        InstalledKeg {
            name: name.to_string(),
            version: "1.0.0".to_string(),
            store_key: "key".to_string(),
            installed_at: 0,
            reason,
        }
    }

    #[test]
    fn transient_kegs_are_hidden_by_default() {
        let installed = vec![
            keg("jq", InstallReason::Retained),
            keg("wget", InstallReason::Transient),
        ];

        let (shown, hidden) = partition_for_display(&installed, false);

        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].name, "jq");
        assert_eq!(hidden, 1);
    }

    #[test]
    fn all_shows_transient_kegs_and_withholds_nothing() {
        let installed = vec![
            keg("jq", InstallReason::Retained),
            keg("wget", InstallReason::Transient),
        ];

        let (shown, hidden) = partition_for_display(&installed, true);

        assert_eq!(shown.len(), 2);
        assert_eq!(hidden, 0);
    }

    #[test]
    fn a_purely_retained_list_reports_nothing_hidden() {
        let installed = vec![keg("jq", InstallReason::Retained)];

        let (shown, hidden) = partition_for_display(&installed, false);

        assert_eq!(shown.len(), 1);
        assert_eq!(hidden, 0);
    }

    /// The `cask:` prefix is an internal key, not something to print or to
    /// hand a caller parsing the JSON.
    #[test]
    fn a_cask_is_reported_by_its_token_and_its_kind() {
        assert_eq!(cask_token("cask:docker"), Some("docker"));
        assert_eq!(cask_token("jq"), None);
    }
}
