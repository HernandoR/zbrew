use console::style;

pub async fn execute(
    installer: &mut zb_installer::Installer,
    json: bool,
) -> Result<(), zb_core::Error> {
    let leaves = installer.leaves().await?;

    if json {
        let rendered =
            serde_json::to_string_pretty(&leaves.names).map_err(|e| zb_core::Error::FileError {
                message: format!("failed to render leaves as JSON: {e}"),
            })?;
        println!("{rendered}");
        return Ok(());
    }

    for name in &leaves.names {
        println!("{name}");
    }

    // Warnings go to stderr so `zb leaves | xargs zb uninstall` still gets a
    // clean list of names, while the incompleteness is still stated.
    for warning in &leaves.warnings {
        eprintln!(
            "{} could not read dependencies for {warning}",
            style("Warning:").yellow().bold()
        );
    }

    Ok(())
}
