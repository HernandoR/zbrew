// Pure string/plist logic used by the macOS patcher. Compiled on every platform
// -- not gated behind `target_os = "macos"` -- so that its unit tests actually
// run in CI regardless of the host. Nothing in here touches the filesystem,
// subprocesses or libc.
#[allow(dead_code)]
pub(crate) mod relocation;

// Signed archives are Homebrew's problem on every platform, not one patcher's,
// and the decision they need is pure: compiled and tested everywhere.
mod phar;

#[cfg(target_os = "linux")]
pub(crate) mod linux;

#[cfg(target_os = "macos")]
mod macho;

#[cfg(target_os = "macos")]
pub(crate) mod macos;
