//
// Copyright (c) 2020-2022 science+computing ag and other contributors
//
// This program and the accompanying materials are made
// available under the terms of the Eclipse Public License 2.0
// which is available at https://www.eclipse.org/legal/epl-2.0/
//
// SPDX-License-Identifier: EPL-2.0
//

//! Implementation of the 'tree-of' subcommand

use std::convert::TryFrom;

//use anyhow::Error;
use anyhow::Result;
use clap::ArgMatches;
//use resiter::AndThen;
use tracing::trace;

use crate::package::condition::ConditionCheckable;
use crate::package::condition::ConditionData;
use crate::package::ParseDependency;
use crate::package::Package;
use crate::package::PackageName;
use crate::package::PackageVersionConstraint;
use crate::repository::Repository;
use crate::util::docker::ImageName;
use crate::util::EnvironmentVariableName;

#[derive(Debug, PartialEq)]
enum DependencyType {
    BUILDTIME,
    RUNTIME,
}

#[derive(Debug)]
struct DependenciesNode {
    name: String,
    //dependency_type: DependencyType,
    dependencies: Vec<(DependenciesNode, DependencyType)>,
}

//fn build_dependencies_tree(p: Package, repo: &Repository, conditional_data: &ConditionData<'_>) -> DependenciesNode {
//    /// helper fn with bad name to check the dependency condition of a dependency and parse the dependency into a tuple of
//    /// name and version for further processing
//    fn process<D: ConditionCheckable + ParseDependency>(
//        d: &D,
//        conditional_data: &ConditionData<'_>,
//    ) -> Result<(bool, PackageName, PackageVersionConstraint)> {
//        // Check whether the condition of the dependency matches our data
//        let take = d.check_condition(conditional_data)?;
//        let (name, version) = d.parse_as_name_and_version()?;
//
//        // (dependency check result, name of the dependency, version of the dependency)
//        Ok((take, name, version))
//    }
//    let deps = p.dependencies();
//    vec![
//        (DependencyType::BUILDTIME, deps.build()),
//        (DependencyType::RUNTIME, deps.runtime()),
//    ].map(|(dep_type, deps)| {
//        let deps = deps.iter().map(move |d| process(d, conditional_data))
//            // Now filter out all dependencies where their condition did not match our
//            // `conditional_data`.
//            .filter(|res| match res {
//                Ok((true, _, _)) => true,
//                Ok((false, _, _)) => false,
//                Err(_) => true,
//            })
//            // Map out the boolean from the condition, because we don't need that later on
//            .map(|res| res.map(|(_, name, vers)| (name, vers)));
//    })
//}
//
fn print_dependencies_tree(node: DependenciesNode, level: usize, is_runtime_dep: bool) {
    let ident = "  ".repeat(level);
    let name = node.name;
    let suffix = if is_runtime_dep { "*" } else { "" };
    println!("{ident}- {name}{suffix}");
    for (node, dep_type) in node.dependencies {
        print_dependencies_tree(node, level+1, dep_type == DependencyType::RUNTIME);
    }
}
fn build_dependencies_tree(p: Package, repo: &Repository, conditional_data: &ConditionData<'_>) -> DependenciesNode {
        /// helper fn with bad name to check the dependency condition of a dependency and parse the dependency into a tuple of
        /// name and version for further processing
        fn process<D: ConditionCheckable + ParseDependency>(
            d: &D,
            conditional_data: &ConditionData<'_>,
            dependency_type: DependencyType,
        ) -> Result<(bool, PackageName, PackageVersionConstraint, DependencyType)> {
            // Check whether the condition of the dependency matches our data
            let take = d.check_condition(conditional_data)?;
            let (name, version) = d.parse_as_name_and_version()?;

            // (dependency check result, name of the dependency, version of the dependency)
            Ok((take, name, version, dependency_type))
        }

        /// Helper fn to get the dependencies of a package
        ///
        /// This function helps getting the dependencies of a package as an iterator over
        /// (Name, Version).
        ///
        /// It also filters out dependencies that do not match the `conditional_data` passed and
        /// makes the dependencies unique over (name, version).
        fn get_package_dependencies<'a>(
            package: &'a Package,
            conditional_data: &'a ConditionData<'_>,
        ) -> impl Iterator<Item = Result<(PackageName, PackageVersionConstraint, DependencyType)>> + 'a {
            package
                .dependencies()
                .build()
                .iter()
                .map(move |d| process(d, conditional_data, DependencyType::BUILDTIME))
                .chain({
                    package
                        .dependencies()
                        .runtime()
                        .iter()
                        .map(move |d| process(d, conditional_data, DependencyType::RUNTIME))
                })
                // Now filter out all dependencies where their condition did not match our
                // `conditional_data`.
                .filter(|res| match res {
                    Ok((true, _, _, _)) => true,
                    Ok((false, _, _, _)) => false,
                    Err(_) => true,
                })
                // Map out the boolean from the condition, because we don't need that later on
                .map(|res| res.map(|(_, name, vers, deptype)| (name, vers, deptype)))
        }

        let deps = get_package_dependencies(&p, conditional_data);
        let mut d = Vec::new();
        //print!("{:?}", deps);
        for dep in deps {
            println!("{:?}", dep);
            let dep = dep.unwrap();
            let pkgs = repo.find_with_version(&dep.0, &dep.1);
            if pkgs.is_empty() {
                panic!("dep not found");
                //return Err(anyhow!(
                //    "Dependency of {} {} not found: {} {}",
                //    p.name(),
                //    p.version(),
                //    name,
                //    constr
                //));
            }
            trace!("Found in repo: {:?}", pkgs);
            assert!(pkgs.len() == 1);
            let pkg = pkgs[0];
            let subtree = build_dependencies_tree(pkg.clone(), repo, conditional_data);
            //let tree = DependenciesNode {
            //    name: dep.as_ref().unwrap().0.to_string(),
            //    dependency_type: dep.unwrap().2,
            //    dependencies: subtree,
            //};
            d.push((subtree, dep.2));
        }
        println!("{:?}", d.len());
        let tree = DependenciesNode {
            name: p.name().to_string(),
            //dependency_type: DependencyType::BUILDTIME,
            dependencies: d,
        };
        //println!("{:?}", d);
        println!("{:?}", tree);
        println!("{:?}", tree.dependencies);
        return tree;
}

/// Implementation of the "tree_of" subcommand
pub async fn tree_of(matches: &ArgMatches, repo: Repository) -> Result<()> {
    let pname = matches
        .get_one::<String>("package_name")
        .map(|s| s.to_owned())
        .map(PackageName::from);
    let pvers = matches
        .get_one::<String>("package_version")
        .map(|s| s.to_owned())
        .map(PackageVersionConstraint::try_from)
        .transpose()?;

    let image_name = matches
        .get_one::<String>("image")
        .map(|s| s.to_owned())
        .map(ImageName::from);

    let additional_env = matches
        .get_many::<String>("env")
        .unwrap_or_default()
        .map(AsRef::as_ref)
        .map(crate::util::env::parse_to_env)
        .collect::<Result<Vec<(EnvironmentVariableName, String)>>>()?;

    let condition_data = ConditionData {
        image_name: image_name.as_ref(),
        env: &additional_env,
    };

    let mut tree = repo.packages()
        .filter(|p| pname.as_ref().map(|n| p.name() == n).unwrap_or(true))
        .filter(|p| {
            pvers
                .as_ref()
                .map(|v| v.matches(p.version()))
                .unwrap_or(true)
        })
        .map(|package| {
            let tree = build_dependencies_tree(package.clone(), &repo, &condition_data);
            println!("{:?}", tree);
            tree
        })
        .collect::<Vec<_>>();
    //println!("{:?}", tree[0]);
    print_dependencies_tree(tree.pop().unwrap(), 0, false);
    Ok(())
       // .and_then_ok(|tree| {
       //     print!("{:?}", tree);
       // })
       // .collect::<Result<()>>()
}
