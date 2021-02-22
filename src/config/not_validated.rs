//
// Copyright (c) 2020-2021 science+computing ag and other contributors
//
// This program and the accompanying materials are made
// available under the terms of the Eclipse Public License 2.0
// which is available at https://www.eclipse.org/legal/epl-2.0/
//
// SPDX-License-Identifier: EPL-2.0
//

use anyhow::anyhow;
use anyhow::Context;
use anyhow::Result;
use getset::Getters;
use serde::Deserialize;
use std::path::PathBuf;

use crate::config::util::*;
use crate::config::Configuration;
use crate::config::ContainerConfig;
use crate::config::DockerConfig;
use crate::package::PhaseName;

/// The configuration that is loaded from the filesystem
#[derive(Debug, Getters, Deserialize)]
pub struct NotValidatedConfiguration {
    #[getset(get = "pub")]
    compatibility: semver::VersionReq,

    #[getset(get = "pub")]
    log_dir: PathBuf,

    #[serde(default = "default_strict_script_interpolation")]
    #[getset(get = "pub")]
    strict_script_interpolation: bool,

    #[serde(default = "default_progress_format")]
    #[getset(get = "pub")]
    progress_format: String,

    #[serde(default = "default_spinner_format")]
    #[getset(get = "pub")]
    spinner_format: String,

    #[serde(default = "default_package_print_format")]
    #[getset(get = "pub")]
    package_print_format: String,

    #[serde(default = "default_build_error_lines")]
    #[getset(get = "pub")]
    build_error_lines: usize,

    #[getset(get = "pub")]
    script_highlight_theme: Option<String>,

    #[getset(get = "pub")]
    script_linter: Option<PathBuf>,

    #[serde(default = "default_script_shebang")]
    #[getset(get = "pub")]
    shebang: String,

    #[serde(rename = "releases_root")]
    #[getset(get = "pub")]
    releases_directory: PathBuf,

    #[serde(rename = "release_stores")]
    #[getset(get = "pub")]
    release_stores: Vec<String>,

    #[serde(rename = "staging")]
    #[getset(get = "pub")]
    staging_directory: PathBuf,

    #[serde(rename = "source_cache")]
    #[getset(get = "pub")]
    source_cache_root: PathBuf,

    #[getset(get = "pub")]
    #[serde(rename = "database_host")]
    database_host: String,

    #[getset(get = "pub")]
    #[serde(rename = "database_port")]
    database_port: String,

    #[getset(get = "pub")]
    #[serde(rename = "database_user")]
    database_user: String,

    #[getset(get = "pub")]
    #[serde(rename = "database_password")]
    database_password: String,

    #[getset(get = "pub")]
    #[serde(rename = "database_name")]
    database_name: String,

    #[getset(get = "pub")]
    docker: DockerConfig,

    #[getset(get = "pub")]
    containers: ContainerConfig,

    #[getset(get = "pub")]
    available_phases: Vec<PhaseName>,
}

impl NotValidatedConfiguration {
    /// Validate the NotValidatedConfiguration object and make it into a Configuration object, if
    /// validation succeeds
    ///
    /// This function does sanity-checking on the configuration values.
    /// It fails with the appropriate error message if a setting is bogus.
    pub fn validate(self) -> Result<Configuration> {
        let crate_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
            .context("Parsing version of crate (CARGO_PKG_VERSION) into semver::Version object")?;

        if !self.compatibility.matches(&crate_version) {
            return Err(anyhow!(
                "Configuration is not compatible to butido {}",
                crate_version
            ));
        }

        // Error if linter is not a file
        if let Some(linter) = self.script_linter.as_ref() {
            if !linter.is_file() {
                return Err(anyhow!("Lint script is not a file: {}", linter.display()));
            }
        }

        // Error if staging_directory is not a directory
        if !self.staging_directory.is_dir() {
            return Err(anyhow!(
                "Not a directory: staging = {}",
                self.staging_directory.display()
            ));
        }

        // Error if releases_directory is not a directory
        if !self.releases_directory.is_dir() {
            return Err(anyhow!(
                "Not a directory: releases = {}",
                self.releases_directory.display()
            ));
        }

        if self.release_stores.is_empty() {
            return Err(anyhow!("You need at least one release store in 'release_stores'"))
        }

        // Error if source_cache_root is not a directory
        if !self.source_cache_root.is_dir() {
            return Err(anyhow!(
                "Not a directory: releases = {}",
                self.source_cache_root.display()
            ));
        }

        // Error if there are no phases configured
        if self.available_phases.is_empty() {
            return Err(anyhow!("No phases configured"));
        }

        // Error if script highlighting theme is not valid
        if let Some(configured_theme) = self.script_highlight_theme.as_ref() {
            let allowed_theme_present = [
                // from syntect
                "base16-ocean.dark",
                "base16-eighties.dark",
                "base16-mocha.dark",
                "base16-ocean.light",
                "InspiredGitHub",
                "Solarized (dark)",
                "Solarized (light)",
            ]
            .iter()
            .any(|allowed_theme| configured_theme == *allowed_theme);

            if !allowed_theme_present {
                return Err(anyhow!("Theme not known: {}", configured_theme));
            }
        }

        Ok(Configuration { inner: self })
    }
}
