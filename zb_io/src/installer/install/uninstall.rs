use std::fs;
use std::path::Path;

use tracing::debug;
use zb_core::{Error, formula_token};

use super::Installer;
use crate::storage::db::Database;

/// What a `gc` run reclaimed.
#[derive(Debug, Default)]
pub struct GcOutcome {
    /// Names of transient kegs that were uninstalled.
    pub removed_packages: Vec<String>,
    /// Store entries deleted because nothing references them any more.
    pub removed_store_keys: Vec<String>,
}

impl GcOutcome {
    pub fn is_empty(&self) -> bool {
        self.removed_packages.is_empty() && self.removed_store_keys.is_empty()
    }
}

impl Installer {
    pub fn uninstall(&mut self, name: &str) -> Result<(), Error> {
        let installed = self.db.get_installed(name).ok_or(Error::NotInstalled {
            name: name.to_string(),
        })?;
        self.uninstall_by_version(name, &installed.version)
    }

    pub fn uninstall_by_version(&mut self, name: &str, version: &str) -> Result<(), Error> {
        let keg_name = formula_token(name);

        let keg_path = self.cellar.keg_path(keg_name, version);
        self.linker.unlink_keg(&keg_path)?;

        // `unlink_keg` discovers what to remove by walking the keg, so if the
        // keg files were deleted by hand it finds nothing and the prefix keeps
        // dangling symlinks. `record_uninstall` below drops the `keg_files`
        // rows, after which nothing (not even `zb doctor`) can attribute those
        // symlinks to a package, so they would linger forever and every shell
        // lookup of them fails with ENOENT. Clean them up while we still know
        // which links belong to this keg. See issue #14.
        remove_dangling_links(&self.db, name, version, &keg_path);

        {
            let tx = self.db.transaction()?;
            tx.record_uninstall(name)?;
            tx.commit()?;
        }

        self.cellar.remove_keg(keg_name, version)?;

        Ok(())
    }

    /// Reclaim disposable state: first the kegs `zb run`/`zbx` materialized
    /// to execute a command once, then every store entry left unreferenced.
    ///
    /// Uninstalling the transient kegs before sweeping the store is what
    /// makes the reclaim complete -- their store entries only drop to zero
    /// references once the keg rows are gone, so a sweep-only gc would leave
    /// both the cellar directories and their blobs behind.
    ///
    /// Only kegs still marked transient are removed. Anything a `zb install`
    /// has since depended on was promoted to retained when that install
    /// re-recorded its dependency closure, so this cannot delete a package
    /// something the user keeps is linked against.
    pub fn gc(&mut self) -> Result<GcOutcome, Error> {
        let transient: Vec<(String, String)> = self
            .db
            .list_installed()?
            .into_iter()
            .filter(|keg| keg.reason.is_transient())
            .map(|keg| (keg.name, keg.version))
            .collect();

        let mut removed_packages = Vec::new();
        for (name, version) in transient {
            self.uninstall_by_version(&name, &version)?;
            removed_packages.push(name);
        }

        let unreferenced = self.db.get_unreferenced_store_keys()?;
        let mut removed_store_keys = Vec::new();

        for store_key in unreferenced {
            self.store.remove_entry(&store_key)?;
            self.db.delete_store_ref(&store_key)?;
            removed_store_keys.push(store_key);
        }

        Ok(GcOutcome {
            removed_packages,
            removed_store_keys,
        })
    }
}

/// Remove prefix symlinks recorded for `name`/`version` that are now dangling
/// and still point into `keg_path`.
///
/// Only dangling links are touched: a link that still resolves either was
/// already handled by `unlink_keg` or has since been taken over by another
/// formula, and in both cases it must be left alone.
fn remove_dangling_links(db: &Database, name: &str, version: &str, keg_path: &Path) {
    let records = match db.list_keg_files() {
        Ok(records) => records,
        Err(e) => {
            debug!("could not list recorded keg files for {name}: {e}");
            return;
        }
    };

    for record in records {
        if record.name != name || record.version != version {
            continue;
        }

        let link = Path::new(&record.linked_path);
        if !link.is_symlink() || link.exists() {
            continue;
        }

        let Ok(target) = fs::read_link(link) else {
            continue;
        };
        let resolved = if target.is_relative() {
            link.parent().unwrap_or(Path::new("")).join(&target)
        } else {
            target
        };
        if resolved != Path::new(&record.target_path) && !resolved.starts_with(keg_path) {
            continue;
        }

        match fs::remove_file(link) {
            Ok(()) => debug!("removed dangling link {}", link.display()),
            Err(e) => debug!("failed to remove dangling link {}: {e}", link.display()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::cellar::Cellar;
    use crate::installer::install::test_support::*;
    use crate::network::api::ApiClient;
    use crate::storage::blob::BlobCache;
    use crate::storage::db::Database;
    use crate::storage::store::Store;
    use crate::{Installer, Linker};

    #[tokio::test]
    async fn uninstall_cleans_everything() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        let bottle = create_bottle_tarball("uninstallme");
        let bottle_sha = sha256_hex(&bottle);

        let tag = get_test_bottle_tag();
        let formula_json = format!(
            r#"{{
                "name": "uninstallme",
                "versions": {{ "stable": "1.0.0" }},
                "dependencies": [],
                "bottle": {{
                    "stable": {{
                        "files": {{
                            "{}": {{
                                "url": "{}/bottles/uninstallme-1.0.0.{}.bottle.tar.gz",
                                "sha256": "{}"
                            }}
                        }}
                    }}
                }}
            }}"#,
            tag,
            mock_server.uri(),
            tag,
            bottle_sha
        );

        Mock::given(method("GET"))
            .and(path("/formula/uninstallme.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(&formula_json))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/bottles/uninstallme-1.0.0.{}.bottle.tar.gz",
                tag
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle.clone()))
            .mount(&mock_server)
            .await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client =
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap();
        let blob_cache = BlobCache::new(&root.join("cache")).unwrap();
        let store = Store::new(&root).unwrap();
        let cellar = Cellar::new(&root).unwrap();
        let linker = Linker::new(&prefix).unwrap();
        let db = Database::open(&root.join("db/zb.sqlite3")).unwrap();

        let mut installer = Installer::new(
            api_client,
            blob_cache,
            store,
            cellar,
            linker,
            db,
            prefix.clone(),
            root.join("locks"),
        );

        installer
            .install(&["uninstallme".to_string()], true)
            .await
            .unwrap();

        assert!(installer.is_installed("uninstallme"));
        assert!(root.join("cellar/uninstallme/1.0.0").exists());
        assert!(prefix.join("bin/uninstallme").exists());

        installer.uninstall("uninstallme").unwrap();

        assert!(!installer.is_installed("uninstallme"));
        assert!(!root.join("cellar/uninstallme/1.0.0").exists());
        assert!(!prefix.join("bin/uninstallme").exists());
    }

    #[tokio::test]
    async fn gc_removes_unreferenced_store_entries() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        let bottle = create_bottle_tarball("gctest");
        let bottle_sha = sha256_hex(&bottle);

        let tag = get_test_bottle_tag();
        let formula_json = format!(
            r#"{{
                "name": "gctest",
                "versions": {{ "stable": "1.0.0" }},
                "dependencies": [],
                "bottle": {{
                    "stable": {{
                        "files": {{
                            "{}": {{
                                "url": "{}/bottles/gctest-1.0.0.{}.bottle.tar.gz",
                                "sha256": "{}"
                            }}
                        }}
                    }}
                }}
            }}"#,
            tag,
            mock_server.uri(),
            tag,
            bottle_sha
        );

        Mock::given(method("GET"))
            .and(path("/formula/gctest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(&formula_json))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/bottles/gctest-1.0.0.{}.bottle.tar.gz", tag)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle.clone()))
            .mount(&mock_server)
            .await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client =
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap();
        let blob_cache = BlobCache::new(&root.join("cache")).unwrap();
        let store = Store::new(&root).unwrap();
        let cellar = Cellar::new(&root).unwrap();
        let linker = Linker::new(&prefix).unwrap();
        let db = Database::open(&root.join("db/zb.sqlite3")).unwrap();

        let mut installer = Installer::new(
            api_client,
            blob_cache,
            store,
            cellar,
            linker,
            db,
            prefix.clone(),
            root.join("locks"),
        );

        installer
            .install(&["gctest".to_string()], true)
            .await
            .unwrap();

        assert!(root.join("store").join(&bottle_sha).exists());

        installer.uninstall("gctest").unwrap();

        assert!(root.join("store").join(&bottle_sha).exists());

        let removed = installer.gc().unwrap();
        assert!(removed.removed_packages.is_empty());
        assert_eq!(removed.removed_store_keys, vec![bottle_sha.clone()]);

        assert!(!root.join("store").join(&bottle_sha).exists());
        assert!(
            installer
                .db
                .get_unreferenced_store_keys()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn gc_does_not_remove_referenced_store_entries() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        let bottle = create_bottle_tarball("keepme");
        let bottle_sha = sha256_hex(&bottle);

        let tag = get_test_bottle_tag();
        let formula_json = format!(
            r#"{{
                "name": "keepme",
                "versions": {{ "stable": "1.0.0" }},
                "dependencies": [],
                "bottle": {{
                    "stable": {{
                        "files": {{
                            "{}": {{
                                "url": "{}/bottles/keepme-1.0.0.{}.bottle.tar.gz",
                                "sha256": "{}"
                            }}
                        }}
                    }}
                }}
            }}"#,
            tag,
            mock_server.uri(),
            tag,
            bottle_sha
        );

        Mock::given(method("GET"))
            .and(path("/formula/keepme.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(&formula_json))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/bottles/keepme-1.0.0.{}.bottle.tar.gz", tag)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle.clone()))
            .mount(&mock_server)
            .await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client =
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap();
        let blob_cache = BlobCache::new(&root.join("cache")).unwrap();
        let store = Store::new(&root).unwrap();
        let cellar = Cellar::new(&root).unwrap();
        let linker = Linker::new(&prefix).unwrap();
        let db = Database::open(&root.join("db/zb.sqlite3")).unwrap();

        let mut installer = Installer::new(
            api_client,
            blob_cache,
            store,
            cellar,
            linker,
            db,
            prefix.clone(),
            root.join("locks"),
        );

        installer
            .install(&["keepme".to_string()], true)
            .await
            .unwrap();

        assert!(root.join("store").join(&bottle_sha).exists());

        let removed = installer.gc().unwrap();
        assert!(removed.is_empty());

        assert!(root.join("store").join(&bottle_sha).exists());
    }

    #[tokio::test]
    async fn uninstall_accepts_full_tap_reference_after_install() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        let bottle = create_bottle_tarball("terraform");
        let sha = sha256_hex(&bottle);
        let tag = get_test_bottle_tag();

        let tap_formula_rb = format!(
            r#"
class Terraform < Formula
  version "1.10.0"
  bottle do
    root_url "{}/v2/hashicorp/tap"
    sha256 {}: "{}"
  end
end
"#,
            mock_server.uri(),
            tag,
            sha
        );

        Mock::given(method("GET"))
            .and(path("/hashicorp/homebrew-tap/main/Formula/terraform.rb"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tap_formula_rb))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/v2/hashicorp/tap/terraform/blobs/sha256:{sha}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
            .mount(&mock_server)
            .await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client = ApiClient::with_base_url(format!("{}/formula", mock_server.uri()))
            .unwrap()
            .with_tap_raw_base_url(mock_server.uri());
        let blob_cache = BlobCache::new(&root.join("cache")).unwrap();
        let store = Store::new(&root).unwrap();
        let cellar = Cellar::new(&root).unwrap();
        let linker = Linker::new(&prefix).unwrap();
        let db = Database::open(&root.join("db/zb.sqlite3")).unwrap();

        let mut installer = Installer::new(
            api_client,
            blob_cache,
            store,
            cellar,
            linker,
            db,
            prefix.to_path_buf(),
            root.join("locks"),
        );

        installer
            .install(&["hashicorp/tap/terraform".to_string()], true)
            .await
            .unwrap();

        assert!(installer.is_installed("hashicorp/tap/terraform"));
        assert!(!installer.is_installed("terraform"));
        assert!(root.join("cellar/terraform/1.10.0").exists());
        installer.uninstall("hashicorp/tap/terraform").unwrap();
        assert!(!installer.is_installed("hashicorp/tap/terraform"));
        assert!(!root.join("cellar/terraform/1.10.0").exists());
    }

    #[tokio::test]
    async fn uninstalling_non_installed_tap_ref_does_not_remove_core_formula() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        let bottle = create_bottle_tarball("terraform");
        let sha = sha256_hex(&bottle);
        let tag = get_test_bottle_tag();
        let core_json = format!(
            r#"{{
                "name": "terraform",
                "versions": {{ "stable": "1.10.0" }},
                "dependencies": [],
                "bottle": {{
                    "stable": {{
                        "files": {{
                            "{}": {{
                                "url": "{}/bottles/terraform-1.10.0.{}.bottle.tar.gz",
                                "sha256": "{}"
                            }}
                        }}
                    }}
                }}
            }}"#,
            tag,
            mock_server.uri(),
            tag,
            sha
        );

        Mock::given(method("GET"))
            .and(path("/formula/terraform.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(core_json))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/bottles/terraform-1.10.0.{}.bottle.tar.gz",
                tag
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
            .mount(&mock_server)
            .await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client =
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap();
        let blob_cache = BlobCache::new(&root.join("cache")).unwrap();
        let store = Store::new(&root).unwrap();
        let cellar = Cellar::new(&root).unwrap();
        let linker = Linker::new(&prefix).unwrap();
        let db = Database::open(&root.join("db/zb.sqlite3")).unwrap();

        let mut installer = Installer::new(
            api_client,
            blob_cache,
            store,
            cellar,
            linker,
            db,
            prefix.to_path_buf(),
            root.join("locks"),
        );
        installer
            .install(&["terraform".to_string()], true)
            .await
            .unwrap();
        assert!(installer.is_installed("terraform"));

        let err = installer.uninstall("hashicorp/tap/terraform").unwrap_err();
        assert!(matches!(err, zb_core::Error::NotInstalled { .. }));
        assert!(installer.is_installed("terraform"));
    }

    /// Install `ghost` 1.0.0 from a mock server and return the installer plus
    /// the `(root, prefix)` paths it was built with.
    async fn install_ghost(
        mock_server: &MockServer,
        tmp: &TempDir,
    ) -> (Installer, std::path::PathBuf, std::path::PathBuf) {
        let bottle = create_bottle_tarball("ghost");
        let bottle_sha = sha256_hex(&bottle);
        let tag = get_test_bottle_tag();
        let formula_json = format!(
            r#"{{
                "name": "ghost",
                "versions": {{ "stable": "1.0.0" }},
                "dependencies": [],
                "bottle": {{
                    "stable": {{
                        "files": {{
                            "{}": {{
                                "url": "{}/bottles/ghost-1.0.0.{}.bottle.tar.gz",
                                "sha256": "{}"
                            }}
                        }}
                    }}
                }}
            }}"#,
            tag,
            mock_server.uri(),
            tag,
            bottle_sha
        );

        Mock::given(method("GET"))
            .and(path("/formula/ghost.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(&formula_json))
            .mount(mock_server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/bottles/ghost-1.0.0.{}.bottle.tar.gz", tag)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
            .mount(mock_server)
            .await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client =
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap();
        let blob_cache = BlobCache::new(&root.join("cache")).unwrap();
        let store = Store::new(&root).unwrap();
        let cellar = Cellar::new(&root).unwrap();
        let linker = Linker::new(&prefix).unwrap();
        let db = Database::open(&root.join("db/zb.sqlite3")).unwrap();

        let mut installer = Installer::new(
            api_client,
            blob_cache,
            store,
            cellar,
            linker,
            db,
            prefix.clone(),
            root.join("locks"),
        );

        installer
            .install(&["ghost".to_string()], true)
            .await
            .unwrap();
        assert!(installer.is_installed("ghost"));

        (installer, root, prefix)
    }

    /// Regression test for issue #14: uninstalling a package whose keg files
    /// were deleted by hand must not leave dangling symlinks in the prefix.
    #[tokio::test]
    async fn uninstall_removes_dangling_links_when_keg_files_are_gone() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let (mut installer, root, prefix) = install_ghost(&mock_server, &tmp).await;

        // Simulate the user manually deleting the keg files (`rm -rf` on the
        // cellar directory) while the prefix symlinks remain.
        fs::remove_dir_all(root.join("cellar/ghost/1.0.0")).unwrap();
        let link = prefix.join("bin/ghost");
        assert!(link.is_symlink(), "precondition: link still present");
        assert!(!link.exists(), "precondition: link is dangling");

        installer.uninstall("ghost").unwrap();

        assert!(!installer.is_installed("ghost"));
        assert!(
            link.symlink_metadata().is_err(),
            "dangling symlink left behind at {}",
            link.display()
        );
    }

    /// A recorded link that has since been taken over by another package must
    /// survive the uninstall, even when our own keg files are gone.
    #[tokio::test]
    async fn uninstall_keeps_recorded_link_owned_by_someone_else() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let (mut installer, root, prefix) = install_ghost(&mock_server, &tmp).await;

        fs::remove_dir_all(root.join("cellar/ghost/1.0.0")).unwrap();

        // Another formula relinked `bin/ghost` at its own, still-present file.
        let other = tmp.path().join("other-bin-ghost");
        fs::write(&other, b"#!/bin/sh\n").unwrap();
        let link = prefix.join("bin/ghost");
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&other, &link).unwrap();

        installer.uninstall("ghost").unwrap();

        assert!(!installer.is_installed("ghost"));
        assert!(link.is_symlink(), "live foreign link was removed");
        assert_eq!(fs::read_link(&link).unwrap(), other);
    }
    /// Mount a formula and its bottle on `mock_server`.
    async fn mount_formula(
        mock_server: &MockServer,
        name: &str,
        deps: &[&str],
        bottle: &[u8],
    ) -> String {
        let sha = sha256_hex(bottle);
        let tag = get_test_bottle_tag();
        let deps_json = deps
            .iter()
            .map(|d| format!("\"{d}\""))
            .collect::<Vec<_>>()
            .join(",");
        let formula_json = format!(
            r#"{{"name":"{name}","versions":{{"stable":"1.0.0"}},"dependencies":[{deps_json}],"bottle":{{"stable":{{"files":{{"{tag}":{{"url":"{uri}/bottles/{name}.tar.gz","sha256":"{sha}"}}}}}}}}}}"#,
            uri = mock_server.uri()
        );

        Mock::given(method("GET"))
            .and(path(format!("/formula/{name}.json")))
            .respond_with(ResponseTemplate::new(200).set_body_string(formula_json))
            .mount(mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!("/bottles/{name}.tar.gz")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle.to_vec()))
            .mount(mock_server)
            .await;

        sha
    }

    fn installer_at(
        mock_server: &MockServer,
        root: &std::path::Path,
        prefix: &std::path::Path,
    ) -> Installer {
        fs::create_dir_all(root.join("db")).unwrap();
        Installer::new(
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap(),
            BlobCache::new(&root.join("cache")).unwrap(),
            Store::new(root).unwrap(),
            Cellar::new(root).unwrap(),
            Linker::new(prefix).unwrap(),
            Database::open(&root.join("db/zb.sqlite3")).unwrap(),
            prefix.to_path_buf(),
            root.join("locks"),
        )
    }

    /// The reclaim `zb list` hiding transient kegs would otherwise only
    /// paper over: gc has to delete the keg row, the cellar directory and
    /// the store entry. See issue #36.
    #[tokio::test]
    async fn gc_reclaims_transient_kegs() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        let bottle = create_bottle_tarball("gcrun");
        let sha = mount_formula(&mock_server, "gcrun", &[], &bottle).await;

        let mut installer = installer_at(&mock_server, &root, &prefix);
        let plan = installer.plan(&["gcrun".to_string()]).await.unwrap();
        installer.execute(plan.transient(), false).await.unwrap();

        assert!(
            installer
                .get_installed("gcrun")
                .unwrap()
                .reason
                .is_transient()
        );
        assert!(root.join("store").join(&sha).exists());

        let outcome = installer.gc().unwrap();
        assert_eq!(outcome.removed_packages, vec!["gcrun".to_string()]);
        assert_eq!(outcome.removed_store_keys, vec![sha.clone()]);

        assert!(!installer.is_installed("gcrun"));
        assert!(!root.join("store").join(&sha).exists());
        assert!(!root.join("cellar/gcrun/1.0.0").exists());
    }

    /// A dependency a `zbx` run left behind must survive gc once something
    /// the user installed on purpose needs it: `install` re-records its whole
    /// closure, which promotes the transient row to retained.
    #[tokio::test]
    async fn gc_keeps_a_transient_dependency_a_later_install_adopted() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");

        let dep_bottle = create_bottle_tarball("gcdep");
        let app_bottle = create_bottle_tarball("gcapp");
        let dep_sha = mount_formula(&mock_server, "gcdep", &[], &dep_bottle).await;
        mount_formula(&mock_server, "gcapp", &["gcdep"], &app_bottle).await;

        let mut installer = installer_at(&mock_server, &root, &prefix);

        let plan = installer.plan(&["gcdep".to_string()]).await.unwrap();
        installer.execute(plan.transient(), false).await.unwrap();
        assert!(
            installer
                .get_installed("gcdep")
                .unwrap()
                .reason
                .is_transient()
        );

        installer
            .install(&["gcapp".to_string()], false)
            .await
            .unwrap();

        let outcome = installer.gc().unwrap();
        assert!(outcome.removed_packages.is_empty());
        assert!(installer.is_installed("gcdep"));
        assert!(root.join("store").join(&dep_sha).exists());
    }
}
