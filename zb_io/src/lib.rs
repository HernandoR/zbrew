//! I/O layer for zbrew: networking, storage, extraction, linking and install
//! orchestration.
//!
//! The public API of this crate is exactly the set of `pub use` re-exports
//! below. Every module is `pub(crate)`, so nothing else is reachable from
//! outside the crate — add a re-export here when a new item needs to cross the
//! crate boundary instead of widening a module.
//!
//! `unreachable_pub` is denied so that a `pub` item with no path out of the
//! crate is a compile error rather than silent API creep. The lint still
//! accepts a type that only leaks through a public signature, so items the
//! consumers never name are marked `pub(crate)` by hand as well.
#![deny(unreachable_pub)]

pub(crate) mod build;
pub(crate) mod cellar;
pub(crate) mod checksum;
pub(crate) mod extraction;
pub(crate) mod installer;
pub(crate) mod network;
pub(crate) mod path;
pub(crate) mod progress;
pub(crate) mod ssl;
pub(crate) mod storage;
#[cfg(test)]
pub(crate) mod test_support;

pub use cellar::{Cellar, Linker};
pub use extraction::patch::relocation::{
    DEFAULT_MACOS_PREFIX, PrefixTooLong, check_prefix_fits, homebrew_prefix_for_host,
};
pub use installer::{
    DiagnosticReport, ExecuteResult, GcOutcome, HomebrewMigrationPackages, HomebrewPackage,
    InstallPlan, Installer, OutdatedPackage, PlanFailure, RepairSummary, create_installer,
    get_homebrew_packages,
};
pub use network::ApiClient;
pub use path::{validate_destructive_path, validate_privileged_path};
pub use progress::{InstallProgress, ProgressCallback};
pub use ssl::{find_ca_bundle_from_prefix, find_ca_dir};
pub use storage::{BlobCache, Database, InstallReason, InstalledKeg, Store};
