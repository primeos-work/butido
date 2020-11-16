use std::path::PathBuf;
use std::result::Result as RResult;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use anyhow::anyhow;
use diesel::PgConnection;
use futures::FutureExt;
use indicatif::ProgressBar;
use itertools::Itertools;
use tokio::stream::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

use crate::endpoint::Endpoint;
use crate::endpoint::EndpointConfiguration;
use crate::filestore::StagingStore;
use crate::job::RunnableJob;
use crate::log::LogItem;
use crate::util::progress::ProgressBars;
use crate::endpoint::ContainerError;

pub struct EndpointScheduler {
    log_dir: Option<PathBuf>,
    endpoints: Vec<Arc<RwLock<Endpoint>>>,

    staging_store: Arc<RwLock<StagingStore>>,
    db: Arc<PgConnection>,
    progressbars: ProgressBars,
    submit: crate::db::models::Submit,
    additional_env: Vec<(String, String)>,
}

impl EndpointScheduler {

    pub async fn setup(endpoints: Vec<EndpointConfiguration>, staging_store: Arc<RwLock<StagingStore>>, db: Arc<PgConnection>, progressbars: ProgressBars, submit: crate::db::models::Submit, log_dir: Option<PathBuf>, additional_env: Vec<(String, String)>) -> Result<Self> {
        let endpoints = Self::setup_endpoints(endpoints).await?;

        Ok(EndpointScheduler {
            log_dir,
            endpoints,
            staging_store,
            db,
            progressbars,
            submit,
            additional_env,
        })
    }

    async fn setup_endpoints(endpoints: Vec<EndpointConfiguration>) -> Result<Vec<Arc<RwLock<Endpoint>>>> {
        let unordered = futures::stream::FuturesUnordered::new();

        for cfg in endpoints.into_iter() {
            unordered.push({
                Endpoint::setup(cfg)
                    .map(|r_ep| {
                        r_ep.map(RwLock::new)
                            .map(Arc::new)
                    })
            });
        }

        unordered.collect().await
    }

    /// Schedule a Job
    ///
    /// # Warning
    ///
    /// This function blocks as long as there is no free endpoint available!
    pub async fn schedule_job(&self, job: RunnableJob, multibar: Arc<indicatif::MultiProgress>) -> Result<JobHandle> {
        let endpoint = self.select_free_endpoint().await?;

        Ok(JobHandle {
            log_dir: self.log_dir.clone(),
            bar: multibar.add(self.progressbars.job_bar(job.uuid())),
            endpoint,
            job,
            staging_store: self.staging_store.clone(),
            db: self.db.clone(),
            submit: self.submit.clone(),
            additional_env: self.additional_env.clone(),
        })
    }

    async fn select_free_endpoint(&self) -> Result<Arc<RwLock<Endpoint>>> {
        loop {
            let unordered = futures::stream::FuturesUnordered::new();
            for ep in self.endpoints.iter().cloned() {
                unordered.push(async move {
                    let wl = ep.write().await;
                    wl.number_of_running_containers().await.map(|u| (u, ep.clone()))
                });
            }

            let endpoints = unordered.collect::<Result<Vec<_>>>().await?;

            if let Some(endpoint) = endpoints
                .iter()
                .sorted_by(|tpla, tplb| tpla.0.cmp(&tplb.0))
                .map(|tpl| tpl.1.clone())
                .next()
            {
                return Ok(endpoint)
            }
        }
    }

}

pub struct JobHandle {
    log_dir: Option<PathBuf>,
    endpoint: Arc<RwLock<Endpoint>>,
    job: RunnableJob,
    bar: ProgressBar,
    db: Arc<PgConnection>,
    staging_store: Arc<RwLock<StagingStore>>,
    submit: crate::db::models::Submit,
    additional_env: Vec<(String, String)>,
}

impl std::fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::result::Result<(), std::fmt::Error> {
        write!(f, "JobHandle ( job: {} )", self.job.uuid())
    }
}

impl JobHandle {
    pub async fn run(self) -> RResult<Vec<PathBuf>, ContainerError> {
        use crate::db::models as dbmodels;
        let (log_sender, log_receiver) = tokio::sync::mpsc::unbounded_channel::<LogItem>();
        let ep = self.endpoint
            .read()
            .await;

        let endpoint = dbmodels::Endpoint::create_or_fetch(&self.db, ep.name())?;
        let package  = dbmodels::Package::create_or_fetch(&self.db, self.job.package())?;
        let image    = dbmodels::Image::create_or_fetch(&self.db, self.job.image())?;

        let job_id = self.job.uuid().clone();
        trace!("Running on Job {} on Endpoint {}", job_id, ep.name());
        let res = ep
            .run_job(self.job, log_sender, self.staging_store, self.additional_env);

        let logres = LogReceiver {
            log_dir: self.log_dir.as_ref(),
            job_id,
            log_receiver,
            bar: &self.bar,
            db: self.db.clone(),
        }.join();

        let (res, logres) = tokio::join!(res, logres);

        trace!("Found result for job {}: {:?}", job_id, res);
        let log = logres.with_context(|| anyhow!("Collecting logs for job on '{}'", ep.name()))?;
        let (paths, container_hash, script) = res.with_context(|| anyhow!("Running job on '{}'", ep.name()))?;

        dbmodels::Job::create(&self.db, &job_id, &self.submit, &endpoint, &package, &image, &container_hash, &script, &log)?;
        Ok(paths)
    }

}

struct LogReceiver<'a> {
    log_dir: Option<&'a PathBuf>,
    job_id: Uuid,
    log_receiver: UnboundedReceiver<LogItem>,
    bar: &'a ProgressBar,
    db: Arc<PgConnection>,
}

impl<'a> LogReceiver<'a> {
    async fn join(mut self) -> Result<String> {
        use resiter::Map;

        let mut logfile = if let Some(log_dir) = self.log_dir.as_ref() {
            Some({
                let path = log_dir.join(self.job_id.to_string()).join(".log");
                tokio::fs::OpenOptions::new()
                    .create(true)
                    .create_new(true)
                    .write(true)
                    .open(path)
                    .await
                    .map(tokio::io::BufWriter::new)?
            })
        } else {
            None
        };

        let mut success = None;
        let mut accu    = vec![];

        while let Some(logitem) = self.log_receiver.recv().await {
            if let Some(lf) = logfile.as_mut() {
                lf.write_all(logitem.display()?.to_string().as_bytes()).await?;
                lf.write_all("\n".as_bytes()).await?;
            }

            match logitem {
                LogItem::Line(ref l) => {
                    // ignore
                },
                LogItem::Progress(u) => {
                    trace!("Setting bar to {}", u as u64);
                    self.bar.set_position(u as u64);
                    self.bar.set_message(&format!("Job: {} running...", self.job_id));
                },
                LogItem::CurrentPhase(ref phasename) => {
                    trace!("Setting bar phase to {}", phasename);
                    self.bar.set_message(&format!("Job: {} Phase: {}", self.job_id, phasename));
                },
                LogItem::State(Ok(ref s)) => {
                    trace!("Setting bar state to Ok: {}", s);
                    self.bar.set_message(&format!("Job: {} State Ok: {}", self.job_id, s));
                    success = Some(true);
                },
                LogItem::State(Err(ref e)) => {
                    trace!("Setting bar state to Err: {}", e);
                    self.bar.set_message(&format!("Job: {} State Err: {}", self.job_id, e));
                    success = Some(false);
                },
            }
            accu.push(logitem);
        }

        trace!("Finishing bar = {:?}", success);
        match success {
            Some(true) => self.bar.finish_with_message(&format!("Job: {} finished successfully", self.job_id)),
            Some(false) => self.bar.finish_with_message(&format!("Job: {} finished with error", self.job_id)),
            None => self.bar.finish_with_message(&format!("Job: {} finished", self.job_id)),
        }

        drop(self.bar);
        if let Some(mut lf) = logfile {
            let _ = lf.flush().await?;
        }

        Ok({
            accu.into_iter()
                .map(|ll| ll.display())
                .map_ok(|d| d.to_string())
                .collect::<Result<Vec<String>>>()?
                .join("\n")
        })
    }
}

