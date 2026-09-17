use std::fs;
use std::path::{Path, PathBuf};

use zb_core::Error;

/// Directory inside a keg where Homebrew stages files that belong to the
/// shared prefix instead of to the keg.
const BOTTLE_DIR: &str = ".bottle";

/// Subtrees of `.bottle/` that are installed into the prefix.
///
/// A bottle is built with `HOMEBREW_PREFIX/etc` and `HOMEBREW_PREFIX/var`
/// redirected into `<keg>/.bottle/`, so a formula that ships configuration —
/// php's `php.ini`, for instance — carries it at `<keg>/.bottle/etc/php/...`
/// and never at `<keg>/etc/`. The linker only walks the keg's own top-level
/// directories, so without this step the configuration is never installed.
const PREFIX_DIRS: &[&str] = &["etc", "var"];

/// Install the prefix-owned files a bottle stages under `<keg>/.bottle/`.
///
/// These are *copied*, not symlinked, because they are the user's to edit.
/// Following Homebrew's `InstallRenamed`, a destination that already exists
/// with different contents is left alone and the bottle's version is written
/// beside it as `<name>.default`, so an upgrade never discards a local edit.
/// Returns the paths written, in the order they were written.
pub(crate) fn install_bottle_prefix_files(
    keg_path: &Path,
    prefix: &Path,
) -> Result<Vec<PathBuf>, Error> {
    let staged = keg_path.join(BOTTLE_DIR);
    let mut installed = Vec::new();

    for dir_name in PREFIX_DIRS {
        let src_dir = staged.join(dir_name);
        if !src_dir.is_dir() {
            continue;
        }
        install_tree(&src_dir, &prefix.join(dir_name), &mut installed)?;
    }

    Ok(installed)
}

fn install_tree(src: &Path, dst: &Path, installed: &mut Vec<PathBuf>) -> Result<(), Error> {
    fs::create_dir_all(dst).map_err(Error::store("failed to create prefix directory"))?;

    for entry in
        fs::read_dir(src).map_err(Error::store("failed to read staged prefix directory"))?
    {
        let entry = entry.map_err(Error::store("failed to read staged prefix entry"))?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            install_tree(&src_path, &dst_path, installed)?;
            continue;
        }

        let Some(target) = destination_for(&src_path, &dst_path)? else {
            continue;
        };
        fs::copy(&src_path, &target).map_err(Error::store("failed to install bottle config"))?;
        installed.push(target);
    }

    Ok(())
}

/// Where a staged file should land, or `None` when the prefix already holds an
/// identical copy and there is nothing to write.
fn destination_for(src: &Path, dst: &Path) -> Result<Option<PathBuf>, Error> {
    if !dst.is_file() {
        return Ok(Some(dst.to_path_buf()));
    }
    if is_identical(src, dst)? {
        return Ok(None);
    }
    Ok(Some(default_sibling(dst)))
}

fn is_identical(src: &Path, dst: &Path) -> Result<bool, Error> {
    let staged = fs::read(src).map_err(Error::store("failed to read staged bottle config"))?;
    match fs::read(dst) {
        Ok(existing) => Ok(existing == staged),
        Err(_) => Ok(false),
    }
}

fn default_sibling(dst: &Path) -> PathBuf {
    let mut name = dst.file_name().unwrap_or_default().to_os_string();
    name.push(".default");
    dst.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn php_keg(root: &Path, php_ini: &str) -> PathBuf {
        let keg = root.join("cellar/php/8.5.0");
        write(&keg.join(".bottle/etc/php/8.5/php.ini"), php_ini);
        write(
            &keg.join(".bottle/etc/php/8.5/php-fpm.d/www.conf"),
            "[www]\n",
        );
        write(&keg.join("bin/php"), "#!/bin/sh\n");
        keg
    }

    #[test]
    fn installs_staged_etc_tree_into_the_prefix() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let keg = php_keg(tmp.path(), "memory_limit = 128M\n");

        let installed = install_bottle_prefix_files(&keg, &prefix).unwrap();

        let php_ini = prefix.join("etc/php/8.5/php.ini");
        let www_conf = prefix.join("etc/php/8.5/php-fpm.d/www.conf");
        assert_eq!(
            fs::read_to_string(&php_ini).unwrap(),
            "memory_limit = 128M\n"
        );
        assert_eq!(fs::read_to_string(&www_conf).unwrap(), "[www]\n");
        assert_eq!(installed.len(), 2);
        assert!(installed.contains(&php_ini));
        assert!(installed.contains(&www_conf));
    }

    #[test]
    fn copies_rather_than_symlinks_so_the_keg_is_not_edited_in_place() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let keg = php_keg(tmp.path(), "memory_limit = 128M\n");

        install_bottle_prefix_files(&keg, &prefix).unwrap();

        let php_ini = prefix.join("etc/php/8.5/php.ini");
        assert!(!php_ini.is_symlink());
        fs::write(&php_ini, "memory_limit = 512M\n").unwrap();
        assert_eq!(
            fs::read_to_string(keg.join(".bottle/etc/php/8.5/php.ini")).unwrap(),
            "memory_limit = 128M\n"
        );
    }

    #[test]
    fn an_edited_config_survives_an_upgrade_and_gets_a_default_beside_it() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let old = php_keg(tmp.path(), "memory_limit = 128M\n");
        install_bottle_prefix_files(&old, &prefix).unwrap();

        let php_ini = prefix.join("etc/php/8.5/php.ini");
        fs::write(&php_ini, "memory_limit = 512M\n").unwrap();

        let new_keg = tmp.path().join("cellar/php/8.5.1");
        write(
            &new_keg.join(".bottle/etc/php/8.5/php.ini"),
            "memory_limit = 256M\n",
        );
        let installed = install_bottle_prefix_files(&new_keg, &prefix).unwrap();

        assert_eq!(
            fs::read_to_string(&php_ini).unwrap(),
            "memory_limit = 512M\n"
        );
        let default = prefix.join("etc/php/8.5/php.ini.default");
        assert_eq!(
            fs::read_to_string(&default).unwrap(),
            "memory_limit = 256M\n"
        );
        assert_eq!(installed, vec![default]);
    }

    #[test]
    fn an_untouched_config_is_left_alone_when_reinstalled() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let keg = php_keg(tmp.path(), "memory_limit = 128M\n");

        install_bottle_prefix_files(&keg, &prefix).unwrap();
        let installed = install_bottle_prefix_files(&keg, &prefix).unwrap();

        assert!(installed.is_empty());
        assert!(!prefix.join("etc/php/8.5/php.ini.default").exists());
    }

    #[test]
    fn installs_staged_var_tree() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let keg = tmp.path().join("cellar/nginx/1.0.0");
        write(&keg.join(".bottle/var/www/index.html"), "hello\n");

        install_bottle_prefix_files(&keg, &prefix).unwrap();

        assert_eq!(
            fs::read_to_string(prefix.join("var/www/index.html")).unwrap(),
            "hello\n"
        );
    }

    #[test]
    fn a_keg_without_staged_files_installs_nothing() {
        let tmp = TempDir::new().unwrap();
        let prefix = tmp.path().join("prefix");
        let keg = tmp.path().join("cellar/jq/1.7");
        write(&keg.join("bin/jq"), "#!/bin/sh\n");

        let installed = install_bottle_prefix_files(&keg, &prefix).unwrap();

        assert!(installed.is_empty());
        assert!(!prefix.join("etc").exists());
    }
}
