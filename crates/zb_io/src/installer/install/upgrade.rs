use std::sync::Arc;

use zb_core::{Error, InstallMethod};

use super::{InstallPlan, Installer, acquire_install_lock};
use crate::network::download::{DownloadProgressCallback, DownloadRequest};
use crate::progress::{InstallProgress, ProgressCallback};

impl Installer {
    /// Upgrade an installed package to its latest version.
    ///
    /// Bottle artifacts for the new version are fetched into the blob cache
    /// *before* the old keg is removed, so a download failure leaves the
    /// existing installation intact. Source-built upgrades have no
    /// equivalent pre-build stage and still go through uninstall-first.
    ///
    /// Uninstall-first on the link step is required either way: a fresh
    /// install would hit `LinkConflict` on the old version's symlinks and
    /// leave the old cellar directory behind on disk (the leak this method
    /// exists to fix).
    ///
    /// Returns `Ok(())` when the package is already on its latest version,
    /// `Error::NotInstalled` when there is no existing installation.
    pub async fn upgrade(
        &mut self,
        name: &str,
        build_from_source: bool,
        link: bool,
        progress: Option<Arc<ProgressCallback>>,
    ) -> Result<(), Error> {
        // One lock for the entire flow — uninstall + install must not race
        // with other zb processes touching the same package.
        let _lock = acquire_install_lock(&self.locks_dir)?;

        let old = self.db.get_installed(name).ok_or(Error::NotInstalled {
            name: name.to_string(),
        })?;

        // `plan_with_options` doesn't consult the installed DB, so an
        // empty-plan check wouldn't fire on already-current packages.
        if self.is_outdated(name).await?.is_none() {
            return Ok(());
        }

        let plan = self
            .plan_with_options(&[name.to_string()], build_from_source)
            .await?;

        // Fetch new bottles before touching the old install — a download
        // failure here leaves the existing keg intact.
        self.prefetch_plan_bottles(&plan, progress.clone()).await?;

        self.uninstall_by_version(name, &old.version)?;

        // We already hold the lock, so call the no-lock variant. An upgrade is
        // one package plus its dependencies, so any per-package failure in the
        // batch fails the upgrade — but every one of them is reported.
        let outcome = self.execute_inner(plan, link, progress).await?;
        match outcome.to_error() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Pre-download bottle artifacts in `plan` into the blob cache. No-op
    /// for source-only plans.
    async fn prefetch_plan_bottles(
        &self,
        plan: &InstallPlan,
        progress: Option<Arc<ProgressCallback>>,
    ) -> Result<(), Error> {
        let requests: Vec<DownloadRequest> = plan
            .items
            .iter()
            .filter_map(|item| match &item.method {
                InstallMethod::Bottle(bottle) => Some(DownloadRequest {
                    url: bottle.url.clone(),
                    sha256: bottle.sha256.clone(),
                    name: item.formula.name.clone(),
                }),
                _ => None,
            })
            .collect();

        if requests.is_empty() {
            return Ok(());
        }

        let download_progress: Option<DownloadProgressCallback> = progress.map(|cb| {
            Arc::new(move |event: InstallProgress| {
                cb(event);
            }) as DownloadProgressCallback
        });

        let mut rx = self
            .downloader
            .download_streaming(requests, download_progress);
        while let Some((_, result)) = rx.recv().await {
            result?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    use crate::test_support::*;

    /// Answer `name`'s formula endpoint with `version` exactly once, and serve
    /// that version's bottle. Upgrades need two answers from one endpoint, so
    /// the mocks are mounted one version at a time, newest last.
    async fn mount_version_once(env: &TestEnv, name: &str, version: &str, bottle: Vec<u8>) {
        let sha = sha256_hex(&bottle);
        Mock::given(method("GET"))
            .and(path(format!("/formula/{name}.json")))
            .respond_with(ResponseTemplate::new(200).set_body_string(formula_json(
                name,
                version,
                &env.bottle_url(name, version),
                &sha,
            )))
            .up_to_n_times(1)
            .mount(&env.server)
            .await;
        Mock::given(method("GET"))
            .and(path(bottle_path(name, version)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
            .up_to_n_times(1)
            .mount(&env.server)
            .await;
    }

    #[tokio::test]
    async fn upgrade_replaces_old_version_and_cleans_up() {
        let env = TestEnv::new().await;
        mount_version_once(
            &env,
            "testpkg",
            "1.0.0",
            create_bottle_tarball_with_version("testpkg", "1.0.0"),
        )
        .await;
        env.mount_bottled_formula(
            "testpkg",
            "2.0.0",
            create_bottle_tarball_with_version("testpkg", "2.0.0"),
        )
        .await;

        let mut installer = env.installer();
        installer
            .install(&["testpkg".to_string()], true)
            .await
            .unwrap();
        assert!(env.root.join("cellar/testpkg/1.0.0").exists());
        assert!(env.prefix.join("bin/testpkg").exists());

        installer
            .upgrade("testpkg", false, true, None)
            .await
            .unwrap();

        assert!(env.root.join("cellar/testpkg/2.0.0").exists());
        assert!(
            !env.root.join("cellar/testpkg/1.0.0").exists(),
            "old cellar dir must be removed"
        );

        let bin_link = env.prefix.join("bin/testpkg");
        assert!(bin_link.exists(), "new version must be linked");
        let target = fs::read_link(&bin_link).unwrap();
        let target_str = target.to_string_lossy();
        assert!(
            target_str.contains("2.0.0"),
            "symlink must point at 2.0.0, got {target_str}"
        );
        assert!(
            !target_str.contains("1.0.0"),
            "symlink must not point at 1.0.0"
        );

        let installed = installer.get_installed("testpkg").unwrap();
        assert_eq!(installed.version, "2.0.0");
    }

    #[tokio::test]
    async fn plain_install_over_older_version_relinks_to_new_version() {
        // Regression test for #331 (https://github.com/lucasgelfond/zerobrew/issues/331):
        // `zb install <pkg>` with an older version already installed failed
        // the link step with conflicts "belonging to" the package itself,
        // leaving the DB reporting the new version while bin/<pkg> kept
        // resolving to the old keg.
        let env = TestEnv::new().await;
        mount_version_once(
            &env,
            "relinkpkg",
            "1.0.0",
            create_bottle_tarball_with_version("relinkpkg", "1.0.0"),
        )
        .await;
        env.mount_bottled_formula(
            "relinkpkg",
            "2.0.0",
            create_bottle_tarball_with_version("relinkpkg", "2.0.0"),
        )
        .await;

        let mut installer = env.installer();
        installer
            .install(&["relinkpkg".to_string()], true)
            .await
            .unwrap();
        let bin_link = env.prefix.join("bin/relinkpkg");
        assert!(
            fs::read_link(&bin_link)
                .unwrap()
                .to_string_lossy()
                .contains("1.0.0")
        );

        installer
            .install(&["relinkpkg".to_string()], true)
            .await
            .expect("install over an older version must not fail the link step");

        let target = fs::read_link(&bin_link).unwrap();
        let target_str = target.to_string_lossy();
        assert!(
            target_str.contains("2.0.0"),
            "bin symlink must point at 2.0.0, got {target_str}"
        );
        assert!(bin_link.exists(), "bin symlink must not be dangling");

        let opt_target = fs::read_link(env.prefix.join("opt/relinkpkg")).unwrap();
        assert!(opt_target.to_string_lossy().contains("2.0.0"));

        let installed = installer.get_installed("relinkpkg").unwrap();
        assert_eq!(installed.version, "2.0.0");
    }

    #[tokio::test]
    async fn upgrade_with_no_link_does_not_create_symlinks() {
        let env = TestEnv::new().await;
        mount_version_once(
            &env,
            "nolinkpkg",
            "1.0.0",
            create_bottle_tarball_with_version("nolinkpkg", "1.0.0"),
        )
        .await;
        env.mount_bottled_formula(
            "nolinkpkg",
            "2.0.0",
            create_bottle_tarball_with_version("nolinkpkg", "2.0.0"),
        )
        .await;

        let mut installer = env.installer();
        installer
            .install(&["nolinkpkg".to_string()], true)
            .await
            .unwrap();
        assert!(env.prefix.join("bin/nolinkpkg").exists());

        installer
            .upgrade("nolinkpkg", false, false, None)
            .await
            .unwrap();

        assert!(env.root.join("cellar/nolinkpkg/2.0.0").exists());
        assert!(!env.root.join("cellar/nolinkpkg/1.0.0").exists());
        assert!(
            !env.prefix.join("bin/nolinkpkg").exists(),
            "no symlinks expected when link=false"
        );
    }

    #[tokio::test]
    async fn upgrade_no_op_when_already_latest() {
        let env = TestEnv::new().await;
        let bottle = create_bottle_tarball("steadypkg");
        let sha = sha256_hex(&bottle);

        env.mount_formula(
            "steadypkg",
            formula_json(
                "steadypkg",
                "1.0.0",
                &env.bottle_url("steadypkg", "1.0.0"),
                &sha,
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(bottle_path("steadypkg", "1.0.0")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle))
            .up_to_n_times(1)
            .mount(&env.server)
            .await;

        let mut installer = env.installer();
        installer
            .install(&["steadypkg".to_string()], true)
            .await
            .unwrap();

        installer
            .upgrade("steadypkg", false, true, None)
            .await
            .unwrap();

        assert!(env.root.join("cellar/steadypkg/1.0.0").exists());
        assert!(env.prefix.join("bin/steadypkg").exists());
        assert_eq!(
            installer.get_installed("steadypkg").unwrap().version,
            "1.0.0"
        );
    }

    #[tokio::test]
    async fn upgrade_errors_when_not_installed() {
        let env = TestEnv::new().await;
        let mut installer = env.installer();

        let err = installer
            .upgrade("nonexistent", false, true, None)
            .await
            .unwrap_err();
        assert!(matches!(err, zb_core::Error::NotInstalled { .. }));
    }

    #[tokio::test]
    async fn upgrade_keeps_old_version_when_new_bottle_download_fails() {
        let env = TestEnv::new().await;
        mount_version_once(&env, "flakypkg", "1.0.0", create_bottle_tarball("flakypkg")).await;

        // Plan resolves to 2.0.0 with a valid sha, but the bottle download
        // returns 500 — exercise the pre-fetch failure path.
        env.mount_formula(
            "flakypkg",
            formula_json(
                "flakypkg",
                "2.0.0",
                &env.bottle_url("flakypkg", "2.0.0"),
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(path(bottle_path("flakypkg", "2.0.0")))
            .respond_with(ResponseTemplate::new(500).set_body_string("download failed"))
            .mount(&env.server)
            .await;

        let mut installer = env.installer();
        installer
            .install(&["flakypkg".to_string()], true)
            .await
            .unwrap();
        assert!(env.root.join("cellar/flakypkg/1.0.0").exists());
        let bin_link = env.prefix.join("bin/flakypkg");
        assert!(bin_link.exists());

        let result = installer.upgrade("flakypkg", false, true, None).await;
        assert!(result.is_err(), "upgrade should fail when bottle 500s");

        assert!(
            env.root.join("cellar/flakypkg/1.0.0").exists(),
            "old cellar dir must be preserved on download failure"
        );
        assert!(
            !env.root.join("cellar/flakypkg/2.0.0").exists(),
            "no partial new cellar should exist"
        );
        assert!(bin_link.exists(), "symlink to old version must remain");
        let target = fs::read_link(&bin_link).unwrap();
        assert!(target.to_string_lossy().contains("1.0.0"));
        let installed = installer.get_installed("flakypkg").unwrap();
        assert_eq!(installed.version, "1.0.0");
    }
}
