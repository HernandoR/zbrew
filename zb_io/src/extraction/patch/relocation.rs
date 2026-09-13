//! The decisions the macOS patcher makes, kept away from macOS itself.
//!
//! Prefix rewriting, the "this path could not be rewritten" diagnosis and the
//! parsing of `codesign` output are pure string work: no filesystem, no
//! subprocesses, no `libc`. Keeping them here means the parts of the patcher
//! that are easy to get subtly wrong can be read — and tested — on their own.

/// Homebrew install prefixes that may be baked into a bottle, longest first so
/// that the longest match wins when one prefix contains another.
pub(crate) const HOMEBREW_PREFIXES: &[&str] = &[
    "/home/linuxbrew/.linuxbrew",
    "/usr/local/Homebrew",
    "/opt/homebrew",
    "/usr/local",
];

/// Sub-paths that mark `/usr/local/...` as a Homebrew path rather than a plain
/// system path. `/usr/local` is a shared location — `/usr/local/lib/libfoo.dylib`
/// is usually a genuine system library — so it is only rewritten when what
/// follows it belongs to Homebrew.
const HOMEBREW_SUBPATHS: &[&str] = &["/Cellar/", "/Caskroom/", "/Homebrew/", "/opt/"];

/// The default macOS install prefix.
///
/// Its length is not a matter of taste: it is the longest prefix that still fits
/// the tightest budget any supported platform imposes, which is `/usr/local`'s
/// 10 characters on Intel. At exactly 10 it fits there and has room to spare
/// against Apple Silicon's 13, so the default configuration needs no
/// per-architecture special case.
pub const DEFAULT_MACOS_PREFIX: &str = "/opt/zbrew";

/// Prefixes short enough to be worth suggesting to someone whose current one
/// does not fit, shortest last.
///
/// A root-level directory such as `/zb` is deliberately absent: SIP makes `/`
/// read-only, so creating one needs an `/etc/synthetic.conf` entry and a reboot,
/// and `validate_destructive_path` rejects a one-component path anyway. It was
/// never advice anyone could act on.
const SUGGESTED_PREFIXES: &[&str] = &[DEFAULT_MACOS_PREFIX, "/opt/zb"];

/// How many skipped paths a warning quotes before it stops listing them.
const EXAMPLES_IN_WARNING: usize = 3;

/// The Homebrew prefix that bottles for `os`/`arch` were built against, or
/// `None` when the platform imposes no length constraint.
///
/// This is the budget `zb init` has to judge a prefix against, before any bottle
/// exists to inspect. Linux is `None` because ELF rewriting resizes the strings
/// it patches, so a longer prefix costs nothing there.
///
/// `os` and `arch` are parameters rather than reads of `cfg!` or
/// `std::env::consts` so that both macOS architectures can be exercised from any
/// host. CI has no Intel macOS runner, which is how this budget came to be
/// hardcoded to Apple Silicon's 13 in the first place.
pub fn homebrew_prefix_for_host(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("/opt/homebrew"),
        // Any other macOS architecture is Intel, whose bottles are built against
        // the shorter `/usr/local`. Assuming the tighter budget is the safe way
        // to be wrong.
        ("macos", _) => Some("/usr/local"),
        _ => None,
    }
}

/// The Homebrew prefix that a bottle carrying `tag` was built against, or `None`
/// when the tag imposes no length constraint.
///
/// More precise than [`homebrew_prefix_for_host`], because it describes the
/// bottle actually selected rather than the host it is destined for. Homebrew
/// tags are `arm64_<codename>` for Apple Silicon, a bare `<codename>` for Intel,
/// `<arch>_linux` for Linux, and `all` for a bottle with no architecture at all.
pub fn homebrew_prefix_for_bottle_tag(tag: &str) -> Option<&'static str> {
    if tag == "all" || tag.ends_with("_linux") {
        return None;
    }

    if tag.starts_with("arm64_") {
        Some("/opt/homebrew")
    } else {
        Some("/usr/local")
    }
}

/// The Homebrew prefix that starts at `position`, if any.
///
/// A match has to sit on a path boundary: it must end at a `/` or at the end of
/// the string, so `/usr/localhost` is not `/usr/local`, and it must not follow a
/// `/`, so a doubled separator is not mistaken for the start of a prefix. What
/// comes before is otherwise unrestricted, because prefixes legitimately appear
/// mid-string in flags like `-L/opt/homebrew/lib` and in `PATH`-style lists.
pub(crate) fn homebrew_prefix_at(
    bytes: &[u8],
    position: usize,
    new_prefix: &str,
) -> Option<&'static str> {
    if position > 0 && bytes[position - 1] == b'/' {
        return None;
    }

    let rest = &bytes[position..];
    HOMEBREW_PREFIXES.iter().copied().find(|prefix| {
        if *prefix == new_prefix || !rest.starts_with(prefix.as_bytes()) {
            return false;
        }

        let tail = &rest[prefix.len()..];
        if !(tail.is_empty() || tail[0] == b'/') {
            return false;
        }

        *prefix != "/usr/local"
            || HOMEBREW_SUBPATHS
                .iter()
                .any(|sub| tail.starts_with(sub.as_bytes()))
    })
}

/// Rewrite every Homebrew prefix in `text` to `new_prefix`, or `None` when the
/// string mentions none of them.
///
/// The scan runs once from left to right and never re-examines what it has
/// emitted, so a new prefix that itself lives under an old one (say
/// `/usr/local/zerobrew`) cannot be rewritten a second time.
pub(crate) fn rewrite_homebrew_prefixes(text: &str, new_prefix: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut copied = 0;
    let mut position = 0;

    while position < bytes.len() {
        match homebrew_prefix_at(bytes, position, new_prefix) {
            Some(prefix) => {
                out.push_str(&text[copied..position]);
                out.push_str(new_prefix);
                position += prefix.len();
                copied = position;
            }
            None => position += 1,
        }
    }

    if out.is_empty() {
        return None;
    }

    out.push_str(&text[copied..]);
    Some(out)
}

/// The first Homebrew prefix mentioned anywhere in `text`.
fn first_homebrew_prefix(text: &str, new_prefix: &str) -> Option<&'static str> {
    let bytes = text.as_bytes();
    (0..bytes.len()).find_map(|position| homebrew_prefix_at(bytes, position, new_prefix))
}

/// A prefix that would have fitted, for the suggestion in the warning.
fn suggested_prefix(max_len: usize, new_prefix: &str) -> Option<&'static str> {
    SUGGESTED_PREFIXES
        .iter()
        .copied()
        .find(|candidate| candidate.len() <= max_len && *candidate != new_prefix)
}

/// Why a hardcoded path survived patching, and what to tell the user about it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TooLongToRewrite {
    /// How many strings could not be rewritten.
    pub count: usize,
    /// A few of them, for the message.
    pub examples: Vec<String>,
    /// The tightest budget any of them imposes: the shortest Homebrew prefix
    /// involved, which is the longest zerobrew prefix that would have fitted.
    pub budget: usize,
}

/// Decide whether the strings `rewrite_strings` refused to touch are worth
/// warning about, and which ones.
///
/// Only a *longer* zerobrew prefix can make a rewrite impossible: an equal or
/// shorter one always fits in the space the bottle already reserved. Anything
/// else in `skipped` is therefore not the length problem of issue #7 and is not
/// reported as one — this is what kept the warning firing on Apple Silicon,
/// where `/opt/homebrew` and `/opt/zerobrew` are both 13 characters long.
pub(crate) fn diagnose_skipped(skipped: &[String], new_prefix: &str) -> Option<TooLongToRewrite> {
    let mut count = 0;
    let mut examples: Vec<String> = Vec::new();
    let mut budget = usize::MAX;

    for text in skipped {
        let Some(old_prefix) = first_homebrew_prefix(text, new_prefix) else {
            continue;
        };
        if new_prefix.len() <= old_prefix.len() {
            continue;
        }

        count += 1;
        budget = budget.min(old_prefix.len());
        if examples.len() < EXAMPLES_IN_WARNING && !examples.contains(text) {
            examples.push(text.clone());
        }
    }

    (count > 0).then_some(TooLongToRewrite {
        count,
        examples,
        budget,
    })
}

impl TooLongToRewrite {
    /// The user-facing warning: which binary, why it was skipped, and the one
    /// thing that actually avoids it.
    pub(crate) fn message(&self, binary: &str, new_prefix: &str) -> String {
        let mut message = format!(
            "{binary}: {} hardcoded path(s) still point into Homebrew's prefix. \
             Rewriting them to {new_prefix} ({} characters) needs more room than the \
             {}-character prefix the bottle was built with, and a path stored in a \
             Mach-O string table cannot grow in place, so they were left as they are. \
             Anything in this package that looks those paths up at run time will fail. \
             Skipped: {}",
            self.count,
            new_prefix.len(),
            self.budget,
            self.examples.join(", "),
        );

        if self.count > self.examples.len() {
            message.push_str(&format!(" (+{} more)", self.count - self.examples.len()));
        }

        match suggested_prefix(self.budget, new_prefix) {
            Some(shorter) => message.push_str(&format!(
                ". Reinstalling zerobrew with a prefix of at most {} characters \
                 (for example `--prefix {shorter}`) avoids this entirely",
                self.budget
            )),
            None => message.push_str(&format!(
                ". Only a prefix of at most {} characters avoids this entirely",
                self.budget
            )),
        }

        message.push_str(". See https://github.com/HernandoR/zerobrew/issues/7");
        message
    }
}

/// Turn the output of `codesign -d --entitlements - [--xml]` into a plist that
/// can be handed straight back to `codesign --entitlements`.
///
/// Older `codesign` wraps the plist in an 8-byte code-signing blob header, and
/// every version writes its `Executable=...` chatter to stderr, so the payload
/// is located by looking for the plist itself rather than by trusting the
/// stream to start with it. A signature with no entitlements yields an empty
/// document, which is reported as `None`: re-signing with an empty entitlement
/// set is not the same thing as re-signing without one.
pub(crate) fn entitlements_plist(stdout: &[u8]) -> Option<String> {
    // The blob header is not valid UTF-8, so the payload is bounded in bytes
    // before anything is decoded.
    let start = find(stdout, b"<?xml").or_else(|| find(stdout, b"<plist"))?;
    let end = rfind(stdout, b"</plist>")? + "</plist>".len();
    let plist = std::str::from_utf8(stdout.get(start..end)?).ok()?;

    // An entitlement set is a dictionary of keys; without one there is nothing
    // worth carrying across the re-sign.
    plist.contains("<key>").then(|| plist.to_string())
}

/// Byte offset of the first occurrence of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Byte offset of the last occurrence of `needle` in `haystack`.
fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// Directories inside a keg that hold binaries which are executed directly.
///
/// `bin` and `sbin` are the obvious ones. `libexec` matters too: Homebrew puts
/// helper executables there, and some of them are spawned as their own process
/// rather than loaded as a library. lima 2.x is the case that motivated this --
/// its Virtualization.framework driver lives at `libexec/lima/lima-driver-vz`
/// and is signed, separately from `bin/limactl`, with the
/// `com.apple.security.virtualization` entitlement it needs in order to create
/// a VM. A helper that cannot be re-signed is a helper that cannot run.
pub(crate) const EXECUTABLE_DIRS: &[&str] = &["bin", "sbin", "libexec"];

/// Whether `relative` -- a path relative to the keg root -- lives somewhere
/// that holds directly executed binaries.
///
/// The path must be relative to the keg, not absolute: a prefix such as
/// `/usr/local/bin/...` would otherwise match on a component that has nothing
/// to do with the keg's own layout.
pub(crate) fn in_executable_dir(relative: &std::path::Path) -> bool {
    use std::path::Component;

    relative.components().any(|component| match component {
        Component::Normal(name) => EXECUTABLE_DIRS.iter().any(|dir| name == *dir),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const NEW_PREFIX: &str = "/opt/zb";

    #[test]
    fn the_host_budget_is_ten_on_intel_and_thirteen_on_apple_silicon() {
        // The whole of issue #86: these two differ, and the old constant knew
        // only the second one.
        assert_eq!(
            homebrew_prefix_for_host("macos", "x86_64").map(str::len),
            Some(10)
        );
        assert_eq!(
            homebrew_prefix_for_host("macos", "aarch64").map(str::len),
            Some(13)
        );
    }

    #[test]
    fn an_unfamiliar_macos_architecture_gets_the_tighter_budget() {
        assert_eq!(
            homebrew_prefix_for_host("macos", "powerpc"),
            Some("/usr/local")
        );
    }

    #[test]
    fn a_linux_host_has_no_prefix_length_budget() {
        // ELF rewriting resizes the strings it patches, so length is free.
        assert_eq!(homebrew_prefix_for_host("linux", "x86_64"), None);
        assert_eq!(homebrew_prefix_for_host("linux", "aarch64"), None);
    }

    #[test]
    fn bottle_tags_name_the_prefix_they_were_built_against() {
        assert_eq!(
            homebrew_prefix_for_bottle_tag("arm64_sonoma"),
            Some("/opt/homebrew")
        );
        assert_eq!(
            homebrew_prefix_for_bottle_tag("sonoma"),
            Some("/usr/local")
        );
        assert_eq!(homebrew_prefix_for_bottle_tag("all"), None);
    }

    #[test]
    fn an_arm64_linux_tag_is_linux_rather_than_apple_silicon() {
        // `arm64_linux` starts with `arm64_` but is not a macOS bottle, so the
        // Linux test has to win.
        assert_eq!(homebrew_prefix_for_bottle_tag("arm64_linux"), None);
        assert_eq!(homebrew_prefix_for_bottle_tag("aarch64_linux"), None);
        assert_eq!(homebrew_prefix_for_bottle_tag("x86_64_linux"), None);
    }

    #[test]
    fn the_default_prefix_fits_every_platform_it_ships_on() {
        // The regression guard for issue #86. `/opt/zerobrew` was 13 and failed
        // this on Intel; the check passed anyway because it was hardcoded to 13.
        for (os, arch) in [("macos", "x86_64"), ("macos", "aarch64")] {
            let budget = homebrew_prefix_for_host(os, arch)
                .map(str::len)
                .expect("macOS always has a budget");
            assert!(
                DEFAULT_MACOS_PREFIX.len() <= budget,
                "default prefix {DEFAULT_MACOS_PREFIX} ({} chars) does not fit the \
                 {budget}-character budget on {os}/{arch}",
                DEFAULT_MACOS_PREFIX.len(),
            );
        }
    }

    #[test]
    fn homebrew_prefixes_are_rewritten_on_path_boundaries_only() {
        let rewrite = |s: &str| rewrite_homebrew_prefixes(s, NEW_PREFIX);

        assert_eq!(
            rewrite("/opt/homebrew/lib/libfoo.dylib").as_deref(),
            Some("/opt/zb/lib/libfoo.dylib")
        );
        assert_eq!(
            rewrite("-L/home/linuxbrew/.linuxbrew/lib -L/opt/homebrew/lib").as_deref(),
            Some("-L/opt/zb/lib -L/opt/zb/lib")
        );
        // The longest prefix wins, and its replacement is not rewritten again.
        assert_eq!(
            rewrite("/usr/local/Homebrew/Library").as_deref(),
            Some("/opt/zb/Library")
        );
        // A match that runs into a longer component is not a prefix at all.
        assert_eq!(rewrite("/opt/homebrewery/lib"), None);
        assert_eq!(rewrite("/usr/locale/share"), None);
        // /usr/local is only Homebrew's when what follows says so.
        assert_eq!(rewrite("/usr/local/lib/libz.dylib"), None);
        assert_eq!(
            rewrite("/usr/local/Cellar/git/2.0/bin/git").as_deref(),
            Some("/opt/zb/Cellar/git/2.0/bin/git")
        );
        // Rewriting into a directory under an old prefix stays put after one pass.
        assert_eq!(
            rewrite_homebrew_prefixes("/usr/local/opt/git", "/usr/local/zb").as_deref(),
            Some("/usr/local/zb/opt/git")
        );
    }

    #[test]
    fn an_equal_length_prefix_is_never_diagnosed_as_too_long() {
        // Apple Silicon: /opt/homebrew and /opt/zerobrew are both 13 characters,
        // so nothing can fail to fit and nothing should be reported.
        let skipped = vec!["/opt/homebrew/opt/git/libexec/git-core".to_string()];
        assert_eq!(diagnose_skipped(&skipped, "/opt/zerobrew"), None);
        assert_eq!(diagnose_skipped(&skipped, "/opt/zb"), None);
        assert_eq!(diagnose_skipped(&[], "/opt/a/very/long/prefix"), None);
    }

    #[test]
    fn a_longer_prefix_is_diagnosed_with_the_tightest_budget() {
        // Intel: /usr/local is 10 characters, /opt/zerobrew is 13.
        let skipped = vec![
            "/usr/local/Cellar/gnupg/2.4.9/bin".to_string(),
            "/usr/local/Cellar/gnupg/2.4.9/libexec".to_string(),
            // 19 characters: /opt/zerobrew fits here, so this one is not part
            // of the length problem even though it was skipped.
            "/usr/local/Homebrew/Library".to_string(),
            "/usr/local/Cellar/gnupg/2.4.9/lib/gnupg".to_string(),
        ];

        let diagnosis = diagnose_skipped(&skipped, "/opt/zerobrew").expect("this cannot fit");
        assert_eq!(diagnosis.count, 3);
        assert_eq!(diagnosis.examples.len(), EXAMPLES_IN_WARNING);
        // /usr/local, not the longer /usr/local/Homebrew, sets the budget.
        assert_eq!(diagnosis.budget, "/usr/local".len());
    }

    #[test]
    fn a_path_with_no_homebrew_prefix_is_not_our_problem() {
        let skipped = vec!["/usr/local/lib/libz.dylib".to_string(), "plain".to_string()];
        assert_eq!(diagnose_skipped(&skipped, "/opt/zerobrew"), None);
    }

    #[test]
    fn the_warning_names_the_binary_the_cause_and_the_way_out() {
        let skipped = vec![
            "/usr/local/Cellar/gnupg/2.4.9/bin".to_string(),
            "/usr/local/Cellar/gnupg/2.4.9/libexec".to_string(),
            "/usr/local/Cellar/gnupg/2.4.9/lib/gnupg".to_string(),
            "/usr/local/Cellar/gnupg/2.4.9/share/gnupg".to_string(),
        ];
        let message = diagnose_skipped(&skipped, "/opt/zerobrew")
            .unwrap()
            .message(
                "/opt/zerobrew/Cellar/gnupg/2.4.9/bin/gpgconf",
                "/opt/zerobrew",
            );

        assert!(message.contains("/opt/zerobrew/Cellar/gnupg/2.4.9/bin/gpgconf"));
        assert!(message.contains("/usr/local/Cellar/gnupg/2.4.9/bin"));
        assert!(message.contains("(+1 more)"));
        assert!(message.contains("--prefix /opt/zb"));
        assert!(message.contains("issues/7"));
    }

    #[test]
    fn the_suggested_prefix_actually_fits() {
        assert_eq!(suggested_prefix(13, "/opt/zerobrew"), Some("/opt/zbrew"));
        assert_eq!(suggested_prefix(10, "/opt/zerobrew"), Some("/opt/zbrew"));
        assert_eq!(suggested_prefix(9, "/opt/zerobrew"), Some("/opt/zb"));
        // Nothing suggestable is shorter than `/opt/zb`, so below 7 there is no
        // advice to give rather than advice that cannot be followed.
        assert_eq!(suggested_prefix(6, "/opt/zerobrew"), None);
        // The prefix already in use is never suggested back to the user.
        assert_eq!(suggested_prefix(13, DEFAULT_MACOS_PREFIX), Some("/opt/zb"));
    }

    const PLIST: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\"><dict>\
         <key>com.apple.security.virtualization</key><true/>\
         </dict></plist>";

    #[test]
    fn entitlements_are_read_out_of_plain_xml_output() {
        let parsed = entitlements_plist(PLIST.as_bytes()).expect("a plist with one key");
        assert!(parsed.starts_with("<?xml"));
        assert!(parsed.ends_with("</plist>"));
        assert!(parsed.contains("com.apple.security.virtualization"));
    }

    #[test]
    fn entitlements_are_read_out_of_a_legacy_blob_wrapper() {
        // Older codesign prefixes the plist with an 8-byte blob header
        // (magic 0xfade7171 plus a length) and appends nothing.
        let mut raw = vec![0xfa, 0xde, 0x71, 0x71, 0x00, 0x00, 0x01, 0x00];
        raw.extend(PLIST.as_bytes());
        raw.push(b'\n');

        let parsed = entitlements_plist(&raw).expect("the wrapper must be stripped");
        assert!(parsed.starts_with("<?xml"));
        assert!(parsed.ends_with("</plist>"));
    }

    #[test]
    fn a_binary_without_entitlements_yields_nothing_to_preserve() {
        assert_eq!(entitlements_plist(b""), None);
        assert_eq!(entitlements_plist(b"Executable=/opt/zb/bin/foo\n"), None);
        assert_eq!(
            entitlements_plist(b"<?xml version=\"1.0\"?><plist version=\"1.0\"><dict/></plist>"),
            None
        );
        // Truncated output is not a plist we would dare re-sign with.
        assert_eq!(
            entitlements_plist(b"<?xml version=\"1.0\"?><plist><dict><key>a</key>"),
            None
        );
    }

    #[test]
    fn executables_are_recognised_in_bin_sbin_and_libexec() {
        for good in [
            "bin/limactl",
            "sbin/daemon",
            "libexec/lima/lima-driver-vz",
            "libexec/helper",
        ] {
            assert!(
                in_executable_dir(Path::new(good)),
                "{good} should be treated as an executable location"
            );
        }
    }

    #[test]
    fn libraries_and_data_are_not_executable_locations() {
        for other in [
            "lib/libfoo.dylib",
            "share/doc/readme",
            "include/foo.h",
            "Frameworks/Foo.framework/Foo",
        ] {
            assert!(
                !in_executable_dir(Path::new(other)),
                "{other} should not be treated as an executable location"
            );
        }
    }

    /// The check runs on the keg-relative path precisely so that a prefix which
    /// happens to contain `bin` does not drag every file in the keg into
    /// re-signing.
    #[test]
    fn a_prefix_containing_bin_does_not_match_by_itself() {
        assert!(!in_executable_dir(Path::new("lib/libfoo.dylib")));
        assert!(in_executable_dir(Path::new("bin/foo")));
    }
}
