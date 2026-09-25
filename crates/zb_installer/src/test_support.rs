//! Fixtures shared by the crate's `wiremock`-backed tests.
//!
//! Every installer test needs the same three things: a bottle tarball, the
//! formula JSON the Homebrew API would answer with, and an `Installer` wired
//! to a mock server over a temporary root/prefix. Building those inline made
//! each test file carry its own copy of the same twenty lines, so they live
//! here instead.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::cellar::Cellar;
use crate::{Installer, Linker};
use zb_net::ApiClient;
use zb_store::BlobCache;
use zb_store::Database;
use zb_store::Store;

pub(crate) fn create_bottle_tarball(formula_name: &str) -> Vec<u8> {
    create_bottle_tarball_with_version(formula_name, "1.0.0")
}

pub(crate) fn create_bottle_tarball_with_version(formula_name: &str, version: &str) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tar::Builder;

    let mut builder = Builder::new(Vec::new());

    let content = format!("#!/bin/sh\necho {} v{}", formula_name, version);

    let mut header = tar::Header::new_gnu();
    header
        .set_path(format!("{}/{}/bin/{}", formula_name, version, formula_name))
        .unwrap();
    header.set_size(content.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();

    builder.append(&header, content.as_bytes()).unwrap();

    let tar_data = builder.into_inner().unwrap();

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar_data).unwrap();
    encoder.finish().unwrap()
}

pub(crate) fn create_bottle_tarball_with_entries(
    formula_name: &str,
    version: &str,
    entries: &[&str],
) -> Vec<u8> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    use tar::Builder;

    let mut builder = Builder::new(Vec::new());

    for rel_path in entries {
        let content = format!("#!/bin/sh\necho {formula_name} {rel_path}");
        let mut header = tar::Header::new_gnu();
        header
            .set_path(format!("{formula_name}/{version}/{rel_path}"))
            .unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append(&header, content.as_bytes()).unwrap();
    }

    let tar_data = builder.into_inner().unwrap();
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar_data).unwrap();
    encoder.finish().unwrap()
}

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    zb_core::checksum::sha256_hex(hasher)
}

pub(crate) fn get_test_bottle_tag() -> &'static str {
    if cfg!(target_os = "linux") {
        "x86_64_linux"
    } else if cfg!(target_arch = "x86_64") {
        "sonoma"
    } else {
        "arm64_sonoma"
    }
}

/// The URL path a bottle of `name`-`version` is served from, for the tag the
/// host running the tests resolves to.
pub(crate) fn bottle_path(name: &str, version: &str) -> String {
    format!(
        "/bottles/{name}-{version}.{tag}.bottle.tar.gz",
        tag = get_test_bottle_tag()
    )
}

/// Formula JSON for a bottled formula with no dependencies.
pub(crate) fn formula_json(name: &str, version: &str, bottle_url: &str, sha256: &str) -> String {
    formula_json_with_deps(name, version, bottle_url, sha256, &[])
}

/// Formula JSON for a bottled formula that depends on `deps`.
pub(crate) fn formula_json_with_deps(
    name: &str,
    version: &str,
    bottle_url: &str,
    sha256: &str,
    deps: &[&str],
) -> String {
    let tag = get_test_bottle_tag();
    let deps_json = deps
        .iter()
        .map(|dep| format!("\"{dep}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{
            "name": "{name}",
            "versions": {{ "stable": "{version}" }},
            "dependencies": [{deps_json}],
            "bottle": {{
                "stable": {{
                    "files": {{
                        "{tag}": {{
                            "url": "{bottle_url}",
                            "sha256": "{sha256}"
                        }}
                    }}
                }}
            }}
        }}"#
    )
}

/// The Ruby source of a tap formula whose bottle lives under `root_url`.
pub(crate) fn tap_formula_rb(
    class_name: &str,
    version: &str,
    root_url: &str,
    sha256: &str,
    deps: &[&str],
) -> String {
    let depends_on = deps
        .iter()
        .map(|dep| format!("  depends_on \"{dep}\"\n"))
        .collect::<String>();
    format!(
        r#"
class {class_name} < Formula
  version "{version}"
{depends_on}  bottle do
    root_url "{root_url}"
    sha256 {tag}: "{sha256}"
  end
end
"#,
        tag = get_test_bottle_tag()
    )
}

/// A stand-in for the Homebrew Cellar: `<prefix>/Cellar/<name>/<version>/.brew/<name>.rb`,
/// holding the formula Ruby source Homebrew keeps beside each keg. Returns
/// the prefix.
pub(crate) fn write_homebrew_keg(
    prefix: &Path,
    name: &str,
    version: &str,
    deps: &[&str],
    sha256: &str,
) -> PathBuf {
    let brew_dir = prefix.join("Cellar").join(name).join(version).join(".brew");
    fs::create_dir_all(&brew_dir).unwrap();
    fs::write(
        brew_dir.join(format!("{name}.rb")),
        core_formula_ruby(name, version, deps, sha256),
    )
    .unwrap();
    prefix.to_path_buf()
}

/// The Ruby source of a bottled homebrew-core formula, as the Cellar holds it.
pub(crate) fn core_formula_ruby(name: &str, version: &str, deps: &[&str], sha256: &str) -> String {
    let class_name = name
        .split(|c: char| !c.is_alphanumeric())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<String>();
    let depends_on = deps
        .iter()
        .map(|dep| format!("  depends_on \"{dep}\"\n"))
        .collect::<String>();

    format!(
        r#"class {class_name} < Formula
  desc "A formula the API no longer serves"
  homepage "https://example.invalid/{name}"
  version "{version}"
{depends_on}
  bottle do
    sha256 cellar: :any_skip_relocation, {tag}: "{sha256}"
  end
end
"#,
        tag = get_test_bottle_tag()
    )
}

/// Like [`core_formula_ruby`], but keg-only — the shape `openssl@3`,
/// `icu4c` and `libpq` have.
pub(crate) fn keg_only_core_formula_ruby(name: &str, version: &str, sha256: &str) -> String {
    core_formula_ruby(name, version, &[], sha256).replace(
        "  bottle do",
        "  keg_only :versioned_formula\n\n  bottle do",
    )
}

/// A mock Homebrew API plus the temporary root and prefix an `Installer`
/// built by [`TestEnv::installer`] operates on.
pub(crate) struct TestEnv {
    pub server: MockServer,
    pub root: PathBuf,
    pub prefix: PathBuf,
    // Kept alive so the root and prefix outlive every installer built here.
    _tmp: TempDir,
}

impl TestEnv {
    pub(crate) async fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();
        Self {
            server: MockServer::start().await,
            root,
            prefix,
            _tmp: tmp,
        }
    }

    pub(crate) fn uri(&self) -> String {
        self.server.uri()
    }

    /// The temporary directory holding [`Self::root`] and [`Self::prefix`],
    /// for fixtures that must live outside both.
    pub(crate) fn tmp_path(&self) -> &Path {
        self._tmp.path()
    }

    pub(crate) fn db_path(&self) -> PathBuf {
        self.root.join("db/zb.sqlite3")
    }

    /// The URL the mock server serves a bottle of `name`-`version` from.
    pub(crate) fn bottle_url(&self, name: &str, version: &str) -> String {
        format!("{}{}", self.uri(), bottle_path(name, version))
    }

    /// An installer reading formulae from the mock server's core API.
    pub(crate) fn installer(&self) -> Installer {
        self.installer_with(ApiClient::with_base_url(format!("{}/formula", self.uri())).unwrap())
    }

    /// An installer that also resolves tap formulae from the mock server.
    pub(crate) fn installer_with_taps(&self) -> Installer {
        self.installer_with(
            ApiClient::with_base_url(format!("{}/formula", self.uri()))
                .unwrap()
                .with_tap_raw_base_url(self.uri()),
        )
    }

    pub(crate) fn installer_with(&self, api_client: ApiClient) -> Installer {
        Installer::new(
            api_client,
            BlobCache::new(&self.root.join("cache")).unwrap(),
            Store::new(&self.root).unwrap(),
            Cellar::new(&self.root).unwrap(),
            Linker::new(&self.prefix).unwrap(),
            Database::open(&self.db_path()).unwrap(),
            self.prefix.clone(),
            self.root.join("locks"),
        )
    }

    /// Answer `GET /formula/<name>.json` with `body`.
    pub(crate) async fn mount_formula(&self, name: &str, body: impl Into<String>) {
        Mock::given(method("GET"))
            .and(path(format!("/formula/{name}.json")))
            .respond_with(ResponseTemplate::new(200).set_body_string(body.into()))
            .mount(&self.server)
            .await;
    }

    /// Serve `bottle` from the conventional bottle URL of `name`-`version`.
    pub(crate) async fn mount_bottle(&self, name: &str, version: &str, bottle: Vec<u8>) {
        Mock::given(method("GET"))
            .and(path(bottle_path(name, version)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
            .mount(&self.server)
            .await;
    }

    /// Serve a whole bottled formula — JSON and payload — and return its sha256.
    pub(crate) async fn mount_bottled_formula(
        &self,
        name: &str,
        version: &str,
        bottle: Vec<u8>,
    ) -> String {
        self.mount_bottled_formula_with_deps(name, version, &[], bottle)
            .await
    }

    pub(crate) async fn mount_bottled_formula_with_deps(
        &self,
        name: &str,
        version: &str,
        deps: &[&str],
        bottle: Vec<u8>,
    ) -> String {
        let sha = sha256_hex(&bottle);
        self.mount_formula(
            name,
            formula_json_with_deps(name, version, &self.bottle_url(name, version), &sha, deps),
        )
        .await;
        self.mount_bottle(name, version, bottle).await;
        sha
    }
}
