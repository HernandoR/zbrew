use std::collections::HashMap;

use zb_core::{Error, select_bottle};

use super::{Installer, OutdatedPackage};

impl Installer {
    pub async fn is_outdated(&self, name: &str) -> Result<Option<OutdatedPackage>, Error> {
        let installed = self.db.get_installed(name).ok_or(Error::NotInstalled {
            name: name.to_string(),
        })?;

        let formula = self.api_client.get_formula(name).await?;
        let is_source = installed.store_key.starts_with("source:");

        if is_source {
            let current_version = formula.effective_version();
            if installed.version == current_version {
                Ok(None)
            } else {
                Ok(Some(OutdatedPackage {
                    name: name.to_string(),
                    installed_version: installed.version,
                    installed_sha256: installed.store_key,
                    current_version,
                    current_sha256: String::new(),
                    is_source_build: true,
                }))
            }
        } else {
            let bottle = select_bottle(&formula)?;
            if installed.store_key == bottle.sha256 {
                Ok(None)
            } else {
                Ok(Some(OutdatedPackage {
                    name: name.to_string(),
                    installed_version: installed.version,
                    installed_sha256: installed.store_key,
                    current_version: formula.effective_version(),
                    current_sha256: bottle.sha256,
                    is_source_build: false,
                }))
            }
        }
    }

    /// Transient kegs are skipped: they exist only until the next `zb gc`,
    /// so reporting them as outdated -- and upgrading them, since `zb upgrade`
    /// with no arguments works off this list -- would spend a download on a
    /// package that is about to be deleted. `is_outdated` still answers for
    /// them, so `zb upgrade <name>` by name keeps working.
    pub async fn check_outdated(&self) -> Result<(Vec<OutdatedPackage>, Vec<String>), Error> {
        let installed: Vec<_> = self
            .db
            .list_installed()?
            .into_iter()
            .filter(|keg| !keg.reason.is_transient())
            .collect();
        if installed.is_empty() {
            return Ok((Vec::new(), Vec::new()));
        }

        let installed_names: std::collections::HashSet<&str> =
            installed.iter().map(|k| k.name.as_str()).collect();

        let bulk_raw = self.api_client.get_all_formulas_raw().await?;
        let bulk_values: Vec<serde_json::Value> = serde_json::from_str(&bulk_raw)
            .map_err(Error::network("failed to parse bulk formula JSON"))?;

        let mut bulk_map: HashMap<String, zb_core::Formula> = HashMap::new();
        for val in bulk_values {
            let name = match val.get("name").and_then(|n| n.as_str()) {
                Some(n) if installed_names.contains(n) => n.to_string(),
                _ => continue,
            };
            if let Ok(f) = serde_json::from_value(val) {
                bulk_map.insert(name, f);
            }
        }

        let mut outdated = Vec::new();
        let mut warnings = Vec::new();

        for keg in &installed {
            let is_tap = keg.name.contains('/');

            let formula = if is_tap || !bulk_map.contains_key(&keg.name) {
                match self.api_client.get_formula(&keg.name).await {
                    Ok(f) => f,
                    Err(e) => {
                        warnings.push(format!("{}: {}", keg.name, e));
                        continue;
                    }
                }
            } else {
                bulk_map.remove(&keg.name).unwrap()
            };

            let is_source = keg.store_key.starts_with("source:");

            if is_source {
                let current_version = formula.effective_version();
                if keg.version != current_version {
                    outdated.push(OutdatedPackage {
                        name: keg.name.clone(),
                        installed_version: keg.version.clone(),
                        installed_sha256: keg.store_key.clone(),
                        current_version,
                        current_sha256: String::new(),
                        is_source_build: true,
                    });
                }
            } else {
                match select_bottle(&formula) {
                    Ok(bottle) => {
                        if keg.store_key != bottle.sha256 {
                            outdated.push(OutdatedPackage {
                                name: keg.name.clone(),
                                installed_version: keg.version.clone(),
                                installed_sha256: keg.store_key.clone(),
                                current_version: formula.effective_version(),
                                current_sha256: bottle.sha256,
                                is_source_build: false,
                            });
                        }
                    }
                    Err(e) => warnings.push(format!("{}: {}", keg.name, e)),
                }
            }
        }

        outdated.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((outdated, warnings))
    }

    pub async fn suggest_formulas(&self, query: &str, limit: usize) -> Result<Vec<String>, Error> {
        self.api_client.suggest_formulas(query, limit).await
    }
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    use crate::storage::db::InstallReason;
    use crate::test_support::*;

    /// The API's answer for `name` at `version`. These tests compare recorded
    /// checksums and versions only, so the bottle itself is never fetched.
    fn api_formula(name: &str, version: &str, sha256: &str) -> String {
        formula_json(
            name,
            version,
            &format!("https://example.com{}", bottle_path(name, version)),
            sha256,
        )
    }

    #[tokio::test]
    async fn suggest_formulas_returns_matches_from_api_client() {
        let env = TestEnv::new().await;

        let bulk = r#"[
            {"name":"python"},
            {"name":"pytest"},
            {"name":"pypy"}
        ]"#;

        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(bulk))
            .mount(&env.server)
            .await;

        let installer = env.installer();
        let suggestions = installer.suggest_formulas("pythn", 3).await.unwrap();
        assert_eq!(suggestions.first().map(String::as_str), Some("python"));
    }

    #[tokio::test]
    async fn is_outdated_returns_none_when_sha256_matches() {
        let env = TestEnv::new().await;
        let mut installer = env.installer();
        let sha = "abc123def456";

        {
            let tx = installer.db.transaction().unwrap();
            tx.record_install("jq", "1.7.1", sha, InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        env.mount_formula("jq", api_formula("jq", "1.7.1", sha))
            .await;

        let result = installer.is_outdated("jq").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn is_outdated_returns_some_when_sha256_differs() {
        let env = TestEnv::new().await;
        let mut installer = env.installer();

        {
            let tx = installer.db.transaction().unwrap();
            tx.record_install("jq", "1.7.0", "old_sha256", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        env.mount_formula("jq", api_formula("jq", "1.7.1", "new_sha256"))
            .await;

        let result = installer.is_outdated("jq").await.unwrap().unwrap();
        assert_eq!(result.name, "jq");
        assert_eq!(result.installed_version, "1.7.0");
        assert_eq!(result.current_version, "1.7.1");
        assert!(!result.is_source_build);
    }

    #[tokio::test]
    async fn is_outdated_errors_for_not_installed() {
        let env = TestEnv::new().await;
        let installer = env.installer();

        let err = installer.is_outdated("jq").await.unwrap_err();
        assert!(matches!(err, zb_core::Error::NotInstalled { .. }));
    }

    #[tokio::test]
    async fn is_outdated_source_build_compares_version_only() {
        let env = TestEnv::new().await;
        let mut installer = env.installer();

        {
            let tx = installer.db.transaction().unwrap();
            tx.record_install("jq", "1.7.1", "source:jq:1.7.1", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        env.mount_formula("jq", api_formula("jq", "1.7.1", "irrelevant"))
            .await;

        let result = installer.is_outdated("jq").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn is_outdated_source_build_detects_new_version() {
        let env = TestEnv::new().await;
        let mut installer = env.installer();

        {
            let tx = installer.db.transaction().unwrap();
            tx.record_install("jq", "1.6", "source:jq:1.6", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        env.mount_formula("jq", api_formula("jq", "1.7.1", "irrelevant"))
            .await;

        let result = installer.is_outdated("jq").await.unwrap().unwrap();
        assert_eq!(result.installed_version, "1.6");
        assert_eq!(result.current_version, "1.7.1");
        assert!(result.is_source_build);
    }

    #[tokio::test]
    async fn check_outdated_empty_when_nothing_installed() {
        let env = TestEnv::new().await;
        let installer = env.installer();

        let (outdated, warnings) = installer.check_outdated().await.unwrap();
        assert!(outdated.is_empty());
        assert!(warnings.is_empty());
    }

    #[tokio::test]
    async fn check_outdated_continues_on_network_failure() {
        let env = TestEnv::new().await;
        let mut installer = env.installer();

        {
            let tx = installer.db.transaction().unwrap();
            tx.record_install("good", "1.0.0", "old_sha", InstallReason::Retained)
                .unwrap();
            tx.record_install("bad", "1.0.0", "old_sha", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        let bulk = format!("[{}]", api_formula("good", "2.0.0", "new_sha"));
        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(bulk))
            .mount(&env.server)
            .await;

        Mock::given(method("GET"))
            .and(path("/formula/bad.json"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&env.server)
            .await;

        let (outdated, warnings) = installer.check_outdated().await.unwrap();
        assert_eq!(outdated.len(), 1);
        assert_eq!(outdated[0].name, "good");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("bad"));
    }

    #[tokio::test]
    async fn check_outdated_warns_on_missing_bottle() {
        let env = TestEnv::new().await;
        let mut installer = env.installer();

        {
            let tx = installer.db.transaction().unwrap();
            tx.record_install("nobottle", "1.0.0", "old_sha", InstallReason::Retained)
                .unwrap();
            tx.commit().unwrap();
        }

        let bulk = r#"[{
            "name": "nobottle",
            "versions": { "stable": "2.0.0" },
            "dependencies": [],
            "bottle": { "stable": { "files": {} } }
        }]"#;

        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(bulk))
            .mount(&env.server)
            .await;

        let (outdated, warnings) = installer.check_outdated().await.unwrap();
        assert!(outdated.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("nobottle"));
    }
}
