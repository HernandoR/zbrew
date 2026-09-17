//! On-disk state for zbrew: the sqlite database, the content-addressed blob
//! cache and the extracted-archive store.
//!
//! The public API of this crate is exactly the set of `pub use` re-exports
//! below. Every module is `pub(crate)`, so nothing else is reachable from
//! outside the crate — add a re-export here when a new item needs to cross the
//! crate boundary instead of widening a module.
//!
//! `unreachable_pub` is denied so that a `pub` item with no path out of the
//! crate is a compile error rather than silent API creep.
#![deny(unreachable_pub)]

pub(crate) mod blob;
pub(crate) mod db;
pub(crate) mod store;

pub use blob::{BlobCache, BlobWriter};
pub use db::{Database, InstallReason, InstallTransaction, InstalledKeg, KegFileRecord, StoreRef};
pub use store::Store;
