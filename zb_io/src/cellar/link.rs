use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use zb_core::{ConflictedLink, Error};

const LINK_DIRS: &[&str] = &["bin", "lib", "libexec", "include", "share", "etc"];
const PYVENV_CFG: &str = "pyvenv.cfg";
const LIBEXEC_SKIP_FILES: &[&str] = &[".gitignore", PYVENV_CFG];

fn should_skip_link_entry(src_dir: &Path, entry_name: &std::ffi::OsStr) -> bool {
    // Homebrew-style Python virtualenv formulae bundle an isolated venv under
    // libexec/. The main executable is exposed via bin/<name> symlinks that
    // resolve into libexec/ within the keg itself, so nothing inside libexec/
    // is meant for the shared prefix. Linking libexec/ contents into the
    // shared prefix/libexec/ causes cross-formula conflicts:
    //   - libexec/{pyvenv.cfg, .gitignore} (metadata)
    //   - libexec/lib*/python3.X/site-packages/ (private dep trees)
    //   - libexec/bin/<shared-dep> (e.g. sqlformat across mycli + pgcli)
    let is_libexec_dir = src_dir.file_name().and_then(|n| n.to_str()) == Some("libexec");

    if is_libexec_dir {
        // Detect a virtualenv libexec by the presence of pyvenv.cfg alongside;
        // when present, skip every entry — including libexec/bin/ — so private
        // venv contents never leak into the shared prefix.
        if src_dir.join(PYVENV_CFG).exists() {
            return true;
        }
        if entry_name
            .to_str()
            .is_some_and(|name| LIBEXEC_SKIP_FILES.contains(&name))
        {
            return true;
        }
    }

    entry_name.to_str() == Some("site-packages") && is_libexec_python_lib_dir(src_dir)
}

fn is_libexec_python_lib_dir(path: &Path) -> bool {
    let mut in_libexec = false;
    let mut previous_was_python_lib = false;

    for component in path.components() {
        let Component::Normal(name) = component else {
            previous_was_python_lib = false;
            continue;
        };
        let Some(name) = name.to_str() else {
            previous_was_python_lib = false;
            continue;
        };

        if name == "libexec" {
            in_libexec = true;
            previous_was_python_lib = false;
            continue;
        }

        if !in_libexec {
            continue;
        }

        if previous_was_python_lib && is_python_version_dir(name) {
            return true;
        }

        previous_was_python_lib = matches!(name, "lib" | "lib64");
    }

    false
}

fn is_python_version_dir(name: &str) -> bool {
    name.strip_prefix("python")
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| c.is_ascii_digit())
}

pub struct Linker {
    prefix: PathBuf,
    opt_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct LinkedFile {
    pub link_path: PathBuf,
    pub target_path: PathBuf,
}

/// Every filesystem mutation performed while linking a keg, in the order it
/// happened, so a failure part-way through can be undone.
#[derive(Debug)]
enum LinkAction {
    CreatedLink(PathBuf),
    CreatedDir(PathBuf),
    /// A symlink that was removed to make room (replaced version, dead link,
    /// or a legacy whole-directory symlink being expanded). `target` is the
    /// raw, unresolved link target so it can be recreated verbatim.
    RemovedSymlink {
        path: PathBuf,
        target: PathBuf,
    },
}

/// Undo log making `link_keg` all-or-none: on any error every symlink and
/// directory it created is removed and every symlink it replaced is restored,
/// so a failed link never leaves orphaned entries in the prefix
/// (issue #6 / upstream lucasgelfond/zerobrew#188).
#[derive(Debug, Default)]
struct LinkJournal {
    actions: Vec<LinkAction>,
}

impl LinkJournal {
    fn created_link(&mut self, path: &Path) {
        self.actions
            .push(LinkAction::CreatedLink(path.to_path_buf()));
    }

    fn created_dir(&mut self, path: &Path) {
        self.actions
            .push(LinkAction::CreatedDir(path.to_path_buf()));
    }

    fn removed_symlink(&mut self, path: &Path, target: &Path) {
        self.actions.push(LinkAction::RemovedSymlink {
            path: path.to_path_buf(),
            target: target.to_path_buf(),
        });
    }

    /// Replay the log backwards. Best-effort: a rollback step that fails must
    /// not mask the original error.
    fn rollback(&mut self) {
        for action in self.actions.drain(..).rev() {
            match action {
                LinkAction::CreatedLink(path) => {
                    let _ = fs::remove_file(&path);
                }
                LinkAction::CreatedDir(path) => {
                    let _ = fs::remove_dir(&path);
                }
                LinkAction::RemovedSymlink { path, target } => {
                    if path.symlink_metadata().is_ok() {
                        let _ = fs::remove_file(&path);
                    }
                    #[cfg(unix)]
                    let _ = std::os::unix::fs::symlink(&target, &path);
                }
            }
        }
    }
}

fn conflict(path: &Path) -> Error {
    Error::LinkConflict {
        conflicts: vec![ConflictedLink {
            path: path.to_path_buf(),
            owned_by: keg_name_from_symlink(path),
        }],
    }
}

fn keg_name_from_path(path: &Path) -> Option<String> {
    let components: Vec<_> = path.components().collect();
    for (i, c) in components.iter().enumerate() {
        if let Component::Normal(s) = c
            && s.eq_ignore_ascii_case("cellar")
            && let Some(Component::Normal(name)) = components.get(i + 1)
        {
            return name.to_str().map(String::from);
        }
    }
    None
}

/// Strip `.` and resolve `..` components without touching the filesystem, so
/// dangling symlink targets can still be attributed to a keg.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Read the symlink at `dst` and resolve a relative target against its parent
/// directory. Returns `None` when `dst` is not a symlink.
fn resolve_link_target(dst: &Path) -> Option<PathBuf> {
    let target = fs::read_link(dst).ok()?;
    Some(if target.is_relative() {
        dst.parent().unwrap_or(Path::new("")).join(&target)
    } else {
        target
    })
}

fn keg_name_from_symlink(dst: &Path) -> Option<String> {
    let resolved = resolve_link_target(dst)?;
    match fs::canonicalize(&resolved) {
        Ok(canonical) => keg_name_from_path(&canonical),
        // Dangling link (e.g. the keg it pointed into was removed): the
        // target path still identifies the owning keg.
        Err(_) => keg_name_from_path(&normalize_lexically(&resolved)),
    }
}

/// Whether the symlink at `dst` may be silently replaced by a link to `src`.
///
/// Regression guard for #331 (https://github.com/lucasgelfond/zerobrew/issues/331):
/// upgrades and reinstalls used to report the previous version's symlinks as
/// conflicts "belonging to" the formula itself, leaving the prefix pointing at
/// the old keg. A link is replaceable when it belongs to another version of
/// the same keg, or when its target no longer exists (dead links block fresh
/// installs but protect nothing).
fn can_replace_existing_link(src: &Path, dst: &Path) -> bool {
    let Some(resolved) = resolve_link_target(dst) else {
        return false;
    };
    if !resolved.exists() {
        return true;
    }
    match (keg_name_from_symlink(dst), keg_name_from_path(src)) {
        (Some(old_owner), Some(new_owner)) => old_owner == new_owner,
        _ => false,
    }
}

impl Linker {
    pub fn new(prefix: &Path) -> io::Result<Self> {
        let bin_dir = prefix.join("bin");
        let opt_dir = prefix.join("opt");
        fs::create_dir_all(&bin_dir)?;
        fs::create_dir_all(&opt_dir)?;

        for dir in LINK_DIRS {
            if *dir != "bin" {
                fs::create_dir_all(prefix.join(dir))?;
            }
        }

        Ok(Self {
            prefix: prefix.to_path_buf(),
            opt_dir,
        })
    }

    /// Pre-flight check: scan all destinations for conflicts without creating any symlinks.
    /// Returns Ok(()) if no conflicts, or Err(LinkConflict) with all conflicts collected.
    pub(crate) fn check_conflicts(&self, keg_path: &Path) -> Result<(), Error> {
        let mut conflicts = Vec::new();
        for dir_name in LINK_DIRS {
            let src_dir = keg_path.join(dir_name);
            let dst_dir = self.prefix.join(dir_name);
            if src_dir.exists() {
                Self::collect_conflicts(&src_dir, &dst_dir, &mut conflicts);
            }
        }
        if conflicts.is_empty() {
            Ok(())
        } else {
            Err(Error::LinkConflict { conflicts })
        }
    }

    fn collect_conflicts(src: &Path, dst: &Path, conflicts: &mut Vec<ConflictedLink>) {
        let entries = match fs::read_dir(src) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let file_name = entry.file_name();
            if should_skip_link_entry(src, &file_name) {
                continue;
            }

            let src_path = entry.path();
            let dst_path = dst.join(&file_name);

            // Use src_path.is_dir() which follows symlinks, so that keg entries
            // like `man -> ../gnuman` (symlinks to directories) are treated as dirs.
            if src_path.is_dir() {
                // When the destination is a symlink to a directory, actual linking will
                // expand it into individual file symlinks. Check the expanded contents.
                if dst_path.symlink_metadata().is_ok()
                    && dst_path.is_symlink()
                    && let Ok(old_target) = fs::read_link(&dst_path)
                {
                    let resolved = if old_target.is_relative() {
                        dst_path.parent().unwrap_or(Path::new("")).join(&old_target)
                    } else {
                        old_target
                    };
                    // A live symlink to a *non-directory* (typically another
                    // keg's file) cannot be expanded — the directory can only
                    // take its place by destroying it.
                    if resolved.exists() && !resolved.is_dir() {
                        conflicts.push(ConflictedLink {
                            path: dst_path.clone(),
                            owned_by: keg_name_from_symlink(&dst_path),
                        });
                        continue;
                    }
                    Self::collect_conflicts_merged(&src_path, &resolved, &dst_path, conflicts);
                    continue;
                }
                // A plain file already occupies the directory's place.
                if dst_path.exists() && !dst_path.is_dir() {
                    conflicts.push(ConflictedLink {
                        path: dst_path,
                        owned_by: None,
                    });
                    continue;
                }
                Self::collect_conflicts(&src_path, &dst_path, conflicts);
                continue;
            }

            if dst_path.symlink_metadata().is_ok() {
                if let Ok(target) = fs::read_link(&dst_path) {
                    let resolved = if target.is_relative() {
                        dst_path.parent().unwrap_or(Path::new("")).join(&target)
                    } else {
                        target
                    };
                    if fs::canonicalize(&resolved).ok() == fs::canonicalize(&src_path).ok() {
                        continue;
                    }
                    if can_replace_existing_link(&src_path, &dst_path) {
                        continue;
                    }
                }
                conflicts.push(ConflictedLink {
                    path: dst_path.clone(),
                    owned_by: keg_name_from_symlink(&dst_path),
                });
            } else if dst_path.exists() {
                conflicts.push(ConflictedLink {
                    path: dst_path,
                    owned_by: None,
                });
            }
        }
    }

    /// Check for conflicts when a directory symlink will be expanded into file-level links.
    /// `src` is the new keg's directory, `old_target` is where the existing symlink points,
    /// and `dst` is the prefix directory that will be created.
    fn collect_conflicts_merged(
        src: &Path,
        old_target: &Path,
        dst: &Path,
        conflicts: &mut Vec<ConflictedLink>,
    ) {
        let new_entries = match fs::read_dir(src) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in new_entries.flatten() {
            let file_name = entry.file_name();
            if should_skip_link_entry(src, &file_name) {
                continue;
            }

            let src_path = entry.path();
            let matching_old = old_target.join(&file_name);
            let dst_path = dst.join(&file_name);

            if src_path.is_dir() {
                if matching_old.exists() {
                    if !matching_old.is_dir() {
                        conflicts.push(ConflictedLink {
                            path: dst_path,
                            owned_by: keg_name_from_path(&matching_old),
                        });
                        continue;
                    }
                    Self::collect_conflicts_merged(&src_path, &matching_old, &dst_path, conflicts);
                } else {
                    Self::collect_conflicts(&src_path, &dst_path, conflicts);
                }
                continue;
            }

            if matching_old.exists()
                && fs::canonicalize(&matching_old).ok() != fs::canonicalize(&src_path).ok()
            {
                // Another version of the same keg (upgrade through a legacy
                // whole-directory symlink) will be replaced during linking.
                let old_owner = fs::canonicalize(&matching_old)
                    .ok()
                    .and_then(|p| keg_name_from_path(&p));
                if old_owner.is_some() && old_owner == keg_name_from_path(&src_path) {
                    continue;
                }
                conflicts.push(ConflictedLink {
                    path: dst_path,
                    owned_by: keg_name_from_symlink(dst).or_else(|| keg_name_from_path(old_target)),
                });
            }
        }
    }

    /// Link a keg into the prefix. All-or-none: a pre-flight scan rejects known
    /// conflicts before anything is touched, and any error raised while linking
    /// (including a conflict the scan could not predict) rolls back every
    /// symlink and directory created so far and restores every symlink
    /// replaced, leaving the prefix exactly as it was.
    pub(crate) fn link_keg(&self, keg_path: &Path) -> Result<Vec<LinkedFile>, Error> {
        self.check_conflicts(keg_path)?;
        self.link_opt(keg_path)?;

        let mut journal = LinkJournal::default();
        let mut linked = Vec::new();
        for dir_name in LINK_DIRS {
            let src_dir = keg_path.join(dir_name);
            let dst_dir = self.prefix.join(dir_name);
            if !src_dir.exists() {
                continue;
            }
            match Self::link_recursive(&src_dir, &dst_dir, &mut journal) {
                Ok(files) => linked.extend(files),
                Err(e) => {
                    journal.rollback();
                    return Err(e);
                }
            }
        }
        Ok(linked)
    }

    fn link_recursive(
        src: &Path,
        dst: &Path,
        journal: &mut LinkJournal,
    ) -> Result<Vec<LinkedFile>, Error> {
        let mut linked = Vec::new();
        if !dst.exists() {
            fs::create_dir_all(dst).map_err(Error::store("failed to create directory"))?;
            journal.created_dir(dst);
        }

        for entry in fs::read_dir(src).map_err(Error::store("failed to read directory"))? {
            let entry = entry.map_err(Error::store("failed to read directory entry"))?;
            let file_name = entry.file_name();
            if should_skip_link_entry(src, &file_name) {
                continue;
            }

            let src_path = entry.path();
            let dst_path = dst.join(&file_name);

            // Use src_path.is_dir() which follows symlinks, so that keg entries
            // like `man -> ../gnuman` (symlinks to directories) are expanded
            // into individual file symlinks instead of conflicting.
            if src_path.is_dir() {
                if dst_path.symlink_metadata().is_ok() && dst_path.is_symlink() {
                    let target = fs::read_link(&dst_path)
                        .map_err(Error::store("failed to read symlink target"))?;
                    let old_target = if target.is_relative() {
                        dst_path.parent().unwrap_or(Path::new("")).join(&target)
                    } else {
                        target.clone()
                    };
                    // A live symlink to a non-directory belongs to someone
                    // else and cannot be expanded — report it instead of
                    // deleting it.
                    if old_target.exists() && !old_target.is_dir() {
                        return Err(conflict(&dst_path));
                    }
                    let _ = fs::remove_file(&dst_path);
                    journal.removed_symlink(&dst_path, &target);
                    // A dangling directory symlink (e.g. the old keg was
                    // removed) has nothing left to expand.
                    if old_target.exists() {
                        Self::link_recursive(&old_target, &dst_path, journal)?;
                    }
                } else if dst_path.exists() && !dst_path.is_dir() {
                    return Err(conflict(&dst_path));
                }
                linked.extend(Self::link_recursive(&src_path, &dst_path, journal)?);
                continue;
            }

            if dst_path.symlink_metadata().is_ok() {
                if let Ok(target) = fs::read_link(&dst_path) {
                    let resolved = if target.is_relative() {
                        dst_path.parent().unwrap_or(Path::new("")).join(&target)
                    } else {
                        target.clone()
                    };
                    if fs::canonicalize(&resolved).ok() == fs::canonicalize(&src_path).ok() {
                        if resolved.exists() {
                            linked.push(LinkedFile {
                                link_path: dst_path,
                                target_path: src_path,
                            });
                            continue;
                        } else {
                            let _ = fs::remove_file(&dst_path);
                            journal.removed_symlink(&dst_path, &target);
                        }
                    } else if can_replace_existing_link(&src_path, &dst_path) {
                        let _ = fs::remove_file(&dst_path);
                        journal.removed_symlink(&dst_path, &target);
                    } else {
                        return Err(conflict(&dst_path));
                    }
                } else {
                    return Err(Error::LinkConflict {
                        conflicts: vec![ConflictedLink {
                            path: dst_path,
                            owned_by: None,
                        }],
                    });
                }
            } else if dst_path.exists() {
                return Err(Error::LinkConflict {
                    conflicts: vec![ConflictedLink {
                        path: dst_path,
                        owned_by: None,
                    }],
                });
            }

            #[cfg(unix)]
            std::os::unix::fs::symlink(&src_path, &dst_path)
                .map_err(Error::store("failed to create symlink"))?;
            journal.created_link(&dst_path);
            linked.push(LinkedFile {
                link_path: dst_path,
                target_path: src_path,
            });
        }
        Ok(linked)
    }

    pub(crate) fn unlink_keg(&self, keg_path: &Path) -> Result<Vec<PathBuf>, Error> {
        self.unlink_opt(keg_path)?;
        let mut unlinked = Vec::new();
        for dir_name in LINK_DIRS {
            let src_dir = keg_path.join(dir_name);
            let dst_dir = self.prefix.join(dir_name);
            if src_dir.exists() {
                unlinked.extend(Self::unlink_recursive(&src_dir, &dst_dir)?);
            }
        }
        Ok(unlinked)
    }

    pub(crate) fn collect_linked_files(&self, keg_path: &Path) -> Result<Vec<LinkedFile>, Error> {
        let mut linked = Vec::new();
        for dir_name in LINK_DIRS {
            let src_dir = keg_path.join(dir_name);
            let dst_dir = self.prefix.join(dir_name);
            if src_dir.exists() {
                linked.extend(Self::collect_linked_recursive(&src_dir, &dst_dir)?);
            }
        }
        Ok(linked)
    }

    fn unlink_recursive(src: &Path, dst: &Path) -> Result<Vec<PathBuf>, Error> {
        let mut unlinked = Vec::new();
        if !src.exists() || !dst.exists() {
            return Ok(unlinked);
        }
        for entry in fs::read_dir(src).map_err(Error::store("failed to read directory"))? {
            let entry = entry.map_err(Error::store("failed to read directory entry"))?;
            let src_path = entry.path();
            let dst_path = dst.join(entry.file_name());

            if src_path.is_dir() && dst_path.is_dir() && !dst_path.is_symlink() {
                unlinked.extend(Self::unlink_recursive(&src_path, &dst_path)?);
                if let Ok(mut entries) = fs::read_dir(&dst_path)
                    && entries.next().is_none()
                {
                    let _ = fs::remove_dir(&dst_path);
                }
                continue;
            }

            if let Ok(target) = fs::read_link(&dst_path) {
                let resolved = if target.is_relative() {
                    dst_path.parent().unwrap_or(Path::new("")).join(&target)
                } else {
                    target
                };
                if fs::canonicalize(&resolved).ok() == fs::canonicalize(&src_path).ok() {
                    let _ = fs::remove_file(&dst_path);
                    unlinked.push(dst_path);
                }
            }
        }
        Ok(unlinked)
    }

    fn collect_linked_recursive(src: &Path, dst: &Path) -> Result<Vec<LinkedFile>, Error> {
        let mut linked = Vec::new();
        if !src.exists() || !dst.exists() {
            return Ok(linked);
        }
        for entry in fs::read_dir(src).map_err(Error::store("failed to read directory"))? {
            let entry = entry.map_err(Error::store("failed to read directory entry"))?;
            let file_name = entry.file_name();
            if should_skip_link_entry(src, &file_name) {
                continue;
            }

            let src_path = entry.path();
            let dst_path = dst.join(file_name);

            if src_path.is_dir() && dst_path.is_dir() && !dst_path.is_symlink() {
                linked.extend(Self::collect_linked_recursive(&src_path, &dst_path)?);
                continue;
            }

            if let Ok(target) = fs::read_link(&dst_path) {
                let resolved = if target.is_relative() {
                    dst_path.parent().unwrap_or(Path::new("")).join(&target)
                } else {
                    target
                };
                if fs::canonicalize(&resolved).ok() == fs::canonicalize(&src_path).ok() {
                    linked.push(LinkedFile {
                        link_path: dst_path,
                        target_path: src_path,
                    });
                }
            }
        }
        Ok(linked)
    }

    fn unlink_opt(&self, keg_path: &Path) -> Result<(), Error> {
        let name = keg_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str());
        if let Some(name) = name {
            let opt_link = self.opt_dir.join(name);
            if let Ok(target) = fs::read_link(&opt_link) {
                let resolved = if target.is_relative() {
                    opt_link.parent().unwrap_or(Path::new("")).join(&target)
                } else {
                    target
                };
                if fs::canonicalize(&resolved).ok() == fs::canonicalize(keg_path).ok() {
                    let _ = fs::remove_file(&opt_link);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn link_opt(&self, keg_path: &Path) -> Result<(), Error> {
        let name = keg_path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::StoreCorruption {
                message: "invalid keg path".into(),
            })?;
        let opt_link = self.opt_dir.join(name);
        if opt_link.symlink_metadata().is_ok() {
            if let Ok(target) = fs::read_link(&opt_link) {
                let resolved = if target.is_relative() {
                    opt_link.parent().unwrap_or(Path::new("")).join(&target)
                } else {
                    target
                };
                if fs::canonicalize(&resolved).ok() == fs::canonicalize(keg_path).ok() {
                    return Ok(());
                }
            }
            let _ = fs::remove_file(&opt_link);
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(keg_path, &opt_link)
            .map_err(Error::store("failed to create opt symlink"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn setup_keg(tmp: &TempDir, name: &str) -> PathBuf {
        let keg_path = tmp.path().join("cellar").join(name).join("1.0.0");
        let bin_dir = keg_path.join("bin");
        fs::create_dir_all(&bin_dir).unwrap();
        let exe = bin_dir.join(name);
        fs::write(&exe, b"hi").unwrap();
        fs::set_permissions(&exe, PermissionsExt::from_mode(0o755)).unwrap();
        keg_path
    }

    #[test]
    fn links_executables_to_bin() {
        let tmp = TempDir::new().unwrap();
        let keg = setup_keg(&tmp, "foo");
        let linker = Linker::new(tmp.path()).unwrap();
        linker.link_keg(&keg).unwrap();
        assert!(tmp.path().join("bin/foo").exists());
    }

    #[test]
    fn merging_directories_works() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();
        let keg1 = prefix.join("cellar/pkg1/1.0.0");
        fs::create_dir_all(keg1.join("lib/pkgconfig")).unwrap();
        fs::write(keg1.join("lib/pkgconfig/pkg1.pc"), b"").unwrap();
        let keg2 = prefix.join("cellar/pkg2/1.0.0");
        fs::create_dir_all(keg2.join("lib/pkgconfig")).unwrap();
        fs::write(keg2.join("lib/pkgconfig/pkg2.pc"), b"").unwrap();
        linker.link_keg(&keg1).unwrap();
        linker.link_keg(&keg2).unwrap();
        assert!(prefix.join("lib/pkgconfig/pkg1.pc").exists());
        assert!(prefix.join("lib/pkgconfig/pkg2.pc").exists());
    }

    #[test]
    fn links_libexec_directory() {
        let tmp = TempDir::new().unwrap();
        let keg = tmp.path().join("cellar/git/2.52.0");
        let libexec_dir = keg.join("libexec/git-core");
        fs::create_dir_all(&libexec_dir).unwrap();

        let helper = libexec_dir.join("git-remote-https");
        fs::write(&helper, b"#!/bin/sh\necho helper").unwrap();
        fs::set_permissions(&helper, PermissionsExt::from_mode(0o755)).unwrap();

        let linker = Linker::new(tmp.path()).unwrap();
        linker.link_keg(&keg).unwrap();

        let linked_helper = tmp.path().join("libexec/git-core/git-remote-https");
        assert!(linked_helper.exists(), "git-remote-https should be linked");
        assert!(linked_helper.is_symlink(), "should be a symlink");
    }

    #[test]
    fn skips_libexec_virtualenv_metadata_to_avoid_conflicts() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg1 = prefix.join("cellar/ranger/1.0.0");
        fs::create_dir_all(keg1.join("libexec/bin")).unwrap();
        fs::create_dir_all(keg1.join("bin")).unwrap();
        fs::write(keg1.join("libexec/.gitignore"), b"# ranger").unwrap();
        fs::write(keg1.join("libexec/pyvenv.cfg"), b"home=/tmp/ranger").unwrap();
        fs::write(
            keg1.join("libexec/bin/sqlformat"),
            b"#!/bin/sh\necho sqlformat",
        )
        .unwrap();
        fs::set_permissions(
            keg1.join("libexec/bin/sqlformat"),
            PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        fs::write(keg1.join("bin/ranger"), b"#!/bin/sh\necho ranger").unwrap();
        fs::set_permissions(keg1.join("bin/ranger"), PermissionsExt::from_mode(0o755)).unwrap();

        let keg2 = prefix.join("cellar/ansible-lint/1.0.0");
        fs::create_dir_all(keg2.join("libexec/bin")).unwrap();
        fs::create_dir_all(keg2.join("bin")).unwrap();
        fs::write(keg2.join("libexec/.gitignore"), b"# ansible-lint").unwrap();
        fs::write(keg2.join("libexec/pyvenv.cfg"), b"home=/tmp/ansible-lint").unwrap();
        fs::write(
            keg2.join("libexec/bin/sqlformat"),
            b"#!/bin/sh\necho sqlformat",
        )
        .unwrap();
        fs::set_permissions(
            keg2.join("libexec/bin/sqlformat"),
            PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        fs::write(
            keg2.join("bin/ansible-lint"),
            b"#!/bin/sh\necho ansible-lint",
        )
        .unwrap();
        fs::set_permissions(
            keg2.join("bin/ansible-lint"),
            PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        linker.link_keg(&keg1).unwrap();
        linker.link_keg(&keg2).unwrap();

        // Nothing inside a virtualenv libexec/ should be linked into the
        // shared prefix — neither metadata files nor libexec/bin/ entries.
        assert!(!prefix.join("libexec/.gitignore").exists());
        assert!(!prefix.join("libexec/pyvenv.cfg").exists());
        assert!(
            !prefix.join("libexec/bin/sqlformat").exists(),
            "libexec/bin/ entries from a venv keg must not leak into shared prefix"
        );

        // Useful entrypoints still link correctly.
        assert!(prefix.join("bin/ranger").exists());
        assert!(prefix.join("bin/ansible-lint").exists());
    }

    #[test]
    fn skips_libexec_python_site_packages_to_avoid_virtualenv_conflicts() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg1 = prefix.join("cellar/visidata/1.0.0");
        fs::create_dir_all(keg1.join("bin")).unwrap();
        fs::write(keg1.join("bin/visidata"), b"#!/bin/sh\necho visidata").unwrap();
        fs::set_permissions(keg1.join("bin/visidata"), PermissionsExt::from_mode(0o755)).unwrap();

        for lib_dir in ["lib", "lib64"] {
            let site_packages = keg1
                .join("libexec")
                .join(lib_dir)
                .join("python3.14/site-packages");
            fs::create_dir_all(site_packages.join("six-1.17.0.dist-info/licenses")).unwrap();
            fs::write(site_packages.join("six.py"), b"visidata six").unwrap();
            fs::write(
                site_packages.join("six-1.17.0.dist-info/licenses/LICENSE"),
                b"license",
            )
            .unwrap();
        }

        let public_site_packages = keg1.join("lib/python3.14/site-packages");
        fs::create_dir_all(&public_site_packages).unwrap();
        fs::write(public_site_packages.join("public.py"), b"public").unwrap();

        let keg2 = prefix.join("cellar/thefuck/1.0.0");
        fs::create_dir_all(keg2.join("bin")).unwrap();
        fs::write(keg2.join("bin/thefuck"), b"#!/bin/sh\necho thefuck").unwrap();
        fs::set_permissions(keg2.join("bin/thefuck"), PermissionsExt::from_mode(0o755)).unwrap();

        for lib_dir in ["lib", "lib64"] {
            let site_packages = keg2
                .join("libexec")
                .join(lib_dir)
                .join("python3.14/site-packages");
            fs::create_dir_all(site_packages.join("six-1.17.0.dist-info/licenses")).unwrap();
            fs::write(site_packages.join("six.py"), b"thefuck six").unwrap();
            fs::write(
                site_packages.join("six-1.17.0.dist-info/licenses/LICENSE"),
                b"license",
            )
            .unwrap();
        }

        linker.link_keg(&keg1).unwrap();

        assert!(prefix.join("bin/visidata").exists());
        assert!(
            prefix
                .join("lib/python3.14/site-packages/public.py")
                .exists()
        );
        assert!(
            !prefix
                .join("libexec/lib/python3.14/site-packages/six.py")
                .exists()
        );
        assert!(
            !prefix
                .join("libexec/lib64/python3.14/site-packages/six.py")
                .exists()
        );

        assert!(linker.check_conflicts(&keg2).is_ok());
        linker.link_keg(&keg2).unwrap();

        assert!(prefix.join("bin/thefuck").exists());
        assert!(
            !prefix
                .join("libexec/lib/python3.14/site-packages/six.py")
                .exists()
        );
        assert!(
            !prefix
                .join("libexec/lib64/python3.14/site-packages/six.py")
                .exists()
        );
    }

    #[test]
    fn two_python_virtualenv_kegs_with_shared_dep_names_install_without_conflict() {
        // Regression test for #377 (https://github.com/lucasgelfond/zerobrew/issues/377):
        // installing two Python CLI apps (mycli, pgcli) failed because shared
        // transitive deps (sqlformat, pygmentize) live in each keg's libexec/bin/
        // and previously got merged into the shared prefix/libexec/bin/, colliding
        // on the second install.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        for name in ["mycli", "pgcli"] {
            let keg = prefix.join(format!("cellar/{name}/1.0.0"));
            fs::create_dir_all(keg.join("bin")).unwrap();
            fs::create_dir_all(keg.join("libexec/bin")).unwrap();
            fs::write(keg.join("libexec/pyvenv.cfg"), b"home=/tmp").unwrap();

            // Main entry point: bin/<name> -> ../libexec/bin/<name>, matching
            // Homebrew's virtualenv keg layout.
            std::os::unix::fs::symlink(
                format!("../libexec/bin/{name}"),
                keg.join(format!("bin/{name}")),
            )
            .unwrap();

            for exe in [name, "sqlformat", "pygmentize"] {
                let p = keg.join("libexec/bin").join(exe);
                fs::write(&p, b"#!/bin/sh\necho dep").unwrap();
                fs::set_permissions(&p, PermissionsExt::from_mode(0o755)).unwrap();
            }
        }

        let mycli = prefix.join("cellar/mycli/1.0.0");
        let pgcli = prefix.join("cellar/pgcli/1.0.0");

        linker.link_keg(&mycli).unwrap();
        assert!(
            linker.check_conflicts(&pgcli).is_ok(),
            "pgcli must not conflict with mycli on shared libexec/bin/ deps"
        );
        linker.link_keg(&pgcli).unwrap();

        assert!(prefix.join("bin/mycli").exists());
        assert!(prefix.join("bin/pgcli").exists());
        assert!(!prefix.join("libexec/bin/sqlformat").exists());
        assert!(!prefix.join("libexec/bin/pygmentize").exists());
    }

    #[test]
    fn check_conflicts_passes_when_clean() {
        let tmp = TempDir::new().unwrap();
        let keg = setup_keg(&tmp, "foo");
        let linker = Linker::new(tmp.path()).unwrap();
        assert!(linker.check_conflicts(&keg).is_ok());
    }

    #[test]
    fn check_conflicts_detects_conflicting_file() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg1 = setup_keg(&tmp, "pkg1");
        linker.link_keg(&keg1).unwrap();

        // Create a second keg with a conflicting binary name
        let keg2 = prefix.join("cellar/pkg2/1.0.0");
        let bin2 = keg2.join("bin");
        fs::create_dir_all(&bin2).unwrap();
        fs::write(bin2.join("pkg1"), b"conflict").unwrap();
        fs::set_permissions(bin2.join("pkg1"), PermissionsExt::from_mode(0o755)).unwrap();

        let result = linker.check_conflicts(&keg2);
        assert!(result.is_err());
        if let Err(Error::LinkConflict { conflicts }) = result {
            assert_eq!(conflicts.len(), 1);
            assert!(conflicts[0].path.ends_with("bin/pkg1"));
            assert_eq!(conflicts[0].owned_by.as_deref(), Some("pkg1"));
        }
    }

    #[test]
    fn check_conflicts_collects_all_conflicts() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        // Create keg1 with two binaries
        let keg1 = prefix.join("Cellar/pkg1/1.0.0");
        let bin1 = keg1.join("bin");
        fs::create_dir_all(&bin1).unwrap();
        fs::write(bin1.join("tool-a"), b"a").unwrap();
        fs::write(bin1.join("tool-b"), b"b").unwrap();
        linker.link_keg(&keg1).unwrap();

        // Create keg2 with overlapping binaries
        let keg2 = prefix.join("Cellar/pkg2/1.0.0");
        let bin2 = keg2.join("bin");
        fs::create_dir_all(&bin2).unwrap();
        fs::write(bin2.join("tool-a"), b"x").unwrap();
        fs::write(bin2.join("tool-b"), b"y").unwrap();

        let result = linker.check_conflicts(&keg2);
        assert!(result.is_err());
        if let Err(Error::LinkConflict { conflicts }) = result {
            assert_eq!(conflicts.len(), 2);
        }
    }

    #[test]
    fn link_keg_rejects_conflicts_without_creating_links() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg1 = setup_keg(&tmp, "alpha");
        linker.link_keg(&keg1).unwrap();

        // keg2 has a binary named "alpha" that conflicts
        let keg2 = prefix.join("cellar/beta/1.0.0");
        let bin2 = keg2.join("bin");
        fs::create_dir_all(&bin2).unwrap();
        fs::write(bin2.join("alpha"), b"other").unwrap();
        fs::write(bin2.join("beta-only"), b"unique").unwrap();

        assert!(linker.link_keg(&keg2).is_err());
        // The non-conflicting file should NOT have been linked (all-or-none)
        assert!(!prefix.join("bin/beta-only").exists());
        // The opt link should also not exist
        assert!(!prefix.join("opt/beta").exists());
    }

    #[test]
    fn symlink_to_directory_in_keg_expands_without_conflict() {
        // Reproduces the gnu-sed / gnu-tar / findutils conflict from issue #69:
        // https://github.com/lucasgelfond/zerobrew/issues/69
        // each keg has `libexec/gnubin/man -> ../gnuman` (symlink to directory).
        // The linker should expand these into individual file symlinks so that
        // man pages from different kegs coexist.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        // keg1: libexec/gnubin/man -> ../gnuman, with gnuman/man1/sed.1
        let keg1 = prefix.join("Cellar/gnu-sed/4.9");
        fs::create_dir_all(keg1.join("libexec/gnuman/man1")).unwrap();
        fs::write(keg1.join("libexec/gnuman/man1/sed.1"), b"sed man").unwrap();
        fs::create_dir_all(keg1.join("libexec/gnubin")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../gnuman", keg1.join("libexec/gnubin/man")).unwrap();

        // keg2: libexec/gnubin/man -> ../gnuman, with gnuman/man1/tar.1
        let keg2 = prefix.join("Cellar/gnu-tar/1.35");
        fs::create_dir_all(keg2.join("libexec/gnuman/man1")).unwrap();
        fs::write(keg2.join("libexec/gnuman/man1/tar.1"), b"tar man").unwrap();
        fs::create_dir_all(keg2.join("libexec/gnubin")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("../gnuman", keg2.join("libexec/gnubin/man")).unwrap();

        // Both should link without conflicts
        linker.link_keg(&keg1).unwrap();
        linker.link_keg(&keg2).unwrap();

        // Both man pages should be accessible
        assert!(prefix.join("libexec/gnubin/man/man1/sed.1").exists());
        assert!(prefix.join("libexec/gnubin/man/man1/tar.1").exists());
        // gnuman dirs should also be expanded and merged
        assert!(prefix.join("libexec/gnuman/man1/sed.1").exists());
        assert!(prefix.join("libexec/gnuman/man1/tar.1").exists());
    }

    #[test]
    fn check_conflicts_passes_for_symlink_to_directory() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg1 = prefix.join("Cellar/pkg1/1.0.0");
        fs::create_dir_all(keg1.join("libexec/realdir")).unwrap();
        fs::write(keg1.join("libexec/realdir/file1"), b"a").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("realdir", keg1.join("libexec/alias")).unwrap();

        let keg2 = prefix.join("Cellar/pkg2/1.0.0");
        fs::create_dir_all(keg2.join("libexec/realdir")).unwrap();
        fs::write(keg2.join("libexec/realdir/file2"), b"b").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("realdir", keg2.join("libexec/alias")).unwrap();

        linker.link_keg(&keg1).unwrap();
        // Pre-flight check should pass since the files don't overlap
        assert!(linker.check_conflicts(&keg2).is_ok());
    }

    #[test]
    fn upgrade_relinks_same_formula_to_new_version() {
        // Regression test for #331 (https://github.com/lucasgelfond/zerobrew/issues/331):
        // installing a newer version of an already-linked formula reported the
        // old version's symlinks as conflicts "belonging to" the formula
        // itself, so the prefix kept pointing at the old keg forever.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let old_keg = prefix.join("cellar/gh/1.0.0");
        fs::create_dir_all(old_keg.join("bin")).unwrap();
        fs::create_dir_all(old_keg.join("share/man/man1")).unwrap();
        fs::write(old_keg.join("bin/gh"), b"old").unwrap();
        fs::write(old_keg.join("share/man/man1/gh.1"), b"old man").unwrap();
        linker.link_keg(&old_keg).unwrap();

        let new_keg = prefix.join("cellar/gh/2.0.0");
        fs::create_dir_all(new_keg.join("bin")).unwrap();
        fs::create_dir_all(new_keg.join("share/man/man1")).unwrap();
        fs::write(new_keg.join("bin/gh"), b"new").unwrap();
        fs::write(new_keg.join("share/man/man1/gh.1"), b"new man").unwrap();

        assert!(
            linker.check_conflicts(&new_keg).is_ok(),
            "another version of the same formula must not count as a conflict"
        );
        linker.link_keg(&new_keg).unwrap();

        for link in ["bin/gh", "share/man/man1/gh.1"] {
            let target = fs::read_link(prefix.join(link)).unwrap();
            let target = target.to_string_lossy();
            assert!(
                target.contains("2.0.0"),
                "{link} must point at 2.0.0, got {target}"
            );
        }
    }

    #[test]
    fn relinks_when_old_keg_directory_was_removed() {
        // #331 fallout: an upgrade that removed the old keg but died before
        // relinking leaves dangling same-formula symlinks; the next install
        // must replace them instead of conflicting.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let old_keg = setup_keg(&tmp, "foo");
        linker.link_keg(&old_keg).unwrap();
        fs::remove_dir_all(&old_keg).unwrap();
        assert!(prefix.join("bin/foo").is_symlink());

        let new_keg = prefix.join("cellar/foo/2.0.0");
        fs::create_dir_all(new_keg.join("bin")).unwrap();
        fs::write(new_keg.join("bin/foo"), b"new").unwrap();

        assert!(linker.check_conflicts(&new_keg).is_ok());
        linker.link_keg(&new_keg).unwrap();

        let target = fs::read_link(prefix.join("bin/foo")).unwrap();
        assert!(target.to_string_lossy().contains("2.0.0"));
        assert!(prefix.join("bin/foo").exists(), "link must not be dangling");
    }

    #[test]
    fn replaces_dangling_symlink_from_other_formula() {
        // Orphaned links whose keg no longer exists (#188 fallout) used to
        // block unrelated installs with phantom conflicts. A dead link
        // protects nothing and is safe to replace.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        std::os::unix::fs::symlink(
            prefix.join("cellar/ghost/1.0.0/bin/tool"),
            prefix.join("bin/tool"),
        )
        .unwrap();

        let keg = prefix.join("cellar/tool/1.0.0");
        fs::create_dir_all(keg.join("bin")).unwrap();
        fs::write(keg.join("bin/tool"), b"real").unwrap();

        assert!(linker.check_conflicts(&keg).is_ok());
        linker.link_keg(&keg).unwrap();

        let target = fs::read_link(prefix.join("bin/tool")).unwrap();
        assert!(target.to_string_lossy().contains("cellar/tool/1.0.0"));
    }

    #[test]
    fn live_symlink_from_other_formula_still_conflicts() {
        // Replacement is limited to same-formula and dead links; a live link
        // owned by a different formula must keep failing all-or-none.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg1 = setup_keg(&tmp, "alpha");
        linker.link_keg(&keg1).unwrap();

        let keg2 = prefix.join("cellar/beta/1.0.0");
        fs::create_dir_all(keg2.join("bin")).unwrap();
        fs::write(keg2.join("bin/alpha"), b"other").unwrap();

        let result = linker.check_conflicts(&keg2);
        assert!(result.is_err());
        if let Err(Error::LinkConflict { conflicts }) = result {
            assert_eq!(conflicts[0].owned_by.as_deref(), Some("alpha"));
        }
        assert!(linker.link_keg(&keg2).is_err());
        let target = fs::read_link(prefix.join("bin/alpha")).unwrap();
        assert!(target.to_string_lossy().contains("alpha/1.0.0"));
    }

    #[test]
    fn upgrade_expands_legacy_directory_symlink_owned_by_same_formula() {
        // Whole-directory symlinks left by older layouts must be expanded and
        // replaced when the owning formula is upgraded, not reported as a
        // conflict for every file inside.
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let old_keg = prefix.join("cellar/foo/1.0.0");
        fs::create_dir_all(old_keg.join("share/doc/foo")).unwrap();
        fs::write(old_keg.join("share/doc/foo/README"), b"old").unwrap();
        fs::create_dir_all(prefix.join("share/doc")).unwrap();
        std::os::unix::fs::symlink(old_keg.join("share/doc/foo"), prefix.join("share/doc/foo"))
            .unwrap();

        let new_keg = prefix.join("cellar/foo/2.0.0");
        fs::create_dir_all(new_keg.join("share/doc/foo")).unwrap();
        fs::write(new_keg.join("share/doc/foo/README"), b"new").unwrap();

        assert!(linker.check_conflicts(&new_keg).is_ok());
        linker.link_keg(&new_keg).unwrap();

        let readme = prefix.join("share/doc/foo/README");
        let target = fs::read_link(&readme).unwrap();
        assert!(target.to_string_lossy().contains("2.0.0"));
    }

    #[test]
    fn dangling_directory_symlink_is_replaced() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        std::os::unix::fs::symlink(
            prefix.join("cellar/foo/0.9.0/share/foo"),
            prefix.join("share/foo"),
        )
        .unwrap();

        let keg = prefix.join("cellar/foo/1.0.0");
        fs::create_dir_all(keg.join("share/foo")).unwrap();
        fs::write(keg.join("share/foo/data.txt"), b"data").unwrap();

        assert!(linker.check_conflicts(&keg).is_ok());
        linker.link_keg(&keg).unwrap();

        assert!(prefix.join("share/foo/data.txt").exists());
    }

    #[test]
    fn keg_name_from_symlink_attributes_dangling_links() {
        let tmp = TempDir::new().unwrap();
        let link = tmp.path().join("gh");
        std::os::unix::fs::symlink(tmp.path().join("cellar/gh/1.0.0/bin/gh"), &link).unwrap();
        assert_eq!(keg_name_from_symlink(&link).as_deref(), Some("gh"));
    }

    /// Regression for #6 (upstream lucasgelfond/zerobrew#188): a keg directory
    /// landing on top of another keg's *file* symlink must be reported as a
    /// conflict up front, not silently delete the other keg's link and fail
    /// mid-way with an unrelated I/O error.
    #[test]
    fn directory_over_foreign_file_link_is_a_conflict() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg_a = prefix.join("cellar/aaa/1.0.0");
        fs::create_dir_all(keg_a.join("share")).unwrap();
        fs::write(keg_a.join("share/thing"), b"aaa").unwrap();
        linker.link_keg(&keg_a).unwrap();
        assert!(prefix.join("share/thing").is_symlink());

        let keg_b = prefix.join("cellar/bbb/1.0.0");
        fs::create_dir_all(keg_b.join("share/thing")).unwrap();
        fs::create_dir_all(keg_b.join("bin")).unwrap();
        fs::write(keg_b.join("share/thing/x"), b"bbb").unwrap();
        fs::write(keg_b.join("bin/bbb"), b"bbb").unwrap();

        let err = linker.check_conflicts(&keg_b).unwrap_err();
        assert!(
            matches!(err, Error::LinkConflict { .. }),
            "expected a link conflict, got {err:?}"
        );

        let err = linker.link_keg(&keg_b).unwrap_err();
        assert!(
            matches!(err, Error::LinkConflict { .. }),
            "expected a link conflict, got {err:?}"
        );

        // All-or-none: the other keg's link survives and nothing from bbb leaks.
        let target = fs::read_link(prefix.join("share/thing")).unwrap();
        assert!(
            target.to_string_lossy().contains("aaa"),
            "aaa's link must not be destroyed by the failed bbb link"
        );
        assert!(
            !prefix.join("bin/bbb").exists(),
            "bbb must leave no orphaned symlinks behind"
        );
    }

    /// Regression for #6: a failure part-way through linking (here: an
    /// unwritable prefix directory, which the pre-flight conflict scan cannot
    /// predict) must roll back every symlink already created for the keg.
    #[test]
    fn partial_link_failure_rolls_back_created_links() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg = prefix.join("cellar/rollback/1.0.0");
        fs::create_dir_all(keg.join("bin")).unwrap();
        fs::create_dir_all(keg.join("lib")).unwrap();
        fs::write(keg.join("bin/rollback"), b"exe").unwrap();
        fs::write(keg.join("lib/librollback.a"), b"lib").unwrap();

        // Make prefix/lib unwritable so the second link fails after the first
        // one has already been created.
        let lib_dir = prefix.join("lib");
        fs::set_permissions(&lib_dir, PermissionsExt::from_mode(0o555)).unwrap();

        let result = linker.link_keg(&keg);

        fs::set_permissions(&lib_dir, PermissionsExt::from_mode(0o755)).unwrap();

        assert!(
            result.is_err(),
            "linking should fail on an unwritable prefix"
        );
        assert!(
            !prefix.join("bin/rollback").exists(),
            "links created before the failure must be rolled back"
        );
    }

    /// Rollback must not clobber links that already existed and were left
    /// untouched (an idempotent re-link of an already linked keg).
    #[test]
    fn rollback_keeps_pre_existing_links_of_the_same_keg() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path();
        let linker = Linker::new(prefix).unwrap();

        let keg = prefix.join("cellar/idem/1.0.0");
        fs::create_dir_all(keg.join("bin")).unwrap();
        fs::write(keg.join("bin/idem"), b"exe").unwrap();
        linker.link_keg(&keg).unwrap();

        // Now add a lib/ entry and make prefix/lib unwritable: relinking fails,
        // but the already-correct bin link belongs to this keg and stays.
        fs::create_dir_all(keg.join("lib")).unwrap();
        fs::write(keg.join("lib/libidem.a"), b"lib").unwrap();
        let lib_dir = prefix.join("lib");
        fs::set_permissions(&lib_dir, PermissionsExt::from_mode(0o555)).unwrap();

        let result = linker.link_keg(&keg);

        fs::set_permissions(&lib_dir, PermissionsExt::from_mode(0o755)).unwrap();

        assert!(result.is_err());
        assert!(
            prefix.join("bin/idem").exists(),
            "pre-existing links must survive a rollback"
        );
    }
}
