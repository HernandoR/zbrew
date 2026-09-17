//! Archive extraction and binary patching for zbrew.
//!
//! The public API of this crate is exactly the set of `pub use` re-exports
//! below. Every module is `pub(crate)`, so nothing else is reachable from
//! outside the crate — add a re-export here when a new item needs to cross the
//! crate boundary instead of widening a module.
//!
//! `unreachable_pub` is denied so that a `pub` item with no path out of the
//! crate is a compile error rather than silent API creep.
#![deny(unreachable_pub)]

pub(crate) mod extract;
pub(crate) mod patch;

pub use extract::{extract_archive, extract_tarball, is_archive};
pub use patch::relocation::{
    DEFAULT_MACOS_PREFIX, PrefixTooLong, check_prefix_fits, homebrew_prefix_for_host,
};

#[cfg(target_os = "linux")]
pub use patch::linux::patch_placeholders;

#[cfg(target_os = "macos")]
pub use patch::macos::{codesign_and_strip_xattrs, patch_homebrew_placeholders};
