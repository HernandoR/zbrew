use clap::CommandFactory;
use clap_mangen::Man;
use std::io;
use std::path::Path;

use crate::cli::Cli;

/// Give every page the same title, footer and version line: `clap_mangen`
/// upper-cases neither the title `.TH` carries by convention, nor fills the
/// date and manual fields.
///
/// The date is load-bearing, not decoration. `.TH` is positional, and an empty
/// field is written as nothing at all rather than `""`, so leaving the date out
/// shifts the source into its place and the manual into the source's -- which is
/// how `man zb` came to head its page "General Commands Manual".
fn page(cmd: clap::Command) -> Man {
    let title = cmd
        .get_display_name()
        .unwrap_or_else(|| cmd.get_name())
        .to_uppercase();
    let version = cmd.get_version().unwrap_or("").to_string();
    Man::new(cmd)
        .title(title)
        .date(chrono::Local::now().format("%Y-%m-%d").to_string())
        .source(format!("zbrew {version}").trim_end().to_string())
        .manual("zbrew Manual")
}

/// The command tree the pages describe. `disable_help_subcommand` keeps a
/// `zb-help.1` out of the set, and building it here is what gives each
/// subcommand its `zb-<name>` display name — and so its file name.
fn command() -> clap::Command {
    let mut cmd = Cli::command().disable_help_subcommand(true);
    cmd.build();
    cmd
}

/// Write `cmd`'s page, and one for each of its subcommands, into `dir`.
fn write_pages(cmd: clap::Command, dir: &Path) -> io::Result<()> {
    for sub in cmd.get_subcommands().filter(|s| !s.is_hide_set()).cloned() {
        write_pages(sub, dir)?;
    }
    page(cmd).generate_to(dir)?;
    Ok(())
}

pub fn execute(output_dir: Option<&Path>) -> Result<(), zb_core::Error> {
    let Some(dir) = output_dir else {
        let mut stdout = io::stdout().lock();
        return page(command())
            .render(&mut stdout)
            .map_err(zb_core::Error::file("failed to write the zb man page"));
    };

    std::fs::create_dir_all(dir).map_err(zb_core::Error::file(&format!(
        "failed to create {}",
        dir.display()
    )))?;
    write_pages(command(), dir).map_err(zb_core::Error::file(&format!(
        "failed to write man pages to {}",
        dir.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_top_level() -> String {
        let mut out = Vec::new();
        page(command())
            .render(&mut out)
            .expect("failed to render zb.1");
        String::from_utf8(out).expect("man page is not UTF-8")
    }

    fn title_line(page: &str) -> String {
        page.lines()
            .find(|line| line.starts_with(".TH "))
            .unwrap_or_else(|| panic!("no .TH header in page: {page}"))
            .to_string()
    }

    #[test]
    fn top_level_page_is_a_section_one_page_for_zb() {
        let page = render_top_level();
        let title = title_line(&page);
        assert!(title.starts_with(".TH ZB 1 "), "{title}");
        assert!(title.ends_with(r#""zbrew Manual""#), "{title}");
        // An empty `.TH` field is written as nothing, and would shift every
        // field after it one place to the left.
        assert!(!title.contains("  "), "empty .TH field in {title}");
        assert!(page.contains(".SH NAME"), "missing NAME section: {page}");
    }

    #[test]
    fn top_level_page_lists_the_subcommands() {
        let page = render_top_level();
        for subcommand in ["install", "uninstall", "bundle", "run", "completion", "man"] {
            assert!(
                page.contains(&format!("zb\\-{subcommand}")),
                "{subcommand} is missing from zb.1: {page}"
            );
        }
    }

    #[test]
    fn a_page_is_written_for_every_subcommand() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        execute(Some(dir.path())).expect("failed to generate man pages");

        let written: Vec<String> = std::fs::read_dir(dir.path())
            .expect("failed to read generated man pages")
            .map(|entry| {
                entry
                    .expect("bad dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        for expected in [
            "zb.1",
            "zb-install.1",
            "zb-uninstall.1",
            "zb-bundle.1",
            // Nested subcommands get a page of their own too.
            "zb-bundle-dump.1",
            "zb-man.1",
        ] {
            assert!(
                written.iter().any(|name| name == expected),
                "{expected} was not generated; got {written:?}"
            );
        }
        // `--help` is a flag on every page, not a command with a page.
        assert!(!written.iter().any(|name| name == "zb-help.1"));
    }

    #[test]
    fn a_subcommand_page_documents_that_subcommands_options() {
        let dir = tempfile::TempDir::new().expect("failed to create temp dir");
        execute(Some(dir.path())).expect("failed to generate man pages");

        let install = std::fs::read_to_string(dir.path().join("zb-install.1"))
            .expect("failed to read zb-install.1");
        assert!(install.contains(".TH ZB-INSTALL 1"), "{install}");
        assert!(install.contains("build\\-from\\-source"), "{install}");
    }
}
