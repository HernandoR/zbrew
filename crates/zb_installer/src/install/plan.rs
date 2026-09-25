use std::collections::{BTreeMap, HashMap, HashSet};

use tracing::warn;
use zb_core::{BuildPlan, Error, Formula, InstallMethod, PackageFailure, select_bottle};

use super::{InstallPlan, Installer, PlannedInstall};
use crate::homebrew::HomebrewCellar;

impl Installer {
    pub async fn plan(&self, names: &[String]) -> Result<InstallPlan, Error> {
        self.plan_with_options(names, false).await
    }

    pub async fn plan_with_options(
        &self,
        names: &[String],
        build_from_source: bool,
    ) -> Result<InstallPlan, Error> {
        let formulas = self.fetch_all_formulas(names).await?;
        let ordered = zb_core::resolve_closure(names, &formulas)?;

        let mut items = Vec::with_capacity(ordered.len());
        for install_name in ordered {
            let formula = formulas.get(&install_name).cloned().unwrap();
            items.push(self.plan_item(install_name, formula, build_from_source)?);
        }

        Ok(InstallPlan {
            items,
            ..Default::default()
        })
    }

    /// Plan what can be planned, reporting the rest instead of failing.
    ///
    /// `local_formulas`, when given, stands in for the API: a formula
    /// homebrew-core has dropped is still described by the copy Homebrew kept
    /// beside its keg, and `zb migrate` has to reproduce exactly the packages
    /// the user has installed -- including those.
    pub async fn plan_best_effort(
        &self,
        names: &[String],
        build_from_source: bool,
        local_formulas: Option<&HomebrewCellar>,
    ) -> (InstallPlan, Vec<PackageFailure>) {
        let (formulas, fetch_failures) = self
            .fetch_all_formulas_best_effort(names, local_formulas)
            .await;
        let mut items = Vec::new();
        let mut failures = Vec::new();
        let mut valid_roots = Vec::new();
        let mut seen_roots = HashSet::new();

        for name in names {
            if !seen_roots.insert(name.clone()) {
                continue;
            }

            if let Some(error) = fetch_failures.get(name) {
                failures.push(PackageFailure {
                    name: name.clone(),
                    error: error.clone(),
                });
                continue;
            }

            if !formulas.contains_key(name) {
                failures.push(PackageFailure {
                    name: name.clone(),
                    error: Error::MissingFormula { name: name.clone() },
                });
                continue;
            }

            if let Some(failure) = root_dependency_failure(name, &formulas, &fetch_failures) {
                failures.push(failure);
                continue;
            }

            valid_roots.push(name.clone());
        }

        if !valid_roots.is_empty() {
            match zb_core::resolve_closure(&valid_roots, &formulas) {
                Ok(ordered) => {
                    for install_name in ordered {
                        let formula = formulas.get(&install_name).cloned().unwrap();
                        match self.plan_item(install_name.clone(), formula, build_from_source) {
                            Ok(item) => items.push(item),
                            Err(error) => failures.push(PackageFailure {
                                name: install_name,
                                error,
                            }),
                        }
                    }
                }
                Err(error) => {
                    failures.extend(valid_roots.into_iter().map(|name| PackageFailure {
                        name,
                        error: error.clone(),
                    }));
                }
            }
        }

        (
            InstallPlan {
                items,
                ..Default::default()
            },
            failures,
        )
    }

    fn plan_item(
        &self,
        install_name: String,
        formula: Formula,
        build_from_source: bool,
    ) -> Result<PlannedInstall, Error> {
        let method = if build_from_source {
            match BuildPlan::from_formula(&formula, &self.prefix) {
                Some(plan) => InstallMethod::Source(plan),
                None => match select_bottle(&formula) {
                    Ok(bottle) => InstallMethod::Bottle(bottle),
                    Err(_) => {
                        return Err(Error::UnsupportedBottle {
                            name: formula.name.clone(),
                        });
                    }
                },
            }
        } else {
            match select_bottle(&formula) {
                Ok(bottle) => InstallMethod::Bottle(bottle),
                Err(_) => match BuildPlan::from_formula(&formula, &self.prefix) {
                    Some(plan) => InstallMethod::Source(plan),
                    None => {
                        return Err(Error::UnsupportedBottle {
                            name: formula.name.clone(),
                        });
                    }
                },
            }
        };

        Ok(PlannedInstall {
            install_name,
            formula,
            method,
        })
    }

    async fn fetch_all_formulas_best_effort(
        &self,
        names: &[String],
        local_formulas: Option<&HomebrewCellar>,
    ) -> (BTreeMap<String, Formula>, HashMap<String, Error>) {
        let mut formulas = BTreeMap::new();
        let mut failures = HashMap::new();
        let mut fetched: HashSet<String> = HashSet::new();
        let mut to_fetch: Vec<String> = names.to_vec();

        while !to_fetch.is_empty() {
            let batch: Vec<String> = to_fetch
                .drain(..)
                .filter(|n| !fetched.contains(n))
                .collect();

            if batch.is_empty() {
                break;
            }

            for n in &batch {
                fetched.insert(n.clone());
            }

            let futures: Vec<_> = batch
                .iter()
                .map(|n| self.api_client.get_formula(n))
                .collect();

            let results = futures::future::join_all(futures).await;

            for (i, result) in results.into_iter().enumerate() {
                let fetch_name = batch[i].clone();
                let formula = match result {
                    Ok(f) => f,
                    Err(error) => {
                        // Only a 404 means "homebrew-core dropped this
                        // formula", which is the one case the local copy can
                        // answer. Every name `zb migrate` passes here came
                        // from `brew leaves`, so by construction it *always*
                        // has a local copy -- falling back on a 5xx or a
                        // transport error would let one API outage route the
                        // entire migration through stale local metadata and
                        // call it success.
                        let recovered = if matches!(error, Error::MissingFormula { .. }) {
                            local_formulas.and_then(|cellar| cellar.local_formula(&fetch_name))
                        } else {
                            None
                        };

                        match recovered {
                            Some(local) => {
                                warn!(
                                    formula = %fetch_name,
                                    "using Homebrew's local copy of the formula; the API no longer serves it"
                                );
                                local
                            }
                            None => {
                                failures.insert(fetch_name, error);
                                continue;
                            }
                        }
                    }
                };

                if select_bottle(&formula).is_err() && !formula.has_source_url() {
                    warn!(
                        formula = %formula.name,
                        "skipping formula with no bottle or source available for this platform"
                    );
                    failures.insert(
                        fetch_name,
                        Error::UnsupportedBottle {
                            name: formula.name.clone(),
                        },
                    );
                    continue;
                }

                for dep in formula.runtime_dependencies() {
                    if !fetched.contains(&dep)
                        && !to_fetch.contains(&dep)
                        && !failures.contains_key(&dep)
                    {
                        to_fetch.push(dep);
                    }
                }

                formulas.insert(fetch_name, formula);
            }
        }

        (formulas, failures)
    }

    async fn fetch_all_formulas(
        &self,
        names: &[String],
    ) -> Result<BTreeMap<String, Formula>, Error> {
        use std::collections::HashSet;

        let mut formulas = BTreeMap::new();
        let mut fetched: HashSet<String> = HashSet::new();
        let mut to_fetch: Vec<String> = names.to_vec();

        while !to_fetch.is_empty() {
            let batch: Vec<String> = to_fetch
                .drain(..)
                .filter(|n| !fetched.contains(n))
                .collect();

            if batch.is_empty() {
                break;
            }

            for n in &batch {
                fetched.insert(n.clone());
            }

            let futures: Vec<_> = batch
                .iter()
                .map(|n| self.api_client.get_formula(n))
                .collect();

            let results = futures::future::join_all(futures).await;

            for (i, result) in results.into_iter().enumerate() {
                let formula = match result {
                    Ok(f) => f,
                    Err(e) => return Err(e),
                };

                if select_bottle(&formula).is_err() && !formula.has_source_url() {
                    warn!(
                        formula = %formula.name,
                        "skipping formula with no bottle or source available for this platform"
                    );
                    continue;
                }

                for dep in formula.runtime_dependencies() {
                    if !fetched.contains(&dep) && !to_fetch.contains(&dep) {
                        to_fetch.push(dep);
                    }
                }

                formulas.insert(batch[i].clone(), formula);
            }
        }

        Ok(formulas)
    }
}

fn root_dependency_failure(
    root: &str,
    formulas: &BTreeMap<String, Formula>,
    fetch_failures: &HashMap<String, Error>,
) -> Option<PackageFailure> {
    let mut seen = HashSet::new();
    let mut stack = vec![root.to_string()];

    while let Some(name) = stack.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }

        let Some(formula) = formulas.get(&name) else {
            continue;
        };

        for dep in formula.runtime_dependencies() {
            if let Some(error) = fetch_failures.get(&dep) {
                return Some(PackageFailure {
                    name: root.to_string(),
                    error: Error::ExecutionError {
                        message: format!("dependency '{dep}' could not be planned: {error}"),
                    },
                });
            }

            if formulas.contains_key(&dep) {
                stack.push(dep);
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};

    use crate::test_support::*;

    const LOCAL_BOTTLE_SHA: &str =
        "1111111111111111111111111111111111111111111111111111111111111111";

    /// A formula homebrew-core has dropped -- `fasd` is the reported case --
    /// is still installed on the user's machine, so `zb migrate` has to
    /// reproduce it. Homebrew's own copy of the formula is the only
    /// description of it left.
    #[tokio::test]
    async fn a_formula_the_api_dropped_is_planned_from_homebrews_local_copy() {
        let env = TestEnv::new().await;
        Mock::given(method("GET"))
            .and(path("/formula/droppedpkg.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&env.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&env.server)
            .await;

        let brew_prefix = write_homebrew_keg(
            &env.tmp_path().join("homebrew-prefix"),
            "droppedpkg",
            "1.0.1",
            &[],
            LOCAL_BOTTLE_SHA,
        );
        let cellar = crate::HomebrewCellar::at(&brew_prefix);

        let installer = env.installer();
        let names = vec!["droppedpkg".to_string()];
        let (plan, failures) = installer
            .plan_best_effort(&names, false, Some(&cellar))
            .await;

        assert!(failures.is_empty(), "{failures:?}");
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].install_name, "droppedpkg");
        assert_eq!(plan.items[0].formula.versions.stable, "1.0.1");
    }

    /// The fallback has to sit inside the fetch loop, not beside it: a
    /// recovered formula's own dependencies still have to be resolved, or the
    /// plan fails on a dependency that was never fetched.
    #[tokio::test]
    async fn a_locally_recovered_formulas_dependencies_are_still_fetched() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("livedep", "2.0.0", create_bottle_tarball("livedep"))
            .await;
        Mock::given(method("GET"))
            .and(path("/formula/droppedparent.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&env.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&env.server)
            .await;

        let brew_prefix = write_homebrew_keg(
            &env.tmp_path().join("homebrew-prefix"),
            "droppedparent",
            "3.2.1",
            &["livedep"],
            LOCAL_BOTTLE_SHA,
        );
        let cellar = crate::HomebrewCellar::at(&brew_prefix);

        let installer = env.installer();
        let names = vec!["droppedparent".to_string()];
        let (plan, failures) = installer
            .plan_best_effort(&names, false, Some(&cellar))
            .await;

        assert!(failures.is_empty(), "{failures:?}");
        let mut planned: Vec<&str> = plan
            .items
            .iter()
            .map(|item| item.install_name.as_str())
            .collect();
        planned.sort();
        assert_eq!(planned, ["droppedparent", "livedep"]);
    }

    /// An API outage must not be mistaken for a dropped formula. Every name
    /// `zb migrate` passes in came from `brew leaves`, so all of them have a
    /// local copy -- without this, one 5xx would silently migrate the whole
    /// machine from stale local metadata and report success.
    #[tokio::test]
    async fn a_server_error_is_reported_rather_than_masked_by_the_local_copy() {
        let env = TestEnv::new().await;
        Mock::given(method("GET"))
            .and(path("/formula/outagepkg.json"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&env.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&env.server)
            .await;

        // The local copy exists and would parse fine; the point is that it
        // must not be reached.
        let brew_prefix = write_homebrew_keg(
            &env.tmp_path().join("homebrew-prefix"),
            "outagepkg",
            "1.0.0",
            &[],
            LOCAL_BOTTLE_SHA,
        );
        let cellar = crate::HomebrewCellar::at(&brew_prefix);

        let installer = env.installer();
        let names = vec!["outagepkg".to_string()];
        let (plan, failures) = installer
            .plan_best_effort(&names, false, Some(&cellar))
            .await;

        assert!(plan.items.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "outagepkg");
        assert!(
            matches!(failures[0].error, zb_core::Error::NetworkFailure { .. }),
            "the outage must surface as itself, got {:?}",
            failures[0].error
        );
    }

    /// The fallback must not turn a typo into a phantom package: a name
    /// Homebrew has never installed is still a failure.
    #[tokio::test]
    async fn a_name_absent_from_both_the_api_and_the_cellar_still_fails() {
        let env = TestEnv::new().await;
        Mock::given(method("GET"))
            .and(path("/formula/ghostpkg.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&env.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&env.server)
            .await;

        let brew_prefix = write_homebrew_keg(
            &env.tmp_path().join("homebrew-prefix"),
            "somethingelse",
            "1.0.0",
            &[],
            LOCAL_BOTTLE_SHA,
        );
        let cellar = crate::HomebrewCellar::at(&brew_prefix);

        let installer = env.installer();
        let names = vec!["ghostpkg".to_string()];
        let (plan, failures) = installer
            .plan_best_effort(&names, false, Some(&cellar))
            .await;

        assert!(plan.items.is_empty());
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "ghostpkg");
    }

    #[tokio::test]
    async fn plans_tapped_formula_with_core_dependency() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("go", "1.24.0", create_bottle_tarball("go"))
            .await;

        Mock::given(method("GET"))
            .and(path("/hashicorp/homebrew-tap/main/Formula/terraform.rb"))
            .respond_with(ResponseTemplate::new(200).set_body_string(tap_formula_rb(
                "Terraform",
                "1.10.0",
                &format!("{}/ghcr/hashicorp/tap", env.uri()),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                &["go"],
            )))
            .mount(&env.server)
            .await;

        let installer = env.installer_with_taps();
        let plan = installer
            .plan(&["hashicorp/tap/terraform".to_string()])
            .await
            .unwrap();

        let planned_names: Vec<String> = plan
            .items
            .iter()
            .map(|item| item.formula.name.clone())
            .collect();
        assert!(planned_names.contains(&"terraform".to_string()));
        assert!(planned_names.contains(&"go".to_string()));
    }

    #[tokio::test]
    async fn falls_back_to_source_when_no_bottle() {
        let env = TestEnv::new().await;
        env.mount_formula(
            "nobottle",
            r#"{
            "name": "nobottle",
            "versions": { "stable": "1.0.0" },
            "dependencies": [],
            "build_dependencies": ["pkgconf"],
            "urls": {
                "stable": {
                    "url": "https://example.com/nobottle-1.0.0.tar.gz",
                    "checksum": "abc123"
                }
            },
            "ruby_source_path": "Formula/n/nobottle.rb",
            "bottle": { "stable": { "files": {} } }
        }"#,
        )
        .await;

        let installer = env.installer();
        let plan = installer.plan(&["nobottle".to_string()]).await.unwrap();

        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].formula.name, "nobottle");
        assert!(matches!(
            plan.items[0].method,
            zb_core::InstallMethod::Source(_)
        ));

        if let zb_core::InstallMethod::Source(ref bp) = plan.items[0].method {
            assert_eq!(bp.source_url, "https://example.com/nobottle-1.0.0.tar.gz");
            assert_eq!(bp.formula_name, "nobottle");
            assert_eq!(bp.build_dependencies, vec!["pkgconf"]);
        }
    }

    #[tokio::test]
    async fn prefers_bottle_over_source() {
        let env = TestEnv::new().await;
        let tag = get_test_bottle_tag();
        env.mount_formula(
            "hasboth",
            format!(
                r#"{{
                "name": "hasboth",
                "versions": {{ "stable": "2.0.0" }},
                "dependencies": [],
                "urls": {{
                    "stable": {{
                        "url": "https://example.com/hasboth-2.0.0.tar.gz",
                        "checksum": "def456"
                    }}
                }},
                "ruby_source_path": "Formula/h/hasboth.rb",
                "bottle": {{
                    "stable": {{
                        "files": {{
                            "{tag}": {{
                                "url": "https://example.com/hasboth.bottle.tar.gz",
                                "sha256": "aabbccdd"
                            }}
                        }}
                    }}
                }}
            }}"#
            ),
        )
        .await;

        let installer = env.installer();
        let plan = installer.plan(&["hasboth".to_string()]).await.unwrap();

        assert_eq!(plan.items.len(), 1);
        assert!(matches!(
            plan.items[0].method,
            zb_core::InstallMethod::Bottle(_)
        ));
    }

    #[tokio::test]
    async fn errors_when_no_bottle_and_no_source() {
        let env = TestEnv::new().await;
        env.mount_formula(
            "nothing",
            r#"{
            "name": "nothing",
            "versions": { "stable": "1.0.0" },
            "dependencies": [],
            "bottle": { "stable": { "files": {} } }
        }"#,
        )
        .await;

        let installer = env.installer();
        let result = installer.plan(&["nothing".to_string()]).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            zb_core::Error::MissingFormula { .. }
        ));
    }

    #[tokio::test]
    async fn plan_best_effort_keeps_valid_formula_when_another_is_missing() {
        let env = TestEnv::new().await;
        env.mount_formula(
            "goodpkg",
            formula_json(
                "goodpkg",
                "1.0.0",
                &env.bottle_url("goodpkg", "1.0.0"),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
        )
        .await;
        Mock::given(method("GET"))
            .and(path("/formula/missingpkg.json"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&env.server)
            .await;
        Mock::given(method("GET"))
            .and(path("/formula.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("[]"))
            .mount(&env.server)
            .await;

        let installer = env.installer();
        let names = vec!["goodpkg".to_string(), "missingpkg".to_string()];
        let (plan, failures) = installer.plan_best_effort(&names, false, None).await;

        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].install_name, "goodpkg");
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].name, "missingpkg");
        assert!(matches!(
            failures[0].error,
            zb_core::Error::MissingFormula { .. }
        ));
    }
}
