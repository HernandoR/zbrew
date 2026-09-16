use console::style;

pub fn execute(installer: &mut zb_io::Installer) -> Result<(), zb_core::Error> {
    println!(
        "{} Running garbage collection...",
        style("==>").cyan().bold()
    );
    let outcome = installer.gc()?;

    if outcome.is_empty() {
        println!("Nothing to reclaim.");
        return Ok(());
    }

    for name in &outcome.removed_packages {
        println!(
            "    {} Removed temporary install {}",
            style("✓").green(),
            style(name).bold()
        );
    }

    for key in &outcome.removed_store_keys {
        println!("    {} Removed {}", style("✓").green(), short_key(key));
    }

    if !outcome.removed_packages.is_empty() {
        println!(
            "{} Removed {} temporary installs",
            style("==>").cyan().bold(),
            style(outcome.removed_packages.len()).green().bold()
        );
    }

    if !outcome.removed_store_keys.is_empty() {
        println!(
            "{} Removed {} store entries",
            style("==>").cyan().bold(),
            style(outcome.removed_store_keys.len()).green().bold()
        );
    }

    Ok(())
}

/// Abbreviate a store key for display. Store keys are sha256 hexes, but a
/// database repaired by hand can hold a shorter one and `&key[..12]` would
/// panic on it rather than print a row.
fn short_key(key: &str) -> &str {
    let end = key
        .char_indices()
        .nth(12)
        .map(|(i, _)| i)
        .unwrap_or(key.len());
    &key[..end]
}

#[cfg(test)]
mod tests {
    use super::short_key;

    #[test]
    fn truncates_a_full_store_key() {
        assert_eq!(short_key(&"a".repeat(64)), "a".repeat(12));
    }

    #[test]
    fn leaves_a_short_store_key_alone() {
        assert_eq!(short_key("abc"), "abc");
    }
}
