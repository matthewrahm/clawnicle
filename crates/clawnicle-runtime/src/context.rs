use std::fmt::Display;
use std::future::Future;
use std::time::{Duration, Instant};

use clawnicle_core::{Error, EventPayload, Result, RetryPolicy};
use clawnicle_journal::Journal;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub struct Context {
    workflow_id: String,
    journal: Journal,
}

impl Context {
    /// Open an existing workflow or start a new one with the given id.
    ///
    /// If a workflow row already exists, the journal is reused as-is; callers
    /// can then invoke [`Context::call`] and the replay engine will
    /// short-circuit previously completed steps (added in a later commit).
    pub fn open_or_start(
        mut journal: Journal,
        workflow_id: impl Into<String>,
        name: &str,
        input_hash: &str,
    ) -> Result<Self> {
        let workflow_id = workflow_id.into();
        if journal.workflow_status(&workflow_id)?.is_none() {
            journal.start_workflow(&workflow_id, name, input_hash, None)?;
        }
        Ok(Self {
            workflow_id,
            journal,
        })
    }

    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    /// Returns the cached final output if the workflow has already completed.
    ///
    /// Used at the top of a workflow function to short-circuit replay when
    /// the workflow previously ran to completion in an earlier process.
    pub fn cached_final_output(&self) -> Result<Option<serde_json::Value>> {
        if self.journal.workflow_status(&self.workflow_id)?.as_deref() != Some("completed") {
            return Ok(None);
        }
        for event in self.journal.read_all(&self.workflow_id)? {
            if let EventPayload::WorkflowCompleted { output } = event.payload {
                return Ok(Some(output));
            }
        }
        Ok(None)
    }

    /// Mark the workflow completed with the given output. Idempotent — if
    /// the workflow is already in terminal state, this is a no-op.
    pub fn complete<T: Serialize>(&mut self, output: &T) -> Result<()> {
        if self.journal.workflow_status(&self.workflow_id)?.as_deref() == Some("completed") {
            return Ok(());
        }
        let value = serde_json::to_value(output)?;
        self.journal.append(
            &self.workflow_id,
            &EventPayload::WorkflowCompleted { output: value },
        )?;
        Ok(())
    }

    /// Execute a tool call once, journaling start and outcome.
    ///
    /// The `step_id` is the idempotency key — the unit used by the replay
    /// engine to detect previously completed work. Pick one that is stable
    /// across restarts and unique per call site (e.g. `"dex_fetch:{mint}"`).
    ///
    /// For retry behaviour, use [`Context::call_with_retry`].
    pub async fn call<Out, Fut, F, E>(&mut self, step_id: &str, tool: F) -> Result<Out>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<Out, E>>,
        Out: Serialize + DeserializeOwned,
        E: Display,
    {
        let mut slot = Some(tool);
        self.call_inner(step_id, RetryPolicy::none(), move || {
            slot.take().expect("FnOnce called twice inside call()")()
        })
        .await
    }

    /// Execute a tool call with the given retry policy.
    ///
    /// On transient failure (closure returns Err, or the attempt times out),
    /// the call is retried up to `policy.max_attempts` times with exponential
    /// backoff bounded by `policy.max_backoff`. Each attempt appends its own
    /// ToolCallStarted and ToolCallFailed/Completed events to the journal.
    pub async fn call_with_retry<Out, Fut, F, E>(
        &mut self,
        step_id: &str,
        policy: RetryPolicy,
        tool: F,
    ) -> Result<Out>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<Out, E>>,
        Out: Serialize + DeserializeOwned,
        E: Display,
    {
        self.call_inner(step_id, policy, tool).await
    }

    async fn call_inner<Out, Fut, F, E>(
        &mut self,
        step_id: &str,
        policy: RetryPolicy,
        mut tool: F,
    ) -> Result<Out>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = std::result::Result<Out, E>>,
        Out: Serialize + DeserializeOwned,
        E: Display,
    {
        if let Some(cached) = self
            .journal
            .lookup_completed_tool_call(&self.workflow_id, step_id)?
        {
            return Ok(serde_json::from_value(cached)?);
        }

        let mut attempt: u32 = 0;
        let mut backoff = policy.initial_backoff;

        loop {
            attempt += 1;

            self.journal.append(
                &self.workflow_id,
                &EventPayload::ToolCallStarted {
                    step_id: step_id.to_string(),
                    name: step_id.to_string(),
                    input_hash: String::new(),
                    attempt,
                },
            )?;

            let start = Instant::now();
            let fut = tool();
            let result: std::result::Result<Out, String> = match policy.timeout {
                Some(t) => match tokio::time::timeout(t, fut).await {
                    Ok(inner) => inner.map_err(|e| e.to_string()),
                    Err(_) => Err(format!("timeout after {t:?}")),
                },
                None => fut.await.map_err(|e| e.to_string()),
            };
            let duration_ms = start.elapsed().as_millis() as u64;

            match result {
                Ok(out) => {
                    let out_value = serde_json::to_value(&out)?;
                    self.journal.append(
                        &self.workflow_id,
                        &EventPayload::ToolCallCompleted {
                            step_id: step_id.to_string(),
                            output: out_value,
                            duration_ms,
                        },
                    )?;
                    return Ok(out);
                }
                Err(msg) => {
                    self.journal.append(
                        &self.workflow_id,
                        &EventPayload::ToolCallFailed {
                            step_id: step_id.to_string(),
                            error: msg.clone(),
                            duration_ms,
                        },
                    )?;

                    if attempt >= policy.max_attempts {
                        return Err(Error::Tool(msg));
                    }

                    tokio::time::sleep(backoff).await;
                    backoff = Duration::from_secs_f32(
                        (backoff.as_secs_f32() * policy.backoff_multiplier)
                            .min(policy.max_backoff.as_secs_f32()),
                    );
                }
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use clawnicle_core::EventPayload;
    use tempfile::tempdir;

    #[tokio::test]
    async fn call_journals_start_and_completion() {
        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();

        let out: u32 = cx
            .call::<u32, _, _, std::io::Error>("add", || async { Ok(3 + 4) })
            .await
            .unwrap();
        assert_eq!(out, 7);

        let cx_journal = Journal::open(dir.path().join("j.db")).unwrap();
        let events = cx_journal.read_all("wf").unwrap();
        // WorkflowStarted + ToolCallStarted + ToolCallCompleted
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[1].payload,
            EventPayload::ToolCallStarted { .. }
        ));
        assert!(matches!(
            events[2].payload,
            EventPayload::ToolCallCompleted { .. }
        ));
    }

    #[tokio::test]
    async fn call_short_circuits_on_replay() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;

        let dir = tempdir().unwrap();
        let db_path = dir.path().join("j.db");
        let calls = Arc::new(AtomicU32::new(0));

        {
            let journal = Journal::open(&db_path).unwrap();
            let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();
            let calls = calls.clone();
            let out: u32 = cx
                .call::<u32, _, _, std::io::Error>("expensive", || {
                    let calls = calls.clone();
                    async move {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Ok(42)
                    }
                })
                .await
                .unwrap();
            assert_eq!(out, 42);
        }

        // Reopen the journal (simulating a restart) and call again with the
        // same step_id — the closure must NOT run again.
        let journal = Journal::open(&db_path).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();
        let out: u32 = cx
            .call::<u32, _, _, std::io::Error>("expensive", || {
                let calls = calls.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(99) // different value; replay must return the cached 42
                }
            })
            .await
            .unwrap();

        assert_eq!(out, 42, "replay must return cached output, not re-execute");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "closure ran exactly once");
    }

    #[tokio::test]
    async fn complete_then_cached_final_output_returns_value() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("j.db");

        {
            let journal = Journal::open(&db_path).unwrap();
            let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();
            assert_eq!(cx.cached_final_output().unwrap(), None);
            cx.complete(&serde_json::json!({ "total": 9 })).unwrap();
        }

        let journal = Journal::open(&db_path).unwrap();
        let cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();
        assert_eq!(
            cx.cached_final_output().unwrap(),
            Some(serde_json::json!({ "total": 9 }))
        );
    }

    #[tokio::test]
    async fn complete_is_idempotent_on_replay() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("j.db");
        let journal = Journal::open(&db_path).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();
        cx.complete(&7u32).unwrap();
        cx.complete(&7u32).unwrap();
        cx.complete(&999u32).unwrap(); // second complete must not overwrite

        let journal = Journal::open(&db_path).unwrap();
        let events = journal.read_all("wf").unwrap();
        let completed_count = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::WorkflowCompleted { .. }))
            .count();
        assert_eq!(completed_count, 1, "exactly one WorkflowCompleted event");
    }

    #[tokio::test]
    async fn call_with_retry_succeeds_after_transient_failures() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::Duration;

        use clawnicle_core::RetryPolicy;

        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();

        let attempts = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy {
            max_attempts: 5,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
            backoff_multiplier: 2.0,
            timeout: None,
        };

        let out: u32 = cx
            .call_with_retry::<u32, _, _, std::io::Error>("flaky-api", policy, || {
                let attempts = attempts.clone();
                async move {
                    let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if n < 3 {
                        Err(std::io::Error::other(format!("transient {n}")))
                    } else {
                        Ok(7)
                    }
                }
            })
            .await
            .unwrap();

        assert_eq!(out, 7);
        assert_eq!(attempts.load(Ordering::SeqCst), 3, "took three attempts");

        // Journal should record 3 ToolCallStarted + 2 ToolCallFailed + 1 ToolCallCompleted.
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let events = journal.read_all("wf").unwrap();
        let started = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::ToolCallStarted { .. }))
            .count();
        let failed = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::ToolCallFailed { .. }))
            .count();
        let completed = events
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::ToolCallCompleted { .. }))
            .count();
        assert_eq!((started, failed, completed), (3, 2, 1));
    }

    #[tokio::test]
    async fn call_with_retry_gives_up_after_max_attempts() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::Duration;

        use clawnicle_core::RetryPolicy;

        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();

        let attempts = Arc::new(AtomicU32::new(0));
        let policy = RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
            backoff_multiplier: 2.0,
            timeout: None,
        };

        let res: Result<u32> = cx
            .call_with_retry::<u32, _, _, std::io::Error>("always-fails", policy, || {
                let attempts = attempts.clone();
                async move {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    Err(std::io::Error::other("always"))
                }
            })
            .await;

        assert!(matches!(res, Err(Error::Tool(_))));
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "exactly max_attempts");
    }

    #[tokio::test]
    async fn call_with_retry_times_out_each_attempt() {
        use std::time::Duration;

        use clawnicle_core::RetryPolicy;

        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();

        let policy = RetryPolicy {
            max_attempts: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            backoff_multiplier: 2.0,
            timeout: Some(Duration::from_millis(20)),
        };

        let res: Result<u32> = cx
            .call_with_retry::<u32, _, _, std::io::Error>("slow", policy, || async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok(99)
            })
            .await;

        assert!(matches!(res, Err(Error::Tool(ref m)) if m.contains("timeout")));
    }

    #[tokio::test]
    async fn call_journals_failure_as_tool_call_failed() {
        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();

        let res: Result<u32> = cx
            .call("flaky", || async {
                Err::<u32, _>(std::io::Error::other("boom"))
            })
            .await;
        assert!(matches!(res, Err(Error::Tool(_))));

        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let events = journal.read_all("wf").unwrap();
        assert!(matches!(
            events.last().unwrap().payload,
            EventPayload::ToolCallFailed { .. }
        ));
    }
}
