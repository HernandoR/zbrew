//! Fixtures shared by the command-level tests: a wiremock-backed Homebrew
//! API plus an `Installer` pointed at a scratch root.
//!
//! Mirrors `zb_io`'s `installer::install::test_support` so a command test
//! reads the same way as the installer tests it drives.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};
use std::io::Write as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zb_io::{ApiClient, BlobCache, Cellar, Database, Installer, Linker, Store};

/// The bottle tag this platform selects, so the mocked formula JSON offers a
/// bottle `select_bottle` accepts.
pub fn bottle_tag() -> &'static str {
    if cfg!(target_os = "linux") {
        "x86_64_linux"
    } else if cfg!(target_arch = "x86_64") {
        "sonoma"
    } else {
        "arm64_sonoma"
    }
}

/// A gzipped tarball laid out like a bottle: `<name>/<version>/bin/<name>`.
pub fn bottle_tarball(name: &str, version: &str) -> Vec<u8> {
    let content = format!("#!/bin/sh\necho {name} v{version}");

    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header
        .set_path(format!("{name}/{version}/bin/{name}"))
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

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher
        .finalize()
        .iter()
        .fold(String::with_capacity(64), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

fn formula_json(
    mock_uri: &str,
    name: &str,
    version: &str,
    deps: &[&str],
    bottle_sha: &str,
) -> String {
    let tag = bottle_tag();
    let deps_json = deps
        .iter()
        .map(|d| format!("\"{d}\""))
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
                            "url": "{mock_uri}/bottles/{name}-{version}.{tag}.bottle.tar.gz",
                            "sha256": "{bottle_sha}"
                        }}
                    }}
                }}
            }}
        }}"#
    )
}

fn bottle_path(name: &str, version: &str) -> String {
    format!(
        "/bottles/{name}-{version}.{tag}.bottle.tar.gz",
        tag = bottle_tag()
    )
}

/// Serve `name` at `version` plus its bottle, for as many requests as arrive.
/// Returns the bottle's sha256, which is also the store key it installs under.
pub async fn mount_formula(
    server: &MockServer,
    name: &str,
    version: &str,
    deps: &[&str],
) -> String {
    mount_formula_inner(server, name, version, deps, None).await
}

/// Like [`mount_formula`], but only answers `limit` requests. Registered
/// before a second mock for the same formula, this is how a test hands out an
/// old version to `install` and a newer one to everything that follows.
pub async fn mount_formula_up_to(
    server: &MockServer,
    name: &str,
    version: &str,
    deps: &[&str],
    limit: u64,
) -> String {
    mount_formula_inner(server, name, version, deps, Some(limit)).await
}

async fn mount_formula_inner(
    server: &MockServer,
    name: &str,
    version: &str,
    deps: &[&str],
    limit: Option<u64>,
) -> String {
    let bottle = bottle_tarball(name, version);
    let sha = sha256_hex(&bottle);

    let json = Mock::given(method("GET"))
        .and(path(format!("/formula/{name}.json")))
        .respond_with(ResponseTemplate::new(200).set_body_string(formula_json(
            &server.uri(),
            name,
            version,
            deps,
            &sha,
        )));
    match limit {
        Some(n) => json.up_to_n_times(n).mount(server).await,
        None => json.mount(server).await,
    }

    Mock::given(method("GET"))
        .and(path(bottle_path(name, version)))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
        .mount(server)
        .await;

    sha
}

/// Serve `name`'s metadata but fail its bottle download, so the formula plans
/// fine and only blows up once the batch is executing.
pub async fn mount_formula_with_failing_bottle(
    server: &MockServer,
    name: &str,
    version: &str,
) -> String {
    let bottle = bottle_tarball(name, version);
    let sha = sha256_hex(&bottle);

    Mock::given(method("GET"))
        .and(path(format!("/formula/{name}.json")))
        .respond_with(ResponseTemplate::new(200).set_body_string(formula_json(
            &server.uri(),
            name,
            version,
            &[],
            &sha,
        )))
        .mount(server)
        .await;

    Mock::given(method("GET"))
        .and(path(bottle_path(name, version)))
        .respond_with(ResponseTemplate::new(500).set_body_string("download failed"))
        .mount(server)
        .await;

    sha
}

/// A formula the API does not know about.
pub async fn mount_missing_formula(server: &MockServer, name: &str) {
    Mock::given(method("GET"))
        .and(path(format!("/formula/{name}.json")))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .mount(server)
        .await;
}

/// The bulk index `suggest_formulas` and the alias lookup fall back to. Mount
/// it whenever a test expects a miss, so neither reaches for the real API.
pub async fn mount_empty_formula_index(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/formula.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
        .mount(server)
        .await;
}

pub fn make_installer(root: &Path, prefix: &Path, mock_uri: &str) -> Installer {
    fs::create_dir_all(root.join("db")).unwrap();
    Installer::new(
        ApiClient::with_base_url(format!("{mock_uri}/formula")).unwrap(),
        BlobCache::new(&root.join("cache")).unwrap(),
        Store::new(root).unwrap(),
        Cellar::new(root).unwrap(),
        Linker::new(prefix).unwrap(),
        Database::open(&root.join("db/zb.sqlite3")).unwrap(),
        prefix.to_path_buf(),
        root.join("locks"),
    )
}
