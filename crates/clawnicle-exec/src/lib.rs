//! Workflow scheduler for Clawnicle.
//!
//! A [`Scheduler`] owns a handler registry (`HashMap<name, async fn>`),
//! a journal path, and a concurrency cap. Callers register workflow
//! handlers at startup, then submit workflows onto the pending queue.
//! `run_until_idle` picks up pending workflows, spawns them onto the
//! Tokio runtime bounded by a semaphore, and awaits completion.
//!
//! Crash recovery: `run_until_idle` calls `Journal::recover_abandoned`
//! before entering its pull loop. Any workflow left in `running` state
//! by a prior process is reset to `pending` and picked back up.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use clawnicle_core::{Error, Result};
use clawnicle_journal::Journal;
use clawnicle_runtime::Context;
use serde_json::Value;

type BoxedHandler = Box<
    dyn Fn(Context, Value) -> Pin<Box<dyn Future<Output = Result<Value>> + Send>> + Send + Sync,
>;

pub struct Scheduler {
    db_path: PathBuf,
    handlers: HashMap<String, Arc<BoxedHandler>>,
    concurrency: usize,
}

#[derive(Debug, Default, Clone)]
pub struct RunStats {
    pub completed: u64,
    pub failed: u64,
    pub recovered: u64,
}

impl Scheduler {
    pub fn new(db_path: impl AsRef<Path>) -> Self {
        Self {
            db_path: db_path.as_ref().to_path_buf(),
            handlers: HashMap::new(),
            concurrency: 4,
        }
    }

    pub fn with_concurrency(mut self, n: usize) -> Self {
        self.concurrency = n.max(1);
        self
    }

    /// Register an async workflow handler by name. The handler receives a
    /// [`Context`] pre-opened on the submitted workflow and the raw
    /// deserialized input value; it returns the final JSON-serializable
    /// output (or an error).
    pub fn register<F, Fut>(&mut self, name: impl Into<String>, handler: F)
    where
        F: Fn(Context, Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value>> + Send + 'static,
    {
        let boxed: BoxedHandler = Box::new(move |cx, v| Box::pin(handler(cx, v)));
        self.handlers.insert(name.into(), Arc::new(boxed));
    }

    /// Submit a workflow onto the pending queue. Returns immediately; the
    /// scheduler picks it up on the next `run_until_idle` or concurrent
    /// pull-loop iteration.
    pub fn submit(&self, workflow_id: &str, name: &str, input: Value) -> Result<()> {
        let mut journal = Journal::open(&self.db_path)?;
        journal.submit_pending(name, workflow_id, &input)?;
        Ok(())
    }

    /// Drain every pending workflow, including any that were running in a
    /// prior process and got recovered by the startup scan. Returns counts
    /// of completed, failed, and recovered workflows.
    pub async fn run_until_idle(&self) -> Result<RunStats> {
        let recovered = {
            let mut journal = Journal::open(&self.db_path)?;
            journal.recover_abandoned()?
        };

        let mut stats = RunStats {
            recovered,
            ..Default::default()
        };

        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.concurrency));
        let mut tasks: tokio::task::JoinSet<WorkflowOutcome> = tokio::task::JoinSet::new();

        loop {
            let claimed = {
                let mut journal = Journal::open(&self.db_path)?;
                journal.claim_next_pending()?
            };

            let Some(claimed) = claimed else {
                break;
            };

            let Some(handler) = self.handlers.get(&claimed.name).cloned() else {
                // Handler missing — mark as failed so we don't loop on it forever.
                let mut journal = Journal::open(&self.db_path)?;
                journal.append(
                    &claimed.id,
                    &clawnicle_core::EventPayload::WorkflowFailed {
                        error: format!("no handler registered for workflow '{}'", claimed.name),
                    },
                )?;
                stats.failed += 1;
                continue;
            };

            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| Error::Journal(format!("scheduler semaphore closed: {e}")))?;
            let db_path = self.db_path.clone();

            tasks.spawn(async move {
                let _permit = permit; // released on drop
                execute_one(db_path, handler, claimed).await
            });
        }

        while let Some(res) = tasks.join_next().await {
            match res {
                Ok(WorkflowOutcome::Completed) => stats.completed += 1,
                Ok(WorkflowOutcome::Failed) => stats.failed += 1,
                Err(e) => {
                    // Join error (panic, cancel). Treat as failure.
                    eprintln!("scheduler task panicked: {e}");
                    stats.failed += 1;
                }
            }
        }

        Ok(stats)
    }
}

enum WorkflowOutcome {
    Completed,
    Failed,
}

async fn execute_one(
    db_path: PathBuf,
    handler: Arc<BoxedHandler>,
    claimed: clawnicle_journal::ClaimedWorkflow,
) -> WorkflowOutcome {
    let journal = match Journal::open(&db_path) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("execute_one: cannot open journal: {e}");
            return WorkflowOutcome::Failed;
        }
    };

    let cx = match Context::open_or_start(journal, &claimed.id, &claimed.name, "") {
        Ok(cx) => cx,
        Err(e) => {
            eprintln!("execute_one: cannot open context for {}: {e}", claimed.id);
            return WorkflowOutcome::Failed;
        }
    };

    match (handler)(cx, claimed.input).await {
        Ok(_output) => WorkflowOutcome::Completed,
        Err(err) => {
            // Workflow function returned Err. Try to record it.
            if let Ok(mut journal) = Journal::open(&db_path) {
                let _ = journal.append(
                    &claimed.id,
                    &clawnicle_core::EventPayload::WorkflowFailed {
                        error: err.to_string(),
                    },
                );
            }
            WorkflowOutcome::Failed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use tempfile::tempdir;

    #[tokio::test]
    async fn runs_submitted_workflows_to_completion() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("j.db");

        let counter = Arc::new(AtomicU32::new(0));
        let mut scheduler = Scheduler::new(&db).with_concurrency(4);

        let c = counter.clone();
        scheduler.register("adder", move |mut cx, input| {
            let c = c.clone();
            async move {
                let n = input.as_u64().unwrap_or(0) as u32;
                c.fetch_add(n, Ordering::SeqCst);
                cx.complete(&n)?;
                Ok(serde_json::json!(n))
            }
        });

        for i in 1..=5u32 {
            scheduler
                .submit(&format!("wf-{i}"), "adder", serde_json::json!(i))
                .unwrap();
        }

        let stats = scheduler.run_until_idle().await.unwrap();
        assert_eq!(stats.completed, 5);
        assert_eq!(stats.failed, 0);
        assert_eq!(counter.load(Ordering::SeqCst), 1 + 2 + 3 + 4 + 5);

        // Every workflow should be in terminal state now.
        let journal = Journal::open(&db).unwrap();
        for i in 1..=5u32 {
            assert_eq!(
                journal
                    .workflow_status(&format!("wf-{i}"))
                    .unwrap()
                    .as_deref(),
                Some("completed")
            );
        }
    }

    #[tokio::test]
    async fn handler_returning_error_marks_workflow_failed() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("j.db");
        let mut scheduler = Scheduler::new(&db);
        scheduler.register("broken", |_cx, _input| async {
            Err(Error::Tool("nope".into()))
        });
        scheduler
            .submit("wf-x", "broken", serde_json::Value::Null)
            .unwrap();

        let stats = scheduler.run_until_idle().await.unwrap();
        assert_eq!(stats.completed, 0);
        assert_eq!(stats.failed, 1);

        let journal = Journal::open(&db).unwrap();
        assert_eq!(
            journal.workflow_status("wf-x").unwrap().as_deref(),
            Some("failed")
        );
    }

    #[tokio::test]
    async fn unknown_workflow_name_fails_fast() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("j.db");
        let scheduler = Scheduler::new(&db);
        scheduler
            .submit("wf-y", "never-registered", serde_json::Value::Null)
            .unwrap();

        let stats = scheduler.run_until_idle().await.unwrap();
        assert_eq!(stats.failed, 1);
        let journal = Journal::open(&db).unwrap();
        assert_eq!(
            journal.workflow_status("wf-y").unwrap().as_deref(),
            Some("failed")
        );
    }

    #[tokio::test]
    async fn recovery_scan_picks_up_abandoned_workflows() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("j.db");

        // Simulate a prior process that claimed but did not finish.
        {
            let mut journal = Journal::open(&db).unwrap();
            journal
                .submit_pending("recover", "wf-r", &serde_json::json!("hello"))
                .unwrap();
            journal.claim_next_pending().unwrap(); // status now 'running'
        }

        let mut scheduler = Scheduler::new(&db);
        scheduler.register("recover", |mut cx, input| async move {
            cx.complete(&input)?;
            Ok(input)
        });

        let stats = scheduler.run_until_idle().await.unwrap();
        assert_eq!(
            stats.recovered, 1,
            "startup scan must reset running -> pending"
        );
        assert_eq!(stats.completed, 1);

        let journal = Journal::open(&db).unwrap();
        assert_eq!(
            journal.workflow_status("wf-r").unwrap().as_deref(),
            Some("completed")
        );
    }
}
