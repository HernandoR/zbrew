//! `brew leaves`: the installed formulas nothing else installed depends on.
//!
//! The answer is a graph question, but zbrew does not have to store the graph.
//! Every dependency edge that matters is already in the formula metadata the
//! API serves, and the bulk index carries all of it in one response — so a
//! single request answers for a cellar of any size, and the answer stays
//! correct for packages installed before this command existed.

use std::collections::{HashMap, HashSet};

use zb_core::Error;

use super::Installer;

/// The result of `zb leaves`.
pub struct Leaves {
    /// Installed formulas that no other installed formula requires, sorted.
    pub names: Vec<String>,
    /// Packages whose dependencies could not be read. Their edges are missing
    /// from the graph, so something they require may be listed as a leaf when
    /// it is not.
    pub warnings: Vec<String>,
}

impl Installer {
    pub async fn leaves(&self) -> Result<Leaves, Error> {
        // A keg `zb run` left behind is not something the user installed and
        // `zb gc` will remove it, so it is neither a leaf nor a dependent.
        let installed: Vec<String> = self
            .db
            .list_installed()?
            .into_iter()
            .filter(|keg| !keg.reason.is_transient())
            .map(|keg| keg.name)
            .collect();

        // Casks carry no dependency metadata zbrew can read, so they can
        // neither be a leaf nor keep a formula from being one.
        let formulas: Vec<&str> = installed
            .iter()
            .map(String::as_str)
            .filter(|name| !name.starts_with("cask:"))
            .collect();

        if formulas.is_empty() {
            return Ok(Leaves {
                names: Vec::new(),
                warnings: Vec::new(),
            });
        }

        let wanted: HashSet<&str> = formulas.iter().copied().collect();
        let mut warnings = Vec::new();
        let mut dependencies = self.bulk_dependencies(&wanted).await?;

        // A tap formula is not in homebrew-core's index, so it needs its own
        // request; without one, whatever it depends on would look like a leaf.
        for name in &formulas {
            if dependencies.contains_key(*name) {
                continue;
            }
            match self.api_client.get_formula(name).await {
                Ok(formula) => {
                    dependencies.insert((*name).to_string(), formula.dependencies);
                }
                Err(e) => warnings.push(format!("{name}: {e}")),
            }
        }

        // Runtime dependencies only, as `brew leaves` uses: a package that is
        // merely somebody's build dependency is still something the user has
        // to decide about keeping.
        let mut required: HashSet<&str> = HashSet::new();
        for deps in dependencies.values() {
            for dep in deps {
                if let Some(name) = wanted.get(dep.as_str()) {
                    required.insert(name);
                }
            }
        }

        let mut names: Vec<String> = formulas
            .iter()
            .filter(|name| !required.contains(*name))
            .map(|name| (*name).to_string())
            .collect();
        names.sort();

        Ok(Leaves { names, warnings })
    }

    /// Runtime dependencies for the installed formulas homebrew-core knows
    /// about, from one request.
    async fn bulk_dependencies(
        &self,
        wanted: &HashSet<&str>,
    ) -> Result<HashMap<String, Vec<String>>, Error> {
        #[derive(serde::Deserialize)]
        struct IndexEntry {
            name: String,
            #[serde(default)]
            dependencies: Vec<String>,
        }

        let raw = self.api_client.get_all_formulas_raw().await?;
        let entries: Vec<IndexEntry> = serde_json::from_str(&raw)
            .map_err(Error::network("failed to parse bulk formula JSON"))?;

        Ok(entries
            .into_iter()
            .filter(|entry| wanted.contains(entry.name.as_str()))
            .map(|entry| (entry.name, entry.dependencies))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TestEnv, create_bottle_tarball};

    async fn install(installer: &mut crate::Installer, names: &[&str]) {
        let plan = installer
            .plan(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
            .await
            .unwrap();
        installer.execute(plan, true).await.unwrap();
    }

    /// The point of `leaves`: a dependency that was pulled in is not something
    /// the user chose, so it must not be offered up as removable.
    #[tokio::test]
    async fn a_dependency_of_an_installed_formula_is_not_a_leaf() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("leafdep", "1.0.0", create_bottle_tarball("leafdep"))
            .await;
        env.mount_bottled_formula_with_deps(
            "leafroot",
            "1.0.0",
            &["leafdep"],
            create_bottle_tarball("leafroot"),
        )
        .await;
        env.mount_bottled_formula("leafalone", "1.0.0", create_bottle_tarball("leafalone"))
            .await;
        env.mount_bulk_formula_index(&[
            ("leafdep", &[]),
            ("leafroot", &["leafdep"]),
            ("leafalone", &[]),
        ])
        .await;

        let mut installer = env.installer();
        install(&mut installer, &["leafroot", "leafalone"]).await;

        let leaves = installer.leaves().await.unwrap();

        assert_eq!(leaves.names, ["leafalone", "leafroot"]);
        assert!(leaves.warnings.is_empty(), "{:?}", leaves.warnings);
    }

    /// Installing a package explicitly does not stop it being a dependency:
    /// the question `leaves` answers is whether anything else needs it.
    #[tokio::test]
    async fn a_formula_that_is_also_a_dependency_is_not_a_leaf() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("sharedlib", "1.0.0", create_bottle_tarball("sharedlib"))
            .await;
        env.mount_bottled_formula_with_deps(
            "sharedapp",
            "1.0.0",
            &["sharedlib"],
            create_bottle_tarball("sharedapp"),
        )
        .await;
        env.mount_bulk_formula_index(&[("sharedlib", &[]), ("sharedapp", &["sharedlib"])])
            .await;

        let mut installer = env.installer();
        install(&mut installer, &["sharedlib"]).await;
        install(&mut installer, &["sharedapp"]).await;

        let leaves = installer.leaves().await.unwrap();

        assert_eq!(leaves.names, ["sharedapp"]);
    }

    /// A dependency that is not installed cannot make anything a non-leaf,
    /// and the formula that names it is still a leaf itself.
    #[tokio::test]
    async fn an_uninstalled_dependency_does_not_appear_anywhere() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("loneapp", "1.0.0", create_bottle_tarball("loneapp"))
            .await;
        // The index claims a dependency the cellar does not have.
        env.mount_bulk_formula_index(&[("loneapp", &["notinstalled"])])
            .await;

        let mut installer = env.installer();
        install(&mut installer, &["loneapp"]).await;

        let leaves = installer.leaves().await.unwrap();

        assert_eq!(leaves.names, ["loneapp"]);
    }

    /// A formula whose metadata cannot be read leaves a hole in the graph.
    /// Saying so is the difference between an incomplete answer and a wrong
    /// one presented as complete.
    #[tokio::test]
    async fn a_formula_with_unreadable_metadata_is_reported() {
        let env = TestEnv::new().await;
        env.mount_bottled_formula("gonepkg", "1.0.0", create_bottle_tarball("gonepkg"))
            .await;

        let mut installer = env.installer();
        install(&mut installer, &["gonepkg"]).await;

        // The formula disappears from the API, as `fasd` did upstream.
        env.server.reset().await;
        env.mount_bulk_formula_index(&[]).await;
        env.mount_missing_formula("gonepkg").await;
        installer.clear_api_cache().unwrap();

        let leaves = installer.leaves().await.unwrap();

        assert_eq!(leaves.names, ["gonepkg"]);
        assert_eq!(leaves.warnings.len(), 1);
        assert!(
            leaves.warnings[0].starts_with("gonepkg:"),
            "the warning should name the formula, got {:?}",
            leaves.warnings
        );
    }
}
