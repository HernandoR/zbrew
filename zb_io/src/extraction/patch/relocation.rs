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

/// Prefixes short enough to be worth suggesting to someone whose current one
/// does not fit, shortest last.
const SUGGESTED_PREFIXES: &[&str] = &["/opt/zerobrew", "/opt/zbrew", "/opt/zb", "/zb"];

/// How many skipped paths a warning quotes before it stops listing them.
const EXAMPLES_IN_WARNING: usize = 3;

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

#[cfg(test)]
mod tests {
    use super::*;

    const NEW_PREFIX: &str = "/opt/zb";

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
        assert_eq!(suggested_prefix(4, "/opt/zerobrew"), Some("/zb"));
        assert_eq!(suggested_prefix(2, "/opt/zerobrew"), None);
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
}
