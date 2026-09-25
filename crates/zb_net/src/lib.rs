//! Networking for zbrew: the Homebrew API client, its sqlite response cache,
//! the bottle downloader and the shared TLS configuration.
//!
//! The public API of this crate is exactly the set of `pub use` re-exports
//! below. Every module is `pub(crate)`, so nothing else is reachable from
//! outside the crate — add a re-export here when a new item needs to cross the
//! crate boundary instead of widening a module.
//!
//! `unreachable_pub` is denied so that a `pub` item with no path out of the
//! crate is a compile error rather than silent API creep.
#![deny(unreachable_pub)]

pub(crate) mod api;
pub(crate) mod cache;
pub(crate) mod download;
pub(crate) mod suggest;
pub(crate) mod tap_formula;
pub(crate) mod tls;

pub use api::ApiClient;
pub use cache::ApiCache;
pub use download::{DownloadProgressCallback, DownloadRequest, DownloadResult, ParallelDownloader};
pub use tap_formula::parse_core_formula_ruby;
pub use tls::shared_tls_config;
