//! Install orchestration for zbrew: planning, install/upgrade/uninstall,
//! doctor, outdated and gc, plus the cellar linking and source-build steps
//! those flows drive.
//!
//! The public API of this crate is exactly the set of `pub use` re-exports
//! below. Every module is `pub(crate)`, so nothing else is reachable from
//! outside the crate — add a re-export here when a new item needs to cross the
//! crate boundary instead of widening a module.
//!
//! `unreachable_pub` is denied so that a `pub` item with no path out of the
//! crate is a compile error rather than silent API creep.
#![deny(unreachable_pub)]

pub(crate) mod build;
pub(crate) mod cellar;
pub(crate) mod homebrew;
pub(crate) mod install;
#[cfg(test)]
pub(crate) mod test_support;

pub use cellar::{Cellar, Linker};
pub use homebrew::{HomebrewMigrationPackages, HomebrewPackage, get_homebrew_packages};
pub use install::doctor::{DiagnosticReport, RepairSummary};
pub use install::{
    ExecuteResult, GcOutcome, InstallPlan, Installer, Leaves, OutdatedPackage, create_installer,
};
