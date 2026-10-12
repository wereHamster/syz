use anyhow::Result;
use async_trait::async_trait;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tracing::instrument::WithSubscriber;
use tracing::{Dispatch, Instrument, Span};

use super::database::pk;
use super::job::{Job, JobClass, LaneKey};
use super::message::{Message, Payload};

/// Runs a job to completion. On success, it returns the outcome message to send to the main
/// loop (if any).
#[async_trait]
pub trait Runner: Send + Sync + 'static {
    async fn run(&self, job: Job) -> Result<Option<Payload>>;
}

/// How much work the dispatcher runs at once, and for how long.
#[derive(Debug, Clone)]
pub struct Limits {
    /// How many audit sweeps run at once, across all lanes.
    pub audits: usize,

    /// How many other jobs run at once, across all lanes.
    pub general: usize,

    /// How long a job may run before it is aborted. A hung job would otherwise block every
    /// later job of its lane.
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            audits: 4,
            general: 8,
            timeout: Duration::from_secs(30 * 60),
        }
    }
}

/// Runs jobs on per-repository lanes: each lane runs one job at a time, in the order they were
/// enqueued. Across lanes, the number of running jobs is capped per job class.
///
/// Enqueueing never blocks, so the main loop can do it while processing a message.
#[derive(Clone)]
pub struct Dispatcher {
    inner: Arc<Inner>,
}

struct Inner {
    runner: Arc<dyn Runner>,

    /// Where outcomes are sent: the main loop's mailbox.
    outcomes: mpsc::Sender<Message>,

    audits: Semaphore,
    general: Semaphore,
    timeout: Duration,

    /// The lanes that have work. A lane is removed by its task once it runs out of jobs.
    lanes: Mutex<HashMap<LaneKey, Lane>>,
}

#[derive(Default)]
struct Lane {
    running: Option<Job>,
    queue: VecDeque<Queued>,
}

struct Queued {
    job: Job,

    /// The span and subscriber at the time the job was enqueued. The job runs under them, so
    /// that its logs, which are how failures reach clients, aren't lost.
    span: Span,
    dispatch: Dispatch,
}

/// What a lane is doing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaneStatus {
    pub running: Option<Job>,
    pub queued: Vec<Job>,
}

impl Dispatcher {
    pub fn new(runner: Arc<dyn Runner>, outcomes: mpsc::Sender<Message>, limits: Limits) -> Self {
        Self {
            inner: Arc::new(Inner {
                runner,
                outcomes,
                audits: Semaphore::new(limits.audits),
                general: Semaphore::new(limits.general),
                timeout: limits.timeout,
                lanes: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Queue the job on its lane, unless a queued job of the lane already covers it. A running
    /// job doesn't: the request queues a follow-up, which resolves a fresh base when it starts.
    pub fn enqueue(&self, job: Job) {
        let key = job.lane().clone();

        let mut lanes = self.inner.lanes.lock().unwrap();
        let idle = !lanes.contains_key(&key);
        let lane = lanes.entry(key.clone()).or_default();

        if lane.queue.iter_mut().any(|queued| queued.job.absorb(&job)) {
            tracing::debug!("Not queueing {}, it is already queued", job);
            return;
        }

        lane.queue.push_back(Queued {
            job,
            span: Span::current(),
            dispatch: tracing::dispatcher::get_default(Dispatch::clone),
        });
        drop(lanes);

        if idle {
            tokio::spawn(self.clone().drain(key).with_current_subscriber());
        }
    }

    /// What each lane with work is doing.
    pub fn lanes(&self) -> HashMap<LaneKey, LaneStatus> {
        self.inner
            .lanes
            .lock()
            .unwrap()
            .iter()
            .map(|(key, lane)| {
                let status = LaneStatus {
                    running: lane.running.clone(),
                    queued: lane.queue.iter().map(|queued| queued.job.clone()).collect(),
                };
                (key.clone(), status)
            })
            .collect()
    }

    /// Run the lane's jobs one after the other, until it has none left.
    async fn drain(self, key: LaneKey) {
        loop {
            let queued = {
                let mut lanes = self.inner.lanes.lock().unwrap();
                let lane = lanes
                    .get_mut(&key)
                    .expect("a lane is only removed by its own task");

                match lane.queue.pop_front() {
                    Some(queued) => {
                        lane.running = Some(queued.job.clone());
                        queued
                    }
                    None => {
                        lanes.remove(&key);
                        return;
                    }
                }
            };

            self.run(queued).await;

            if let Some(lane) = self.inner.lanes.lock().unwrap().get_mut(&key) {
                lane.running = None;
            }
        }
    }

    async fn run(&self, queued: Queued) {
        let Queued {
            job,
            span,
            dispatch,
        } = queued;

        let semaphore = match job.class() {
            JobClass::Audit => &self.inner.audits,
            JobClass::General => &self.inner.general,
        };
        let permit = semaphore
            .acquire()
            .await
            .expect("the dispatcher never closes its semaphores");

        let description = job.to_string();
        let runner = self.inner.runner.clone();

        // Spawned, so that a panicking job cannot take the lane down with it.
        let mut task = tokio::spawn(
            async move { runner.run(job).await }
                .instrument(span.clone())
                .with_subscriber(dispatch.clone()),
        );

        let result = match tokio::time::timeout(self.inner.timeout, &mut task).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => Err(anyhow::anyhow!("{}", e)),
            Err(_) => {
                task.abort();
                Err(anyhow::anyhow!("timed out after {:?}", self.inner.timeout))
            }
        };

        drop(permit);

        async {
            match result {
                Ok(Some(payload)) => {
                    // The lane waits until the main loop has applied the outcome, so that the
                    // next job reads the state it left. Otherwise a publish queued behind a
                    // scan could read the targets from before the scan.
                    let (processed, applied) = oneshot::channel();
                    let message = Message {
                        message_id: pk(),
                        payload,
                        processed: Some(processed),
                    };
                    if self.inner.outcomes.send(message).await.is_err() || applied.await.is_err() {
                        tracing::error!(
                            "Failed to report the outcome of {}: application mailbox unreachable",
                            description
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::error!("{} failed: {:#}", description, e),
            }
        }
        .instrument(span)
        .with_subscriber(dispatch)
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::database::SweepKind;
    use crate::core::platform::Platform;
    use futures::future::BoxFuture;
    use futures::FutureExt;

    type Behavior = dyn Fn(Job) -> BoxFuture<'static, Result<Option<Payload>>> + Send + Sync;

    /// Records when jobs start and finish, and runs them as `behavior` says.
    struct FakeRunner {
        log: Arc<Mutex<Vec<String>>>,
        behavior: Box<Behavior>,
    }

    #[async_trait]
    impl Runner for FakeRunner {
        async fn run(&self, job: Job) -> Result<Option<Payload>> {
            let name = job.to_string();
            self.log.lock().unwrap().push(format!("start {}", name));
            let result = (self.behavior)(job).await;
            self.log.lock().unwrap().push(format!("end {}", name));
            result
        }
    }

    struct Harness {
        dispatcher: Dispatcher,
        log: Arc<Mutex<Vec<String>>>,
        outcomes: mpsc::Receiver<Message>,
    }

    impl Harness {
        fn new(
            limits: Limits,
            behavior: impl Fn(Job) -> BoxFuture<'static, Result<Option<Payload>>>
                + Send
                + Sync
                + 'static,
        ) -> Self {
            let log = Arc::new(Mutex::new(Vec::new()));
            let (tx, outcomes) = mpsc::channel(100);
            let runner = FakeRunner {
                log: log.clone(),
                behavior: Box::new(behavior),
            };
            Self {
                dispatcher: Dispatcher::new(Arc::new(runner), tx, limits),
                log,
                outcomes,
            }
        }

        fn log(&self) -> Vec<String> {
            self.log.lock().unwrap().clone()
        }

        /// Wait until every lane ran out of jobs.
        async fn settle(&self) {
            while !self.dispatcher.lanes().is_empty() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }
    }

    fn lane(repository: &str) -> LaneKey {
        LaneKey {
            platform: Platform::GitHub,
            repository: repository.to_string(),
        }
    }

    fn scan(repository: &str, trigger_bumps: bool) -> Job {
        Job::Scan {
            lane: lane(repository),
            project_id: repository.to_string(),
            trigger_bumps,
        }
    }

    fn publish_bump(repository: &str, bump_id: &str) -> Job {
        Job::PublishBump {
            lane: lane(repository),
            bump_id: bump_id.to_string(),
        }
    }

    fn publish_sweep(repository: &str, kind: SweepKind) -> Job {
        Job::PublishSweep {
            lane: lane(repository),
            project_id: repository.to_string(),
            kind,
        }
    }

    fn close(repository: &str, branch: &str) -> Job {
        Job::ClosePullRequests {
            lane: lane(repository),
            branches: vec![branch.to_string()],
        }
    }

    fn sleep(ms: u64) -> BoxFuture<'static, Result<Option<Payload>>> {
        async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            Ok(None)
        }
        .boxed()
    }

    /// The most jobs that were running at the same time, according to the log.
    fn max_overlap(log: &[String]) -> usize {
        let mut running = 0usize;
        let mut max = 0;
        for entry in log {
            if entry.starts_with("start ") {
                running += 1;
                max = max.max(running);
            } else {
                running -= 1;
            }
        }
        max
    }

    #[tokio::test(start_paused = true)]
    async fn runs_the_jobs_of_a_lane_one_at_a_time_in_order() {
        let h = Harness::new(Limits::default(), |_| sleep(10));

        h.dispatcher.enqueue(publish_bump("a", "1"));
        h.dispatcher.enqueue(publish_bump("a", "2"));
        h.dispatcher.enqueue(close("a", "syz/x"));
        h.settle().await;

        assert_eq!(
            h.log(),
            vec![
                "start publish of bump 1 in github:a",
                "end publish of bump 1 in github:a",
                "start publish of bump 2 in github:a",
                "end publish of bump 2 in github:a",
                "start close of syz/x in github:a",
                "end close of syz/x in github:a",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn runs_different_lanes_concurrently() {
        let h = Harness::new(Limits::default(), |_| sleep(10));

        h.dispatcher.enqueue(publish_bump("a", "1"));
        h.dispatcher.enqueue(publish_bump("b", "2"));
        h.settle().await;

        assert_eq!(max_overlap(&h.log()), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn caps_the_general_jobs_across_lanes() {
        let limits = Limits {
            general: 2,
            ..Limits::default()
        };
        let h = Harness::new(limits, |_| sleep(10));

        for repository in ["a", "b", "c", "d"] {
            h.dispatcher.enqueue(publish_bump(repository, "1"));
        }
        h.settle().await;

        assert_eq!(h.log().len(), 8);
        assert_eq!(max_overlap(&h.log()), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn audits_have_a_cap_of_their_own() {
        let limits = Limits {
            audits: 1,
            general: 1,
            ..Limits::default()
        };
        let h = Harness::new(limits, |_| sleep(10));

        h.dispatcher.enqueue(publish_sweep("a", SweepKind::Audit));
        h.dispatcher.enqueue(publish_sweep("b", SweepKind::Audit));
        h.dispatcher
            .enqueue(publish_sweep("c", SweepKind::Transitive));
        h.settle().await;

        // One audit and the transitive sweep run together; the second audit waits.
        assert_eq!(max_overlap(&h.log()), 2);
        assert_eq!(
            &h.log()[..2],
            &[
                "start audit sweep of github:a",
                "start transitive sweep of github:c"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn coalesces_queued_jobs_but_not_the_running_one() {
        let h = Harness::new(Limits::default(), |_| sleep(10));

        h.dispatcher.enqueue(publish_bump("a", "1"));
        // Let the first job start.
        tokio::time::sleep(Duration::from_millis(1)).await;

        h.dispatcher.enqueue(publish_bump("a", "1"));
        h.dispatcher.enqueue(publish_bump("a", "1"));
        h.dispatcher.enqueue(scan("a", false));
        h.dispatcher.enqueue(publish_sweep("a", SweepKind::Audit));
        h.dispatcher.enqueue(scan("a", true));
        h.dispatcher.enqueue(publish_sweep("a", SweepKind::Audit));
        h.dispatcher.enqueue(close("a", "syz/x"));
        h.dispatcher.enqueue(close("a", "syz/x"));

        assert_eq!(
            h.dispatcher.lanes()[&lane("a")],
            LaneStatus {
                running: Some(publish_bump("a", "1")),
                queued: vec![
                    publish_bump("a", "1"),
                    scan("a", true),
                    publish_sweep("a", SweepKind::Audit),
                    close("a", "syz/x"),
                    close("a", "syz/x"),
                ],
            }
        );

        h.settle().await;
    }

    #[tokio::test(start_paused = true)]
    async fn sends_the_outcome_to_the_mailbox() {
        let mut h = Harness::new(Limits::default(), |_| {
            async { Ok(Some(Payload::Purge)) }.boxed()
        });

        h.dispatcher.enqueue(scan("a", false));

        let message = h.outcomes.recv().await.unwrap();
        assert!(matches!(message.payload, Payload::Purge));
    }

    #[tokio::test(start_paused = true)]
    async fn the_next_job_waits_until_the_outcome_is_applied() {
        let mut h = Harness::new(Limits::default(), |job| match job {
            Job::Scan { .. } => async { Ok(Some(Payload::Purge)) }.boxed(),
            _ => sleep(0),
        });

        h.dispatcher.enqueue(scan("a", false));
        h.dispatcher.enqueue(publish_bump("a", "1"));

        let message = h.outcomes.recv().await.unwrap();
        // Give the lane every chance to move on if it didn't wait.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            h.log(),
            vec!["start scan of github:a", "end scan of github:a"]
        );

        message.processed.unwrap().send(()).unwrap();
        h.settle().await;
        assert_eq!(
            &h.log()[2..],
            &[
                "start publish of bump 1 in github:a",
                "end publish of bump 1 in github:a"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_lane_moves_on_if_the_outcome_is_dropped_unapplied() {
        let mut h = Harness::new(Limits::default(), |job| match job {
            Job::Scan { .. } => async { Ok(Some(Payload::Purge)) }.boxed(),
            _ => sleep(0),
        });

        h.dispatcher.enqueue(scan("a", false));
        h.dispatcher.enqueue(publish_bump("a", "1"));

        drop(h.outcomes.recv().await.unwrap());
        h.settle().await;
        assert_eq!(h.log().len(), 4);
    }

    #[tokio::test(start_paused = true)]
    async fn a_panicking_job_does_not_wedge_its_lane() {
        let h = Harness::new(Limits::default(), |job| {
            async move {
                if matches!(job, Job::PublishBump { .. }) {
                    panic!("boom");
                }
                Ok(None)
            }
            .boxed()
        });

        h.dispatcher.enqueue(publish_bump("a", "1"));
        h.dispatcher.enqueue(close("a", "syz/x"));
        h.settle().await;

        assert!(h
            .log()
            .contains(&"end close of syz/x in github:a".to_string()));
    }

    #[tokio::test(start_paused = true)]
    async fn a_hung_job_times_out_and_the_lane_moves_on() {
        let limits = Limits {
            timeout: Duration::from_secs(1),
            ..Limits::default()
        };
        let h = Harness::new(limits, |job| match job {
            Job::PublishBump { .. } => futures::future::pending().boxed(),
            _ => sleep(0),
        });

        h.dispatcher.enqueue(publish_bump("a", "1"));
        h.dispatcher.enqueue(close("a", "syz/x"));
        h.settle().await;

        assert_eq!(
            h.log(),
            vec![
                "start publish of bump 1 in github:a",
                "start close of syz/x in github:a",
                "end close of syz/x in github:a",
            ]
        );
    }

    /// Collects the messages of the log events it sees, with the names of their spans.
    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<String>>>);

    impl<S> tracing_subscriber::Layer<S> for Capture
    where
        S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            struct Message(String);
            impl tracing::field::Visit for Message {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    if field.name() == "message" {
                        self.0 = format!("{:?}", value);
                    }
                }
            }

            let mut message = Message(String::new());
            event.record(&mut message);

            let spans: Vec<&str> = ctx
                .event_scope(event)
                .map(|scope| scope.from_root().map(|span| span.name()).collect())
                .unwrap_or_default();

            self.0
                .lock()
                .unwrap()
                .push(format!("[{}] {}", spans.join("/"), message.0));
        }
    }

    #[tokio::test(start_paused = true)]
    async fn jobs_log_under_the_span_and_subscriber_they_were_enqueued_with() {
        use tracing_subscriber::layer::SubscriberExt;

        let capture = Capture::default();
        let dispatch = Dispatch::new(tracing_subscriber::registry().with(capture.clone()));

        // With a single dispatcher, tracing caches whether a log statement is enabled from the
        // default dispatcher of whichever thread reaches it first, which may be another test's
        // thread that has none. A second one makes it ask every dispatcher, as in production,
        // where the process also has a global one.
        let _second = Dispatch::new(tracing_subscriber::registry());

        let h = Harness::new(Limits::default(), |job| {
            async move {
                tracing::info!("working");
                match job {
                    Job::PublishBump { .. } => Err(anyhow::anyhow!("boom")),
                    _ => Ok(None),
                }
            }
            .boxed()
        });

        // Enqueued like the main loop does: inside the span of a message, under the
        // subscriber that forwards logs to clients.
        async {
            tracing::info_span!("message").in_scope(|| {
                h.dispatcher.enqueue(publish_bump("a", "1"));
            });
        }
        .with_subscriber(dispatch)
        .await;
        h.settle().await;

        assert_eq!(
            *capture.0.lock().unwrap(),
            vec![
                "[message] working",
                "[message] publish of bump 1 in github:a failed: boom",
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_lane_is_removed_once_it_runs_out_of_jobs() {
        let h = Harness::new(Limits::default(), |_| sleep(10));

        h.dispatcher.enqueue(publish_bump("a", "1"));
        assert_eq!(h.dispatcher.lanes().len(), 1);
        h.settle().await;
        assert!(h.dispatcher.lanes().is_empty());

        // And comes back with new work.
        h.dispatcher.enqueue(publish_bump("a", "2"));
        h.settle().await;
        assert_eq!(h.log().len(), 4);
    }
}
