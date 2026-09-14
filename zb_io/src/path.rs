use std::path::{Component, Path, PathBuf};

use zb_core::Error;

const MAX_PATH_LEN: usize = 4096;

pub fn validate_privileged_path(path: &Path) -> Result<(), Error> {
    let path_str = path.to_string_lossy();

    if path_str.len() > MAX_PATH_LEN {
        return Err(Error::InvalidArgument {
            message: format!(
                "path exceeds maximum length of {MAX_PATH_LEN} bytes: {}",
                path.display()
            ),
        });
    }

    if path_str.bytes().any(|b| b.is_ascii_control()) {
        return Err(Error::InvalidArgument {
            message: "path contains control characters".to_string(),
        });
    }

    if path_str.starts_with('-') {
        return Err(Error::InvalidArgument {
            message: format!("path starts with '-': {}", path.display()),
        });
    }

    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(Error::InvalidArgument {
                message: format!("path contains '..' traversal: {}", path.display()),
            });
        }
    }

    Ok(())
}

/// Directories that must never be the target of a recursive delete, even when
/// the caller asks for it explicitly.
const PROTECTED_DIRS: &[&str] = &[
    // POSIX / Linux
    "/",
    "/bin",
    "/boot",
    "/dev",
    "/etc",
    "/home",
    "/lib",
    "/lib32",
    "/lib64",
    "/media",
    "/mnt",
    "/opt",
    "/proc",
    "/root",
    "/run",
    "/sbin",
    "/srv",
    "/sys",
    "/tmp",
    "/usr",
    "/usr/bin",
    "/usr/include",
    "/usr/lib",
    "/usr/local",
    "/usr/local/bin",
    "/usr/local/lib",
    "/usr/sbin",
    "/usr/share",
    "/var",
    // macOS
    "/Applications",
    "/Library",
    "/System",
    "/Users",
    "/Volumes",
    "/private",
    "/private/etc",
    "/private/tmp",
    "/private/var",
];

/// Minimum number of non-root components a directory must have before we are
/// willing to delete it recursively. `/opt/zbrew` (2) is acceptable;
/// `/usr` (1) and `/` (0) are not.
const MIN_DESTRUCTIVE_DEPTH: usize = 2;

/// Validate that `path` is safe to delete recursively.
///
/// This guards operations that wipe a whole directory tree (`zb reset`).
/// The root and prefix come from `--root`/`--prefix` and the `ZBREW_ROOT`/
/// `ZBREW_PREFIX` environment variables, so without this check a stale or
/// mistyped value is taken at face value and its entire contents deleted.
///
/// On top of [`validate_privileged_path`] this requires the path to be
/// absolute, to not be a well-known system directory, to not be the user's
/// home directory, and to be at least [`MIN_DESTRUCTIVE_DEPTH`] deep.
pub fn validate_destructive_path(path: &Path) -> Result<(), Error> {
    validate_destructive_path_with_home(
        path,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
    )
}

/// Inner form of [`validate_destructive_path`] with the home directory injected,
/// so the rules can be tested without mutating process-global environment.
fn validate_destructive_path_with_home(path: &Path, home: Option<&Path>) -> Result<(), Error> {
    validate_privileged_path(path)?;

    if !path.is_absolute() {
        return Err(Error::InvalidArgument {
            message: format!(
                "refusing to recursively delete a relative path: {}",
                path.display()
            ),
        });
    }

    // `components()` drops `.` and trailing separators, so a protected
    // directory cannot be smuggled past the list as `/usr/local/` or
    // `/usr/./local`. `..` is already rejected by validate_privileged_path.
    let normalized = lexically_normalize(path);

    if PROTECTED_DIRS.contains(&normalized.to_string_lossy().as_ref()) {
        return Err(Error::InvalidArgument {
            message: format!(
                "refusing to recursively delete the system directory {}",
                normalized.display()
            ),
        });
    }

    if let Some(home) = home
        && home.is_absolute()
        && lexically_normalize(home) == normalized
    {
        return Err(Error::InvalidArgument {
            message: format!(
                "refusing to recursively delete the home directory {}",
                normalized.display()
            ),
        });
    }

    let depth = normalized
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();

    if depth < MIN_DESTRUCTIVE_DEPTH {
        return Err(Error::InvalidArgument {
            message: format!(
                "refusing to recursively delete {}: paths shallower than \
                 {MIN_DESTRUCTIVE_DEPTH} components are assumed to be system directories",
                normalized.display()
            ),
        });
    }

    Ok(())
}

/// Collapse `.` components and trailing separators without touching the
/// filesystem. `..` is not resolved; callers reject it beforehand.
fn lexically_normalize(path: &Path) -> PathBuf {
    path.components().collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn accepts_normal_absolute_path() {
        assert!(validate_privileged_path(Path::new("/opt/zbrew")).is_ok());
    }

    #[test]
    fn accepts_normal_relative_path() {
        assert!(validate_privileged_path(Path::new("zbrew/store")).is_ok());
    }

    #[test]
    fn rejects_parent_dir_traversal() {
        let err = validate_privileged_path(Path::new("/opt/../etc/shadow")).unwrap_err();
        assert!(err.to_string().contains("'..'"));
    }

    #[test]
    fn rejects_control_characters() {
        let bad = "/opt/zero\x07brew";
        let err = validate_privileged_path(Path::new(bad)).unwrap_err();
        assert!(err.to_string().contains("control characters"));
    }

    #[test]
    fn rejects_null_byte_in_path() {
        let bad = "/opt/zero\x00brew";
        let err = validate_privileged_path(Path::new(bad)).unwrap_err();
        assert!(err.to_string().contains("control characters"));
    }

    #[test]
    fn rejects_newline_in_path() {
        let bad = "/opt/zero\nbrew";
        let err = validate_privileged_path(Path::new(bad)).unwrap_err();
        assert!(err.to_string().contains("control characters"));
    }

    #[test]
    fn rejects_excessively_long_path() {
        let long = "/".to_string() + &"a".repeat(MAX_PATH_LEN + 1);
        let err = validate_privileged_path(Path::new(&long)).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum length"));
    }

    #[test]
    fn accepts_path_at_max_length() {
        let long = "/".to_string() + &"a".repeat(MAX_PATH_LEN - 1);
        assert!(validate_privileged_path(Path::new(&long)).is_ok());
    }

    #[test]
    fn rejects_trailing_dotdot() {
        let err = validate_privileged_path(Path::new("/opt/zbrew/..")).unwrap_err();
        assert!(err.to_string().contains("'..'"));
    }

    #[test]
    fn rejects_leading_dash() {
        let err = validate_privileged_path(Path::new("-rf")).unwrap_err();
        assert!(err.to_string().contains("starts with '-'"));
    }

    #[test]
    fn rejects_leading_double_dash() {
        let err = validate_privileged_path(Path::new("--help")).unwrap_err();
        assert!(err.to_string().contains("starts with '-'"));
    }

    #[test]
    fn destructive_rejects_root() {
        let err = validate_destructive_path(Path::new("/")).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    #[test]
    fn destructive_rejects_system_dirs() {
        for bad in [
            "/usr",
            "/usr/local",
            "/etc",
            "/home",
            "/var",
            "/opt",
            "/tmp",
            "/Users",
            "/System",
        ] {
            let err = validate_destructive_path(Path::new(bad)).unwrap_err();
            assert!(
                err.to_string().contains("refusing"),
                "{bad} should be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn destructive_rejects_system_dir_with_trailing_slash_or_dot() {
        for bad in ["/usr/local/", "/usr/./local", "/usr//local"] {
            let err = validate_destructive_path(Path::new(bad)).unwrap_err();
            assert!(
                err.to_string().contains("refusing"),
                "{bad} should be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn destructive_rejects_relative_path() {
        let err = validate_destructive_path(Path::new("zbrew/store")).unwrap_err();
        assert!(err.to_string().contains("relative"), "{err}");
    }

    #[test]
    fn destructive_rejects_shallow_path() {
        let err = validate_destructive_path(Path::new("/zbrew")).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    #[test]
    fn destructive_rejects_home_dir() {
        let home = Path::new("/home/someuser");
        let err = validate_destructive_path_with_home(home, Some(home)).unwrap_err();
        assert!(err.to_string().contains("home directory"), "{err}");
    }

    #[test]
    fn destructive_allows_subdir_of_home() {
        let home = Path::new("/home/someuser");
        assert!(
            validate_destructive_path_with_home(
                Path::new("/home/someuser/.local/share/zbrew"),
                Some(home)
            )
            .is_ok()
        );
    }

    #[test]
    fn destructive_accepts_normal_zbrew_roots() {
        for good in [
            "/opt/zbrew",
            "/opt/zbrew/prefix",
            "/home/someuser/.local/share/zbrew",
        ] {
            assert!(
                validate_destructive_path_with_home(Path::new(good), None).is_ok(),
                "{good} should be accepted"
            );
        }
    }

    #[test]
    fn destructive_still_rejects_traversal() {
        let err = validate_destructive_path(Path::new("/opt/../etc/shadow")).unwrap_err();
        assert!(err.to_string().contains("'..'"), "{err}");
    }
}
