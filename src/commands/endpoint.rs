//
// Copyright (c) 2020-2021 science+computing ag and other contributors
//
// This program and the accompanying materials are made
// available under the terms of the Eclipse Public License 2.0
// which is available at https://www.eclipse.org/legal/epl-2.0/
//
// SPDX-License-Identifier: EPL-2.0
//

use std::str::FromStr;
use std::sync::Arc;

use anyhow::Result;
use anyhow::anyhow;
use clap::ArgMatches;
use log::{debug, info};
use itertools::Itertools;
use tokio_stream::StreamExt;

use crate::config::Configuration;
use crate::util::progress::ProgressBars;

pub async fn endpoint(matches: &ArgMatches, config: &Configuration, progress_generator: ProgressBars) -> Result<()> {
    let endpoint_names = matches
        .value_of("endpoint_name")
        .map(String::from)
        .map(|ep| vec![ep])
        .unwrap_or_else(|| {
            config.docker()
                .endpoints()
                .iter()
                .map(|ep| ep.name())
                .cloned()
                .collect()
        });

    match matches.subcommand() {
        Some(("ping", matches)) => ping(endpoint_names, matches, config, progress_generator).await,
        Some((other, _)) => Err(anyhow!("Unknown subcommand: {}", other)),
        None => Err(anyhow!("No subcommand")),
    }
}

async fn ping(endpoint_names: Vec<String>,
    matches: &ArgMatches,
    config: &Configuration,
    progress_generator: ProgressBars
) -> Result<()> {
    let n_pings = matches.value_of("ping_n").map(u64::from_str).transpose()?.unwrap(); // safe by clap
    let sleep = matches.value_of("ping_sleep").map(u64::from_str).transpose()?.unwrap(); // safe by clap

    let endpoint_configurations = config
        .docker()
        .endpoints()
        .iter()
        .filter(|ep| endpoint_names.contains(ep.name()))
        .cloned()
        .map(|ep_cfg| {
            crate::endpoint::EndpointConfiguration::builder()
                .endpoint(ep_cfg)
                .required_images(config.docker().images().clone())
                .required_docker_versions(config.docker().docker_versions().clone())
                .required_docker_api_versions(config.docker().docker_api_versions().clone())
                .build()
        })
        .collect::<Vec<_>>();

    info!("Endpoint config build");
    info!("Connecting to {n} endpoints: {eps}", 
        n = endpoint_configurations.len(), 
        eps = endpoint_configurations.iter().map(|epc| epc.endpoint().name()).join(", "));

    let endpoints = crate::endpoint::util::setup_endpoints(endpoint_configurations).await?;

    let multibar = Arc::new({
        let mp = indicatif::MultiProgress::new();
        if progress_generator.hide() {
            mp.set_draw_target(indicatif::ProgressDrawTarget::hidden());
        }
        mp
    });

    let ping_process = endpoints
        .iter()
        .map(|endpoint| {
            let bar = multibar.add(progress_generator.bar());
            bar.set_length(n_pings);
            bar.set_message(&format!("Pinging {}", endpoint.name()));

            async move {
                for i in 1..(n_pings + 1) {
                    debug!("Pinging {} for the {} time", endpoint.name(), i);
                    let r = endpoint.ping().await;
                    bar.inc(1);
                    if let Err(e) = r {
                        bar.finish_with_message(&format!("Pinging {} failed", endpoint.name()));
                        return Err(e)
                    }

                    tokio::time::sleep(tokio::time::Duration::from_secs(sleep)).await;
                }

                bar.finish_with_message(&format!("Pinging {} successful", endpoint.name()));
                Ok(())
            }
        })
        .collect::<futures::stream::FuturesUnordered<_>>()
        .collect::<Result<()>>();

    let multibar_block = tokio::task::spawn_blocking(move || multibar.join());
    tokio::join!(ping_process, multibar_block).0
}
