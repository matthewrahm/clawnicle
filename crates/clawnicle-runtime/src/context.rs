use std::fmt::Display;
use std::future::Future;
use std::time::Instant;

use clawnicle_core::{Error, EventPayload, Result};
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

    /// Execute a tool call, journaling start and outcome.
    ///
    /// The `step_id` is the idempotency key — the unit used by the replay
    /// engine to detect previously completed work. Pick one that is stable
    /// across restarts and unique per call site (e.g. `"dex_fetch:{mint}"`).
    pub async fn call<Out, Fut, F, E>(&mut self, step_id: &str, tool: F) -> Result<Out>
    where
        F: FnOnce() -> Fut,
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

        self.journal.append(
            &self.workflow_id,
            &EventPayload::ToolCallStarted {
                step_id: step_id.to_string(),
                name: step_id.to_string(),
                input_hash: String::new(),
                attempt: 1,
            },
        )?;

        let start = Instant::now();
        let result = tool().await;
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
                Ok(out)
            }
            Err(e) => {
                let msg = e.to_string();
                self.journal.append(
                    &self.workflow_id,
                    &EventPayload::ToolCallFailed {
                        step_id: step_id.to_string(),
                        error: msg.clone(),
                        duration_ms,
                    },
                )?;
                Err(Error::Tool(msg))
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
