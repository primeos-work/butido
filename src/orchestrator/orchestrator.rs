//
// Copyright (c) 2020-2021 science+computing ag and other contributors
//
// This program and the accompanying materials are made
// available under the terms of the Eclipse Public License 2.0
// which is available at https://www.eclipse.org/legal/epl-2.0/
//
// SPDX-License-Identifier: EPL-2.0
//

#![allow(unused)]

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Error;
use anyhow::Result;
use anyhow::anyhow;
use diesel::PgConnection;
use indicatif::ProgressBar;
use log::trace;
use tokio::stream::StreamExt;
use tokio::sync::RwLock;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::Sender;
use typed_builder::TypedBuilder;
use uuid::Uuid;

use crate::config::Configuration;
use crate::db::models as dbmodels;
use crate::endpoint::EndpointConfiguration;
use crate::endpoint::EndpointScheduler;
use crate::filestore::Artifact;
use crate::filestore::MergedStores;
use crate::filestore::ReleaseStore;
use crate::filestore::StagingStore;
use crate::job::JobDefinition;
use crate::job::RunnableJob;
use crate::job::Tree as JobTree;
use crate::source::SourceCache;
use crate::util::progress::ProgressBars;

pub struct Orchestrator<'a> {
    scheduler: EndpointScheduler,
    progress_generator: ProgressBars,
    merged_stores: MergedStores,
    source_cache: SourceCache,
    jobtree: JobTree,
    config: &'a Configuration,
    database: Arc<PgConnection>,
}

#[derive(TypedBuilder)]
pub struct OrchestratorSetup<'a> {
    progress_generator: ProgressBars,
    endpoint_config: Vec<EndpointConfiguration>,
    staging_store: Arc<RwLock<StagingStore>>,
    release_store: Arc<RwLock<ReleaseStore>>,
    source_cache: SourceCache,
    jobtree: JobTree,
    database: Arc<PgConnection>,
    submit: dbmodels::Submit,
    log_dir: Option<PathBuf>,
    config: &'a Configuration,
}

impl<'a> OrchestratorSetup<'a> {
    pub async fn setup(self) -> Result<Orchestrator<'a>> {
        let scheduler = EndpointScheduler::setup(
            self.endpoint_config,
            self.staging_store.clone(),
            self.database.clone(),
            self.submit.clone(),
            self.log_dir,
        )
        .await?;

        Ok(Orchestrator {
            scheduler,
            progress_generator: self.progress_generator,
            merged_stores: MergedStores::new(self.release_store, self.staging_store),
            source_cache: self.source_cache,
            jobtree: self.jobtree,
            config: self.config,
            database: self.database,
        })
    }
}

/// Helper type
///
/// Represents a result that came from the run of a job inside a container
///
/// It is either a list of artifacts (with their respective database artifact objects)
/// or a UUID and an Error object, where the UUID is the job UUID and the error is the
/// anyhow::Error that was issued.
type JobResult = std::result::Result<(Uuid, Vec<Artifact>), Vec<(Uuid, Error)>>;

impl<'a> Orchestrator<'a> {
    pub async fn run(self, output: &mut Vec<Artifact>) -> Result<Vec<(Uuid, Error)>> {
        let (results, errors) = self.run_tree().await?;
        output.extend(results.into_iter());
        Ok(errors)
    }

    async fn run_tree(self) -> Result<(Vec<Artifact>, Vec<(Uuid, Error)>)> {
        let multibar = Arc::new(indicatif::MultiProgress::new());

        // For each job in the jobtree, built a tuple with
        //
        // 1. The receiver that is used by the task to receive results from dependency tasks from
        // 2. The task itself (as a TaskPreparation object)
        // 3. The sender, that can be used to send results to this task
        // 4. An Option<Sender> that this tasks uses to send its results with
        //    This is an Option<> because we need to set it later and the root of the tree needs a
        //    special handling, as this very function will wait on a receiver that gets the results
        //    of the root task
        let jobs: Vec<(Receiver<JobResult>, TaskPreparation, Sender<JobResult>, _)> = self.jobtree
            .inner()
            .iter()
            .map(|(uuid, jobdef)| {
                // We initialize the channel with 100 elements here, as there is unlikely a task
                // that depends on 100 other tasks.
                // Either way, this might be increased in future.
                let (sender, receiver) = tokio::sync::mpsc::channel(100);

                trace!("Creating TaskPreparation object for job {}", uuid);
                let tp = TaskPreparation {
                    uuid: *uuid,
                    jobdef,

                    bar: multibar.add(self.progress_generator.bar()),
                    config: self.config,
                    source_cache: &self.source_cache,
                    scheduler: &self.scheduler,
                    merged_stores: &self.merged_stores,
                    database: self.database.clone(),
                };

                (receiver, tp, sender, std::cell::RefCell::new(None as Option<Sender<JobResult>>))
            })
            .collect();

        // Associate tasks with their appropriate sender
        //
        // Right now, the tuple yielded from above contains (rx, task, tx, _), where rx and tx belong
        // to eachother.
        // But what we need is the tx (sender) that the task should send its result to, of course.
        //
        // So this algorithm in plain text is:
        //   for each job
        //      find the job that depends on this job
        //      use the sender of the found job and set it as sender for this job
        for job in jobs.iter() {
            *job.3.borrow_mut() = jobs.iter()
                .find(|j| j.1.jobdef.dependencies.contains(&job.1.uuid))
                .map(|j| j.2.clone());
        }

        // Find the id of the root task
        //
        // By now, all tasks should be associated with their respective sender.
        // Only one has None sender: The task that is the "root" of the tree.
        // By that property, we can find the root task.
        //
        // Here, we copy its uuid, because we need it later.
        let root_job_id = jobs.iter()
            .find(|j| j.3.borrow().is_none())
            .map(|j| j.1.uuid)
            .ok_or_else(|| anyhow!("Failed to find root task"))?;
        trace!("Root job id = {}", root_job_id);

        // Create a sender and a receiver for the root of the tree
        let (root_sender, mut root_receiver) = tokio::sync::mpsc::channel(100);

        // Make all prepared jobs into real jobs and run them
        //
        // This maps each TaskPreparation with its sender and receiver to a JobTask and calls the
        // async fn JobTask::run() to run the task.
        //
        // The JobTask::run implementation handles the rest, we just have to wait for all futures
        // to succeed.
        let running_jobs = jobs
            .into_iter()
            .map(|prep| {
                trace!("Creating JobTask for = {}", prep.1.uuid);
                let root_sender = root_sender.clone();
                JobTask {
                    uuid: prep.1.uuid,
                    jobdef: prep.1.jobdef,

                    bar: prep.1.bar.clone(),

                    config: prep.1.config,
                    source_cache: prep.1.source_cache,
                    scheduler: prep.1.scheduler,
                    merged_stores: prep.1.merged_stores,
                    database: prep.1.database.clone(),

                    receiver: prep.0,

                    // the sender is set or we need to use the root sender
                    sender: prep.3.into_inner().unwrap_or(root_sender),
                }
            })
            .map(|task| task.run())
            .collect::<futures::stream::FuturesUnordered<_>>()
            .collect::<Result<()>>();

        let root_recv = root_receiver.recv();
        let multibar_block = tokio::task::spawn_blocking(move || multibar.join());

        let (root_recv, _, jobs_result) = tokio::join!(root_recv, multibar_block, running_jobs);
        let _ = jobs_result?;
        match root_recv {
            None                     => Err(anyhow!("No result received...")),
            Some(Ok((_, artifacts))) => Ok((artifacts, vec![])),
            Some(Err(errors))        => Ok((vec![], errors)),
        }
    }
}

/// Helper type: A task with all things attached, but not sender and receivers
///
/// This is the preparation of the JobTask, but without the associated sender and receiver, because
/// it is not mapped to the task yet.
///
/// This simply holds data and does not contain any more functionality
struct TaskPreparation<'a> {
    /// The UUID of this job
    uuid: Uuid,
    jobdef: &'a JobDefinition,

    bar: ProgressBar,

    config: &'a Configuration,
    source_cache: &'a SourceCache,
    scheduler: &'a EndpointScheduler,
    merged_stores: &'a MergedStores,
    database: Arc<PgConnection>,
}

/// Helper type for executing one job task
///
/// This type represents a task for a job that can immediately be executed (see `JobTask::run()`).
struct JobTask<'a> {
    /// The UUID of this job
    uuid: Uuid,
    jobdef: &'a JobDefinition,

    bar: ProgressBar,

    config: &'a Configuration,
    source_cache: &'a SourceCache,
    scheduler: &'a EndpointScheduler,
    merged_stores: &'a MergedStores,
    database: Arc<PgConnection>,

    /// Channel where the dependencies arrive
    receiver: Receiver<JobResult>,

    /// Channel to send the own build outputs to
    sender: Sender<JobResult>,
}

impl<'a> JobTask<'a> {

    /// Run the job
    ///
    /// This function runs the job from this object on the scheduler as soon as all dependend jobs
    /// returned successfully.
    async fn run(mut self) -> Result<()> {
        trace!("[{}]: Running", self.uuid);

        // A list of job run results from dependencies that were received from the tasks for the
        // dependencies
        let mut received_dependencies: Vec<(Uuid, Vec<Artifact>)> = vec![];

        // A list of errors that were received from the tasks for the dependencies
        let mut received_errors: Vec<(Uuid, Error)> = vec![];

        // Helper function to check whether all UUIDs are in a list of UUIDs
        let all_dependencies_are_in = |dependency_uuids: &[Uuid], list: &[(Uuid, Vec<_>)]| {
            dependency_uuids.iter().all(|dependency_uuid| {
                list.iter().map(|tpl| tpl.0).any(|id| id == *dependency_uuid)
            })
        };

        // as long as the job definition lists dependencies that are not in the received_dependencies list...
        while !all_dependencies_are_in(&self.jobdef.dependencies, &received_dependencies) {
            // Update the status bar message
            self.bar.set_message(&format!("Waiting ({}/{})...", received_dependencies.len(), self.jobdef.dependencies.len()));
            trace!("[{}]: Updated bar", self.uuid);

            trace!("[{}]: receiving...", self.uuid);
            // receive from the receiver
            match self.receiver.recv().await {
                Some(Ok(v)) => {
                    // The task we depend on succeeded and returned an
                    // (uuid of the job, [Artifact])
                    trace!("[{}]: Received: {:?}", self.uuid, v);
                    received_dependencies.push(v)
                },
                Some(Err(mut e)) => {
                    // The task we depend on failed
                    // we log that error for now
                    trace!("[{}]: Received: {:?}", self.uuid, e);
                    received_errors.append(&mut e);
                },
                None => {
                    // The task we depend on finished... we must check what we have now...
                    trace!("[{}]: Received nothing, channel seems to be empty", self.uuid);

                    // Find all dependencies that we need but which are not received
                    let received = received_dependencies.iter().map(|tpl| tpl.0).collect::<Vec<_>>();
                    let missing_deps: Vec<_> = self.jobdef
                        .dependencies
                        .iter()
                        .filter(|d| !received.contains(d))
                        .collect();
                    trace!("[{}]: Missing dependencies = {:?}", self.uuid, missing_deps);

                    // ... if there are any, error
                    if !missing_deps.is_empty() {
                        return Err(anyhow!("Childs finished, but dependencies still missing: {:?}", missing_deps))
                    } else {
                        // all dependencies are received
                        break;
                    }
                },
            }

            trace!("[{}]: Received errors = {:?}", self.uuid, received_errors);
            // if there are any errors from child tasks
            if !received_errors.is_empty() {
                // send them to the parent,...
                self.sender.send(Err(received_errors)).await;

                // ... and stop operation, because the whole tree will fail anyways.
                return Ok(())
            }
        }

        // Map the list of received dependencies from
        //      Vec<(Uuid, Vec<Artifact>)>
        // to
        //      Vec<Artifact>
        let dependency_artifacts = received_dependencies
            .iter()
            .map(|tpl| tpl.1.iter())
            .flatten()
            .cloned()
            .collect();
        trace!("[{}]: Dependency artifacts = {:?}", self.uuid, dependency_artifacts);
        self.bar.set_message("Preparing...");

        // Create a RunnableJob object
        let runnable = RunnableJob::build_from_job(
            &self.jobdef.job,
            self.source_cache,
            self.config,
            dependency_artifacts)
            .await?;

        self.bar.set_message("Scheduling...");
        let job_uuid = *self.jobdef.job.uuid();

        // Schedule the job on the scheduler
        match self.scheduler.schedule_job(runnable, self.bar).await?.run().await {
            // if the scheduler run reports an error,
            // that is an error from the actual execution of the job ...
            Err(e) => {
                trace!("[{}]: Scheduler returned error = {:?}", self.uuid, e);
                // ... and we send that to our parent
                self.sender.send(Err(vec![(job_uuid, e)])).await?;
            },

            // if the scheduler run reports success,
            // it returns the database artifact objects it created!
            Ok(mut artifacts) => {
                trace!("[{}]: Scheduler returned artifacts = {:?}", self.uuid, artifacts);
                artifacts.extend(received_dependencies.into_iter().map(|(_, v)| v.into_iter()).flatten());
                self.sender
                    .send(Ok((self.uuid, artifacts)))
                    .await?;
            },
        }

        trace!("[{}]: Finished successfully", self.uuid);
        Ok(())
    }
}

