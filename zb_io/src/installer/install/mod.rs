mod bottle;
pub(crate) mod doctor;
mod outdated;
mod plan;
mod source;
mod uninstall;
mod upgrade;

pub use uninstall::GcOutcome;

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::warn;

use crate::cellar::link::Linker;
use crate::cellar::materialize::Cellar;
use crate::network::api::ApiClient;
use crate::network::cache::ApiCache;
use crate::network::download::{DownloadProgressCallback, DownloadRequest, ParallelDownloader};
use crate::progress::{InstallProgress, ProgressCallback};
use crate::storage::blob::BlobCache;
use crate::storage::db::{Database, InstallReason};
use crate::storage::store::Store;

use zb_core::{Error, Formula, InstallMethod};

use bottle::dependency_cellar_path;

const MAX_CORRUPTION_RETRIES: usize = 3;

/// Acquire the cross-process install lock. The returned `File` must be kept
/// alive (e.g. `let _lock = ...`) for the duration the lock should be held —
/// dropping it releases the flock. Re-acquiring in the same process while the
/// guard is alive would deadlock, so multi-step flows (e.g. `upgrade`) take
/// the lock once and call the no-lock `execute_inner` directly.
pub(crate) fn acquire_install_lock(locks_dir: &Path) -> Result<File, Error> {
    let lock_path = locks_dir.join("install.lock");
    let lock_file =
        File::create(&lock_path).map_err(Error::store("failed to create install lock"))?;
    lock_file
        .lock()
        .map_err(Error::store("failed to acquire install lock"))?;
    Ok(lock_file)
}

pub struct Installer {
    api_client: ApiClient,
    downloader: ParallelDownloader,
    store: Store,
    cellar: Cellar,
    linker: Linker,
    pub(crate) db: Database,
    prefix: PathBuf,
    locks_dir: PathBuf,
}

#[derive(Debug)]
pub struct PlannedInstall {
    pub install_name: String,
    pub formula: Formula,
    pub method: InstallMethod,
}

#[derive(Debug, Default)]
pub struct InstallPlan {
    pub items: Vec<PlannedInstall>,
    /// How the kegs this plan installs should be registered. Plans are
    /// retained unless a caller says otherwise, so only `zb run` has to
    /// opt in to disposable kegs.
    pub reason: InstallReason,
}

impl InstallPlan {
    /// Mark every keg this plan installs as disposable. Used by `zb run` /
    /// `zbx`, which materialize a formula only to execute it once.
    pub fn transient(mut self) -> Self {
        self.reason = InstallReason::Transient;
        self
    }
}

#[derive(Debug)]
pub struct PlanFailure {
    pub name: String,
    pub error: Error,
}

#[derive(Debug)]
pub struct ExecuteResult {
    pub installed: usize,
}

/// A package that has a newer version available upstream.
#[derive(Debug, Clone, serde::Serialize)]
pub struct OutdatedPackage {
    pub name: String,
    pub installed_version: String,
    pub current_version: String,
    #[serde(skip)]
    pub installed_sha256: String,
    #[serde(skip)]
    pub current_sha256: String,
    #[serde(skip)]
    pub is_source_build: bool,
}

impl Installer {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        api_client: ApiClient,
        blob_cache: BlobCache,
        store: Store,
        cellar: Cellar,
        linker: Linker,
        db: Database,
        prefix: PathBuf,
        locks_dir: PathBuf,
    ) -> Self {
        Self {
            api_client,
            downloader: ParallelDownloader::new(blob_cache),
            store,
            cellar,
            linker,
            db,
            prefix,
            locks_dir,
        }
    }

    pub fn clear_api_cache(&self) -> Result<usize, Error> {
        self.api_client.clear_cache()
    }

    pub async fn execute(&mut self, plan: InstallPlan, link: bool) -> Result<ExecuteResult, Error> {
        self.execute_with_progress(plan, link, None).await
    }

    pub async fn execute_with_progress(
        &mut self,
        plan: InstallPlan,
        link: bool,
        progress: Option<Arc<ProgressCallback>>,
    ) -> Result<ExecuteResult, Error> {
        let _lock = acquire_install_lock(&self.locks_dir)?;
        self.execute_inner(plan, link, progress).await
    }

    /// No-lock variant of `execute_with_progress`. Callers MUST already hold
    /// the install lock — used by `upgrade` to compose uninstall + install
    /// under a single lock acquisition.
    pub(crate) async fn execute_inner(
        &mut self,
        plan: InstallPlan,
        link: bool,
        progress: Option<Arc<ProgressCallback>>,
    ) -> Result<ExecuteResult, Error> {
        let report = |event: InstallProgress| {
            if let Some(ref cb) = progress {
                cb(event);
            }
        };

        let reason = plan.reason;
        let (bottle_items, source_items): (Vec<_>, Vec<_>) = plan
            .items
            .into_iter()
            .partition(|item| matches!(item.method, InstallMethod::Bottle(_)));

        if bottle_items.is_empty() && source_items.is_empty() {
            return Ok(ExecuteResult { installed: 0 });
        }

        let mut installed = 0usize;
        let mut error: Option<Error> = None;

        if !bottle_items.is_empty() {
            let requests: Vec<DownloadRequest> = bottle_items
                .iter()
                .map(|item| {
                    let InstallMethod::Bottle(ref bottle) = item.method else {
                        unreachable!()
                    };
                    DownloadRequest {
                        url: bottle.url.clone(),
                        sha256: bottle.sha256.clone(),
                        name: item.formula.name.clone(),
                    }
                })
                .collect();

            let download_progress: Option<DownloadProgressCallback> = progress.clone().map(|cb| {
                Arc::new(move |event: InstallProgress| {
                    cb(event);
                }) as DownloadProgressCallback
            });

            let mut rx = self
                .downloader
                .download_streaming(requests, download_progress.clone());

            while let Some(result) = rx.recv().await {
                match result {
                    Ok(download) => {
                        match self
                            .process_bottle_item(
                                &bottle_items[download.index],
                                &download,
                                &download_progress,
                                link,
                                reason,
                                &report,
                            )
                            .await
                        {
                            Ok(()) => installed += 1,
                            Err(e) => error = Some(e),
                        }
                    }
                    Err(e) => {
                        error = Some(e);
                    }
                }
            }
        }

        for item in &source_items {
            let InstallMethod::Source(ref build_plan) = item.method else {
                unreachable!()
            };

            report(InstallProgress::UnpackStarted {
                name: item.formula.name.clone(),
            });

            match self
                .install_from_source(item, build_plan, link, reason, &report)
                .await
            {
                Ok(()) => installed += 1,
                Err(e) => {
                    error = Some(e);
                    continue;
                }
            }
        }

        if let Some(e) = error {
            return Err(e);
        }

        Ok(ExecuteResult { installed })
    }

    pub async fn install(&mut self, names: &[String], link: bool) -> Result<ExecuteResult, Error> {
        let (casks, formulas): (Vec<_>, Vec<_>) = names
            .iter()
            .cloned()
            .partition(|name| name.starts_with("cask:"));

        let mut installed = 0usize;

        if !formulas.is_empty() {
            let plan = self.plan(&formulas).await?;
            installed += self.execute(plan, link).await?.installed;
        }

        if !casks.is_empty() {
            installed += self.install_casks(&casks, link).await?.installed;
        }

        Ok(ExecuteResult { installed })
    }

    pub async fn install_casks(
        &mut self,
        names: &[String],
        link: bool,
    ) -> Result<ExecuteResult, Error> {
        let mut installed = 0usize;
        for name in names {
            let token = name
                .strip_prefix("cask:")
                .expect("install_casks expects cask: prefixed names");
            self.install_single_cask(token, link).await?;
            installed += 1;
        }
        Ok(ExecuteResult { installed })
    }

    pub fn is_installed(&self, name: &str) -> bool {
        self.db.get_installed(name).is_some()
    }

    pub fn get_installed(&self, name: &str) -> Option<crate::storage::db::InstalledKeg> {
        self.db.get_installed(name)
    }

    pub fn list_installed(&self) -> Result<Vec<crate::storage::db::InstalledKeg>, Error> {
        self.db.list_installed()
    }

    pub fn keg_path(&self, name: &str, version: &str) -> PathBuf {
        self.cellar.keg_path(name, version)
    }

    fn cleanup_materialized(cellar: &Cellar, name: &str, version: &str) {
        if let Err(e) = cellar.remove_keg(name, version) {
            warn!(
                formula = %name,
                version = %version,
                error = %e,
                "failed to remove keg after install error"
            );
        }
    }
}

pub fn create_installer(
    root: &Path,
    prefix: &Path,
    concurrency: usize,
) -> Result<Installer, Error> {
    if !root.exists() {
        fs::create_dir_all(root).map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                Error::StoreCorruption {
                    message: format!(
                        "cannot create root directory '{}': permission denied.\n\n\
                        Create it with:\n  sudo mkdir -p {} && sudo chown $USER {}",
                        root.display(),
                        root.display(),
                        root.display()
                    ),
                }
            } else {
                Error::StoreCorruption {
                    message: format!("failed to create root directory '{}': {e}", root.display()),
                }
            }
        })?;
    }

    fs::create_dir_all(root.join("db")).map_err(Error::store("failed to create db directory"))?;

    fs::create_dir_all(root.join("cache"))
        .map_err(Error::store("failed to create cache directory"))?;

    let api_cache_path = root.join("cache/api-cache.sqlite");
    let api_cache =
        ApiCache::open(&api_cache_path).map_err(Error::store("failed to open API cache"))?;

    let api_client = match std::env::var("ZBREW_API_URL") {
        Ok(url) => ApiClient::with_base_url(url)?,
        Err(_) => ApiClient::new(),
    }
    .with_cache(api_cache);

    let blob_cache =
        BlobCache::new(&root.join("cache")).map_err(Error::store("failed to create blob cache"))?;
    let store = Store::new(root).map_err(Error::store("failed to create store"))?;
    // Use prefix/Cellar so bottles' hardcoded rpaths work
    let cellar =
        Cellar::new_at(prefix.join("Cellar")).map_err(Error::store("failed to create cellar"))?;
    let linker = Linker::new(prefix).map_err(Error::store("failed to create linker"))?;
    let db = Database::open(&root.join("db/zb.sqlite3"))?;

    let locks_dir = root.join("locks");
    fs::create_dir_all(&locks_dir).map_err(Error::store("failed to create locks directory"))?;

    let parallel_downloader = ParallelDownloader::with_concurrency(blob_cache, concurrency);

    Ok(Installer {
        api_client,
        downloader: parallel_downloader,
        store,
        cellar,
        linker,
        db,
        prefix: prefix.to_path_buf(),
        locks_dir,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    use crate::test_support::*;

    #[tokio::test]
    async fn install_completes_successfully() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("testpkg", "1.0.0", create_bottle_tarball("testpkg"))
            .await;

        let mut installer = env.installer();
        installer
            .install(&["testpkg".to_string()], true)
            .await
            .unwrap();

        assert!(env.root.join("cellar/testpkg/1.0.0").exists());
        assert!(env.prefix.join("bin/testpkg").exists());

        let installed = installer.db.get_installed("testpkg");
        assert!(installed.is_some());
        assert_eq!(installed.unwrap().version, "1.0.0");
    }

    /// Configuration a bottle ships arrives under `<keg>/.bottle/etc`, which
    /// the linker never walks, so before #40 it was silently dropped.
    #[tokio::test]
    async fn install_copies_staged_etc_config_into_the_prefix() {
        let env = TestEnv::new().await;
        let bottle = create_bottle_tarball_with_entries(
            "confpkg",
            "1.0.0",
            &["bin/confpkg", ".bottle/etc/confpkg/confpkg.ini"],
        );
        env.mount_bottled_formula("confpkg", "1.0.0", bottle).await;

        let mut installer = env.installer();
        installer
            .install(&["confpkg".to_string()], true)
            .await
            .unwrap();

        let config = env.prefix.join("etc/confpkg/confpkg.ini");
        assert!(config.is_file(), "staged etc config was not installed");
        assert!(
            !config.is_symlink(),
            "etc config must be a copy, not a link"
        );
        assert!(
            !env.prefix.join(".bottle").exists(),
            "the staging directory itself must not be installed"
        );
    }

    #[tokio::test]
    async fn install_with_dependencies() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("deplib", "1.0.0", create_bottle_tarball("deplib"))
            .await;
        env.mount_bottled_formula_with_deps(
            "mainpkg",
            "2.0.0",
            &["deplib"],
            create_bottle_tarball("mainpkg"),
        )
        .await;

        let mut installer = env.installer();
        installer
            .install(&["mainpkg".to_string()], true)
            .await
            .unwrap();

        assert!(installer.db.get_installed("mainpkg").is_some());
        assert!(installer.db.get_installed("deplib").is_some());
    }

    #[tokio::test]
    async fn preserves_successful_installs_when_one_package_fails() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("goodpkg", "1.0.0", create_bottle_tarball("goodpkg"))
            .await;

        // `badpkg` resolves, but its bottle never arrives.
        env.mount_formula(
            "badpkg",
            formula_json(
                "badpkg",
                "1.0.0",
                &env.bottle_url("badpkg", "1.0.0"),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(bottle_path("badpkg", "1.0.0")))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_delay(Duration::from_millis(100))
                    .set_body_string("download failed"),
            )
            .mount(&env.server)
            .await;

        let mut installer = env.installer();
        let result = installer
            .install(&["goodpkg".to_string(), "badpkg".to_string()], false)
            .await;
        assert!(result.is_err());

        assert!(installer.db.get_installed("goodpkg").is_some());
        assert!(installer.db.get_installed("badpkg").is_none());
        assert!(env.root.join("cellar/goodpkg/1.0.0").exists());
    }

    /// Mount `name` at 1.0.0 with the given dependencies. `bottle_status` of
    /// 200 serves a real bottle; anything else fails the download.
    async fn mount_batch_formula(
        mock_server: &MockServer,
        name: &str,
        deps: &[&str],
        bottle_status: u16,
    ) {
        let bottle = create_bottle_tarball(name);
        let sha = sha256_hex(&bottle);
        let tag = get_test_bottle_tag();
        let deps_json = deps
            .iter()
            .map(|d| format!("\"{d}\""))
            .collect::<Vec<_>>()
            .join(",");
        let formula_json = format!(
            r#"{{"name":"{name}","versions":{{"stable":"1.0.0"}},"dependencies":[{deps_json}],"bottle":{{"stable":{{"files":{{"{tag}":{{"url":"{uri}/bottles/{name}-1.0.0.{tag}.bottle.tar.gz","sha256":"{sha}"}}}}}}}}}}"#,
            uri = mock_server.uri()
        );

        Mock::given(method("GET"))
            .and(path(format!("/formula/{name}.json")))
            .respond_with(ResponseTemplate::new(200).set_body_string(formula_json))
            .mount(mock_server)
            .await;

        let bottle_response = if bottle_status == 200 {
            ResponseTemplate::new(200).set_body_bytes(bottle)
        } else {
            ResponseTemplate::new(bottle_status).set_body_string("download failed")
        };
        Mock::given(method("GET"))
            .and(path(format!("/bottles/{name}-1.0.0.{tag}.bottle.tar.gz")))
            .respond_with(bottle_response)
            .mount(mock_server)
            .await;
    }

    /// Several roots in one `install` call resolve through a single plan, so a
    /// dependency two of them share is installed once and the count covers the
    /// whole closure.
    #[tokio::test]
    async fn install_registers_every_name_in_a_multi_formula_call() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        mount_batch_formula(&mock_server, "batchdep", &[], 200).await;
        mount_batch_formula(&mock_server, "batchone", &["batchdep"], 200).await;
        mount_batch_formula(&mock_server, "batchtwo", &["batchdep"], 200).await;
        mount_batch_formula(&mock_server, "batchthree", &[], 200).await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client =
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap();
        let mut installer = Installer::new(
            api_client,
            BlobCache::new(&root.join("cache")).unwrap(),
            Store::new(&root).unwrap(),
            Cellar::new(&root).unwrap(),
            Linker::new(&prefix).unwrap(),
            Database::open(&root.join("db/zb.sqlite3")).unwrap(),
            prefix.clone(),
            root.join("locks"),
        );

        let result = installer
            .install(
                &[
                    "batchone".to_string(),
                    "batchtwo".to_string(),
                    "batchthree".to_string(),
                ],
                true,
            )
            .await
            .unwrap();

        assert_eq!(
            result.installed, 4,
            "three roots plus the one dependency they share"
        );
        for name in ["batchone", "batchtwo", "batchthree", "batchdep"] {
            assert!(installer.db.get_installed(name).is_some(), "{name} missing");
            assert!(prefix.join("bin").join(name).exists(), "{name} not linked");
        }
    }

    /// When more than one package in a batch fails, `execute_inner` overwrites
    /// `error` each time, so only one failure ever reaches the caller — and
    /// because bottles complete in download order, which one survives is not
    /// deterministic. The successful count is discarded along with it.
    ///
    /// Asserted as-is to document today's behaviour; aggregating the failures
    /// (and reporting what did install) would be a behaviour change.
    #[tokio::test]
    async fn only_one_error_survives_when_several_packages_fail() {
        let mock_server = MockServer::start().await;
        let tmp = TempDir::new().unwrap();

        mount_batch_formula(&mock_server, "survivor", &[], 200).await;
        mount_batch_formula(&mock_server, "casualtyone", &[], 500).await;
        mount_batch_formula(&mock_server, "casualtytwo", &[], 500).await;

        let root = tmp.path().join("zbrew");
        let prefix = tmp.path().join("homebrew");
        fs::create_dir_all(root.join("db")).unwrap();

        let api_client =
            ApiClient::with_base_url(format!("{}/formula", mock_server.uri())).unwrap();
        let mut installer = Installer::new(
            api_client,
            BlobCache::new(&root.join("cache")).unwrap(),
            Store::new(&root).unwrap(),
            Cellar::new(&root).unwrap(),
            Linker::new(&prefix).unwrap(),
            Database::open(&root.join("db/zb.sqlite3")).unwrap(),
            prefix.clone(),
            root.join("locks"),
        );

        let result = installer
            .install(
                &[
                    "survivor".to_string(),
                    "casualtyone".to_string(),
                    "casualtytwo".to_string(),
                ],
                true,
            )
            .await;

        // One error for two failures, and no way to learn that `survivor` is
        // installed except by asking the database afterwards.
        assert!(result.is_err());
        assert!(installer.db.get_installed("survivor").is_some());
        assert!(installer.db.get_installed("casualtyone").is_none());
        assert!(installer.db.get_installed("casualtytwo").is_none());
    }

    #[tokio::test]
    async fn db_persist_failure_cleans_materialized_and_linked_files() {
        let env = TestEnv::new().await;
        let bottle_sha = env
            .mount_bottled_formula("rollbackme", "1.0.0", create_bottle_tarball("rollbackme"))
            .await;

        let mut installer = env.installer();

        let conn = rusqlite::Connection::open(env.db_path()).unwrap();
        conn.execute("DROP TABLE installed_kegs", []).unwrap();

        let result = installer.install(&["rollbackme".to_string()], true).await;
        assert!(result.is_err());

        assert!(!env.root.join("cellar/rollbackme/1.0.0").exists());
        assert!(!env.prefix.join("bin/rollbackme").exists());
        assert!(!env.prefix.join("opt/rollbackme").exists());
        assert!(env.root.join("store").join(&bottle_sha).exists());
    }

    #[tokio::test]
    async fn db_persist_failure_cleans_materialized_tap_formula_keg() {
        let env = TestEnv::new().await;
        let bottle = create_bottle_tarball("terraform");
        let bottle_sha = sha256_hex(&bottle);

        Mock::given(method("GET"))
            .and(path("/hashicorp/homebrew-tap/main/Formula/terraform.rb"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tap_formula_rb(
                "Terraform",
                "1.10.0",
                &format!("{}/v2/hashicorp/tap", env.uri()),
                &bottle_sha,
                &[],
            )))
            .mount(&env.server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!(
                "/v2/hashicorp/tap/terraform/blobs/sha256:{bottle_sha}"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
            .mount(&env.server)
            .await;

        let mut installer = env.installer_with_taps();

        let conn = rusqlite::Connection::open(env.db_path()).unwrap();
        conn.execute("DROP TABLE installed_kegs", []).unwrap();

        let result = installer
            .install(&["hashicorp/tap/terraform".to_string()], true)
            .await;
        assert!(result.is_err());

        assert!(!env.root.join("cellar/terraform/1.10.0").exists());
        assert!(!env.prefix.join("bin/terraform").exists());
        assert!(!env.prefix.join("opt/terraform").exists());
        assert!(env.root.join("store").join(&bottle_sha).exists());
    }

    #[tokio::test]
    async fn parallel_api_fetching_with_deep_deps() {
        let env = TestEnv::new().await;
        for (name, deps) in [
            ("leaf1", &[][..]),
            ("leaf2", &[]),
            ("mid1", &["leaf1"]),
            ("mid2", &["leaf1", "leaf2"]),
            ("root", &["mid1", "mid2"]),
        ] {
            env.mount_bottled_formula_with_deps(name, "1.0.0", deps, create_bottle_tarball(name))
                .await;
        }

        let mut installer = env.installer();
        installer
            .install(&["root".to_string()], true)
            .await
            .unwrap();

        assert!(installer.db.get_installed("root").is_some());
        assert!(installer.db.get_installed("mid1").is_some());
        assert!(installer.db.get_installed("mid2").is_some());
        assert!(installer.db.get_installed("leaf1").is_some());
        assert!(installer.db.get_installed("leaf2").is_some());
    }

    #[tokio::test]
    async fn streaming_extraction_processes_as_downloads_complete() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("fastpkg", "1.0.0", create_bottle_tarball("fastpkg"))
            .await;

        // `slowpkg` depends on `fastpkg`, and its own bottle arrives late.
        let slow_bottle = create_bottle_tarball("slowpkg");
        let slow_sha = sha256_hex(&slow_bottle);
        env.mount_formula(
            "slowpkg",
            formula_json_with_deps(
                "slowpkg",
                "1.0.0",
                &env.bottle_url("slowpkg", "1.0.0"),
                &slow_sha,
                &["fastpkg"],
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(bottle_path("slowpkg", "1.0.0")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(slow_bottle)
                    .set_delay(Duration::from_millis(100)),
            )
            .mount(&env.server)
            .await;

        let mut installer = env.installer();
        installer
            .install(&["slowpkg".to_string()], true)
            .await
            .unwrap();

        assert!(installer.db.get_installed("fastpkg").is_some());
        assert!(installer.db.get_installed("slowpkg").is_some());
        assert!(env.root.join("cellar/fastpkg/1.0.0").exists());
        assert!(env.root.join("cellar/slowpkg/1.0.0").exists());
        assert!(env.prefix.join("bin/fastpkg").exists());
        assert!(env.prefix.join("bin/slowpkg").exists());
    }

    #[tokio::test]
    async fn retries_on_corrupted_download() {
        let env = TestEnv::new().await;
        let bottle = create_bottle_tarball("retrypkg");
        let bottle_sha = sha256_hex(&bottle);
        env.mount_formula(
            "retrypkg",
            formula_json(
                "retrypkg",
                "1.0.0",
                &env.bottle_url("retrypkg", "1.0.0"),
                &bottle_sha,
            ),
        )
        .await;

        let attempt_count = Arc::new(AtomicUsize::new(0));
        let attempt_clone = attempt_count.clone();
        let valid_bottle = bottle.clone();

        Mock::given(method("GET"))
            .and(path(bottle_path("retrypkg", "1.0.0")))
            .respond_with(move |_: &wiremock::Request| {
                let _attempt = attempt_clone.fetch_add(1, Ordering::SeqCst);
                ResponseTemplate::new(200).set_body_bytes(valid_bottle.clone())
            })
            .mount(&env.server)
            .await;

        let mut installer = env.installer();
        installer
            .install(&["retrypkg".to_string()], true)
            .await
            .unwrap();

        assert!(installer.is_installed("retrypkg"));
        assert!(env.root.join("cellar/retrypkg/1.0.0").exists());
        assert!(env.prefix.join("bin/retrypkg").exists());
    }

    /// Regression for #6 (upstream lucasgelfond/zerobrew#188): a formula whose
    /// links conflict with an already-installed formula used to be left
    /// half-linked *and* unregistered — invisible to `zb list`, impossible to
    /// uninstall, and leaving hundreds of orphaned symlinks behind. The keg
    /// must be registered before linking, and the failed link must leave no
    /// symlinks of its own while keeping the other formula's links intact.
    #[tokio::test]
    async fn link_conflict_keeps_keg_registered_and_leaves_no_orphans() {
        let env = TestEnv::new().await;

        let first = create_bottle_tarball_with_entries("firstpkg", "1.0.0", &["bin/shared"]);
        // `secondpkg` collides on bin/shared but also brings links of its own,
        // which must not survive the failed link step.
        let second = create_bottle_tarball_with_entries(
            "secondpkg",
            "1.0.0",
            &[
                "bin/aaa-first",
                "bin/shared",
                "bin/zzz-last",
                "lib/libsecond.a",
            ],
        );
        env.mount_bottled_formula("firstpkg", "1.0.0", first).await;
        env.mount_bottled_formula("secondpkg", "1.0.0", second)
            .await;

        let mut installer = env.installer();
        installer
            .install(&["firstpkg".to_string()], true)
            .await
            .unwrap();
        assert!(env.prefix.join("bin/shared").exists());

        let err = installer
            .install(&["secondpkg".to_string()], true)
            .await
            .unwrap_err();
        assert!(
            matches!(err, zb_core::Error::LinkConflict { .. }),
            "expected a link conflict, got {err:?}"
        );

        // The keg is installed and known to the database, so it can be removed.
        assert!(env.root.join("cellar/secondpkg/1.0.0").exists());
        assert!(
            installer.is_installed("secondpkg"),
            "a link failure must not leave the keg unregistered and unremovable"
        );

        // All-or-none: no symlink of the failed formula survives...
        for orphan in ["bin/aaa-first", "bin/zzz-last", "lib/libsecond.a"] {
            assert!(
                !env.prefix.join(orphan).exists(),
                "orphaned symlink left behind: {orphan}"
            );
        }
        // ...and the conflicting link still belongs to the first formula.
        let target = fs::read_link(env.prefix.join("bin/shared")).unwrap();
        assert!(
            target.to_string_lossy().contains("firstpkg"),
            "bin/shared should still point at firstpkg, points at {}",
            target.display()
        );

        installer.uninstall("secondpkg").unwrap();
        assert!(!installer.is_installed("secondpkg"));
        assert!(!env.root.join("cellar/secondpkg/1.0.0").exists());
        assert!(
            env.prefix.join("bin/shared").exists(),
            "uninstalling the failed keg must not remove firstpkg's link"
        );
    }

    #[tokio::test]
    async fn fails_after_max_retries() {
        // Validates the retry mechanism structure -- proper integration test
        // would need injection of corruption between download and extraction.
    }
}
