//
// Copyright (c) 2020-2022 science+computing ag and other contributors
//
// This program and the accompanying materials are made
// available under the terms of the Eclipse Public License 2.0
// which is available at https://www.eclipse.org/legal/epl-2.0/
//
// SPDX-License-Identifier: EPL-2.0
//

//! Implementation of the 'metrics' subcommand

use std::io::Write;
use std::path::Path;

use anyhow::Error;
use anyhow::Result;
use diesel::r2d2::ConnectionManager;
use diesel::r2d2::Pool;
use diesel::PgConnection;
use diesel::QueryDsl;
use diesel::RunQueryDsl;
use walkdir::WalkDir;

use crate::config::Configuration;
use crate::repository::Repository;

pub async fn metrics(
    repo_path: &Path,
    config: &Configuration,
    repo: Repository,
    pool: Pool<ConnectionManager<PgConnection>>,
) -> Result<()> {
    let mut out = std::io::stdout();

    let nfiles = WalkDir::new(repo_path)
        .follow_links(true)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|d| d.file_type().is_file())
        .filter(|f| {
            f.path()
                .file_name()
                .map(|name| name == "pkg.toml")
                .unwrap_or(false)
        })
        .count();

    let n_artifacts = async {
        crate::schema::artifacts::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_endpoints = async {
        crate::schema::endpoints::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_envvars = async {
        crate::schema::envvars::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_githashes = async {
        crate::schema::githashes::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_images = async {
        crate::schema::images::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_jobs = async {
        crate::schema::jobs::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_packages = async {
        crate::schema::packages::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_releasestores = async {
        crate::schema::release_stores::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_releases = async {
        crate::schema::releases::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };
    let n_submits = async {
        crate::schema::submits::table
            .count()
            .get_result::<i64>(&mut pool.get().unwrap())
    };

    let (
        n_artifacts,
        n_endpoints,
        n_envvars,
        n_githashes,
        n_images,
        n_jobs,
        n_packages,
        n_releasestores,
        n_releases,
        n_submits,
    ) = tokio::try_join!(
        n_artifacts,
        n_endpoints,
        n_envvars,
        n_githashes,
        n_images,
        n_jobs,
        n_packages,
        n_releasestores,
        n_releases,
        n_submits
    )?;

    write!(
        out,
        "{}",
        indoc::formatdoc!(
            r#"
        Butido release {release}

        Configuration:
        - {configured_endpoints} endpoints
        - {configured_images} images
        - {configured_release_stores} release stores
        - {configured_phases} phases

        Repository:
        - {nfiles} files
        - {repo_packages} packages

        Database:
        - {n_artifacts} artifacts
        - {n_endpoints} endpoints
        - {n_envvars} envvars
        - {n_githashes} githashes
        - {n_images} images
        - {n_jobs} jobs
        - {n_packages} packages
        - {n_releasestores} releasestores
        - {n_releases} releases
        - {n_submits} submits
    "#,
            release = clap::crate_version!(),
            configured_endpoints = config.docker().endpoints().len(),
            configured_images = config.docker().images().len(),
            configured_release_stores = config.release_stores().len(),
            configured_phases = config.available_phases().len(),
            nfiles = nfiles,
            repo_packages = repo.packages().count(),
            n_artifacts = n_artifacts,
            n_endpoints = n_endpoints,
            n_envvars = n_envvars,
            n_githashes = n_githashes,
            n_images = n_images,
            n_jobs = n_jobs,
            n_packages = n_packages,
            n_releasestores = n_releasestores,
            n_releases = n_releases,
            n_submits = n_submits,
        )
    )
    .map_err(Error::from)
}
