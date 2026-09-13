use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use tracing::{debug, warn};
use zb_core::Error;

use super::macho::{self, Region};
use super::relocation::{
    HOMEBREW_PREFIXES, diagnose_skipped, entitlements_plist, homebrew_prefix_at,
    rewrite_homebrew_prefixes,
};

/// Patch hardcoded Homebrew paths in text files.
fn patch_text_file_strings(path: &Path, new_prefix: &str, new_cellar: &str) -> Result<(), Error> {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };

    let mut buf = [0u8; 8192];
    let n = match file.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };

    if buf[..n].contains(&0) {
        return Ok(());
    }

    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    if !content.contains("@@HOMEBREW_")
        && !content.contains("/opt/homebrew")
        && !content.contains("/usr/local")
        && !content.contains("/home/linuxbrew")
    {
        return Ok(());
    }

    let mut new_content = content.clone();
    let mut changed = false;

    new_content = new_content
        .replace("@@HOMEBREW_PREFIX@@", new_prefix)
        .replace("@@HOMEBREW_CELLAR@@", new_cellar)
        .replace("@@HOMEBREW_REPOSITORY@@", new_prefix)
        .replace("@@HOMEBREW_LIBRARY@@", &format!("{}/Library", new_prefix))
        .replace("@@HOMEBREW_PERL@@", "/usr/bin/perl")
        .replace("@@HOMEBREW_JAVA@@", "/usr/bin/java");

    if new_content != content {
        changed = true;
    }

    for old_prefix in HOMEBREW_PREFIXES {
        if old_prefix == &new_prefix {
            continue;
        }
        let replaced = new_content.replace(old_prefix, new_prefix);
        if replaced != new_content {
            new_content = replaced;
            changed = true;
        }
    }

    if !changed {
        return Ok(());
    }

    let metadata = fs::metadata(path).map_err(Error::store("failed to read metadata"))?;
    let original_mode = metadata.permissions().mode();
    let is_readonly = original_mode & 0o200 == 0;

    if is_readonly {
        let mut perms = metadata.permissions();
        perms.set_mode(original_mode | 0o200);
        fs::set_permissions(path, perms).map_err(Error::store("failed to make writable"))?;
    }

    fs::write(path, new_content).map_err(Error::store("failed to write file"))?;

    if is_readonly {
        let mut perms = metadata.permissions();
        perms.set_mode(original_mode);
        fs::set_permissions(path, perms).map_err(Error::store("failed to restore permissions"))?;
    }

    Ok(())
}

/// The entitlements a binary is signed with, as an XML plist, read *before* it
/// is modified.
///
/// `--preserve-metadata=entitlements` only carries over what is still in the
/// file when `codesign` runs, and by then there may be nothing left:
/// `install_name_tool` drops the original signature (replacing it with a bare
/// ad-hoc one) as soon as it rewrites a load command. Reading the entitlements
/// up front and handing them back explicitly is what keeps restricted
/// entitlements such as `com.apple.security.virtualization` — without which
/// lima's `limactl` cannot start a `vz` VM — alive across patching.
fn read_entitlements(path: &Path) -> Option<Vec<u8>> {
    let run = |xml: bool| {
        let mut command = Command::new("codesign");
        command.args(["-d", "--entitlements", "-"]);
        if xml {
            command.arg("--xml");
        }
        command.arg(path).output().ok()
    };

    // `--xml` is what asks for a plain plist; `codesign` versions that predate
    // it fail the whole invocation, so the wrapped form is the fallback.
    let output = run(true)
        .filter(|o| o.status.success())
        .or_else(|| run(false).filter(|o| o.status.success()))?;

    entitlements_plist(&output.stdout).map(String::into_bytes)
}

/// Ad-hoc re-sign a Mach-O file that we modified, restoring `entitlements`.
///
/// Any edit invalidates the existing signature, and macOS refuses to execute a
/// binary whose signature does not match its contents (fatally so on Apple
/// silicon), so a failure here is an error rather than a warning: the
/// alternative is shipping a keg that dies with `Killed: 9` at first use.
/// The hardened runtime, signing flags and requirements are preserved the way
/// Homebrew does it; entitlements are passed back in explicitly when the caller
/// captured them, because by this point they may already be gone from the file.
fn codesign(path: &Path, entitlements: Option<&[u8]>) -> Result<(), Error> {
    let mut args = vec!["--force".to_string(), "--sign".to_string(), "-".to_string()];

    // `--entitlements` and `--preserve-metadata=entitlements` are mutually
    // exclusive, and the plist has to outlive the command that reads it.
    let plist = match entitlements {
        Some(bytes) => {
            let mut file = tempfile::Builder::new()
                .prefix("zb-entitlements-")
                .suffix(".plist")
                .tempfile()
                .map_err(Error::store("failed to create an entitlements file"))?;
            file.write_all(bytes)
                .and_then(|()| file.flush())
                .map_err(Error::store("failed to write entitlements"))?;

            args.push("--entitlements".to_string());
            args.push(file.path().to_string_lossy().into_owned());
            args.push("--preserve-metadata=requirements,flags,runtime".to_string());
            Some(file)
        }
        None => {
            args.push("--preserve-metadata=entitlements,requirements,flags,runtime".to_string());
            None
        }
    };

    args.push(path.to_string_lossy().into_owned());

    let output = Command::new("codesign")
        .args(&args)
        .output()
        .map_err(Error::exec(&format!(
            "failed to run codesign for {}",
            path.display()
        )))?;

    drop(plist);

    if !output.status.success() {
        return Err(Error::ExecutionError {
            message: format!(
                "failed to re-sign {}: {}",
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    Ok(())
}

/// Report Homebrew paths that survived patching because they sit outside any
/// region we are allowed to write to.
///
/// These are the binaries that will misbehave at runtime, so they are named
/// rather than left to fail mysteriously later.
fn unreachable_paths(data: &[u8], regions: &[Region], new_prefix: &str) -> usize {
    let mut found = 0;
    let mut position = 0;

    while position < data.len() {
        match homebrew_prefix_at(data, position, new_prefix) {
            Some(prefix) => {
                if !macho::contains(regions, position) {
                    found += 1;
                }
                position += prefix.len();
            }
            None => position += 1,
        }
    }

    found
}

/// Rewrite hardcoded Homebrew paths in a Mach-O image, returning the new
/// contents when anything actually changed.
///
/// Only the byte ranges that the Mach-O structure declares as C string storage
/// are touched, and each one is rewritten as a whole string, so a path that
/// happens to appear in code or in a length-prefixed constant is left alone
/// instead of being silently corrupted.
fn patch_macho_bytes(path: &Path, contents: &[u8], new_prefix: &str) -> Option<Vec<u8>> {
    let regions = match macho::patchable_regions(contents) {
        Ok(regions) => regions,
        Err(macho::MachoError::NotMacho) => return None,
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "could not parse Mach-O structure; leaving the binary unpatched"
            );
            return None;
        }
    };

    let mut patched = contents.to_vec();
    let report = macho::rewrite_strings(&mut patched, &regions, |text| {
        rewrite_homebrew_prefixes(text, new_prefix)
    });

    // One warning per binary rather than one per string: a bottle that hits
    // this hits it dozens of times, and a wall of identical lines is what made
    // the old warning look like a failure even when it was harmless.
    if let Some(diagnosis) = diagnose_skipped(&report.skipped, new_prefix) {
        warn!(
            "{}",
            diagnosis.message(&path.display().to_string(), new_prefix)
        );
    }

    // Paths outside the string tables — copies inside code, length-prefixed Go
    // and Rust strings, debug info — are left alone deliberately: rewriting
    // them is not safe here regardless of how long the prefix is. That is a
    // fact about every Homebrew-built binary, not a problem with this install,
    // so it is recorded for debugging rather than shown to the user.
    let unreachable = unreachable_paths(&patched, &regions, new_prefix);
    if unreachable > 0 {
        debug!(
            path = %path.display(),
            count = unreachable,
            "left {unreachable} Homebrew path(s) that live outside the binary's string \
             storage untouched; rewriting those bytes could corrupt the image"
        );
    }

    (report.patched > 0 && patched != contents).then_some(patched)
}

/// Rewrite a Mach-O file in place, re-signing it if it changed.
fn patch_macho_binary_strings(path: &Path, new_prefix: &str) -> Result<(), Error> {
    let metadata = fs::metadata(path).map_err(Error::store("failed to read metadata"))?;
    let contents = fs::read(path).map_err(Error::store("failed to read file"))?;

    let Some(patched) = patch_macho_bytes(path, &contents, new_prefix) else {
        return Ok(());
    };

    // Read while the original signature is still intact.
    let entitlements = read_entitlements(path);

    // Write through a temporary file so a crash mid-write cannot leave a
    // half-rewritten binary behind, and keep it writable until codesign is done
    // with it: signing a read-only file fails.
    let file_name = path
        .file_name()
        .ok_or_else(|| Error::StoreCorruption {
            message: format!("cannot patch {} (no file name)", path.display()),
        })?
        .to_string_lossy()
        .into_owned();
    let temp_path = path.with_file_name(format!(".{file_name}.zb-patch"));

    fs::write(&temp_path, &patched).map_err(Error::store("failed to write patched binary"))?;

    let mode = metadata.permissions().mode();
    fs::set_permissions(&temp_path, fs::Permissions::from_mode(mode | 0o200))
        .map_err(Error::store("failed to set permissions on patched binary"))?;

    fs::rename(&temp_path, path).map_err(Error::store("failed to replace patched binary"))?;

    let signed = codesign(path, entitlements.as_deref());

    // Restore the original mode even if signing failed, so the keg is never
    // left more permissive than the bottle intended.
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(Error::store("failed to restore permissions after patching"))?;

    signed
}

/// Collects patch failures from the parallel passes, keeping the first one so
/// the caller can report something more useful than a count.
#[derive(Default)]
struct PatchFailures {
    count: AtomicUsize,
    first: Mutex<Option<Error>>,
}

impl PatchFailures {
    fn record(&self, error: Error) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut first) = self.first.lock()
            && first.is_none()
        {
            *first = Some(error);
        }
    }

    fn into_result(self, keg_path: &Path) -> Result<(), Error> {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return Ok(());
        }

        let first = self
            .first
            .into_inner()
            .ok()
            .flatten()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "unknown error".to_string());

        Err(Error::StoreCorruption {
            message: format!(
                "failed to patch {} Mach-O file(s) in {}: {}",
                count,
                keg_path.display(),
                first
            ),
        })
    }
}

/// Whether `path` is a Mach-O file, read from its magic alone so that walking a
/// keg does not mean reading every byte of it.
fn is_macho_file(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    file.read_exact(&mut magic).is_ok() && macho::is_macho(&magic)
}

/// Run `install_name_tool` with `args` against `path`.
fn install_name_tool(path: &Path, args: &[&str]) -> Result<(), Error> {
    let output = Command::new("install_name_tool")
        .args(args)
        .arg(path)
        .output()
        .map_err(Error::exec(&format!(
            "failed to run install_name_tool for {}",
            path.display()
        )))?;

    if !output.status.success() {
        return Err(Error::ExecutionError {
            message: format!(
                "install_name_tool {} failed for {}: {}",
                args.join(" "),
                path.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    Ok(())
}

/// Read one kind of path out of a Mach-O file with `otool`.
fn otool(path: &Path, flag: &str) -> Vec<String> {
    let Ok(output) = Command::new("otool").arg(flag).arg(path).output() else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let mut paths = match flag {
        // `otool -L` lists each dependency as "<path> (compatibility version ...)",
        // which also tells the dependency lines apart from the per-architecture
        // headers that must not be fed to install_name_tool.
        "-L" => stdout
            .lines()
            .filter(|line| line.contains("(compatibility version"))
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect(),
        // `otool -D` prints the install name, if there is one, under a header line.
        "-D" => stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.ends_with(':'))
            .map(str::to_string)
            .collect(),
        // Rpaths are invisible to `otool -L`, so they come from the full listing.
        "-l" => rpaths_from_load_commands(&stdout),
        _ => Vec::new(),
    };

    // A universal binary repeats its load commands once per architecture.
    paths.sort();
    paths.dedup();
    paths
}

/// Pull the `LC_RPATH` paths out of an `otool -l` listing.
fn rpaths_from_load_commands(listing: &str) -> Vec<String> {
    let mut rpaths = Vec::new();
    let mut lines = listing.lines();

    while let Some(line) = lines.next() {
        if line.trim() != "cmd LC_RPATH" {
            continue;
        }
        // The path follows within the command's few remaining fields, rendered
        // as "path <value> (offset N)".
        for field in lines.by_ref().take(3) {
            if let Some(value) = field.trim().strip_prefix("path ") {
                let value = value.split(" (offset").next().unwrap_or(value).trim();
                rpaths.push(value.to_string());
                break;
            }
        }
    }

    rpaths
}

/// Patch @@HOMEBREW_CELLAR@@ and @@HOMEBREW_PREFIX@@ placeholders in Mach-O binaries.
/// Also fixes version mismatches where a bottle references a different version of itself.
/// Additionally patches hardcoded Homebrew paths in binary data sections and text files.
/// Uses rayon for parallel processing.
pub fn patch_homebrew_placeholders(
    keg_path: &Path,
    cellar_dir: &Path,
    pkg_name: &str,
    pkg_version: &str,
) -> Result<(), Error> {
    use rayon::prelude::*;
    use regex::Regex;

    // Derive prefix from cellar (cellar_dir is typically prefix/Cellar)
    let prefix = cellar_dir.parent().unwrap_or(Path::new("/opt/homebrew"));

    let cellar_str = cellar_dir.to_string_lossy().to_string();
    let prefix_str = prefix.to_string_lossy().to_string();

    let version_pattern = format!(r"(/Cellar/{}/)([^/]+)(/)", regex::escape(pkg_name));
    let version_regex = Regex::new(&version_pattern).ok();

    // Collect all Mach-O files first (skip symlinks to avoid double-processing)
    let macho_files: Vec<PathBuf> = walkdir::WalkDir::new(keg_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        // Skip symlinks - only process actual files
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .filter(|path| is_macho_file(path))
        .collect();

    let failures = PatchFailures::default();

    // First pass: patch binary strings in Mach-O files
    macho_files.par_iter().for_each(|path| {
        if let Err(e) = patch_macho_binary_strings(path, &prefix_str) {
            failures.record(e);
        }
    });

    // Second pass: patch text files
    let text_files: Vec<PathBuf> = walkdir::WalkDir::new(keg_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    text_files.par_iter().for_each(|path| {
        let _ = patch_text_file_strings(path, &prefix_str, &cellar_str);
    });

    // Helper to patch a single path reference
    let patch_path = |old_path: &str| -> Option<String> {
        let mut new_path = old_path.to_string();
        let mut changed = false;

        // Replace Homebrew placeholders
        if old_path.contains("@@HOMEBREW_CELLAR@@") || old_path.contains("@@HOMEBREW_PREFIX@@") {
            new_path = new_path
                .replace("@@HOMEBREW_CELLAR@@", &cellar_str)
                .replace("@@HOMEBREW_PREFIX@@", &prefix_str);
            changed = true;
        }

        // Rewrite prefixes that the in-place pass could not fit. install_name_tool
        // rewrites the load commands properly, so length is no obstacle here.
        if let Some(rewritten) = rewrite_homebrew_prefixes(&new_path, &prefix_str) {
            new_path = rewritten;
            changed = true;
        }

        // Fix version mismatches for this package
        if let Some(re) = &version_regex
            && re.is_match(&new_path)
        {
            let replacement = format!("/Cellar/{}/{}/", pkg_name, pkg_version);
            let fixed = re.replace(&new_path, |caps: &regex::Captures| {
                let matched_version = &caps[2];
                if matched_version != pkg_version {
                    replacement.clone()
                } else {
                    caps[0].to_string()
                }
            });
            if fixed != new_path {
                new_path = fixed.to_string();
                changed = true;
            }
        }

        if changed && new_path != old_path {
            Some(new_path)
        } else {
            None
        }
    };

    // Third pass: Process Mach-O files for install_name_tool patching
    macho_files.par_iter().for_each(|path| {
        // Get file permissions and make writable if needed
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                failures.record(Error::StoreCorruption {
                    message: format!("failed to read metadata for {}: {e}", path.display()),
                });
                return;
            }
        };
        let original_mode = metadata.permissions().mode();
        let is_readonly = original_mode & 0o200 == 0;

        // Make writable for patching
        if is_readonly
            && let Err(e) =
                fs::set_permissions(path, fs::Permissions::from_mode(original_mode | 0o200))
        {
            failures.record(Error::StoreCorruption {
                message: format!("failed to make {} writable: {e}", path.display()),
            });
            return;
        }

        // Every edit is worked out before the first one is applied, so the
        // entitlements can be read while the binary still carries the signature
        // it was bottled with: `install_name_tool` replaces that signature with
        // a bare ad-hoc one — entitlements and all — the moment it writes.
        let changes = |flag: &str| -> Vec<(String, String)> {
            otool(path, flag)
                .into_iter()
                .filter_map(|old| patch_path(&old).map(|new| (old, new)))
                .collect()
        };
        // Library dependencies (-L), the install name ID (-D), and rpaths (-l).
        let dylibs = changes("-L");
        let ids = changes("-D");
        let rpaths = changes("-l");

        let entitlements = (!dylibs.is_empty() || !ids.is_empty() || !rpaths.is_empty())
            .then(|| read_entitlements(path))
            .flatten();
        let mut patched_any = false;

        for (old_path, new_path) in &dylibs {
            match install_name_tool(path, &["-change", old_path, new_path]) {
                Ok(()) => patched_any = true,
                Err(e) => failures.record(e),
            }
        }

        for (_, new_id) in &ids {
            match install_name_tool(path, &["-id", new_id]) {
                Ok(()) => patched_any = true,
                Err(e) => failures.record(e),
            }
        }

        // Rpaths are only search hints, so a binary with a stale one still runs
        // as long as its dependencies are absolute: warn instead of failing.
        for (old_rpath, new_rpath) in &rpaths {
            match install_name_tool(path, &["-rpath", old_rpath, new_rpath]) {
                Ok(()) => patched_any = true,
                Err(e) => warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to rewrite rpath {old_rpath}"
                ),
            }
        }

        // Re-sign if we patched anything (patching invalidates code signature)
        if patched_any && let Err(e) = codesign(path, entitlements.as_deref()) {
            failures.record(e);
        }

        // Restore original permissions
        if is_readonly
            && let Err(e) = fs::set_permissions(path, fs::Permissions::from_mode(original_mode))
        {
            failures.record(Error::StoreCorruption {
                message: format!("failed to restore permissions on {}: {e}", path.display()),
            });
        }
    });

    failures.into_result(keg_path)
}

/// Strip quarantine extended attributes and ad-hoc sign unsigned Mach-O binaries.
/// Homebrew bottles from ghcr.io are already adhoc signed, so this is mostly a no-op.
/// We use a fast heuristic: only process binaries that fail signature verification.
pub fn codesign_and_strip_xattrs(keg_path: &Path) -> Result<(), Error> {
    use rayon::prelude::*;

    // First, do a quick recursive xattr strip (single command, very fast)
    let _ = Command::new("xattr")
        .args(["-rd", "com.apple.quarantine", &keg_path.to_string_lossy()])
        .stderr(std::process::Stdio::null())
        .output();
    let _ = Command::new("xattr")
        .args(["-rd", "com.apple.provenance", &keg_path.to_string_lossy()])
        .stderr(std::process::Stdio::null())
        .output();

    // Find executables in bin/ directories only (where signing matters)
    // Skip dylibs and other Mach-O files - they inherit signing from their loader
    let bin_files: Vec<PathBuf> = walkdir::WalkDir::new(keg_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let path = e.path();
            path.is_file() && path.to_string_lossy().contains("/bin/")
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    let failures = PatchFailures::default();

    // Only process files that need signing
    bin_files.par_iter().for_each(|path| {
        if !is_macho_file(path) {
            return;
        }

        // Verify signature - if valid, skip
        let verify = Command::new("codesign")
            .args(["-v", &path.to_string_lossy()])
            .stderr(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status();

        if verify.map(|s| s.success()).unwrap_or(false) {
            return; // Already signed
        }

        // Whatever entitlements the bottle carries have to be carried over by
        // hand: an ad-hoc re-sign drops the ones already in the file.
        let entitlements = read_entitlements(path);

        // Get permissions and make writable
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(e) => {
                failures.record(Error::StoreCorruption {
                    message: format!("failed to read metadata for {}: {e}", path.display()),
                });
                return;
            }
        };
        let original_mode = metadata.permissions().mode();
        let is_readonly = original_mode & 0o200 == 0;

        if is_readonly {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(original_mode | 0o200));
        }

        // An executable that will not pass Gatekeeper is a broken install, so a
        // signing failure here is reported rather than swallowed.
        if let Err(e) = codesign(path, entitlements.as_deref()) {
            failures.record(e);
        }

        // Restore permissions
        if is_readonly {
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(original_mode));
        }
    });

    failures.into_result(keg_path)
}

#[cfg(test)]
mod tests {
    use super::super::macho::test_support::TestMacho;
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    const NEW_PREFIX: &str = "/opt/zb";

    /// A Mach-O image whose C strings hold a Homebrew path, and whose code
    /// section holds a byte-identical one that must never be rewritten.
    fn fixture() -> Vec<u8> {
        TestMacho::new(
            b"\x01\x02/opt/homebrew/opt/git/libexec/git-core\x03\x04",
            b"/opt/homebrew/opt/git/libexec/git-core\0/opt/homebrew/lib/libfoo.dylib\0",
        )
        .with_rpath("/opt/homebrew/lib")
        .build()
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Build a real, signed Mach-O executable that embeds `literal`.
    /// Returns `None` when there is no C compiler to build it with.
    fn compile_fixture(dir: &Path, literal: &str) -> Option<PathBuf> {
        let source = dir.join("fixture.c");
        fs::write(
            &source,
            format!(
                "const char *path = \"{literal}\";\nint main(void) {{ return path[0] == 0; }}\n"
            ),
        )
        .unwrap();

        let binary = dir.join("fixture");
        let built = Command::new("cc")
            .arg("-o")
            .arg(&binary)
            .arg(&source)
            .output()
            .ok()?;
        built.status.success().then_some(binary)
    }

    #[test]
    fn patches_c_strings_and_load_commands() {
        let patched = patch_macho_bytes(Path::new("fixture"), &fixture(), NEW_PREFIX)
            .expect("the fixture has patchable paths");

        assert!(find(&patched, b"/opt/zb/opt/git/libexec/git-core\0").is_some());
        assert!(find(&patched, b"/opt/zb/lib/libfoo.dylib\0").is_some());
        assert!(find(&patched, b"/opt/zb/lib\0").is_some());
    }

    #[test]
    fn leaves_paths_outside_string_data_alone() {
        let original = fixture();
        let patched = patch_macho_bytes(Path::new("fixture"), &original, NEW_PREFIX)
            .expect("the fixture has patchable paths");

        let code = b"\x01\x02/opt/homebrew/opt/git/libexec/git-core\x03\x04";
        assert!(
            find(&patched, code).is_some(),
            "a path inside the code section must be left untouched"
        );
        assert_eq!(
            original.len(),
            patched.len(),
            "patching must never resize the image"
        );
    }

    #[test]
    fn a_path_it_cannot_shorten_is_left_intact_rather_than_truncated() {
        let original = fixture();
        let long_prefix = "/opt/a/much/longer/zerobrew/prefix";

        assert!(
            patch_macho_bytes(Path::new("fixture"), &original, long_prefix).is_none(),
            "nothing fits, so nothing should be rewritten"
        );
    }

    #[test]
    fn a_shorter_path_keeps_its_tail() {
        let patched = patch_macho_bytes(Path::new("fixture"), &fixture(), NEW_PREFIX).unwrap();

        // The bug this guards against is zero-filling the *replaced* prefix's
        // leftover bytes in place, which cuts the string short at the first NUL.
        assert!(find(&patched, b"/opt/zb\0").is_none());
    }

    #[test]
    fn non_macho_files_are_ignored() {
        let tmp = TempDir::new().unwrap();
        let script = tmp.path().join("script.sh");
        fs::write(&script, b"#!/bin/sh\nexec /opt/homebrew/bin/foo\n").unwrap();

        patch_macho_binary_strings(&script, NEW_PREFIX).unwrap();

        assert_eq!(
            fs::read(&script).unwrap(),
            b"#!/bin/sh\nexec /opt/homebrew/bin/foo\n"
        );
    }

    #[test]
    fn rpaths_are_read_from_the_load_command_listing() {
        let listing = "\
Load command 12
          cmd LC_RPATH
      cmdsize 32
         path /opt/homebrew/lib (offset 12)
Load command 13
          cmd LC_LOAD_DYLIB
      cmdsize 56
         name /usr/lib/libSystem.B.dylib (offset 24)
Load command 14
          cmd LC_RPATH
      cmdsize 40
         path @loader_path/../lib (offset 12)
";
        assert_eq!(
            rpaths_from_load_commands(listing),
            vec![
                "/opt/homebrew/lib".to_string(),
                "@loader_path/../lib".to_string()
            ]
        );
    }

    #[test]
    fn a_patched_binary_stays_executable_and_signed() {
        let tmp = TempDir::new().unwrap();
        let literal = "/opt/homebrew/opt/git/libexec/git-core";
        let Some(binary) = compile_fixture(tmp.path(), literal) else {
            eprintln!("skipping: no C compiler available to build a real Mach-O fixture");
            return;
        };

        let mode = 0o555;
        fs::set_permissions(&binary, fs::Permissions::from_mode(mode)).unwrap();

        patch_macho_binary_strings(&binary, NEW_PREFIX).expect("patching should succeed");

        let patched = fs::read(&binary).unwrap();
        assert!(find(&patched, b"/opt/zb/opt/git/libexec/git-core\0").is_some());
        assert!(find(&patched, literal.as_bytes()).is_none());

        assert_eq!(
            fs::metadata(&binary).unwrap().permissions().mode() & 0o7777,
            mode,
            "the original permissions should be restored"
        );

        let verified = Command::new("codesign")
            .arg("-v")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            verified.status.success(),
            "patched binary must stay signed: {}",
            String::from_utf8_lossy(&verified.stderr)
        );

        assert!(
            Command::new(&binary).status().unwrap().success(),
            "patched binary must still run"
        );
    }

    #[test]
    fn a_failed_signature_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let binary = tmp.path().join("broken");
        // Structurally valid enough to patch, but not something codesign will
        // accept — exactly the case that used to be logged and forgotten.
        fs::write(&binary, fixture()).unwrap();

        let error = patch_macho_binary_strings(&binary, NEW_PREFIX)
            .expect_err("codesign cannot sign this, and that must not pass silently");
        assert!(
            error.to_string().contains("re-sign"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn test_version_regex_only_matches_cellar_paths() {
        use regex::Regex;

        let pkg_name = "mpdecimal";
        let pkg_version = "4.0.1";
        let pattern = format!(r"(/Cellar/{}/)([^/]+)(/)", regex::escape(pkg_name));
        let re = Regex::new(&pattern).expect("version regex should compile");

        let cellar_path = "/opt/zerobrew/Cellar/mpdecimal/3.9.0/lib/libmpdec.4.dylib";
        assert!(re.is_match(cellar_path));

        let replacement = format!("/Cellar/{}/{}/", pkg_name, pkg_version);
        let fixed = re.replace(cellar_path, |caps: &regex::Captures| {
            let matched_version = &caps[2];
            if matched_version != pkg_version {
                replacement.clone()
            } else {
                caps[0].to_string()
            }
        });
        assert_eq!(
            fixed,
            "/opt/zerobrew/Cellar/mpdecimal/4.0.1/lib/libmpdec.4.dylib"
        );

        let opt_path = "/opt/zerobrew/opt/mpdecimal/lib/libmpdec.4.dylib";
        assert!(!re.is_match(opt_path));

        let cellar_same_version = "/opt/zerobrew/Cellar/mpdecimal/4.0.1/lib/libmpdec.4.dylib";
        let unchanged = re.replace(cellar_same_version, |caps: &regex::Captures| {
            let matched_version = &caps[2];
            if matched_version != pkg_version {
                replacement.clone()
            } else {
                caps[0].to_string()
            }
        });
        assert_eq!(unchanged, cellar_same_version);
    }

    #[test]
    fn test_patch_text_file_strings() {
        let tmp = TempDir::new().unwrap();
        let test_file = tmp.path().join("test_script.sh");

        let content = r#"#!/bin/bash
export GIT_EXEC_PATH=/opt/homebrew/opt/git/libexec/git-core
export PREFIX=@@HOMEBREW_PREFIX@@
export CELLAR=@@HOMEBREW_CELLAR@@
export LIBRARY=@@HOMEBREW_LIBRARY@@
export PERL=@@HOMEBREW_PERL@@
echo "Hello from $PREFIX"
"#;

        fs::write(&test_file, content).unwrap();

        let new_prefix = "/opt/zerobrew/prefix";
        let new_cellar = format!("{}/Cellar", new_prefix);

        let result = patch_text_file_strings(&test_file, new_prefix, &new_cellar);
        assert!(result.is_ok());

        let patched = fs::read_to_string(&test_file).unwrap();
        assert!(patched.contains(new_prefix));
        assert!(!patched.contains("/opt/homebrew"));
        assert!(!patched.contains("@@HOMEBREW_"));
        assert!(patched.contains("/opt/zerobrew/prefix/opt/git/libexec/git-core"));
        assert!(patched.contains("/opt/zerobrew/prefix/Cellar"));
        assert!(patched.contains("/opt/zerobrew/prefix/Library"));
        assert!(patched.contains("/usr/bin/perl"));
    }
}
