// Pure string/plist logic used by the macOS patcher. Compiled on every platform
// -- not gated behind `target_os = "macos"` -- so that its unit tests actually
// run in CI regardless of the host. Nothing in here touches the filesystem,
// subprocesses or libc.
#[allow(dead_code)]
pub mod relocation;

// Signed archives are Homebrew's problem on every platform, not one patcher's,
// and the decision they need is pure: compiled and tested everywhere.
mod phar;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
mod macho;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "linux")]
pub use linux::patch_placeholders;

#[cfg(target_os = "macos")]
pub use macos::{codesign_and_strip_xattrs, patch_homebrew_placeholders};
