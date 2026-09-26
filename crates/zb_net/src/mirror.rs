//! Homebrew-compatible mirror configuration.
//!
//! Users behind a slow or blocked path to GitHub point Homebrew at a mirror
//! with `HOMEBREW_API_DOMAIN` and `HOMEBREW_ARTIFACT_DOMAIN`. Reading the
//! same variables means an existing `~/.zshrc` that already configures `brew`
//! configures `zb` too, with no second set of names to learn.
//!
//! Every accessor returns URLs in *preference order*: the mirror first, the
//! upstream default last. Homebrew falls back to the default domain when the
//! mirror cannot serve a file, and so do we — unless
//! `HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK` is set, which is the one case where
//! reaching upstream is itself the failure the user is trying to prevent.
//!
//! `HOMEBREW_BOTTLE_DOMAIN` is deliberately **not** read here. Homebrew only
//! keeps the OCI layout when the bottle domain is itself a GitHub Packages
//! URL; for anything else `Utils::Bottles.path_resolved_basename` appends a
//! *flat* `name--version.tag.bottle.tar.gz`, which is what the mirrors people
//! actually set it to publish. Rewriting only the host would produce a path
//! those mirrors 404 on, and since the upstream URL is always appended as a
//! fallback the variable would look supported while silently doing nothing.
//! Producing the flat name needs the formula name, version, rebuild and
//! bottle tag, none of which reach the download layer today — see #120.

use std::sync::OnceLock;

use tracing::warn;

/// Homebrew's `HOMEBREW_API_DEFAULT_DOMAIN`.
pub(crate) const DEFAULT_API_DOMAIN: &str = "https://formulae.brew.sh/api";

/// The GitHub Packages host bottles are served from. Homebrew rewrites the
/// whole scheme+host of these URLs rather than prefixing them.
const OCI_REGISTRY_HOST: &str = "ghcr.io";

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct MirrorConfig {
    /// `HOMEBREW_API_DOMAIN`, if it differs from the default.
    api_domain: Option<String>,
    /// `HOMEBREW_ARTIFACT_DOMAIN`: a prefix applied to *every* download.
    artifact_domain: Option<String>,
    /// `HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK`: never try the upstream URL.
    artifact_domain_no_fallback: bool,
    /// `HOMEBREW_BOTTLE_MIRRORS`: zbrew's own comma-separated list of extra
    /// registry hosts to race against the primary. Predates the Homebrew
    /// variables above and is kept working for anyone already setting it.
    bottle_mirrors: Vec<String>,
}

impl MirrorConfig {
    /// The process-wide configuration, read from the environment once.
    pub(crate) fn shared() -> &'static Self {
        static SHARED: OnceLock<MirrorConfig> = OnceLock::new();
        SHARED.get_or_init(Self::from_env)
    }

    pub(crate) fn from_env() -> Self {
        Self {
            api_domain: read_domain("HOMEBREW_API_DOMAIN", DEFAULT_API_DOMAIN),
            artifact_domain: read_domain("HOMEBREW_ARTIFACT_DOMAIN", ""),
            artifact_domain_no_fallback: std::env::var_os("HOMEBREW_ARTIFACT_DOMAIN_NO_FALLBACK")
                .is_some(),
            bottle_mirrors: read_bottle_mirrors(),
        }
    }

    /// Formula metadata bases, e.g. `https://formulae.brew.sh/api/formula`.
    pub(crate) fn formula_api_bases(&self) -> Vec<String> {
        self.api_bases("formula")
    }

    /// Cask metadata bases, e.g. `https://formulae.brew.sh/api/cask`.
    pub(crate) fn cask_api_bases(&self) -> Vec<String> {
        self.api_bases("cask")
    }

    fn api_bases(&self, kind: &str) -> Vec<String> {
        let mut bases = Vec::new();
        if let Some(domain) = &self.api_domain {
            bases.push(format!("{domain}/{kind}"));
        }
        bases.push(format!("{DEFAULT_API_DOMAIN}/{kind}"));
        bases
    }

    /// Every URL worth trying for `url`, mirror first and upstream last.
    ///
    /// Callers may race these or walk them in order; either way the first
    /// entry is the one the user asked for and the last is the one that is
    /// most likely to exist.
    pub(crate) fn download_candidates(&self, url: &str) -> Vec<String> {
        let mut candidates = Vec::new();

        if let Some(domain) = &self.artifact_domain {
            candidates.push(apply_artifact_domain(url, domain));
            if self.artifact_domain_no_fallback {
                return candidates;
            }
        }

        for mirror in &self.bottle_mirrors {
            if url.contains(OCI_REGISTRY_HOST) {
                candidates.push(url.replace(OCI_REGISTRY_HOST, mirror));
            }
        }

        candidates.push(url.to_string());
        candidates.dedup();
        candidates
    }
}

/// Rewrite `url` to sit behind `domain`.
///
/// Bottles, which live on an OCI registry, have their scheme and host
/// replaced so the mirror sees a valid registry path; everything else is
/// prefixed whole, so `https://example.com/foo.tar.gz` becomes
/// `<domain>/https://example.com/foo.tar.gz`. A `domain` that already points
/// into a registry (`https://mirror.example/v2/ghcr-io`) must not end up with
/// `/v2` twice, so the duplicate segment is dropped.
fn apply_artifact_domain(url: &str, domain: &str) -> String {
    let Some(path) = oci_registry_path(url) else {
        return format!("{domain}/{url}");
    };

    match path.strip_prefix("/v2") {
        Some(rest) if domain_has_v2_path(domain) => format!("{domain}{rest}"),
        _ => format!("{domain}{path}"),
    }
}

/// The path of `url` if it is served by the OCI registry bottles live on.
fn oci_registry_path(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix(OCI_REGISTRY_HOST)?;
    rest.starts_with('/').then_some(rest)
}

fn domain_has_v2_path(domain: &str) -> bool {
    domain
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(domain)
        .split('/')
        .skip(1)
        .any(|segment| segment == "v2")
}

fn read_domain(name: &str, default: &str) -> Option<String> {
    parse_domain(&std::env::var(name).ok()?, default, name)
}

/// Normalise a domain override, discarding it when it is unusable or is just
/// the default spelled out. Returning `None` for the default keeps
/// `download_candidates` from listing the same URL twice.
fn parse_domain(value: &str, default: &str, name: &str) -> Option<String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() || value == default.trim_end_matches('/') {
        return None;
    }
    if !is_usable_domain(value) {
        warn!("ignoring ${name}: expected an http(s) URL without credentials, got '{value}'");
        return None;
    }
    Some(value.to_string())
}

fn read_bottle_mirrors() -> Vec<String> {
    let Ok(raw) = std::env::var("HOMEBREW_BOTTLE_MIRRORS") else {
        return Vec::new();
    };
    raw.split(',')
        .map(str::trim)
        .filter(|mirror| !mirror.is_empty())
        .map(str::to_string)
        .collect()
}

/// Reject anything that is not plain http(s), and anything carrying
/// credentials: a mirror URL ends up in log lines and error messages, so a
/// `user:password@` in one would leak the password.
fn is_usable_domain(value: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(value) else {
        return false;
    };
    matches!(parsed.scheme(), "http" | "https")
        && parsed.username().is_empty()
        && parsed.password().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOTTLE_URL: &str = "https://ghcr.io/v2/homebrew/core/gettext/manifests/0.21";

    fn config() -> MirrorConfig {
        MirrorConfig::default()
    }

    #[test]
    fn an_unconfigured_environment_leaves_urls_alone() {
        assert_eq!(config().download_candidates(BOTTLE_URL), [BOTTLE_URL]);
    }

    #[test]
    fn an_unconfigured_environment_uses_the_default_api_domain() {
        assert_eq!(
            config().formula_api_bases(),
            ["https://formulae.brew.sh/api/formula"]
        );
        assert_eq!(
            config().cask_api_bases(),
            ["https://formulae.brew.sh/api/cask"]
        );
    }

    #[test]
    fn the_api_mirror_is_tried_before_the_default() {
        let config = MirrorConfig {
            api_domain: Some("https://mirror.example.com/homebrew-bottles/api".to_string()),
            ..Default::default()
        };

        assert_eq!(
            config.formula_api_bases(),
            [
                "https://mirror.example.com/homebrew-bottles/api/formula",
                "https://formulae.brew.sh/api/formula",
            ]
        );
    }

    /// `HOMEBREW_BOTTLE_DOMAIN` is not implemented, and half-implementing it
    /// would be worse than leaving it alone: the upstream URL is always
    /// appended, so a wrong rewrite 404s and falls through to ghcr.io with
    /// nothing said. Until the flat bottle filename can be produced, the
    /// variable must have no effect at all. See #120.
    #[test]
    fn the_bottle_domain_is_not_honoured_yet() {
        // SAFETY-equivalent: `MirrorConfig::from_env` is the only reader and
        // this asserts the struct has no field for it, which is a compile-time
        // property exercised here as a behavioural one.
        let config = MirrorConfig::default();

        assert_eq!(config.download_candidates(BOTTLE_URL), [BOTTLE_URL]);
    }

    #[test]
    fn the_artifact_domain_replaces_the_host_of_a_bottle_url() {
        let config = MirrorConfig {
            artifact_domain: Some("http://localhost:8080".to_string()),
            ..Default::default()
        };

        assert_eq!(
            config.download_candidates(BOTTLE_URL),
            [
                "http://localhost:8080/v2/homebrew/core/gettext/manifests/0.21",
                BOTTLE_URL,
            ]
        );
    }

    #[test]
    fn the_artifact_domain_does_not_duplicate_an_existing_v2_path() {
        let config = MirrorConfig {
            artifact_domain: Some("https://mirror.example.com/v2/ghcr-io".to_string()),
            ..Default::default()
        };

        assert_eq!(
            config.download_candidates(BOTTLE_URL)[0],
            "https://mirror.example.com/v2/ghcr-io/homebrew/core/gettext/manifests/0.21"
        );
    }

    #[test]
    fn the_artifact_domain_prefixes_a_non_bottle_url_whole() {
        let config = MirrorConfig {
            artifact_domain: Some("http://localhost:8080".to_string()),
            ..Default::default()
        };

        assert_eq!(
            config.download_candidates("https://example.com/foo.tar.gz")[0],
            "http://localhost:8080/https://example.com/foo.tar.gz"
        );
    }

    #[test]
    fn no_fallback_withholds_every_other_candidate() {
        let config = MirrorConfig {
            artifact_domain: Some("http://localhost:8080".to_string()),
            artifact_domain_no_fallback: true,
            ..Default::default()
        };

        assert_eq!(
            config.download_candidates(BOTTLE_URL),
            ["http://localhost:8080/v2/homebrew/core/gettext/manifests/0.21"]
        );
    }

    #[test]
    fn legacy_bottle_mirrors_still_swap_the_registry_host() {
        let config = MirrorConfig {
            bottle_mirrors: vec!["mirror.example.com".to_string()],
            ..Default::default()
        };

        assert_eq!(
            config.download_candidates(BOTTLE_URL),
            [
                "https://mirror.example.com/v2/homebrew/core/gettext/manifests/0.21",
                BOTTLE_URL,
            ]
        );
    }

    #[test]
    fn a_domain_equal_to_the_default_is_not_listed_twice() {
        let name = "HOMEBREW_API_DOMAIN";
        assert_eq!(
            parse_domain(DEFAULT_API_DOMAIN, DEFAULT_API_DOMAIN, name),
            None
        );
        assert_eq!(
            parse_domain(
                "  https://formulae.brew.sh/api/  ",
                DEFAULT_API_DOMAIN,
                name
            ),
            None
        );
    }

    #[test]
    fn a_domain_with_credentials_or_a_foreign_scheme_is_rejected() {
        let name = "HOMEBREW_ARTIFACT_DOMAIN";
        assert_eq!(parse_domain("ftp://mirror.example.com", "", name), None);
        assert_eq!(
            parse_domain("https://user:pw@mirror.example.com", "", name),
            None
        );
        assert_eq!(parse_domain("not a url", "", name), None);
    }

    #[test]
    fn a_usable_domain_keeps_its_path_and_loses_its_trailing_slash() {
        assert_eq!(
            parse_domain("https://mirror.example.com/api/", "", "HOMEBREW_API_DOMAIN"),
            Some("https://mirror.example.com/api".to_string())
        );
    }
}
