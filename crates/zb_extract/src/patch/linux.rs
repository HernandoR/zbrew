use std::fs;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use rayon::prelude::*;
use tracing::{debug, warn};
use zb_core::Error;

const LINUX_HOMEBREW_PREFIX: &str = "/home/linuxbrew/.linuxbrew";

/// Number of leading bytes needed to read `e_ident` and `e_type` from an ELF
/// header. `e_type` sits at offset 16 for both ELF32 and ELF64.
const ELF_TYPE_PROBE_LEN: usize = 18;

/// What a cheap ELF header probe says about a file, before any full parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElfKind {
    /// Not an ELF file (no magic, unreadable, or truncated header).
    NotElf,
    /// A valid ELF file that cannot carry runtime search paths: relocatable
    /// objects (`ET_REL`, i.e. `.o` files and kernel modules), core dumps, and
    /// anything else that is neither `ET_EXEC` nor `ET_DYN`.
    NoRuntimePaths,
    /// `ET_EXEC` or `ET_DYN`: may carry RPATH/RUNPATH and an interpreter.
    Patchable,
}

/// Classify a file from its first few header bytes without reading the body.
///
/// Homebrew bottles ship relocatable objects alongside executables and shared
/// libraries (`llvm` alone ships thousands of `.o` files). Those have no
/// dynamic segment, no RPATH and no interpreter, so there is nothing to patch,
/// and handing them to the ELF rewriter only produces spurious parse errors:
/// C++ objects use `SHT_GROUP` (COMDAT) sections, which the rewriter cannot
/// represent. Filtering them out here keeps genuine failures warn-worthy.
fn classify_elf(path: &Path) -> ElfKind {
    let mut file = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return ElfKind::NotElf,
    };

    let mut head = [0u8; ELF_TYPE_PROBE_LEN];
    if file.read_exact(&mut head).is_err() {
        return ElfKind::NotElf;
    }
    if head[..4] != *b"\x7fELF" {
        return ElfKind::NotElf;
    }

    // `e_ident[EI_DATA]` selects the byte order of every multi-byte header
    // field, including `e_type`. `.0` unwraps object 0.40's newtypes back to
    // the raw ELF ABI constants.
    const EI_DATA: usize = 5;
    let e_type = match head[EI_DATA] {
        x if x == object::elf::ELFDATA2LSB.0 => u16::from_le_bytes([head[16], head[17]]),
        x if x == object::elf::ELFDATA2MSB.0 => u16::from_be_bytes([head[16], head[17]]),
        _ => return ElfKind::NotElf,
    };

    if e_type == object::elf::ET_EXEC.0 || e_type == object::elf::ET_DYN.0 {
        ElfKind::Patchable
    } else {
        ElfKind::NoRuntimePaths
    }
}

/// Patch @@HOMEBREW_CELLAR@@ and @@HOMEBREW_PREFIX@@ placeholders in both ELF binaries and text files.
#[cfg(target_os = "linux")]
pub fn patch_placeholders(
    keg_path: &Path,
    prefix_dir: &Path,
    _pkg_name: &str,
    _pkg_version: &str,
) -> Result<(), Error> {
    patch_elf_placeholders(keg_path, prefix_dir)?;
    patch_text_placeholders(keg_path, prefix_dir)?;
    Ok(())
}

fn rewrite_homebrew_prefixes(input: &str, prefix_dir: &Path) -> String {
    let prefix_str = prefix_dir.to_string_lossy().into_owned();
    input
        .replace("@@HOMEBREW_PREFIX@@", &prefix_str)
        .replace("@@HOMEBREW_REPOSITORY@@", &prefix_str)
        .replace("@@HOMEBREW_LIBRARY@@", &format!("{}/Library", prefix_str))
        .replace(LINUX_HOMEBREW_PREFIX, &prefix_str)
}

/// Detect if zbrew has installed its own glibc and return the path to its ld.so interpreter.
/// Returns None if zbrew's glibc is not found, indicating we should use the system ld.so.
fn detect_zbrew_glibc(prefix_dir: &Path) -> Option<PathBuf> {
    let cellar = prefix_dir.join("Cellar").join("glibc");

    if !cellar.exists() {
        return None;
    }

    // Look for glibc installations in the Cellar
    let glibc_entries = match fs::read_dir(&cellar) {
        Ok(entries) => entries,
        Err(_) => return None,
    };

    // Find the most recent glibc version directory
    let mut glibc_versions: Vec<PathBuf> = glibc_entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();

    if glibc_versions.is_empty() {
        return None;
    }

    // Sort to get the newest version (simple lexicographic sort should work for version numbers)
    glibc_versions.sort();
    glibc_versions.reverse();

    // Look for the ld.so interpreter in the glibc lib directory
    // Common names: ld-linux-x86-64.so.2, ld-linux-aarch64.so.1, ld-linux.so.2, etc.
    for glibc_dir in glibc_versions {
        let lib_dir = glibc_dir.join("lib");
        if !lib_dir.exists() {
            continue;
        }

        let entries = match fs::read_dir(&lib_dir) {
            Ok(e) => e,
            Err(_) => continue,
        };

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let filename = match path.file_name() {
                Some(name) => name.to_string_lossy(),
                None => continue,
            };

            // Match ld-linux*.so* patterns
            if filename.starts_with("ld-linux") && filename.contains(".so") {
                return Some(path);
            }
            // Also check for ld64.so.2 (ppc64)
            if filename == "ld64.so.2" || filename.starts_with("ld64.so.") {
                return Some(path);
            }
            // And ld-linux.so.* variants
            if filename.starts_with("ld-linux.so.") {
                return Some(path);
            }
        }
    }

    None
}

/// Find the system's dynamic linker (ld.so).
/// Returns the path to the system ld.so if found, None otherwise.
fn find_system_ld_so() -> Option<PathBuf> {
    // Common paths for system dynamic linkers on Linux
    let candidates = [
        "/lib64/ld-linux-x86-64.so.2",     // x86_64
        "/usr/lib64/ld-linux-x86-64.so.2", // x86_64
        "/lib/ld-linux-aarch64.so.1",      // aarch64/ARM64
        "/usr/lib/ld-linux-aarch64.so.1",  // aarch64/ARM64
        "/lib/ld-linux-armhf.so.3",        // ARM hard float
        "/usr/lib/ld-linux-armhf.so.3",    // ARM hard float
        "/lib/ld-linux.so.3",              // ARM
        "/lib/ld-linux.so.2",              // old ARM
        "/lib64/ld64.so.2",                // ppc64
        "/lib64/ld64.so.1",                // s390x
    ];

    for candidate in &candidates {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return Some(path);
        }
    }

    None
}

/// Patch @@HOMEBREW_CELLAR@@ and @@HOMEBREW_PREFIX@@ placeholders in ELF binaries.
/// Uses `arwen` crate to natively update RPATH, RUNPATH, and optionally the ELF interpreter.
///
/// Returns the number of files that were eligible for patching but could not be
/// patched. Files that carry no runtime search paths are not eligible and are
/// not counted.
fn patch_elf_placeholders(keg_path: &Path, prefix_dir: &Path) -> Result<usize, Error> {
    let lib_path = prefix_dir.join("lib").to_string_lossy().to_string();

    // Detect if zbrew has installed its own glibc
    let zbrew_interpreter = detect_zbrew_glibc(prefix_dir);

    // Determine which interpreter to use:
    // - If zbrew has glibc, use zbrew's ld.so
    // - Otherwise, use the system ld.so (fallback)
    let target_interpreter = if let Some(ref zb_ld) = zbrew_interpreter {
        Some(zb_ld.clone())
    } else {
        // Find system ld.so - common paths for Linux
        find_system_ld_so()
    };

    // Collect the ELF files that can actually carry runtime search paths.
    // Relocatable objects and core dumps are skipped quietly: they have nothing
    // to patch, so failing to rewrite them is not an error worth reporting.
    let elf_files: Vec<PathBuf> = walkdir::WalkDir::new(keg_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| match classify_elf(e.path()) {
            ElfKind::Patchable => true,
            ElfKind::NoRuntimePaths => {
                debug!(
                    path = %e.path().display(),
                    "skipping ELF without runtime search paths"
                );
                false
            }
            ElfKind::NotElf => false,
        })
        .map(|e| e.path().to_path_buf())
        .collect();

    let patch_failures = AtomicUsize::new(0);
    // Use a dashmap or similar for thread-safe inode tracking if needed,
    // but we can just collect and then process, or use a Mutex.
    let processed_inodes = std::sync::Mutex::new(std::collections::HashSet::new());

    // Clone for use in parallel closure
    let target_interpreter = target_interpreter.clone();
    let new_prefix = prefix_dir.to_string_lossy().to_string();

    elf_files.par_iter().for_each(|path| {
        // Check hardlinks
        if let Ok(meta) = fs::metadata(path) {
            use std::os::unix::fs::MetadataExt;
            let inode = (meta.dev(), meta.ino());
            let mut inodes = processed_inodes.lock().unwrap();
            if !inodes.insert(inode) {
                return; // Already processed this inode
            }
        }

        // Get permissions and make writable if needed
        let metadata = match fs::metadata(path) {
            Ok(m) => m,
            Err(_) => return,
        };
        let original_mode = metadata.permissions().mode();
        let is_readonly = original_mode & 0o200 == 0;

        if is_readonly {
            let mut perms = metadata.permissions();
            perms.set_mode(original_mode | 0o200);
            if let Err(e) = fs::set_permissions(path, perms) {
                warn!(
                    path = %path.display(),
                    error = %e,
                    "failed to make ELF writable for patching"
                );
                patch_failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }

        let result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let content = fs::read(path)?;
            let mut elf = arwen::elf::ElfContainer::parse(&content)?;

            // Check if it is a dynamic ELF.
            //
            // `.0` unwraps object 0.40's newtypes (`ProgramType`, `FileType`)
            // back to the raw integer. The header and segment values on the
            // left come from `arwen`, which links its own, older `object`, so
            // the two sides are different crate versions and the newtypes do
            // not unify. Comparing the raw values is still correct: these are
            // ELF ABI constants, fixed by the spec, not crate-defined numbers.
            let has_dynamic_segment = elf
                .inner
                .builder()
                .segments
                .iter()
                .any(|s| s.p_type == object::elf::PT_DYNAMIC.0);
            if !has_dynamic_segment {
                return Ok(());
            }

            // Set page size for alignment
            let page_size = elf.get_page_size();
            let _ = elf.set_page_size(page_size);

            // RPATH
            let old_rpaths = elf.get_rpath();
            let mut new_rpaths: Vec<String> = if old_rpaths.is_empty() {
                Vec::new()
            } else {
                old_rpaths
                    .iter()
                    .map(|r| rewrite_homebrew_prefixes(r, prefix_dir))
                    .filter(|r| r.starts_with(&new_prefix) || r.starts_with("$ORIGIN"))
                    .collect()
            };

            if !new_rpaths.contains(&lib_path) {
                new_rpaths.push(lib_path.clone());
            }

            let new_rpath_str = new_rpaths.join(":");
            if !new_rpath_str.is_empty() {
                let _ = elf.set_runpath(&new_rpath_str);
            }

            // Interpreter
            let is_executable = elf.inner.builder().header.e_type == object::elf::ET_EXEC.0
                || (elf.inner.builder().header.e_type == object::elf::ET_DYN.0
                    && elf.inner.elf_interpreter().is_some());

            if is_executable && let Some(current_interp_bytes) = elf.inner.elf_interpreter() {
                let current_interp_str = String::from_utf8_lossy(current_interp_bytes);

                let target_interp_path = if current_interp_str.contains("@@HOMEBREW_PREFIX@@")
                    || current_interp_str.contains(LINUX_HOMEBREW_PREFIX)
                {
                    let expanded = rewrite_homebrew_prefixes(&current_interp_str, prefix_dir);
                    let expanded_path = PathBuf::from(&expanded);
                    if expanded_path.exists() {
                        Some(expanded_path)
                    } else {
                        find_system_ld_so()
                    }
                } else {
                    target_interpreter.clone()
                };

                if let Some(target_path) = target_interp_path {
                    let target_str = target_path.to_string_lossy();
                    let _ = elf.set_interpreter(&target_str);
                }
            }

            // Atomic write
            let temp_path = path.with_extension("tmp_patch");
            {
                let mut temp_file = fs::File::create(&temp_path)?;
                elf.write(&mut temp_file)?;
            }
            fs::rename(temp_path, path)?;

            // Restore original permissions (including execute bit) after atomic write
            let mut perms = metadata.permissions();
            perms.set_mode(original_mode);
            fs::set_permissions(path, perms)?;

            Ok(())
        })();

        if let Err(e) = result {
            warn!(path = %path.display(), error = %e, "failed to patch ELF");
            patch_failures.fetch_add(1, Ordering::Relaxed);
        }
    });

    let failures = patch_failures.load(Ordering::Relaxed);
    if failures > 0 {
        warn!(
            failures,
            "failed to patch ELF files; packages may not work correctly until manually patched"
        );
    }

    Ok(failures)
}

/// Patch text files containing @@HOMEBREW_...@@ placeholders
fn patch_text_placeholders(keg_path: &Path, prefix_dir: &Path) -> Result<(), Error> {
    let cellar_str = prefix_dir.join("Cellar").to_string_lossy().to_string();

    // We search for files that are text and contain the placeholders.
    // To avoid reading every large file, we might filter by extension or size,
    // but Homebrew generally patches everything that looks like text.
    // For safety, we skip anything that looks like a binary (has null bytes in first 8kb).

    let files: Vec<PathBuf> = walkdir::WalkDir::new(keg_path)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();

    let patch_failures = AtomicUsize::new(0);

    files.par_iter().for_each(|path| {
        let result = (|| -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            // An archive that carries a hash of its own bytes is never rewritten:
            // restoring the bytes it was signed with is the only edit that leaves
            // it runnable, and that is handled there.
            if super::phar::restore_self_signed(path) {
                return Ok(());
            }

            // Check if file is likely text
            let mut file = fs::File::open(path)?;
            let mut buf = [0u8; 8192];
            let n = file.read(&mut buf)?;
            if buf[..n].contains(&0) {
                // Determine if it is ELF - we already handled those, but other binaries should be skipped too
                return Ok(());
            }

            // Read full content string
            let content = match fs::read_to_string(path) {
                Ok(c) => c,
                Err(_) => return Ok(()), // Not valid UTF-8, skip
            };

            if !content.contains("@@HOMEBREW_") && !content.contains(LINUX_HOMEBREW_PREFIX) {
                return Ok(());
            }

            let new_content = rewrite_homebrew_prefixes(&content, prefix_dir)
                .replace("@@HOMEBREW_CELLAR@@", &cellar_str)
                .replace("@@HOMEBREW_PERL@@", "/usr/bin/perl")
                .replace("@@HOMEBREW_JAVA@@", "/usr/bin/java");

            // Write back
            // Check readonly
            let metadata = fs::metadata(path)?;
            let original_mode = metadata.permissions().mode();
            let is_readonly = original_mode & 0o200 == 0;

            if is_readonly {
                let mut perms = metadata.permissions();
                perms.set_mode(original_mode | 0o200);
                fs::set_permissions(path, perms)?;
            }

            fs::write(path, new_content)?;

            if is_readonly {
                let mut perms = metadata.permissions();
                perms.set_mode(original_mode);
                fs::set_permissions(path, perms)?;
            }

            Ok(())
        })();

        if let Err(e) = result {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to patch text file"
            );
            patch_failures.fetch_add(1, Ordering::Relaxed);
        }
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use std::process::Command;
    use tempfile::TempDir;

    use super::super::phar::tests::{bottled, phar_signature_verifies, signed_phar};

    /// A keg holding one file, laid out the way a bottle extracts.
    fn keg_with(tmp: &TempDir, name: &str, contents: &[u8]) -> (PathBuf, PathBuf, PathBuf) {
        let prefix = tmp.path().join("prefix");
        let pkg_dir = prefix.join("Cellar/testpkg/1.0.0");
        let bin_dir = pkg_dir.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();

        let path = bin_dir.join(name);
        fs::write(&path, contents).unwrap();
        // Bottles ship executables read-only, which is the path the patcher has
        // to unlock before it can write.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o555)).unwrap();

        (prefix, pkg_dir, path)
    }

    /// Regression test for issue #39 / upstream #389: the composer bottle ships
    /// its whole program as a signed PHP archive, and Homebrew bottled
    /// `@@HOMEBREW_PREFIX@@` into it. Left literal, the archive no longer
    /// hashes to its own signature and PHP refuses to run it.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_placeholdered_phar_is_restored_to_the_bytes_its_signature_covers() {
        let tmp = TempDir::new().unwrap();

        let original = signed_phar(
            "#!/usr/bin/env php\n<?php Phar::mapPhar();\n",
            b"\0\x01cacert: /home/linuxbrew/.linuxbrew/etc/openssl@3/cert.pem\0",
        );
        let (prefix, pkg_dir, path) =
            keg_with(&tmp, "composer", &bottled(&original, LINUX_HOMEBREW_PREFIX));
        patch_placeholders(&pkg_dir, &prefix, "testpkg", "1.0.0").unwrap();

        let patched = fs::read(&path).unwrap();
        assert!(
            phar_signature_verifies(&patched),
            "the installed archive has to hash to the signature it carries"
        );
        assert_eq!(patched, original);
    }

    /// The other half of the rule, as a guard rather than a regression: an
    /// intact phar comes out byte-identical even when the text pass can read it
    /// and can see a Homebrew path inside it. Today the digest bytes are what
    /// keeps a real phar out of the rewriter -- they are rarely valid UTF-8 --
    /// which is a coincidence, not a decision.
    #[test]
    #[cfg(target_os = "linux")]
    fn an_intact_phar_is_never_rewritten() {
        let tmp = TempDir::new().unwrap();

        // The stub is long enough that the body sits past the 8 KiB the text
        // pass sniffs, so the file is not dismissed as binary on sight.
        let stub = format!(
            "#!/usr/bin/env php\n<?php /* {} */ Phar::mapPhar();\n// {LINUX_HOMEBREW_PREFIX}/etc\n",
            "pad ".repeat(2500)
        );
        let original = signed_phar(&stub, b"payload\n");

        let (prefix, pkg_dir, path) = keg_with(&tmp, "tool.phar", &original);
        patch_placeholders(&pkg_dir, &prefix, "testpkg", "1.0.0").unwrap();

        assert_eq!(
            fs::read(&path).unwrap(),
            original,
            "rewriting a path inside a signed archive invalidates its signature"
        );
    }

    fn compile_dummy_elf(dir: &Path, name: &str) -> Option<PathBuf> {
        let src_path = dir.join(format!("{}.c", name));
        if fs::write(&src_path, "int main() { return 0; }").is_err() {
            return None;
        }

        let out_path = dir.join(name);
        let status = Command::new("cc")
            .arg(&src_path)
            .arg("-o")
            .arg(&out_path)
            .arg("-Wl,-rpath,@@HOMEBREW_PREFIX@@/lib")
            .status()
            .ok()?;

        if status.success() {
            Some(out_path)
        } else {
            None
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn rewrites_linuxbrew_prefixes() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");

        assert_eq!(
            rewrite_homebrew_prefixes(
                "/home/linuxbrew/.linuxbrew/opt/expat/lib:@@HOMEBREW_PREFIX@@/lib",
                &prefix
            ),
            format!(
                "{}/opt/expat/lib:{}/lib",
                prefix.display(),
                prefix.display()
            )
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn patches_text_files() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let cellar = prefix.join("Cellar");
        let pkg_dir = cellar.join("testpkg/1.0.0");
        let bin_dir = pkg_dir.join("bin");

        fs::create_dir_all(&bin_dir).unwrap();

        let script_path = bin_dir.join("script.sh");
        fs::write(
            &script_path,
            "#!/home/linuxbrew/.linuxbrew/opt/python@3.14/bin/python3.14\necho @@HOMEBREW_PREFIX@@\necho @@HOMEBREW_CELLAR@@\necho @@HOMEBREW_LIBRARY@@\necho @@HOMEBREW_PERL@@",
        )
        .unwrap();

        let result = patch_placeholders(&pkg_dir, &prefix, "testpkg", "1.0.0");
        assert!(result.is_ok());

        let content = fs::read_to_string(&script_path).unwrap();
        assert!(content.contains(prefix.to_str().unwrap()));
        assert!(content.starts_with(&format!(
            "#!{}/opt/python@3.14/bin/python3.14",
            prefix.display()
        )));
        assert!(!content.contains(LINUX_HOMEBREW_PREFIX));
        assert!(content.contains(cellar.to_str().unwrap()));
        assert!(content.contains(&format!("{}/Library", prefix.to_str().unwrap())));
        assert!(content.contains("/usr/bin/perl"));
        assert!(!content.contains("@@HOMEBREW_"));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn patches_elf_file() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let cellar = prefix.join("Cellar");
        let pkg_dir = cellar.join("testpkg/1.0.0");
        let bin_dir = pkg_dir.join("bin");

        fs::create_dir_all(&bin_dir).unwrap();

        let elf_path = match compile_dummy_elf(&bin_dir, "testbin") {
            Some(p) => p,
            None => {
                eprintln!("Skipping ELF patch test: cc not found");
                return;
            }
        };

        // Record original permissions (should include execute bit from cc)
        let original_mode = fs::metadata(&elf_path).unwrap().permissions().mode();
        assert!(
            original_mode & 0o111 != 0,
            "compiled binary should be executable"
        );

        let result = patch_placeholders(&pkg_dir, &prefix, "testpkg", "1.0.0");
        assert!(result.is_ok());

        // Verify permissions are preserved after patching
        let new_mode = fs::metadata(&elf_path).unwrap().permissions().mode();
        assert_eq!(
            original_mode & 0o777,
            new_mode & 0o777,
            "permissions should be preserved after patching"
        );
    }

    /// Compile a C++ relocatable object that contains `SHT_GROUP` (COMDAT)
    /// sections, the shape that llvm's `lib/objects-Release/**/*.o` files have.
    fn compile_comdat_object(dir: &Path, name: &str) -> Option<PathBuf> {
        let src_path = dir.join(format!("{}.cpp", name));
        let source = r#"
#include <string>
#include <vector>
template <typename T> struct Box { T v; T get() const { return v; } };
inline int helper(int x) { return x + 1; }
std::vector<std::string> names() { return {"a", "b"}; }
int value() { Box<int> b{1}; return b.get() + helper(2); }
"#;
        fs::write(&src_path, source).ok()?;

        let out_path = dir.join(format!("{}.o", name));
        let status = Command::new("c++")
            .arg("-c")
            .arg(&src_path)
            .arg("-o")
            .arg(&out_path)
            // -fno-inline forces the inline/template bodies to be emitted
            // out-of-line into COMDAT groups instead of being inlined away.
            .args([
                "-O0",
                "-fno-inline",
                "-ffunction-sections",
                "-fdata-sections",
            ])
            .status()
            .ok()?;

        if status.success() {
            Some(out_path)
        } else {
            None
        }
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn classifies_relocatable_objects_as_unpatchable() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        // Not an ELF file at all.
        let text = dir.join("notes.txt");
        fs::write(&text, "just text").unwrap();
        assert_eq!(classify_elf(&text), ElfKind::NotElf);

        // Too short to hold an ELF header.
        let stub = dir.join("stub");
        fs::write(&stub, b"\x7fELF").unwrap();
        assert_eq!(classify_elf(&stub), ElfKind::NotElf);

        // A static archive starts with "!<arch>\n", not the ELF magic.
        let archive = dir.join("libfoo.a");
        fs::write(
            &archive,
            b"!<arch>\n/               0           0     0     0       4         `\n",
        )
        .unwrap();
        assert_eq!(classify_elf(&archive), ElfKind::NotElf);

        match compile_comdat_object(dir, "reloc") {
            Some(obj) => assert_eq!(
                classify_elf(&obj),
                ElfKind::NoRuntimePaths,
                "ET_REL objects carry no RPATH or interpreter"
            ),
            None => eprintln!("Skipping ET_REL classification: c++ not found"),
        }

        match compile_dummy_elf(dir, "exe") {
            Some(exe) => assert_eq!(classify_elf(&exe), ElfKind::Patchable),
            None => eprintln!("Skipping ET_EXEC/ET_DYN classification: cc not found"),
        }
    }

    /// Regression test for issue #13 / upstream #346: llvm bottles ship C++
    /// relocatable objects with COMDAT groups. The ELF rewriter cannot parse
    /// those, so before the fix every one of them produced a
    /// "Failed to patch ELF ... parse error" warning and bumped the failure
    /// count, even though there was nothing to patch.
    #[test]
    #[cfg(target_os = "linux")]
    fn relocatable_objects_do_not_count_as_patch_failures() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let pkg_dir = prefix.join("Cellar/llvm/22.1.4");
        let obj_dir = pkg_dir.join("lib/objects-Release/obj.MLIRCAPIIR");

        fs::create_dir_all(&obj_dir).unwrap();

        let obj = match compile_comdat_object(&obj_dir, "BuiltinAttributes.cpp") {
            Some(p) => p,
            None => {
                eprintln!("Skipping COMDAT object test: c++ not found");
                return;
            }
        };
        let before = fs::read(&obj).unwrap();

        let failures = patch_elf_placeholders(&pkg_dir, &prefix).unwrap();
        assert_eq!(
            failures, 0,
            "relocatable objects have no runtime paths and must be skipped quietly"
        );

        // The object must be left byte-identical: we never rewrite it.
        assert_eq!(before, fs::read(&obj).unwrap());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn test_glibc_detection() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");

        // Test 1: No glibc installed - should return None
        assert!(detect_zbrew_glibc(&prefix).is_none());

        // Test 2: Create a mock glibc installation
        let glibc_dir = prefix.join("Cellar/glibc/2.38");
        let lib_dir = glibc_dir.join("lib");
        fs::create_dir_all(&lib_dir).unwrap();

        // Create a mock ld-linux-x86-64.so.2
        let ld_so = lib_dir.join("ld-linux-x86-64.so.2");
        fs::write(&ld_so, "mock").unwrap();

        // Should now detect the glibc
        let detected = detect_zbrew_glibc(&prefix);
        assert!(detected.is_some());
        assert_eq!(detected.unwrap(), ld_so);

        // Test 3: Multiple glibc versions - should pick the newest
        let glibc_dir_newer = prefix.join("Cellar/glibc/2.39");
        let lib_dir_newer = glibc_dir_newer.join("lib");
        fs::create_dir_all(&lib_dir_newer).unwrap();
        let ld_so_newer = lib_dir_newer.join("ld-linux-x86-64.so.2");
        fs::write(&ld_so_newer, "mock").unwrap();

        let detected = detect_zbrew_glibc(&prefix);
        assert!(detected.is_some());
        assert_eq!(detected.unwrap(), ld_so_newer);
    }
}
