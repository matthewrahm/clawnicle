use std::fmt::Display;
use std::future::Future;
use std::time::{Duration, Instant};

use clawnicle_core::{
    Budget, BudgetUsage, CancelToken, Error, EventPayload, LlmRequest, LlmResponse, Result,
    RetryPolicy,
};
use clawnicle_journal::Journal;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub struct Context {
    workflow_id: String,
    journal: Journal,
    budget: Budget,
    usage: BudgetUsage,
    cancel: Option<CancelToken>,
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
            budget: Budget::unlimited(),
            usage: BudgetUsage::zero(),
            cancel: None,
        })
    }

    pub fn workflow_id(&self) -> &str {
        &self.workflow_id
    }

    /// Attach a per-workflow [`Budget`]. Builder-style so it chains off
    /// `open_or_start`.
    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    /// Attach a [`CancelToken`] so external callers can interrupt the
    /// workflow between tool-call attempts.
    pub fn with_cancel_token(mut self, token: CancelToken) -> Self {
        self.cancel = Some(token);
        self
    }

    fn check_cancelled(&self) -> Result<()> {
        if let Some(token) = &self.cancel
            && token.is_cancelled()
        {
            return Err(Error::Cancelled);
        }
        Ok(())
    }

    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    pub fn usage(&self) -> &BudgetUsage {
        &self.usage
    }

    /// Add to the token counter. Returns Err(BudgetExceeded) if the new total
    /// breaches the budget cap. LLM provider integrations (Week 4) will call
    /// this after each completion.
    pub fn charge_tokens(&mut self, n: u64) -> Result<()> {
        self.usage.tokens = self.usage.tokens.saturating_add(n);
        self.check_budget()
    }

    /// Add to the USD counter in micros (1 USD = 1_000_000 micros).
    pub fn charge_usd_micros(&mut self, n: u64) -> Result<()> {
        self.usage.usd_micros = self.usage.usd_micros.saturating_add(n);
        self.check_budget()
    }

    fn check_budget(&self) -> Result<()> {
        if let Some(field) = self.usage.exceeds(&self.budget) {
            return Err(Error::BudgetExceeded(field));
        }
        Ok(())
    }

    /// Returns true if the given step already has a completed tool call in
    /// the journal. A subsequent `call()` with this step_id will short-circuit
    /// from cache. Exposed for demos and observability; replay itself does
    /// not need the caller to check this.
    pub fn has_cached_tool_call(&self, step_id: &str) -> Result<bool> {
        Ok(self
            .journal
            .lookup_completed_tool_call(&self.workflow_id, step_id)?
            .is_some())
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

    /// Spawn a child workflow inline and await its result.
    ///
    /// The child gets its own workflow row with `parent_id = self.workflow_id`,
    /// its own sequence of journal events, and its own replay semantics.
    /// Idempotent on replay: if the child has already completed in a prior
    /// run of the parent, the cached final output is returned without
    /// re-executing the handler.
    ///
    /// The child runs in the current task (not the scheduler's task pool),
    /// which is fine for sequential composition. For true fan-out across
    /// multiple children, call `spawn_child` concurrently from a parent that
    /// has access to tokio primitives (`tokio::join!`, `JoinSet`).
    pub async fn spawn_child<F, Fut>(
        &mut self,
        child_id: &str,
        name: &str,
        input: &serde_json::Value,
        handler: F,
    ) -> Result<serde_json::Value>
    where
        F: FnOnce(Context, serde_json::Value) -> Fut,
        Fut: Future<Output = Result<serde_json::Value>>,
    {
        // Replay short-circuit: if the child already completed, skip.
        let parent_path = self.journal.path().to_path_buf();
        let probe = Journal::open(&parent_path)?;
        if probe.workflow_status(child_id)?.as_deref() == Some("completed") {
            for event in probe.read_all(child_id)? {
                if let EventPayload::WorkflowCompleted { output } = event.payload {
                    return Ok(output);
                }
            }
            return Ok(serde_json::Value::Null);
        }
        drop(probe);

        // Create or resume the child workflow row.
        {
            let mut journal = Journal::open(&parent_path)?;
            journal.start_child_workflow(&self.workflow_id, child_id, name, input)?;
        }

        // Open a fresh Context on the child and hand it to the handler.
        let child_journal = Journal::open(&parent_path)?;
        let mut child_cx = Context::open_or_start(child_journal, child_id, name, "")?;

        match handler(
            Context::open_or_start(Journal::open(&parent_path)?, child_id, name, "")?,
            input.clone(),
        )
        .await
        {
            Ok(output) => {
                // Ensure the child is marked complete. If the handler already
                // called cx.complete() the status is already 'completed' and
                // Context::complete is a no-op; otherwise we emit it here.
                child_cx.complete(&output)?;
                Ok(output)
            }
            Err(err) => {
                let mut journal = Journal::open(&parent_path)?;
                journal.append(
                    child_id,
                    &EventPayload::WorkflowFailed {
                        error: err.to_string(),
                    },
                )?;
                Err(err)
            }
        }
    }

    /// Execute an LLM completion with journal-backed prompt caching.
    ///
    /// On first call, the provider is invoked and an `LlmCallCompleted`
    /// event is written with the request's prompt hash. On any subsequent
    /// call (same process or after restart) where a cached event with the
    /// same prompt hash exists for this workflow, the cached response is
    /// returned without touching the provider.
    ///
    /// Tokens from a fresh call are charged against the workflow budget via
    /// [`Context::charge_tokens`]. Tokens on cache hits are NOT charged —
    /// budget is a per-session counter; reconstruction from the journal is
    /// not implemented in v0.
    pub async fn complete_llm<P>(
        &mut self,
        step_id: &str,
        provider: &P,
        request: &LlmRequest,
    ) -> Result<LlmResponse>
    where
        P: clawnicle_llm::LlmProvider,
    {
        let prompt_hash = request.prompt_hash();

        if let Some(cached) = self
            .journal
            .lookup_completed_llm_call(&self.workflow_id, &prompt_hash)?
        {
            return Ok(cached);
        }

        self.check_cancelled()?;
        self.check_budget()?;

        let response = provider.complete(request).await?;

        self.journal.append(
            &self.workflow_id,
            &EventPayload::LlmCallCompleted {
                step_id: step_id.to_string(),
                model: response.model.clone(),
                prompt_hash,
                response: response.content.clone(),
                tokens_in: response.tokens_in,
                tokens_out: response.tokens_out,
            },
        )?;

        self.charge_tokens(u64::from(response.tokens_in) + u64::from(response.tokens_out))?;

        Ok(response)
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

        self.check_cancelled()?;
        self.check_budget()?;

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

            self.usage.wallclock_ms = self.usage.wallclock_ms.saturating_add(duration_ms);

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

                    self.check_cancelled()?;
                    self.check_budget()?;

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
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

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
    async fn budget_exceeded_after_tokens_charged_past_cap() {
        use clawnicle_core::Budget;

        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h")
            .unwrap()
            .with_budget(Budget::unlimited().with_max_tokens(1000));

        cx.charge_tokens(600).unwrap();
        cx.charge_tokens(300).unwrap();
        let over = cx.charge_tokens(200);
        assert!(matches!(over, Err(Error::BudgetExceeded("tokens"))));
        assert_eq!(cx.usage().tokens, 1100);
    }

    #[tokio::test]
    async fn budget_exceeded_blocks_next_call() {
        use clawnicle_core::Budget;

        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h")
            .unwrap()
            .with_budget(Budget::unlimited().with_max_wallclock_ms(50));

        let slow: std::result::Result<u32, Error> = cx
            .call::<u32, _, _, std::io::Error>("slow", || async {
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                Ok(1)
            })
            .await;
        assert!(slow.is_ok(), "first call still runs and records time");

        let next: std::result::Result<u32, Error> = cx
            .call::<u32, _, _, std::io::Error>("next", || async { Ok(2) })
            .await;
        assert!(
            matches!(next, Err(Error::BudgetExceeded("wallclock"))),
            "next call must be refused because wallclock cap is already breached"
        );
    }

    #[tokio::test]
    async fn complete_llm_caches_response_via_journal() {
        use clawnicle_core::{LlmMessage, LlmRequest, LlmResponse};
        use clawnicle_llm::MockProvider;

        let dir = tempdir().unwrap();
        let db = dir.path().join("j.db");

        let provider = MockProvider::new(vec![LlmResponse {
            model: "claude-haiku-4-5".into(),
            content: "hello".into(),
            tokens_in: 10,
            tokens_out: 3,
        }]);

        let req = LlmRequest::new("claude-haiku-4-5", vec![LlmMessage::user("say hi")]);

        {
            let journal = Journal::open(&db).unwrap();
            let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();
            let r = cx.complete_llm("greet", &provider, &req).await.unwrap();
            assert_eq!(r.content, "hello");
            assert_eq!(cx.usage().tokens, 13);
        }

        // Reopen the journal. Same request hashes the same; provider must NOT
        // be hit again even though its queue is empty.
        let journal = Journal::open(&db).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h").unwrap();
        let r = cx.complete_llm("greet", &provider, &req).await.unwrap();
        assert_eq!(r.content, "hello");
        assert_eq!(
            provider.call_count(),
            1,
            "provider must be hit exactly once across both runs"
        );
    }

    #[tokio::test]
    async fn complete_llm_charges_tokens_against_budget() {
        use clawnicle_core::{Budget, LlmMessage, LlmRequest, LlmResponse};
        use clawnicle_llm::MockProvider;

        let dir = tempdir().unwrap();
        let provider = MockProvider::new(vec![
            LlmResponse {
                model: "m".into(),
                content: "a".into(),
                tokens_in: 400,
                tokens_out: 100,
            },
            LlmResponse {
                model: "m".into(),
                content: "b".into(),
                tokens_in: 400,
                tokens_out: 200,
            },
        ]);

        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h")
            .unwrap()
            .with_budget(Budget::unlimited().with_max_tokens(1000));

        let req_a = LlmRequest::new("m", vec![LlmMessage::user("a")]);
        let req_b = LlmRequest::new("m", vec![LlmMessage::user("b")]);

        cx.complete_llm("s1", &provider, &req_a).await.unwrap();
        assert_eq!(cx.usage().tokens, 500);

        let second = cx.complete_llm("s2", &provider, &req_b).await;
        assert!(
            matches!(second, Err(Error::BudgetExceeded("tokens"))),
            "tokens=500+600 breaches cap=1000"
        );
    }

    #[tokio::test]
    async fn spawn_child_runs_and_returns_output() {
        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let mut parent = Context::open_or_start(journal, "parent-1", "parent", "").unwrap();

        let result = parent
            .spawn_child(
                "child-1",
                "greet",
                &serde_json::json!({ "who": "world" }),
                |mut child_cx, input| async move {
                    let who = input["who"].as_str().unwrap().to_string();
                    let greeting = format!("hello, {who}");
                    child_cx.complete(&greeting)?;
                    Ok(serde_json::json!(greeting))
                },
            )
            .await
            .unwrap();
        assert_eq!(result, serde_json::json!("hello, world"));

        // Child row has parent_id set and status='completed'.
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        assert_eq!(
            journal.workflow_status("child-1").unwrap().as_deref(),
            Some("completed")
        );
    }

    #[tokio::test]
    async fn spawn_child_short_circuits_on_parent_replay() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};

        let dir = tempdir().unwrap();
        let db = dir.path().join("j.db");
        let child_invocations = Arc::new(AtomicU32::new(0));

        // First parent run completes the child.
        {
            let journal = Journal::open(&db).unwrap();
            let mut parent = Context::open_or_start(journal, "parent-2", "parent", "").unwrap();
            let counter = child_invocations.clone();
            let out = parent
                .spawn_child(
                    "child-2",
                    "count",
                    &serde_json::Value::Null,
                    move |mut cx, _| {
                        let counter = counter.clone();
                        async move {
                            counter.fetch_add(1, Ordering::SeqCst);
                            cx.complete(&42)?;
                            Ok(serde_json::json!(42))
                        }
                    },
                )
                .await
                .unwrap();
            assert_eq!(out, serde_json::json!(42));
        }

        // Second parent run (simulating a restart) — spawn_child for the same
        // child_id must return the cached output without running the handler.
        {
            let journal = Journal::open(&db).unwrap();
            let mut parent = Context::open_or_start(journal, "parent-2", "parent", "").unwrap();
            let counter = child_invocations.clone();
            let out = parent
                .spawn_child(
                    "child-2",
                    "count",
                    &serde_json::Value::Null,
                    move |_cx, _| {
                        let counter = counter.clone();
                        async move {
                            counter.fetch_add(1, Ordering::SeqCst);
                            Ok(serde_json::json!(999))
                        }
                    },
                )
                .await
                .unwrap();
            assert_eq!(out, serde_json::json!(42));
        }

        assert_eq!(
            child_invocations.load(Ordering::SeqCst),
            1,
            "child handler ran exactly once across both parent runs"
        );
    }

    #[tokio::test]
    async fn cancel_before_first_call_returns_cancelled() {
        use clawnicle_core::CancelToken;

        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let token = CancelToken::new();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h")
            .unwrap()
            .with_cancel_token(token.clone());

        token.cancel();

        let res: Result<u32> = cx
            .call::<u32, _, _, std::io::Error>("never", || async { Ok(1) })
            .await;
        assert!(matches!(res, Err(Error::Cancelled)));
    }

    #[tokio::test]
    async fn cancel_between_retry_attempts_stops_retrying() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::Duration;

        use clawnicle_core::{CancelToken, RetryPolicy};

        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        let token = CancelToken::new();
        let mut cx = Context::open_or_start(journal, "wf", "demo", "h")
            .unwrap()
            .with_cancel_token(token.clone());

        let attempts = Arc::new(AtomicU32::new(0));
        let token_for_closure = token.clone();
        let policy = RetryPolicy {
            max_attempts: 10,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(5),
            backoff_multiplier: 2.0,
            timeout: None,
        };

        let res: Result<u32> = cx
            .call_with_retry::<u32, _, _, std::io::Error>("flaky", policy, || {
                let attempts = attempts.clone();
                let token = token_for_closure.clone();
                async move {
                    let n = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                    if n == 2 {
                        token.cancel();
                    }
                    Err(std::io::Error::other("nope"))
                }
            })
            .await;

        assert!(matches!(res, Err(Error::Cancelled)));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "attempts stopped after cancel was flipped during attempt 2"
        );
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
