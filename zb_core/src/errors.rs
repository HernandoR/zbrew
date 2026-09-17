use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConflictedLink {
    pub path: PathBuf,
    pub owned_by: Option<String>,
}

/// One package of a batch that did not make it, named alongside its reason.
///
/// Batch commands (`install`, `upgrade`, `uninstall`) keep one of these per
/// failed package instead of collapsing to a single error, so every failure
/// reaches the user and each one can be traced back to the package it came
/// from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageFailure {
    pub name: String,
    pub error: Error,
}

impl PackageFailure {
    pub fn new(name: impl Into<String>, error: Error) -> Self {
        Self {
            name: name.into(),
            error,
        }
    }
}

/// Collapse per-package failures into a single error for callers that have to
/// return a `Result`. A lone failure is returned as-is so single-package
/// commands keep their precise error variant; several become a `BatchFailure`
/// that names every one of them.
pub fn collapse_failures(failures: Vec<PackageFailure>) -> Option<Error> {
    match failures.len() {
        0 => None,
        1 => Some(failures.into_iter().next().unwrap().error),
        _ => Some(Error::BatchFailure { failures }),
    }
}

/// `BatchFailure` carries several packages' failures at once; build it with
/// [`collapse_failures`] rather than by hand, so a lone failure keeps its own
/// variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnsupportedBottle { name: String },
    ChecksumMismatch { expected: String, actual: String },
    LinkConflict { conflicts: Vec<ConflictedLink> },
    StoreCorruption { message: String },
    NetworkFailure { message: String },
    MissingFormula { name: String },
    UnsupportedTap { name: String },
    UnsupportedFormula { name: String, reason: String },
    DependencyCycle { cycle: Vec<String> },
    NotInstalled { name: String },
    FileError { message: String },
    InvalidArgument { message: String },
    ExecutionError { message: String },
    BatchFailure { failures: Vec<PackageFailure> },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::UnsupportedBottle { name } => {
                write!(f, "unsupported bottle for formula '{name}'")
            }
            Error::ChecksumMismatch { expected, actual } => {
                write!(f, "checksum mismatch (expected {expected}, got {actual})")
            }
            Error::LinkConflict { conflicts } => {
                if conflicts.len() == 1 {
                    let c = &conflicts[0];
                    write!(f, "link conflict at '{}'", c.path.display())?;
                    if let Some(ref owner) = c.owned_by {
                        write!(f, " (owned by {owner})")?;
                    }
                } else {
                    write!(f, "link conflicts:")?;
                    for c in conflicts {
                        write!(f, "\n  '{}'", c.path.display())?;
                        if let Some(ref owner) = c.owned_by {
                            write!(f, " (owned by {owner})")?;
                        }
                    }
                }
                Ok(())
            }
            Error::StoreCorruption { message } => write!(f, "store corruption: {message}"),
            Error::NetworkFailure { message } => write!(f, "network failure: {message}"),
            Error::MissingFormula { name } => write!(f, "missing formula '{name}'"),
            Error::UnsupportedTap { name } => {
                write!(
                    f,
                    "tap formula '{name}' is not supported (only homebrew/core)"
                )
            }
            Error::UnsupportedFormula { name, reason } => {
                write!(f, "formula '{name}' is not supported: {reason}")
            }
            Error::DependencyCycle { cycle } => {
                let rendered = cycle.join(" -> ");
                write!(f, "dependency cycle detected: {rendered}")
            }
            Error::NotInstalled { name } => write!(f, "formula '{name}' is not installed"),
            Error::FileError { message } => write!(f, "file error: {message}"),
            Error::InvalidArgument { message } => write!(f, "invalid argument: {message}"),
            Error::ExecutionError { message } => write!(f, "{message}"),
            Error::BatchFailure { failures } => {
                write!(f, "{} packages failed:", failures.len())?;
                for failure in failures {
                    write!(f, "\n  {}: {}", failure.name, failure.error)?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for Error {}

macro_rules! error_helpers {
    ($($fn_name:ident => $variant:ident),* $(,)?) => {
        impl Error {
            $(
                pub fn $fn_name<E: fmt::Display>(ctx: &str) -> impl FnOnce(E) -> Self + '_ {
                    move |err| Self::$variant { message: format!("{ctx}: {err}") }
                }
            )*
        }
    };
}

error_helpers! {
    store   => StoreCorruption,
    network => NetworkFailure,
    file    => FileError,
    exec    => ExecutionError,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_bottle_display_includes_name() {
        let err = Error::UnsupportedBottle {
            name: "libheif".to_string(),
        };

        assert!(err.to_string().contains("libheif"));
    }

    #[test]
    fn collapse_failures_keeps_a_lone_error_intact() {
        let failures = vec![PackageFailure::new(
            "ghost",
            Error::MissingFormula {
                name: "ghost".to_string(),
            },
        )];

        assert_eq!(
            collapse_failures(failures),
            Some(Error::MissingFormula {
                name: "ghost".to_string()
            })
        );
    }

    #[test]
    fn collapse_failures_names_every_package_when_several_fail() {
        let failures = vec![
            PackageFailure::new(
                "one",
                Error::NetworkFailure {
                    message: "timed out".to_string(),
                },
            ),
            PackageFailure::new(
                "two",
                Error::MissingFormula {
                    name: "two".to_string(),
                },
            ),
        ];

        let rendered = collapse_failures(failures).unwrap().to_string();

        assert!(rendered.contains("2 packages failed"));
        assert!(rendered.contains("one: network failure: timed out"));
        assert!(rendered.contains("two: missing formula 'two'"));
    }

    #[test]
    fn collapse_failures_is_none_when_nothing_failed() {
        assert_eq!(collapse_failures(Vec::new()), None);
    }
}
