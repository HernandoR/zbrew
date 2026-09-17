use console::style;
use zb_io::InstalledKeg;

pub fn execute(installer: &mut zb_io::Installer, all: bool) -> Result<(), zb_core::Error> {
    let installed = installer.list_installed()?;
    let (shown, hidden) = partition_for_display(&installed, all);

    if shown.is_empty() {
        println!("No formulas installed.");
    } else {
        for keg in shown {
            if keg.reason.is_transient() {
                println!(
                    "{} {} {}",
                    style(&keg.name).bold(),
                    style(&keg.version).dim(),
                    style("(temporary)").yellow()
                );
            } else {
                println!("{} {}", style(&keg.name).bold(), style(&keg.version).dim());
            }
        }
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
    use zb_io::InstallReason;

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
}
