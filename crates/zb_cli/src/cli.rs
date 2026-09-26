use clap::error::ErrorKind;
use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "zb")]
#[command(about = "Zbrew - A fast Homebrew-compatible package installer")]
#[command(version)]
#[command(arg_required_else_help = true)]
pub struct Cli {
    #[arg(long, env = "ZBREW_ROOT", help = "Path to zbrew data directory")]
    pub root: Option<PathBuf>,

    #[arg(long, help = "Path to Homebrew-style prefix (overrides ZBREW_PREFIX)")]
    pub prefix: Option<PathBuf>,

    #[arg(
        long,
        default_value = "20",
        value_parser = parse_concurrency,
        help = "Number of concurrent download threads"
    )]
    pub concurrency: usize,

    #[arg(
        long = "auto-init",
        global = true,
        env = "ZBREW_AUTO_INIT",
        help = "Automatically initialize without prompting"
    )]
    pub auto_init: bool,

    #[arg(long, short = 'v', global = true, action = clap::ArgAction::Count, help = "Increase output verbosity")]
    pub verbose: u8,

    #[arg(
        long,
        short = 'q',
        global = true,
        conflicts_with = "verbose",
        help = "Suppress output (only show errors)"
    )]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Commands,
}

impl Cli {
    /// Parse the process arguments, answering a bare `zb` with the help text
    /// the way `brew` does: on stdout, exiting 0.
    ///
    /// clap renders the full help when the required subcommand is missing,
    /// but by design routes every error kind other than `DisplayHelp` and
    /// `DisplayVersion` to stderr with exit code 2. Asking a tool what it can
    /// do is not an error: `zb | less` should page the help, and
    /// `zb && echo ok` should print `ok`, both of which hold for `brew`.
    /// Anything the user actually got *wrong* still fails through clap.
    pub fn parse_or_help() -> Self {
        match Self::try_parse() {
            Ok(cli) => cli,
            Err(err) => {
                if !is_bare_invocation(std::env::args_os().len(), err.kind()) {
                    err.exit();
                }

                Self::command()
                    .print_help()
                    .expect("failed to write help to stdout");
                std::process::exit(0);
            }
        }
    }
}

/// `true` when clap's complaint is "you gave me nothing", not "you gave me
/// something wrong".
///
/// For this `Cli` -- whose subcommand is required implicitly, by being a
/// non-`Option` field rather than by an explicit `subcommand_required` -- the
/// kind alone already separates the two: a bare `zb` gives
/// `DisplayHelpOnMissingArgumentOrSubcommand` while `zb --root /tmp` gives
/// `MissingSubcommand`. The argument count is a guard, not the discriminator:
/// the kinds do collapse under other `subcommand_required` configurations
/// (clap-rs/clap#6397), and a future clap or a change to this struct must not
/// quietly turn an incomplete invocation into a success.
fn is_bare_invocation(argc: usize, kind: ErrorKind) -> bool {
    argc <= 1 && kind == ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
}

fn parse_concurrency(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("invalid value '{}': expected a positive integer", value))?;
    if parsed == 0 {
        return Err("concurrency must be at least 1".to_string());
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;
    use clap::error::ErrorKind;

    /// `Cli` is not `Debug`, so `unwrap_err` is unavailable.
    fn parse_error<const N: usize>(args: [&str; N]) -> clap::Error {
        match Cli::try_parse_from(args) {
            Ok(_) => panic!("expected {args:?} to be rejected"),
            Err(err) => err,
        }
    }

    #[test]
    fn a_bare_invocation_asks_for_help_rather_than_failing() {
        let err = parse_error(["zb"]);

        assert!(super::is_bare_invocation(1, err.kind()));
    }

    /// The kind alone carries this today; the assertion is on the kind so the
    /// test fails if that stops being true, rather than passing because the
    /// argument count happened to rule it out.
    #[test]
    fn an_incomplete_invocation_is_still_an_error() {
        let err = parse_error(["zb", "--root", "/tmp"]);

        assert_eq!(err.kind(), ErrorKind::MissingSubcommand);
        assert!(!super::is_bare_invocation(3, err.kind()));
    }

    #[test]
    fn a_bare_invocation_is_the_one_kind_that_asks_for_help() {
        assert_eq!(
            parse_error(["zb"]).kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn a_rejected_argument_is_still_an_error() {
        let err = parse_error(["zb", "--concurrency", "0", "list"]);

        assert!(!super::is_bare_invocation(1, err.kind()));
    }

    #[test]
    fn accepts_positive_concurrency() {
        let cli = Cli::try_parse_from(["zb", "--concurrency", "4", "list"]).unwrap();
        assert_eq!(cli.concurrency, 4);
    }

    #[test]
    fn rejects_zero_concurrency() {
        let result = Cli::try_parse_from(["zb", "--concurrency", "0", "list"]);
        assert!(result.is_err());
        let err = result.err().map(|e| e.to_string()).unwrap_or_default();
        assert!(err.contains("at least 1"));
    }

    #[test]
    fn accepts_verbose_levels() {
        let cli = Cli::try_parse_from(["zb", "-vv", "list"]).unwrap();
        assert_eq!(cli.verbose, 2);
        assert!(!cli.quiet);
    }

    #[test]
    fn rejects_quiet_with_verbose() {
        let result = Cli::try_parse_from(["zb", "-v", "-q", "list"]);
        assert!(result.is_err());
    }

    #[test]
    fn list_hides_temporary_installs_unless_all() {
        let cli = Cli::try_parse_from(["zb", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            super::Commands::List { all: false, .. }
        ));

        let cli = Cli::try_parse_from(["zb", "list", "--all"]).unwrap();
        assert!(matches!(
            cli.command,
            super::Commands::List { all: true, .. }
        ));

        let cli = Cli::try_parse_from(["zb", "list", "-a"]).unwrap();
        assert!(matches!(
            cli.command,
            super::Commands::List { all: true, .. }
        ));
    }

    #[test]
    fn install_accepts_the_cask_flag() {
        let cli = Cli::try_parse_from(["zb", "install", "--cask", "docker-desktop"]).unwrap();
        let super::Commands::Install { formulas, cask, .. } = cli.command else {
            panic!("expected an install command");
        };
        assert!(cask);
        assert_eq!(formulas, vec!["docker-desktop".to_string()]);
    }

    #[test]
    fn uninstall_accepts_the_cask_flag() {
        let cli = Cli::try_parse_from(["zb", "uninstall", "--cask", "docker-desktop"]).unwrap();
        let super::Commands::Uninstall { formulas, cask, .. } = cli.command else {
            panic!("expected an uninstall command");
        };
        assert!(cask);
        assert_eq!(formulas, vec!["docker-desktop".to_string()]);
    }

    #[test]
    fn uninstall_cask_and_all_conflict() {
        let result = Cli::try_parse_from(["zb", "uninstall", "--cask", "--all"]);
        assert!(result.is_err());
    }

    #[test]
    fn outdated_quiet_and_verbose_conflict() {
        let result = Cli::try_parse_from(["zb", "outdated", "--quiet", "--verbose"]);
        assert!(result.is_err());
    }

    #[test]
    fn outdated_quiet_and_json_conflict() {
        let result = Cli::try_parse_from(["zb", "outdated", "--quiet", "--json"]);
        assert!(result.is_err());
    }

    #[test]
    fn outdated_verbose_and_json_conflict() {
        let result = Cli::try_parse_from(["zb", "outdated", "--verbose", "--json"]);
        assert!(result.is_err());
    }
}

#[derive(Subcommand)]
pub enum Commands {
    /// Install formulas and casks
    Install {
        #[arg(required = true, num_args = 1..)]
        formulas: Vec<String>,
        #[arg(long, help = "Treat every argument as a cask token")]
        cask: bool,
        #[arg(long, help = "Do not create symlinks after installation")]
        no_link: bool,
        #[arg(long, short = 's', help = "Build from source instead of using bottles")]
        build_from_source: bool,
    },
    /// Install or dump from a Brewfile
    Bundle {
        #[command(subcommand)]
        command: Option<BundleCommands>,
    },
    /// Uninstall formulas and casks
    Uninstall {
        #[arg(required_unless_present = "all", num_args = 1..)]
        formulas: Vec<String>,
        #[arg(
            long,
            conflicts_with = "all",
            help = "Treat every argument as a cask token"
        )]
        cask: bool,
        #[arg(long, help = "Uninstall all installed packages")]
        all: bool,
    },
    /// Migrate packages from Homebrew
    Migrate {
        #[arg(long, short = 'y', help = "Skip confirmation prompts")]
        yes: bool,
        #[arg(long, help = "Force uninstall from Homebrew even if errors occur")]
        force: bool,
    },
    /// List installed packages
    List {
        #[arg(
            long,
            short = 'a',
            help = "Include temporary installs created by zb run"
        )]
        all: bool,
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    /// List installed formulas that nothing else installed depends on
    Leaves {
        #[arg(long, help = "Output as JSON")]
        json: bool,
    },
    /// Show information about an installed package
    Info {
        #[arg(help = "Name of the installed package")]
        formula: String,
    },
    /// Run diagnostics and optionally repair issues
    Doctor {
        #[arg(long, help = "Automatically repair detected issues")]
        repair: bool,
    },
    /// Remove unreferenced store entries
    Gc,
    /// Reset zbrew data directories
    Reset {
        #[arg(long, short = 'y', help = "Skip confirmation prompts")]
        yes: bool,
    },
    /// Initialize zbrew directories
    Init {
        #[arg(long, help = "Do not modify shell configuration files")]
        no_modify_path: bool,
    },
    /// Generate shell completions
    Completion {
        #[arg(
            value_enum,
            help = "Target shell for completions (e.g., bash, zsh, fish)"
        )]
        shell: clap_complete::shells::Shell,
    },
    /// Generate the zb manual page
    Man {
        #[arg(
            long,
            value_name = "DIR",
            help = "Write zb.1 and a page per subcommand to DIR instead of writing zb.1 to stdout"
        )]
        output_dir: Option<PathBuf>,
    },
    /// Run an installed formula as a command
    Run {
        #[arg(help = "Name of the formula to run")]
        formula: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Refresh cached formula metadata
    Update,
    /// List installed packages with newer versions available
    Outdated {
        #[arg(long, conflicts_with_all = ["quiet", "verbose"], help = "Output as JSON")]
        json: bool,
    },
    /// Upgrade installed packages to the latest versions
    Upgrade {
        #[arg(required = false, num_args = 0..)]
        formulas: Vec<String>,
        #[arg(long, short = 's', help = "Build from source instead of using bottles")]
        build_from_source: bool,
        #[arg(long, help = "Do not create symlinks after installation")]
        no_link: bool,
    },
}

#[derive(Subcommand)]
pub enum BundleCommands {
    /// Install packages from a Brewfile
    Install {
        #[arg(
            long,
            short = 'f',
            value_name = "FILE",
            default_value = "Brewfile",
            help = "Path to the Brewfile"
        )]
        file: PathBuf,
        #[arg(long, help = "Do not create symlinks after installation")]
        no_link: bool,
    },
    /// Report whether every Brewfile entry is installed
    Check {
        #[arg(
            long,
            short = 'f',
            value_name = "FILE",
            default_value = "Brewfile",
            help = "Path to the Brewfile"
        )]
        file: PathBuf,
    },
    /// Dump installed packages to a Brewfile
    Dump {
        #[arg(
            long,
            short = 'f',
            value_name = "FILE",
            default_value = "Brewfile",
            help = "Output file path"
        )]
        file: PathBuf,
        #[arg(long, help = "Overwrite existing file")]
        force: bool,
    },
}
