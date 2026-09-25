use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use zb_core::Error;
use zb_net::DownloadRequest;
use zb_store::InstallReason;

use super::Installer;
use super::bottle::FailedInstallGuard;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaskBinary {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedCask {
    pub install_name: String,
    pub token: String,
    pub version: String,
    pub url: String,
    pub sha256: String,
    pub binaries: Vec<CaskBinary>,
}

pub(crate) fn resolve_cask(token: &str, cask: &Value) -> Result<ResolvedCask, Error> {
    let mut url = required_string(cask, "url")?;
    let mut sha256 = required_string(cask, "sha256")?;
    let version = required_string(cask, "version")?;

    if let Some(variation) = select_platform_variation(cask) {
        if let Some(variation_url) = variation.get("url").and_then(Value::as_str) {
            url = variation_url.to_string();
        }
        if let Some(variation_sha) = variation.get("sha256").and_then(Value::as_str) {
            sha256 = variation_sha.to_string();
        }
    }

    if sha256 == "no_check" {
        return Err(Error::InvalidArgument {
            message: format!("cask '{token}' uses an unsupported checksum mode: no_check"),
        });
    }

    let binaries = parse_binary_artifacts(cask)?;
    if binaries.is_empty() {
        let found = artifact_types(cask);
        return Err(Error::InvalidArgument {
            message: format!(
                "cask '{token}' has no binary artifacts (found: {found}); \
                 only casks with 'binary' artifacts are currently supported"
            ),
        });
    }

    Ok(ResolvedCask {
        install_name: format!("cask:{token}"),
        token: token.to_string(),
        version,
        url,
        sha256,
        binaries,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String, Error> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .ok_or_else(|| Error::InvalidArgument {
            message: format!("failed to parse cask JSON: missing string field '{field}'"),
        })
}

fn select_platform_variation(cask: &Value) -> Option<&Value> {
    let variations = cask.get("variations")?;
    preferred_variation_keys()
        .iter()
        .find_map(|key| variations.get(key))
}

fn preferred_variation_keys() -> &'static [&'static str] {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        &["x86_64_linux", "arm64_linux"]
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        &["arm64_linux", "x86_64_linux"]
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        &[
            "arm64_tahoe",
            "arm64_sequoia",
            "arm64_sonoma",
            "arm64_ventura",
            "arm64_monterey",
            "arm64_big_sur",
        ]
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        &[
            "tahoe", "sequoia", "sonoma", "ventura", "monterey", "big_sur", "catalina",
        ]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        &[]
    }
}

fn artifact_types(cask: &Value) -> String {
    let types: Vec<&str> = cask
        .get("artifacts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|a| a.as_object())
        .flat_map(|obj| obj.keys())
        .map(String::as_str)
        .collect();

    if types.is_empty() {
        "none".to_string()
    } else {
        types.join(", ")
    }
}

fn parse_binary_artifacts(cask: &Value) -> Result<Vec<CaskBinary>, Error> {
    let mut binaries = Vec::new();
    let artifacts = cask
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidArgument {
            message: "failed to parse cask JSON: missing artifacts array".to_string(),
        })?;

    for artifact in artifacts {
        let Some(entries) = artifact.get("binary").and_then(Value::as_array) else {
            continue;
        };

        for entry in entries {
            let (source, target) = parse_binary_entry(entry)?;
            binaries.push(CaskBinary { source, target });
        }
    }

    Ok(binaries)
}

fn parse_binary_entry(entry: &Value) -> Result<(String, String), Error> {
    if let Some(path) = entry.as_str() {
        return Ok((path.to_string(), basename(path)?));
    }

    let array = entry.as_array().ok_or_else(|| Error::InvalidArgument {
        message: "unsupported cask binary artifact shape".to_string(),
    })?;
    let source = array
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidArgument {
            message: "unsupported cask binary source".to_string(),
        })?;

    let target = array
        .get(1)
        .and_then(Value::as_object)
        .and_then(|obj| obj.get("target"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| basename(source).unwrap_or_else(|_| source.to_string()));

    if target.contains('/') || target.contains('$') || target.contains('~') {
        return Err(Error::InvalidArgument {
            message: format!("unsupported cask binary target path '{target}'"),
        });
    }

    Ok((source.to_string(), target))
}

fn basename(path: &str) -> Result<String, Error> {
    let name = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::InvalidArgument {
            message: format!("invalid cask binary path '{path}'"),
        })?;
    Ok(name.to_string())
}

impl Installer {
    pub(super) async fn install_single_cask(
        &mut self,
        token: &str,
        link: bool,
    ) -> Result<(), Error> {
        let cask_json = self.api_client.get_cask(token).await?;
        let cask = resolve_cask(token, &cask_json)?;

        let blob_path = self
            .downloader
            .download_single(
                DownloadRequest {
                    url: cask.url.clone(),
                    sha256: cask.sha256.clone(),
                    name: cask.install_name.clone(),
                },
                None,
            )
            .await?;

        let keg_path = self.cellar.keg_path(&cask.install_name, &cask.version);
        let mut cleanup = FailedInstallGuard::new(
            &self.linker,
            &self.cellar,
            &cask.install_name,
            &cask.version,
            &keg_path,
            link,
        );

        if zb_extract::is_archive(&blob_path)? {
            let extracted = self.store.ensure_entry(&cask.sha256, &blob_path)?;
            stage_cask_binaries(&extracted, &keg_path, &cask)?;
        } else {
            stage_raw_cask_binary(&blob_path, &keg_path, &cask)?;
        }

        let linked_files = if link {
            self.linker.link_keg(&keg_path)?
        } else {
            Vec::new()
        };

        let tx = self.db.transaction()?;
        tx.record_install(
            &cask.install_name,
            &cask.version,
            &cask.sha256,
            InstallReason::Retained,
        )?;
        for linked in &linked_files {
            tx.record_linked_file(
                &cask.install_name,
                &cask.version,
                &linked.link_path.to_string_lossy(),
                &linked.target_path.to_string_lossy(),
            )?;
        }
        tx.commit()?;

        cleanup.disarm();
        Ok(())
    }
}

fn stage_cask_binaries(
    extracted_root: &Path,
    keg_path: &Path,
    cask: &ResolvedCask,
) -> Result<(), Error> {
    let bin_dir = keg_path.join("bin");
    fs::create_dir_all(&bin_dir).map_err(Error::store("failed to create cask bin dir"))?;

    for binary in &cask.binaries {
        let source = resolve_cask_source_path(extracted_root, cask, &binary.source)?;
        if !source.exists() {
            return Err(Error::InvalidArgument {
                message: format!(
                    "cask '{}' binary source '{}' not found in archive",
                    cask.token, binary.source
                ),
            });
        }

        let target = bin_dir.join(&binary.target);
        if target.exists() {
            fs::remove_file(&target)
                .map_err(Error::store("failed to replace existing cask binary"))?;
        }

        fs::copy(&source, &target).map_err(|e| Error::StoreCorruption {
            message: format!("failed to stage cask binary '{}': {e}", binary.target),
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = fs::metadata(&target)
                .map_err(Error::store("failed to read staged cask binary metadata"))?
                .permissions();
            if perms.mode() & 0o111 == 0 {
                perms.set_mode(0o755);
                fs::set_permissions(&target, perms)
                    .map_err(Error::store("failed to make staged cask binary executable"))?;
            }
        }
    }

    Ok(())
}

fn stage_raw_cask_binary(
    blob_path: &Path,
    keg_path: &Path,
    cask: &ResolvedCask,
) -> Result<(), Error> {
    if cask.binaries.len() != 1 {
        return Err(Error::InvalidArgument {
            message: format!(
                "cask '{}' has {} binary artifacts but the download is a raw binary; expected exactly 1",
                cask.token,
                cask.binaries.len()
            ),
        });
    }

    let binary = &cask.binaries[0];
    let bin_dir = keg_path.join("bin");
    fs::create_dir_all(&bin_dir).map_err(Error::store("failed to create cask bin dir"))?;

    let target = bin_dir.join(&binary.target);
    if target.exists() {
        fs::remove_file(&target).map_err(Error::store("failed to replace existing cask binary"))?;
    }

    fs::copy(blob_path, &target).map_err(|e| Error::StoreCorruption {
        message: format!("failed to stage cask binary '{}': {e}", binary.target),
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755))
            .map_err(Error::store("failed to make staged cask binary executable"))?;
    }

    Ok(())
}

fn resolve_cask_source_path(
    extracted_root: &Path,
    cask: &ResolvedCask,
    source: &str,
) -> Result<PathBuf, Error> {
    if source.starts_with("$APPDIR") {
        return Err(Error::InvalidArgument {
            message: format!(
                "cask '{}' uses APPDIR artifacts which are not supported yet",
                cask.token
            ),
        });
    }

    let mut normalized = source.to_string();
    let caskroom_prefix = format!("$HOMEBREW_PREFIX/Caskroom/{}/{}/", cask.token, cask.version);
    if let Some(stripped) = normalized.strip_prefix(&caskroom_prefix) {
        normalized = stripped.to_string();
    }

    let source_path = Path::new(&normalized);
    if source_path.is_absolute() {
        return Err(Error::InvalidArgument {
            message: format!(
                "cask '{}' binary source '{}' must be a relative path",
                cask.token, source
            ),
        });
    }

    for component in source_path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(Error::InvalidArgument {
                message: format!(
                    "cask '{}' binary source '{}' cannot contain '..'",
                    cask.token, source
                ),
            });
        }
    }

    Ok(extracted_root.join(source_path))
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;

    use tempfile::TempDir;

    #[test]
    fn resolve_cask_uses_platform_variation_url_and_sha() {
        let cask = serde_json::json!({
            "token": "test",
            "version": "1.0.0",
            "url": "https://example.com/darwin.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [{ "binary": [["op"]] }],
            "variations": {
                "x86_64_linux": {
                    "url": "https://example.com/linux.zip",
                    "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
                }
            }
        });

        let _resolved = resolve_cask("test", &cask).unwrap();
        #[cfg(target_os = "linux")]
        {
            assert_eq!(_resolved.url, "https://example.com/linux.zip");
            assert_eq!(
                _resolved.sha256,
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            );
        }
    }

    #[test]
    fn resolve_cask_parses_binary_targets() {
        let cask = serde_json::json!({
            "token": "test",
            "version": "1.0.0",
            "url": "https://example.com/test.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [{
                "binary": [
                    ["bin/tool"],
                    ["bin/tool2", {"target": "tool-two"}]
                ]
            }]
        });

        let resolved = resolve_cask("test", &cask).unwrap();
        assert_eq!(resolved.binaries.len(), 2);
        assert_eq!(resolved.binaries[0].target, "tool");
        assert_eq!(resolved.binaries[1].target, "tool-two");
    }

    #[test]
    fn resolve_cask_missing_required_field_is_invalid_argument() {
        let cask = serde_json::json!({
            "token": "test",
            "version": "1.0.0",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [{ "binary": [["op"]] }]
        });

        let err = resolve_cask("test", &cask).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }));
    }

    #[test]
    fn resolve_cask_missing_artifacts_array_is_invalid_argument() {
        let cask = serde_json::json!({
            "token": "test",
            "version": "1.0.0",
            "url": "https://example.com/test.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        });

        let err = resolve_cask("test", &cask).unwrap_err();
        assert!(matches!(err, Error::InvalidArgument { .. }));
    }

    #[test]
    fn resolve_cask_no_binary_artifacts_lists_found_types() {
        let cask = serde_json::json!({
            "token": "ghostty",
            "version": "1.0.0",
            "url": "https://example.com/Ghostty.dmg",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [
                { "app": ["Ghostty.app"] },
                { "zap": [{ "trash": ["~/.config/ghostty/"] }] }
            ]
        });

        let err = resolve_cask("ghostty", &cask).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no binary artifacts"), "got: {msg}");
        assert!(msg.contains("app"), "got: {msg}");
        assert!(msg.contains("zap"), "got: {msg}");
    }

    #[test]
    fn stage_raw_cask_binary_copies_and_marks_executable() {
        let tmp = TempDir::new().unwrap();
        let blob_path = tmp.path().join("claude");
        fs::write(&blob_path, b"#!/bin/sh\necho hello").unwrap();

        let keg_path = tmp.path().join("keg");
        let cask = ResolvedCask {
            install_name: "cask:claude-code".to_string(),
            token: "claude-code".to_string(),
            version: "1.0.0".to_string(),
            url: "https://example.com/claude".to_string(),
            sha256: "aaa".to_string(),
            binaries: vec![CaskBinary {
                source: "claude".to_string(),
                target: "claude".to_string(),
            }],
        };

        stage_raw_cask_binary(&blob_path, &keg_path, &cask).unwrap();

        let target = keg_path.join("bin/claude");
        assert!(target.exists());
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "#!/bin/sh\necho hello"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).unwrap().permissions().mode();
            assert_eq!(mode & 0o755, 0o755);
        }
    }

    #[test]
    fn stage_raw_cask_binary_rejects_multiple_binaries() {
        let tmp = TempDir::new().unwrap();
        let blob_path = tmp.path().join("blob");
        fs::write(&blob_path, b"data").unwrap();

        let keg_path = tmp.path().join("keg");
        let cask = ResolvedCask {
            install_name: "cask:multi".to_string(),
            token: "multi".to_string(),
            version: "1.0.0".to_string(),
            url: "https://example.com/multi".to_string(),
            sha256: "bbb".to_string(),
            binaries: vec![
                CaskBinary {
                    source: "a".to_string(),
                    target: "a".to_string(),
                },
                CaskBinary {
                    source: "b".to_string(),
                    target: "b".to_string(),
                },
            ],
        };

        let err = stage_raw_cask_binary(&blob_path, &keg_path, &cask).unwrap_err();
        assert!(err.to_string().contains("raw binary"));
    }
}
