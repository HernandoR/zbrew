//! Homebrew casks: reading a cask's JSON, staging the artifacts it declares
//! into a keg, and putting its `.app` bundles where the desktop looks for
//! them.
//!
//! Two artifact types are supported. A `binary` lands in `<keg>/bin` and is
//! linked into the prefix like any other executable. An `app` is staged in
//! `<keg>/Applications` and then **moved** into the app directory, with a
//! symlink left behind in the keg pointing at where it went — a `.app` has to
//! be a real directory where macOS looks for it, because Launch Services and
//! Gatekeeper do not follow a symlinked bundle reliably. That symlink is the
//! only record of the installed copy, and it is what uninstall follows to
//! remove it.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tracing::warn;
use zb_core::{ConflictedLink, Error, validate_destructive_path};
use zb_net::DownloadRequest;
use zb_store::InstallReason;

use super::Installer;
use super::bottle::FailedInstallGuard;

/// Where inside a keg a cask's `.app` bundles are staged. Deliberately not one
/// of the linker's `LINK_DIRS`: nothing under it belongs in the prefix.
const CASK_APPS_DIR: &str = "Applications";

/// One `source` → `target` mapping out of a cask's `artifacts` array.
///
/// `source` is a path inside the downloaded archive; `target` is the name the
/// artifact takes once staged — `bin/<target>` for a binary, `Applications/
/// <target>` for an app. Both artifact types have the same shape in the JSON
/// and the same rules for what a target may be, so they share one type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CaskArtifact {
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
    pub binaries: Vec<CaskArtifact>,
    pub apps: Vec<CaskArtifact>,
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

    let binaries = parse_artifacts(cask, "binary")?;
    let apps = parse_artifacts(cask, "app")?;
    if binaries.is_empty() && apps.is_empty() {
        let found = artifact_types(cask);
        return Err(Error::InvalidArgument {
            message: format!(
                "cask '{token}' has no supported artifacts (found: {found}); \
                 only casks with 'binary' or 'app' artifacts can be installed"
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
        apps,
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

/// Every declaration of artifact type `kind` (`"binary"` or `"app"`) in the
/// cask's `artifacts` array.
///
/// One entry of that array is one declaration, not one artifact per element:
/// the array under the type key is a Ruby argument list, so element 0 is the
/// source and element 1, when present, is the keyword hash. A cask that
/// installs several binaries repeats the whole `{"binary": …}` object.
fn parse_artifacts(cask: &Value, kind: &str) -> Result<Vec<CaskArtifact>, Error> {
    let artifacts = cask
        .get("artifacts")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::InvalidArgument {
            message: "failed to parse cask JSON: missing artifacts array".to_string(),
        })?;

    let mut parsed = Vec::new();
    for artifact in artifacts {
        let Some(args) = artifact.get(kind).and_then(Value::as_array) else {
            continue;
        };
        parsed.push(parse_artifact_declaration(artifact, args, kind)?);
    }

    Ok(parsed)
}

/// One declaration: `{"<kind>": [<source>, {"target": …}], "target": …}`.
///
/// The target can arrive two ways and the API uses both at once, so both are
/// read: the keyword hash in the argument list first, then the declaration's
/// own `target` key.
fn parse_artifact_declaration(
    artifact: &Value,
    args: &[Value],
    kind: &str,
) -> Result<CaskArtifact, Error> {
    let source = args
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| Error::InvalidArgument {
            message: format!("unsupported cask {kind} source"),
        })?;

    let target = args
        .get(1)
        .and_then(Value::as_object)
        .and_then(|keywords| keywords.get("target"))
        .and_then(Value::as_str)
        .or_else(|| artifact.get("target").and_then(Value::as_str))
        .unwrap_or(source);

    Ok(CaskArtifact {
        source: source.to_string(),
        target: artifact_file_name(target, kind)?,
    })
}

/// The name an artifact takes in the directory zbrew stages it into.
///
/// A cask target is the full path Homebrew would install to, written against
/// Homebrew's own layout: `/Applications/iTerm.app`, `/usr/local/bin/docker`,
/// `$HOMEBREW_PREFIX/bin/code`. zbrew has its own prefix and its own app
/// directory, so the target is read for the *name* it asks for and placed
/// under the directory zbrew is staging into — the same position Homebrew
/// would give it, in zbrew's tree rather than Homebrew's.
///
/// Keeping only the last component is also what makes a target unable to
/// escape that directory, so no separate traversal check is needed.
fn artifact_file_name(target: &str, kind: &str) -> Result<String, Error> {
    let name = Path::new(target)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| Error::InvalidArgument {
            message: format!("cask {kind} target '{target}' names no file"),
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

        let app_dir = self.app_dir.clone();

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

        // The previous install of this cask goes first, the way `upgrade`
        // retires an old bottle: after the download, so a failed download
        // leaves it intact, and before the failure guard is armed, so the
        // guard only ever tears down the keg this install created. Its keg,
        // its prefix links, its bundles and its database row all go together.
        // Retiring only the bundles, as this used to, left the old keg in the
        // cellar on a version bump -- holding a symlink into the bundle the
        // new keg now owns -- and let a failed same-version reinstall wipe a
        // keg the database still listed as installed.
        if let Some(previous) = self.db.get_installed(&cask.install_name) {
            self.uninstall_by_version(&cask.install_name, &previous.version)?;
        }

        let keg_path = self.cellar.keg_path(&cask.install_name, &cask.version);
        let mut cleanup = FailedInstallGuard::new(
            &self.linker,
            &self.cellar,
            &cask.install_name,
            &cask.version,
            &keg_path,
            &app_dir,
            link,
        );

        let staging = CaskStaging::new(&cask, &keg_path);
        if zb_extract::is_archive(&blob_path)? {
            let extracted = self.store.ensure_entry(&cask.sha256, &blob_path)?;
            staging.stage_archive(&extracted)?;
        } else {
            staging.stage_raw_binary(&blob_path)?;
        }

        let linked_files = if link {
            let linked = self.linker.link_keg(&keg_path)?;
            staging.link_apps(&app_dir)?;
            linked
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

/// Puts one cask's artifacts in place: first into the keg, then -- for apps --
/// into the app directory.
///
/// The cask and the keg it is being staged into are the state every step
/// needs, so they are held once here instead of being threaded through a row
/// of free functions.
struct CaskStaging<'a> {
    cask: &'a ResolvedCask,
    keg_path: &'a Path,
}

impl<'a> CaskStaging<'a> {
    fn new(cask: &'a ResolvedCask, keg_path: &'a Path) -> Self {
        Self { cask, keg_path }
    }

    fn apps_dir(&self) -> PathBuf {
        self.keg_path.join(CASK_APPS_DIR)
    }

    /// Stage every artifact out of an unpacked archive.
    ///
    /// Apps go first: a binary artifact may name its source with `$APPDIR`,
    /// which points inside an app this same cask installs, so the app has to
    /// be staged before that path resolves to anything.
    fn stage_archive(&self, extracted_root: &Path) -> Result<(), Error> {
        self.stage_apps(extracted_root)?;
        self.stage_binaries(extracted_root)?;
        Ok(())
    }

    fn stage_apps(&self, extracted_root: &Path) -> Result<(), Error> {
        if self.cask.apps.is_empty() {
            return Ok(());
        }

        let apps_dir = self.apps_dir();
        fs::create_dir_all(&apps_dir).map_err(Error::store("failed to create cask app dir"))?;

        for app in &self.cask.apps {
            let source = self.resolve_relative(extracted_root, &app.source, "app")?;
            if !source.exists() {
                return Err(Error::InvalidArgument {
                    message: format!(
                        "cask '{}' app source '{}' not found in archive",
                        self.cask.token, app.source
                    ),
                });
            }

            let target = apps_dir.join(&app.target);
            if target.symlink_metadata().is_ok() {
                remove_path_any(&target)
                    .map_err(Error::store("failed to replace existing staged cask app"))?;
            }
            copy_path_recursive(&source, &target)?;
        }

        Ok(())
    }

    fn stage_binaries(&self, extracted_root: &Path) -> Result<(), Error> {
        if self.cask.binaries.is_empty() {
            return Ok(());
        }

        let bin_dir = self.keg_path.join("bin");
        fs::create_dir_all(&bin_dir).map_err(Error::store("failed to create cask bin dir"))?;

        for binary in &self.cask.binaries {
            let source = self.resolve_binary_source(extracted_root, &binary.source)?;
            if !source.exists() {
                return Err(Error::InvalidArgument {
                    message: format!(
                        "cask '{}' binary source '{}' not found",
                        self.cask.token, binary.source
                    ),
                });
            }

            let target = bin_dir.join(&binary.target);
            if target.symlink_metadata().is_ok() {
                fs::remove_file(&target)
                    .map_err(Error::store("failed to replace existing cask binary"))?;
            }

            fs::copy(&source, &target).map_err(|e| Error::StoreCorruption {
                message: format!("failed to stage cask binary '{}': {e}", binary.target),
            })?;

            make_executable(&target)?;
        }

        Ok(())
    }

    /// Stage a download that is a bare executable rather than an archive.
    fn stage_raw_binary(&self, blob_path: &Path) -> Result<(), Error> {
        if !self.cask.apps.is_empty() {
            return Err(Error::InvalidArgument {
                message: format!(
                    "cask '{}' ships its app bundle in a container zbrew cannot unpack: \
                     the download is neither a tar nor a zip archive. Casks commonly use \
                     a .dmg disk image, which is not supported yet",
                    self.cask.token
                ),
            });
        }

        if self.cask.binaries.len() != 1 {
            return Err(Error::InvalidArgument {
                message: format!(
                    "cask '{}' has {} binary artifacts but the download is a raw binary; expected exactly 1",
                    self.cask.token,
                    self.cask.binaries.len()
                ),
            });
        }

        let binary = &self.cask.binaries[0];
        let bin_dir = self.keg_path.join("bin");
        fs::create_dir_all(&bin_dir).map_err(Error::store("failed to create cask bin dir"))?;

        let target = bin_dir.join(&binary.target);
        if target.symlink_metadata().is_ok() {
            fs::remove_file(&target)
                .map_err(Error::store("failed to replace existing cask binary"))?;
        }

        fs::copy(blob_path, &target).map_err(|e| Error::StoreCorruption {
            message: format!("failed to stage cask binary '{}': {e}", binary.target),
        })?;

        make_executable(&target)?;
        Ok(())
    }

    /// Move every staged bundle into `app_dir`.
    ///
    /// All-or-none: a bundle that cannot be installed rolls back the ones
    /// already moved, so a half-installed cask never reaches the app
    /// directory. Nothing is recorded in `keg_files` — the symlink each move
    /// leaves in the keg is the record, and it is where uninstall looks.
    fn link_apps(&self, app_dir: &Path) -> Result<(), Error> {
        if self.cask.apps.is_empty() {
            return Ok(());
        }

        fs::create_dir_all(app_dir).map_err(Error::store("failed to create the app directory"))?;

        let apps_dir = self.apps_dir();
        let mut installed: Vec<PathBuf> = Vec::new();

        let result = (|| -> Result<(), Error> {
            for app in &self.cask.apps {
                let staged = apps_dir.join(&app.target);
                if !staged.exists() {
                    return Err(Error::InvalidArgument {
                        message: format!(
                            "cask '{}' app '{}' was not staged",
                            self.cask.token, app.target
                        ),
                    });
                }

                let target = app_dir.join(&app.target);
                if target.symlink_metadata().is_ok() {
                    return Err(Error::LinkConflict {
                        conflicts: vec![ConflictedLink {
                            path: target,
                            owned_by: None,
                        }],
                    });
                }

                move_app_into_place(&staged, &target)?;
                installed.push(target);
            }

            Ok(())
        })();

        if result.is_err() {
            for target in &installed {
                let _ = remove_path_any(target);
            }
        }

        result
    }

    /// Where a binary artifact's source lives once the archive is unpacked.
    ///
    /// `$APPDIR` is Homebrew's name for the app directory, and a cask that
    /// ships a command-line tool inside its own bundle points at it that way.
    /// zbrew resolves it against the keg's staging directory rather than the
    /// real app directory, because the binary is staged before the bundle is
    /// moved out.
    fn resolve_binary_source(&self, extracted_root: &Path, source: &str) -> Result<PathBuf, Error> {
        if let Some(rest) = source.strip_prefix("$APPDIR") {
            let relative = rest.trim_start_matches('/');
            return self.resolve_relative(&self.apps_dir(), relative, "binary");
        }

        self.resolve_relative(extracted_root, source, "binary")
    }

    /// Join a cask-declared source path onto `root`, refusing anything that
    /// could resolve outside it.
    fn resolve_relative(&self, root: &Path, source: &str, kind: &str) -> Result<PathBuf, Error> {
        let caskroom_prefix = format!(
            "$HOMEBREW_PREFIX/Caskroom/{}/{}/",
            self.cask.token, self.cask.version
        );
        let normalized = source.strip_prefix(&caskroom_prefix).unwrap_or(source);

        let source_path = Path::new(normalized);
        if source_path.is_absolute() {
            return Err(Error::InvalidArgument {
                message: format!(
                    "cask '{}' {kind} source '{source}' must be a relative path",
                    self.cask.token
                ),
            });
        }

        for component in source_path.components() {
            if matches!(component, std::path::Component::ParentDir) {
                return Err(Error::InvalidArgument {
                    message: format!(
                        "cask '{}' {kind} source '{source}' cannot contain '..'",
                        self.cask.token
                    ),
                });
            }
        }

        Ok(root.join(source_path))
    }
}

/// Every bundle under `app_dir` that the keg at `keg_path` installed, read
/// from the symlinks [`move_app_into_place`] left behind.
///
/// A symlink whose target is not inside `app_dir` is skipped with a warning
/// rather than returned. The keg is just a directory on disk: a symlink under
/// `<keg>/Applications` can also arrive from the archive itself, because
/// extraction validates entry *names* but not symlink *targets*, and the
/// cellar recreates them verbatim. Returning such a target would hand an
/// arbitrary path — `~/Documents`, say — to a recursive delete, which is not
/// something an uninstall is entitled to do. A relative link lands here too,
/// and is skipped for the same reason instead of failing the uninstall.
fn installed_app_paths(keg_path: &Path, app_dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let apps_dir = keg_path.join(CASK_APPS_DIR);
    if !apps_dir.exists() {
        return Ok(Vec::new());
    }

    let mut installed = Vec::new();
    for entry in fs::read_dir(&apps_dir).map_err(Error::store("failed to read cask app dir"))? {
        let entry = entry.map_err(Error::store("failed to read cask app entry"))?;
        let staged = entry.path();
        // A staged bundle that is still a real directory was never moved, so
        // there is nothing in the app directory to account for.
        if !staged.is_symlink() {
            continue;
        }

        let target = fs::read_link(&staged).map_err(Error::store("failed to read app symlink"))?;
        if !is_installed_app_of(&target, app_dir) {
            warn!(
                link = %staged.display(),
                target = %target.display(),
                app_dir = %app_dir.display(),
                "ignoring a keg app link that does not point into the app directory"
            );
            continue;
        }

        installed.push(target);
    }

    Ok(installed)
}

/// Whether `target` is a bundle zbrew installed: an entry of `app_dir`
/// itself, named absolutely.
///
/// Deliberately strict. Only a direct child counts, and only an absolute path
/// — the links [`move_app_into_place`] writes are always both, so anything
/// else did not come from an install and is not ours to delete.
fn is_installed_app_of(target: &Path, app_dir: &Path) -> bool {
    target.is_absolute() && target.parent() == Some(app_dir)
}

/// Remove every bundle the keg at `keg_path` installed into `app_dir`.
///
/// A keg with no `Applications` directory — every formula, and every cask that
/// ships only binaries — is a no-op.
pub(super) fn remove_installed_apps(keg_path: &Path, app_dir: &Path) -> Result<(), Error> {
    for app in installed_app_paths(keg_path, app_dir)? {
        remove_installed_app(&app)?;
    }
    Ok(())
}

/// Delete one installed bundle, if it is still there.
///
/// The caller has already established that the path is a direct child of the
/// app directory. `validate_destructive_path` is the backstop for the case
/// that leaves open — an app directory pointed somewhere absurd through
/// `ZBREW_APPDIR`, which would make `/Applications` or `$HOME` a legitimate
/// child of it.
fn remove_installed_app(app: &Path) -> Result<(), Error> {
    if app.symlink_metadata().is_err() {
        return Ok(());
    }

    validate_destructive_path(app)?;
    remove_path_any(app).map_err(Error::store("failed to remove installed app"))
}

/// Move a staged bundle into the app directory, leaving a symlink in the keg
/// that points at where it went.
fn move_app_into_place(staged: &Path, target: &Path) -> Result<(), Error> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(Error::store("failed to create the app directory"))?;
    }

    // A rename across filesystems fails with EXDEV, and the cellar and the app
    // directory are routinely on different ones. A copy that dies part-way
    // (out of space, an unreadable file inside the bundle) has already
    // written into the app directory, at a path that did not exist on entry
    // and that nothing else can account for: the caller's rollback only knows
    // about bundles that were fully moved, and the keg entry is still a real
    // directory so uninstall would not find it either. So the copy cleans up
    // after itself. The staged bundle is untouched at that point, so nothing
    // is lost.
    if fs::rename(staged, target).is_err() {
        let copied = copy_path_recursive(staged, target).and_then(|()| {
            remove_path_any(staged).map_err(Error::store(
                "failed to remove the staged app after copying it",
            ))
        });
        if let Err(e) = copied {
            let _ = remove_path_any(target);
            return Err(e);
        }
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(target, staged).map_err(Error::store(
        "failed to record the installed app in the keg",
    ))?;

    Ok(())
}

/// Copy a file, a symlink or a whole directory tree. Symlinks are recreated as
/// symlinks rather than followed, so a bundle's internal `Versions/Current`
/// links survive the copy.
fn copy_path_recursive(src: &Path, dst: &Path) -> Result<(), Error> {
    let metadata =
        fs::symlink_metadata(src).map_err(Error::store("failed to read source metadata"))?;

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(src).map_err(Error::store("failed to read source symlink"))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, dst).map_err(Error::store("failed to copy symlink"))?;
        #[cfg(not(unix))]
        fs::copy(src, dst).map_err(Error::store("failed to copy symlink as file"))?;
        return Ok(());
    }

    if metadata.is_dir() {
        fs::create_dir_all(dst).map_err(Error::store("failed to create target directory"))?;
        for entry in fs::read_dir(src).map_err(Error::store("failed to read source directory"))? {
            let entry = entry.map_err(Error::store("failed to read source directory entry"))?;
            copy_path_recursive(&entry.path(), &dst.join(entry.file_name()))?;
        }
        return Ok(());
    }

    fs::copy(src, dst).map_err(Error::store("failed to copy file"))?;
    Ok(())
}

/// Remove a path whatever it is: a symlink and a file are unlinked, a
/// directory is removed with its contents. A symlink is never followed.
fn remove_path_any(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)
    } else {
        fs::remove_dir_all(path)
    }
}

fn make_executable(path: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(Error::store("failed to read staged cask binary metadata"))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)
            .map_err(Error::store("failed to make staged cask binary executable"))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn cask_with(binaries: Vec<CaskArtifact>, apps: Vec<CaskArtifact>) -> ResolvedCask {
        ResolvedCask {
            install_name: "cask:test".to_string(),
            token: "test".to_string(),
            version: "1.0.0".to_string(),
            url: "https://example.com/test.zip".to_string(),
            sha256: "aaa".to_string(),
            binaries,
            apps,
        }
    }

    fn artifact(source: &str, target: &str) -> CaskArtifact {
        CaskArtifact {
            source: source.to_string(),
            target: target.to_string(),
        }
    }

    /// A bundle deep enough to tell a real copy from an empty directory.
    fn write_app_bundle(root: &Path, name: &str) {
        let contents = root.join(name).join("Contents");
        fs::create_dir_all(contents.join("MacOS")).unwrap();
        fs::write(contents.join("Info.plist"), b"plist").unwrap();
        fs::write(contents.join("MacOS/test-cli"), b"#!/bin/sh\necho from-app").unwrap();
    }

    #[test]
    fn resolve_cask_uses_platform_variation_url_and_sha() {
        let cask = serde_json::json!({
            "token": "test",
            "version": "1.0.0",
            "url": "https://example.com/darwin.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [{ "binary": ["op"] }],
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
    fn resolve_cask_parses_a_binary_only_cask() {
        let cask = serde_json::json!({
            "token": "test",
            "version": "1.0.0",
            "url": "https://example.com/test.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [{ "binary": ["bin/tool"] }]
        });

        let resolved = resolve_cask("test", &cask).unwrap();
        assert_eq!(resolved.binaries.len(), 1);
        assert_eq!(resolved.binaries[0].source, "bin/tool");
        assert_eq!(resolved.binaries[0].target, "tool");
        assert!(resolved.apps.is_empty());
    }

    /// The three artifact shapes `formulae.brew.sh` actually emits, captured
    /// verbatim from the API. One `artifacts` entry is one declaration: the
    /// array under the type key is a Ruby argument list, so element 1 is the
    /// keyword hash and not a second artifact.
    #[test]
    fn resolve_cask_parses_the_shapes_the_homebrew_api_emits() {
        // iterm2: an app and nothing else. visual-studio-code: an app plus
        // binaries inside it, target as a sibling key. docker-desktop: target
        // in the keyword hash *and* as a sibling key, both absolute.
        let cask = serde_json::json!({
            "token": "mixed",
            "version": "1.0.0",
            "url": "https://example.com/mixed.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [
                { "uninstall": [{ "quit": "com.example" }] },
                { "app": ["iTerm.app"], "target": "/Applications/iTerm.app" },
                {
                    "binary": ["$APPDIR/Visual Studio Code.app/Contents/Resources/app/bin/code"],
                    "target": "$HOMEBREW_PREFIX/bin/code"
                },
                {
                    "binary": [
                        "$APPDIR/Docker.app/Contents/Resources/bin/docker",
                        { "target": "/usr/local/bin/docker" }
                    ],
                    "target": "/usr/local/bin/docker"
                },
                { "zap": [{ "trash": ["~/.example"] }] }
            ]
        });

        let resolved = resolve_cask("mixed", &cask).unwrap();

        assert_eq!(resolved.apps.len(), 1);
        assert_eq!(resolved.apps[0].source, "iTerm.app");
        assert_eq!(resolved.apps[0].target, "iTerm.app");

        let binaries: Vec<(&str, &str)> = resolved
            .binaries
            .iter()
            .map(|b| (b.source.as_str(), b.target.as_str()))
            .collect();
        assert_eq!(
            binaries,
            [
                (
                    "$APPDIR/Visual Studio Code.app/Contents/Resources/app/bin/code",
                    "code"
                ),
                ("$APPDIR/Docker.app/Contents/Resources/bin/docker", "docker"),
            ]
        );
    }

    /// A cask installing several binaries repeats the whole `{"binary": …}`
    /// object rather than listing them in one array.
    #[test]
    fn resolve_cask_collects_one_artifact_per_declaration() {
        let cask = serde_json::json!({
            "token": "many",
            "version": "1.0.0",
            "url": "https://example.com/many.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [
                { "binary": ["bin/one"], "target": "$HOMEBREW_PREFIX/bin/one" },
                { "binary": ["bin/two"], "target": "$HOMEBREW_PREFIX/bin/renamed" }
            ]
        });

        let resolved = resolve_cask("many", &cask).unwrap();
        assert_eq!(resolved.binaries.len(), 2);
        assert_eq!(resolved.binaries[0].target, "one");
        assert_eq!(resolved.binaries[1].target, "renamed");
    }

    /// A cask target is the full path Homebrew would install to. zbrew keeps
    /// the name it asks for and puts it in zbrew's own directory, which is
    /// also what stops a target escaping that directory.
    #[test]
    fn a_cask_target_contributes_its_name_and_never_a_path() {
        for (target, expected) in [
            ("/Applications/Test.app", "Test.app"),
            ("$HOMEBREW_PREFIX/bin/code", "code"),
            ("~/Applications/Test.app", "Test.app"),
            ("../../../etc/passwd", "passwd"),
        ] {
            assert_eq!(artifact_file_name(target, "app").unwrap(), expected);
        }

        for bad in ["", "/", "..", "."] {
            assert!(
                artifact_file_name(bad, "app").is_err(),
                "target '{bad}' names no file and must be refused"
            );
        }
    }

    #[test]
    fn resolve_cask_missing_required_field_is_invalid_argument() {
        let cask = serde_json::json!({
            "token": "test",
            "version": "1.0.0",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [{ "binary": ["op"] }]
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

    /// The case from issue #29: a cask whose only artifact is an app used to
    /// be rejected outright.
    #[test]
    fn resolve_cask_accepts_a_cask_whose_only_artifact_is_an_app() {
        let cask = serde_json::json!({
            "token": "ghostty",
            "version": "1.0.0",
            "url": "https://example.com/Ghostty.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [
                { "app": ["Ghostty.app"] },
                { "zap": [{ "trash": ["~/.config/ghostty/"] }] }
            ]
        });

        let resolved = resolve_cask("ghostty", &cask).unwrap();
        assert_eq!(resolved.apps.len(), 1);
        assert_eq!(resolved.apps[0].target, "Ghostty.app");
        assert!(resolved.binaries.is_empty());
    }

    /// A cask made only of artifact types zbrew cannot install still says so,
    /// and names what it found.
    #[test]
    fn resolve_cask_with_no_installable_artifact_lists_the_types_it_found() {
        let cask = serde_json::json!({
            "token": "somepkg",
            "version": "1.0.0",
            "url": "https://example.com/somepkg.zip",
            "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "artifacts": [
                { "pkg": ["Some.pkg"] },
                { "uninstall": [{ "pkgutil": ["com.example.some"] }] }
            ]
        });

        let err = resolve_cask("somepkg", &cask).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no supported artifacts"), "got: {msg}");
        assert!(msg.contains("pkg"), "got: {msg}");
        assert!(msg.contains("uninstall"), "got: {msg}");
    }

    #[test]
    fn stage_raw_binary_copies_and_marks_executable() {
        let tmp = TempDir::new().unwrap();
        let blob_path = tmp.path().join("claude");
        fs::write(&blob_path, b"#!/bin/sh\necho hello").unwrap();

        let keg_path = tmp.path().join("keg");
        let cask = cask_with(vec![artifact("claude", "claude")], vec![]);

        CaskStaging::new(&cask, &keg_path)
            .stage_raw_binary(&blob_path)
            .unwrap();

        let target = keg_path.join("bin/claude");
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
    fn stage_raw_binary_rejects_multiple_binaries() {
        let tmp = TempDir::new().unwrap();
        let blob_path = tmp.path().join("blob");
        fs::write(&blob_path, b"data").unwrap();

        let keg_path = tmp.path().join("keg");
        let cask = cask_with(vec![artifact("a", "a"), artifact("b", "b")], vec![]);

        let err = CaskStaging::new(&cask, &keg_path)
            .stage_raw_binary(&blob_path)
            .unwrap_err();
        assert!(err.to_string().contains("raw binary"));
    }

    /// An app cask whose download is not an archive is almost always a `.dmg`.
    /// Saying so beats reporting it as a malformed binary artifact.
    #[test]
    fn stage_raw_binary_names_the_disk_image_case_for_an_app_cask() {
        let tmp = TempDir::new().unwrap();
        let blob_path = tmp.path().join("Ghostty.dmg");
        fs::write(&blob_path, b"not an archive").unwrap();

        let keg_path = tmp.path().join("keg");
        let cask = cask_with(vec![], vec![artifact("Ghostty.app", "Ghostty.app")]);

        let err = CaskStaging::new(&cask, &keg_path)
            .stage_raw_binary(&blob_path)
            .unwrap_err();
        assert!(err.to_string().contains(".dmg"), "got: {err}");
    }

    /// Apps are staged before binaries so that a binary named with `$APPDIR`
    /// -- a command-line tool shipped inside the cask's own bundle -- resolves.
    #[test]
    fn stage_archive_copies_the_app_and_resolves_an_appdir_binary() {
        let tmp = TempDir::new().unwrap();
        let extracted_root = tmp.path().join("extract");
        write_app_bundle(&extracted_root, "Test.app");

        let keg_path = tmp.path().join("keg");
        let cask = cask_with(
            vec![artifact(
                "$APPDIR/Test.app/Contents/MacOS/test-cli",
                "test-cli",
            )],
            vec![artifact("Test.app", "Test.app")],
        );

        CaskStaging::new(&cask, &keg_path)
            .stage_archive(&extracted_root)
            .unwrap();

        assert!(
            keg_path
                .join("Applications/Test.app/Contents/Info.plist")
                .exists()
        );
        assert_eq!(
            fs::read_to_string(keg_path.join("bin/test-cli")).unwrap(),
            "#!/bin/sh\necho from-app"
        );
    }

    #[test]
    fn stage_archive_reports_an_app_source_the_archive_does_not_have() {
        let tmp = TempDir::new().unwrap();
        let extracted_root = tmp.path().join("extract");
        fs::create_dir_all(&extracted_root).unwrap();

        let keg_path = tmp.path().join("keg");
        let cask = cask_with(vec![], vec![artifact("Missing.app", "Missing.app")]);

        let err = CaskStaging::new(&cask, &keg_path)
            .stage_archive(&extracted_root)
            .unwrap_err();
        assert!(
            err.to_string().contains("not found in archive"),
            "got: {err}"
        );
    }

    /// The bundle itself moves into the app directory and the keg keeps a
    /// symlink to it: macOS will not launch a `.app` reliably through a
    /// symlink, and the symlink is what uninstall follows.
    #[test]
    fn link_apps_moves_the_bundle_and_leaves_a_symlink_behind() {
        let tmp = TempDir::new().unwrap();
        let keg_path = tmp.path().join("keg");
        write_app_bundle(&keg_path.join(CASK_APPS_DIR), "Test.app");

        let app_dir = tmp.path().join("Applications");
        let cask = cask_with(vec![], vec![artifact("Test.app", "Test.app")]);

        CaskStaging::new(&cask, &keg_path)
            .link_apps(&app_dir)
            .unwrap();

        let installed = app_dir.join("Test.app");
        assert!(installed.join("Contents/Info.plist").exists());
        assert!(!installed.is_symlink(), "the installed bundle must be real");

        let staged = keg_path.join(CASK_APPS_DIR).join("Test.app");
        assert!(staged.is_symlink());
        assert_eq!(fs::read_link(&staged).unwrap(), installed);
    }

    #[test]
    fn link_apps_refuses_to_overwrite_something_already_in_the_app_directory() {
        let tmp = TempDir::new().unwrap();
        let keg_path = tmp.path().join("keg");
        write_app_bundle(&keg_path.join(CASK_APPS_DIR), "Test.app");

        let app_dir = tmp.path().join("Applications");
        fs::create_dir_all(app_dir.join("Test.app")).unwrap();
        fs::write(app_dir.join("Test.app/someone-elses"), b"keep me").unwrap();

        let cask = cask_with(vec![], vec![artifact("Test.app", "Test.app")]);
        let err = CaskStaging::new(&cask, &keg_path)
            .link_apps(&app_dir)
            .unwrap_err();

        assert!(matches!(err, Error::LinkConflict { .. }), "got {err:?}");
        assert!(
            app_dir.join("Test.app/someone-elses").exists(),
            "the existing bundle must be untouched"
        );
    }

    /// All-or-none: the bundle that was already moved is put back when a later
    /// one conflicts, so a half-installed cask never reaches the app directory.
    #[test]
    fn link_apps_rolls_back_the_bundles_it_already_moved() {
        let tmp = TempDir::new().unwrap();
        let keg_path = tmp.path().join("keg");
        let staged_dir = keg_path.join(CASK_APPS_DIR);
        write_app_bundle(&staged_dir, "First.app");
        write_app_bundle(&staged_dir, "Second.app");

        let app_dir = tmp.path().join("Applications");
        fs::create_dir_all(app_dir.join("Second.app")).unwrap();

        let cask = cask_with(
            vec![],
            vec![
                artifact("First.app", "First.app"),
                artifact("Second.app", "Second.app"),
            ],
        );

        let err = CaskStaging::new(&cask, &keg_path)
            .link_apps(&app_dir)
            .unwrap_err();

        assert!(matches!(err, Error::LinkConflict { .. }), "got {err:?}");
        assert!(
            !app_dir.join("First.app").exists(),
            "the bundle moved before the conflict must be rolled back"
        );
    }

    #[test]
    fn remove_installed_apps_deletes_what_the_keg_installed() {
        let tmp = TempDir::new().unwrap();
        let keg_path = tmp.path().join("keg");
        write_app_bundle(&keg_path.join(CASK_APPS_DIR), "Test.app");

        let app_dir = tmp.path().join("Applications");
        let cask = cask_with(vec![], vec![artifact("Test.app", "Test.app")]);
        CaskStaging::new(&cask, &keg_path)
            .link_apps(&app_dir)
            .unwrap();
        assert!(app_dir.join("Test.app").exists());

        remove_installed_apps(&keg_path, &app_dir).unwrap();

        assert!(!app_dir.join("Test.app").exists());
        assert!(
            app_dir.exists(),
            "the app directory itself belongs to the user"
        );
    }

    /// A keg with no staged bundles -- every formula, and every cask that
    /// ships only binaries -- has nothing to remove.
    #[test]
    fn remove_installed_apps_is_a_noop_for_a_keg_without_apps() {
        let tmp = TempDir::new().unwrap();
        let keg_path = tmp.path().join("keg");
        fs::create_dir_all(keg_path.join("bin")).unwrap();

        remove_installed_apps(&keg_path, &tmp.path().join("Applications")).unwrap();
        assert!(keg_path.join("bin").exists());
    }

    /// An uninstall must not follow a link out of the app directory.
    ///
    /// A keg is a directory on disk and an archive can put a symlink in it:
    /// extraction validates entry *names*, not symlink *targets*, and the
    /// cellar recreates them verbatim. So `Applications/x -> ~/Documents` is
    /// reachable without anything zbrew installed pointing there, and it must
    /// never reach a recursive delete.
    #[test]
    fn remove_installed_apps_ignores_a_link_that_points_outside_the_app_dir() {
        let tmp = TempDir::new().unwrap();
        let keg_path = tmp.path().join("keg");
        let staged_dir = keg_path.join(CASK_APPS_DIR);
        fs::create_dir_all(&staged_dir).unwrap();

        let app_dir = tmp.path().join("Applications");
        fs::create_dir_all(&app_dir).unwrap();

        let outsider = tmp.path().join("Documents");
        fs::create_dir_all(&outsider).unwrap();
        fs::write(outsider.join("thesis.txt"), b"years of work").unwrap();

        #[cfg(unix)]
        {
            // Straight out of the app directory...
            std::os::unix::fs::symlink(&outsider, staged_dir.join("Escape.app")).unwrap();
            // ...and the same thing written relatively.
            std::os::unix::fs::symlink("../../Documents", staged_dir.join("Relative.app")).unwrap();
            // A link one level *below* the app directory is not ours either.
            std::os::unix::fs::symlink(
                app_dir.join("nested/Deep.app"),
                staged_dir.join("Deep.app"),
            )
            .unwrap();
        }

        remove_installed_apps(&keg_path, &app_dir).unwrap();

        assert!(
            outsider.join("thesis.txt").exists(),
            "uninstall must not delete a path outside the app directory"
        );
    }

    /// A bundle the user has already deleted is not an error, and a staged
    /// entry that was never moved is not something to delete.
    #[test]
    fn remove_installed_apps_tolerates_a_bundle_that_is_already_gone() {
        let tmp = TempDir::new().unwrap();
        let keg_path = tmp.path().join("keg");
        let staged_dir = keg_path.join(CASK_APPS_DIR);
        write_app_bundle(&staged_dir, "NeverMoved.app");

        let app_dir = tmp.path().join("Applications");
        fs::create_dir_all(&app_dir).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(app_dir.join("Gone.app"), staged_dir.join("Gone.app")).unwrap();

        remove_installed_apps(&keg_path, &app_dir).unwrap();

        assert!(
            staged_dir.join("NeverMoved.app").is_dir(),
            "a bundle that was never moved out is the keg's own"
        );
    }
}
