use std::collections::{BTreeMap, HashMap, HashSet};

use tracing::warn;
use zb_core::{BuildPlan, Error, Formula, InstallMethod, select_bottle};

use super::{InstallPlan, Installer, PlanFailure, PlannedInstall};

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

    pub async fn plan_best_effort(
        &self,
        names: &[String],
        build_from_source: bool,
    ) -> (InstallPlan, Vec<PlanFailure>) {
        let (formulas, fetch_failures) = self.fetch_all_formulas_best_effort(names).await;
        let mut items = Vec::new();
        let mut failures = Vec::new();
        let mut valid_roots = Vec::new();
        let mut seen_roots = HashSet::new();

        for name in names {
            if !seen_roots.insert(name.clone()) {
                continue;
            }

            if let Some(error) = fetch_failures.get(name) {
                failures.push(PlanFailure {
                    name: name.clone(),
                    error: error.clone(),
                });
                continue;
            }

            if !formulas.contains_key(name) {
                failures.push(PlanFailure {
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
                            Err(error) => failures.push(PlanFailure {
                                name: install_name,
                                error,
                            }),
                        }
                    }
                }
                Err(error) => {
                    failures.extend(valid_roots.into_iter().map(|name| PlanFailure {
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
                        failures.insert(fetch_name, error);
                        continue;
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
) -> Option<PlanFailure> {
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
                return Some(PlanFailure {
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
        let (plan, failures) = installer.plan_best_effort(&names, false).await;

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
