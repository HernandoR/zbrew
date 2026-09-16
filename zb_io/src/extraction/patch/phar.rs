//! Archives that carry a hash of their own bytes, and what relocation is
//! allowed to do to them.
//!
//! A PHP archive (`.phar`) ends with a digest of everything in front of it, and
//! PHP refuses to run one whose digest no longer matches. Patching such a file
//! is therefore not a question of doing it carefully: any edit at all breaks
//! it, whether or not it changes the file's length.
//!
//! That leaves exactly one useful thing relocation can do, and it is what
//! Homebrew does. Homebrew bottles these files with `@@HOMEBREW_PREFIX@@` in
//! place of the prefix they were built with, and pours them back only at the
//! default prefix -- `pour_bottle? only_if: :default_prefix` in the `composer`
//! formula, with the comment "Keg-relocation breaks the formula when it
//! replaces the prefix with a non-default value". Expanding the placeholders
//! back to the *Homebrew* prefix reproduces the signed bytes exactly, so the
//! archive runs. Expanding them to zbrew's prefix would not, and neither does
//! leaving them there: both are a file PHP will not load.
//!
//! Which Homebrew prefix a bottle was built against is not guessed. Each
//! candidate is tried and the archive's own signature says which one is right,
//! so a restoration is only written when it is provably the original.

use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use sha2::{Digest, Sha256, Sha512};
use tracing::{debug, warn};

use super::relocation::HOMEBREW_PREFIXES;

/// The four bytes every signed phar ends with.
const MAGIC: &[u8; 4] = b"GBMB";

/// The fixed tail behind the digest: a 4-byte signature type and [`MAGIC`].
const TRAILER_LEN: usize = 8;

/// Signature types, from php-src `ext/phar/phar_internal.h`. Only the two this
/// can actually recompute are named; MD5, SHA-1 and the OpenSSL types are left
/// unchecked rather than pulling in hashes solely to second-guess them.
const SIG_SHA256: u32 = 0x0003;
const SIG_SHA512: u32 = 0x0004;

/// The marker Homebrew leaves in place of a path it expects to substitute back.
const PLACEHOLDER_MARKER: &[u8] = b"@@HOMEBREW_";

/// What a signed archive needs, once its signature has been consulted.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Phar {
    /// The digest covers the bytes on disk: this file is already what it was
    /// signed as, so nothing may touch it.
    Intact,
    /// Homebrew placeholders expanded back to the prefix that makes the digest
    /// match, which is the prefix the archive was signed with.
    Restored {
        prefix: &'static str,
        contents: Vec<u8>,
    },
    /// Signed with an algorithm this does not recompute. Left alone, which is
    /// harmless unless it was placeholdered -- and then nothing here can help.
    Unchecked { placeholdered: bool },
    /// The digest does not match and no Homebrew prefix restores it.
    Broken,
}

/// A signature this can recompute, and so can verify a restoration against.
#[derive(Debug, Clone, Copy)]
enum Algorithm {
    Sha256,
    Sha512,
}

impl Algorithm {
    fn from_signature_type(kind: u32) -> Option<Self> {
        match kind {
            SIG_SHA256 => Some(Self::Sha256),
            SIG_SHA512 => Some(Self::Sha512),
            _ => None,
        }
    }

    /// How many bytes the digest itself occupies.
    fn digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha512 => 64,
        }
    }

    fn digest(self, body: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => Sha256::digest(body).to_vec(),
            Self::Sha512 => Sha512::digest(body).to_vec(),
        }
    }

    /// Whether the digest stored at the end of `contents` covers everything in
    /// front of it.
    fn verifies(self, contents: &[u8]) -> bool {
        let Some(split) = contents.len().checked_sub(TRAILER_LEN + self.digest_len()) else {
            return false;
        };

        self.digest(&contents[..split]) == contents[split..contents.len() - TRAILER_LEN]
    }
}

/// Inspect `contents` as a signed archive, or `None` when it is not one.
pub(crate) fn inspect(contents: &[u8]) -> Option<Phar> {
    let Some(algorithm) = Algorithm::from_signature_type(signature_type(contents)?) else {
        return Some(Phar::Unchecked {
            placeholdered: contains(contents, PLACEHOLDER_MARKER),
        });
    };

    let verify = |candidate: &[u8]| algorithm.verifies(candidate);

    if verify(contents) {
        return Some(Phar::Intact);
    }

    if !contains(contents, PLACEHOLDER_MARKER) {
        return Some(Phar::Broken);
    }

    let restored = HOMEBREW_PREFIXES.iter().copied().find_map(|prefix| {
        let candidate = expand_placeholders(contents, prefix);
        verify(&candidate).then_some(Phar::Restored {
            prefix,
            contents: candidate,
        })
    });

    Some(restored.unwrap_or(Phar::Broken))
}

/// The signature type of a phar, or `None` when the file carries no signature
/// trailer at all.
fn signature_type(contents: &[u8]) -> Option<u32> {
    let len = contents.len();
    if len < TRAILER_LEN || &contents[len - MAGIC.len()..] != MAGIC {
        return None;
    }

    let mut kind = [0u8; 4];
    kind.copy_from_slice(&contents[len - TRAILER_LEN..len - MAGIC.len()]);
    Some(u32::from_le_bytes(kind))
}

/// Substitute Homebrew's placeholders as an install into `prefix` would have.
///
/// The expansions follow Homebrew's own: the repository lives beside the prefix
/// except under `/usr/local`, where it is `/usr/local/Homebrew`. A wrong guess
/// costs one digest and is rejected by the caller, so the list of candidates
/// does not have to be narrowed to prefixes that make sense.
fn expand_placeholders(contents: &[u8], prefix: &str) -> Vec<u8> {
    let repository = if prefix == "/usr/local" {
        "/usr/local/Homebrew".to_string()
    } else {
        prefix.to_string()
    };

    let mut out = replace(contents, b"@@HOMEBREW_CELLAR@@", format!("{prefix}/Cellar"));
    out = replace(
        &out,
        b"@@HOMEBREW_LIBRARY@@",
        format!("{repository}/Library"),
    );
    out = replace(&out, b"@@HOMEBREW_REPOSITORY@@", repository);
    replace(&out, b"@@HOMEBREW_PREFIX@@", prefix.to_string())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn replace(haystack: &[u8], needle: &[u8], replacement: String) -> Vec<u8> {
    let mut out = Vec::with_capacity(haystack.len());
    let mut position = 0;

    while position < haystack.len() {
        if haystack[position..].starts_with(needle) {
            out.extend_from_slice(replacement.as_bytes());
            position += needle.len();
        } else {
            out.push(haystack[position]);
            position += 1;
        }
    }

    out
}

/// Whether `path` ends in a phar signature trailer, read without loading the
/// file. Every file in a keg passes through this, so it costs one seek.
///
/// The last four bytes are the whole test, which is also how PHP itself finds
/// the signature: `phar_parse_pharfile` reads the tail and looks for `GBMB`.
fn is_signed_archive(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    if file.seek(SeekFrom::End(-(MAGIC.len() as i64))).is_err() {
        return false;
    }

    let mut magic = [0u8; MAGIC.len()];
    file.read_exact(&mut magic).is_ok() && &magic == MAGIC
}

/// Deal with `path` if it is a self-signed archive, and report whether it was
/// one.
///
/// A `true` answer means the caller must leave the file alone: what could be
/// done for it has been done here, and rewriting paths in it can only break it.
pub(crate) fn restore_self_signed(path: &Path) -> bool {
    if !is_signed_archive(path) {
        return false;
    }

    let Ok(contents) = fs::read(path) else {
        // Unreadable, but the trailer says signed, so still not ours to rewrite.
        return true;
    };

    match inspect(&contents) {
        None => false,
        Some(Phar::Intact) => {
            debug!(
                path = %path.display(),
                "left a signed PHP archive untouched; any rewrite would invalidate its signature"
            );
            true
        }
        Some(Phar::Restored { prefix, contents }) => {
            match write_back(path, &contents) {
                Ok(()) => debug!(
                    path = %path.display(),
                    "restored the Homebrew placeholders in a signed PHP archive to {prefix}, \
                     the prefix its signature was computed over"
                ),
                Err(e) => warn!(
                    path = %path.display(),
                    error = %e,
                    "could not write back the restored PHP archive; it will not run"
                ),
            }
            true
        }
        Some(Phar::Unchecked {
            placeholdered: false,
        }) => true,
        Some(Phar::Unchecked {
            placeholdered: true,
        }) => {
            warn!(
                "{}",
                unrunnable(
                    path,
                    "Homebrew replaced a path inside it and it is signed with an algorithm \
                     zbrew does not recompute (MD5, SHA-1 or an OpenSSL key), so the bytes \
                     it was signed with cannot be identified"
                )
            );
            true
        }
        Some(Phar::Broken) => {
            warn!(
                "{}",
                unrunnable(
                    path,
                    "no Homebrew prefix reproduces the bytes it was signed with"
                )
            );
            true
        }
    }
}

/// The warning for an archive that will not run and cannot be repaired here.
fn unrunnable(path: &Path, why: &str) -> String {
    format!(
        "{}: this PHP archive carries a signature over its own bytes, and zbrew cannot leave \
         behind a copy that satisfies it, because {why}. Rewriting paths inside it would only \
         move the damage, so it was left as it is. The package is installed, but PHP will \
         refuse to run this program; install it from its own distribution instead. \
         See https://github.com/HernandoR/zbrew/issues/39",
        path.display(),
    )
}

/// Replace the file's contents, unlocking and relocking it the way bottles need.
fn write_back(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mode = fs::metadata(path)?.permissions().mode();
    let readonly = mode & 0o200 == 0;

    if readonly {
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o200))?;
    }
    let written = fs::write(path, contents);
    if readonly {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }

    written
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Build a phar-shaped file: a PHP stub, the halt marker, a body, and the
    /// SHA-512 trailer PHP checks the whole of it against.
    pub(crate) fn signed_phar(stub: &str, body: &[u8]) -> Vec<u8> {
        let mut phar = Vec::new();
        phar.extend_from_slice(stub.as_bytes());
        phar.extend_from_slice(b"__HALT_COMPILER(); ?>\r\n");
        phar.extend_from_slice(body);

        let digest = Sha512::digest(&phar);
        phar.extend_from_slice(&digest);
        phar.extend_from_slice(&SIG_SHA512.to_le_bytes());
        phar.extend_from_slice(MAGIC);
        phar
    }

    /// What Homebrew's bottling does to the archive: the prefix it was built
    /// against, replaced by the placeholder it expects to substitute back.
    pub(crate) fn bottled(contents: &[u8], prefix: &str) -> Vec<u8> {
        replace(
            contents,
            prefix.as_bytes(),
            "@@HOMEBREW_PREFIX@@".to_string(),
        )
    }

    /// The check PHP itself makes before running an archive.
    pub(crate) fn phar_signature_verifies(contents: &[u8]) -> bool {
        matches!(inspect(contents), Some(Phar::Intact))
    }

    /// The composer bottle's shape: a CA-bundle search list mentioning the
    /// prefix the bottle was built against, which Homebrew placeholdered.
    fn composer_shaped(prefix: &str) -> Vec<u8> {
        signed_phar(
            "#!/usr/bin/env php\n<?php Phar::mapPhar('composer.phar');\n",
            format!("\0\x02'{prefix}/etc/openssl@3/cert.pem',\n'/etc/pki/tls/cert.pem',\0")
                .as_bytes(),
        )
    }

    #[test]
    fn a_file_without_the_trailer_is_not_a_signed_archive() {
        assert_eq!(inspect(b""), None);
        assert_eq!(inspect(b"#!/bin/sh\necho hello\n"), None);
        assert_eq!(inspect(b"GBMB"), None);
    }

    #[test]
    fn an_untouched_archive_is_reported_intact() {
        assert_eq!(
            inspect(&composer_shaped("/opt/homebrew")),
            Some(Phar::Intact)
        );
    }

    /// The bug: Homebrew ships the archive with its prefix replaced by a
    /// placeholder, which is 6 bytes longer and hashes to something else.
    #[test]
    fn placeholders_are_expanded_to_the_prefix_the_signature_proves() {
        for prefix in ["/opt/homebrew", "/usr/local", "/home/linuxbrew/.linuxbrew"] {
            let original = composer_shaped(prefix);
            let bottled = bottled(&original, prefix);

            assert_ne!(
                bottled, original,
                "{prefix} has to be placeholdered for this to test anything"
            );
            assert_eq!(
                inspect(&bottled),
                Some(Phar::Restored {
                    prefix,
                    contents: original,
                }),
                "{prefix} should be identified by the signature alone"
            );
        }
    }

    #[test]
    fn the_cellar_and_repository_placeholders_are_expanded_too() {
        let original = signed_phar(
            "<?php\n",
            b"\0/opt/homebrew/Cellar/php/8.4.0\n/opt/homebrew/Library/Taps\n",
        );
        let bottled = replace(
            &replace(
                &original,
                b"/opt/homebrew/Cellar",
                "@@HOMEBREW_CELLAR@@".to_string(),
            ),
            b"/opt/homebrew/Library",
            "@@HOMEBREW_LIBRARY@@".to_string(),
        );

        assert_eq!(
            inspect(&bottled),
            Some(Phar::Restored {
                prefix: "/opt/homebrew",
                contents: original,
            })
        );
    }

    #[test]
    fn an_archive_no_prefix_restores_is_reported_broken() {
        // Placeholdered *and* edited: no substitution gets the bytes back.
        let mut bottled = bottled(&composer_shaped("/opt/homebrew"), "/opt/homebrew");
        bottled[0] = b' ';
        assert_eq!(inspect(&bottled), Some(Phar::Broken));

        // Damaged with no placeholder in sight: nothing to try.
        let mut damaged = composer_shaped("/usr/local");
        damaged[0] = b' ';
        assert_eq!(inspect(&damaged), Some(Phar::Broken));
    }

    #[test]
    fn an_algorithm_we_do_not_recompute_is_left_alone() {
        let mut md5_signed = composer_shaped("/opt/homebrew");
        let len = md5_signed.len();
        md5_signed[len - TRAILER_LEN..len - MAGIC.len()].copy_from_slice(&1u32.to_le_bytes());

        assert_eq!(
            inspect(&md5_signed),
            Some(Phar::Unchecked {
                placeholdered: false
            })
        );
    }

    #[test]
    fn a_restored_archive_keeps_the_mode_the_bottle_shipped() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("composer");

        let original = composer_shaped("/opt/homebrew");
        fs::write(&path, bottled(&original, "/opt/homebrew")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();

        assert!(restore_self_signed(&path));
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o555
        );
    }

    #[test]
    fn an_ordinary_file_is_not_claimed() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("script.sh");
        fs::write(&path, "#!/bin/sh\necho @@HOMEBREW_PREFIX@@\n").unwrap();

        assert!(!restore_self_signed(&path));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "#!/bin/sh\necho @@HOMEBREW_PREFIX@@\n",
            "claiming it would leave the text patcher's work undone"
        );
    }
}
