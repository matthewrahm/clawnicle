use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use clawnicle_core::{Error, Event, EventPayload, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::schema::SCHEMA_SQL;

#[derive(Debug, Clone)]
pub struct WorkflowSummary {
    pub id: String,
    pub name: String,
    pub status: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub event_count: i64,
}

/// A workflow picked up from the pending queue by [`Journal::claim_next_pending`].
///
/// The scheduler hands these fields back to the registered handler: the
/// `input` JSON value goes to the workflow function, and the `id` and `name`
/// flow into the `Context` the handler runs against.
#[derive(Debug, Clone)]
pub struct ClaimedWorkflow {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

pub struct Journal {
    conn: Connection,
    path: std::path::PathBuf,
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path_buf = path.as_ref().to_path_buf();
        let conn = Connection::open(&path_buf).map_err(journal_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(journal_err)?;
        Ok(Self {
            conn,
            path: path_buf,
        })
    }

    /// Filesystem path this journal was opened from. Used by the runtime to
    /// open a fresh handle for child workflows.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn start_workflow(
        &mut self,
        workflow_id: &str,
        name: &str,
        input_hash: &str,
        parent_id: Option<&str>,
    ) -> Result<()> {
        let now = now_ms();
        let tx = self.conn.transaction().map_err(journal_err)?;

        tx.execute(
            r#"INSERT INTO workflows
                 (id, name, status, input_hash, parent_id, created_at, updated_at)
               VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?5)"#,
            params![workflow_id, name, input_hash, parent_id, now],
        )
        .map_err(journal_err)?;

        let payload = EventPayload::WorkflowStarted {
            name: name.to_string(),
            input_hash: input_hash.to_string(),
        };
        append_in_tx(&tx, workflow_id, &payload, now)?;

        tx.commit().map_err(journal_err)?;
        Ok(())
    }

    pub fn append(&mut self, workflow_id: &str, payload: &EventPayload) -> Result<i64> {
        let now = now_ms();
        let tx = self.conn.transaction().map_err(journal_err)?;

        let seq = append_in_tx(&tx, workflow_id, payload, now)?;

        match payload {
            EventPayload::WorkflowCompleted { .. } => {
                tx.execute(
                    "UPDATE workflows SET status = 'completed', updated_at = ?1 WHERE id = ?2",
                    params![now, workflow_id],
                )
                .map_err(journal_err)?;
            }
            EventPayload::WorkflowFailed { .. } => {
                tx.execute(
                    "UPDATE workflows SET status = 'failed', updated_at = ?1 WHERE id = ?2",
                    params![now, workflow_id],
                )
                .map_err(journal_err)?;
            }
            _ => {
                tx.execute(
                    "UPDATE workflows SET updated_at = ?1 WHERE id = ?2",
                    params![now, workflow_id],
                )
                .map_err(journal_err)?;
            }
        }

        tx.commit().map_err(journal_err)?;
        Ok(seq)
    }

    pub fn list_workflows(&self) -> Result<Vec<WorkflowSummary>> {
        let mut stmt = self
            .conn
            .prepare(
                r#"SELECT w.id, w.name, w.status, w.created_at, w.updated_at,
                          COALESCE(e.cnt, 0) AS event_count
                   FROM workflows w
                   LEFT JOIN (
                       SELECT workflow_id, COUNT(*) AS cnt
                       FROM events
                       GROUP BY workflow_id
                   ) e ON e.workflow_id = w.id
                   ORDER BY w.updated_at DESC"#,
            )
            .map_err(journal_err)?;

        let rows = stmt
            .query_map([], |r| {
                Ok(WorkflowSummary {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    status: r.get(2)?,
                    created_at_ms: r.get(3)?,
                    updated_at_ms: r.get(4)?,
                    event_count: r.get(5)?,
                })
            })
            .map_err(journal_err)?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(journal_err)?);
        }
        Ok(out)
    }

    pub fn workflow_summary(&self, workflow_id: &str) -> Result<Option<WorkflowSummary>> {
        self.conn
            .query_row(
                r#"SELECT w.id, w.name, w.status, w.created_at, w.updated_at,
                          (SELECT COUNT(*) FROM events WHERE workflow_id = w.id) AS event_count
                   FROM workflows w
                   WHERE w.id = ?1"#,
                params![workflow_id],
                |r| {
                    Ok(WorkflowSummary {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        status: r.get(2)?,
                        created_at_ms: r.get(3)?,
                        updated_at_ms: r.get(4)?,
                        event_count: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(journal_err)
    }

    /// Submit a workflow to the pending queue. The row is created with
    /// `status = 'pending'` and NO events (WorkflowStarted is emitted at
    /// claim time, not submit time).
    pub fn submit_pending(
        &mut self,
        name: &str,
        workflow_id: &str,
        input: &serde_json::Value,
    ) -> Result<()> {
        let now = now_ms();
        let input_json = serde_json::to_string(input)?;
        let input_hash = hash_input(&input_json);
        self.conn
            .execute(
                r#"INSERT INTO workflows
                     (id, name, status, input_hash, input_json, parent_id, created_at, updated_at)
                   VALUES (?1, ?2, 'pending', ?3, ?4, NULL, ?5, ?5)"#,
                params![workflow_id, name, input_hash, input_json, now],
            )
            .map_err(journal_err)?;
        Ok(())
    }

    /// Insert a child workflow row with `parent_id` set and immediately mark
    /// it `running`, emitting the `WorkflowStarted` event in the same
    /// transaction. If a row with this id already exists, this is a no-op —
    /// the caller can then proceed to run the child and its tool calls will
    /// short-circuit via the normal replay path.
    pub fn start_child_workflow(
        &mut self,
        parent_id: &str,
        child_id: &str,
        name: &str,
        input: &serde_json::Value,
    ) -> Result<()> {
        if self.workflow_status(child_id)?.is_some() {
            return Ok(());
        }
        let now = now_ms();
        let input_json = serde_json::to_string(input)?;
        let input_hash = hash_input(&input_json);

        let tx = self.conn.transaction().map_err(journal_err)?;
        tx.execute(
            r#"INSERT INTO workflows
                 (id, name, status, input_hash, input_json, parent_id, created_at, updated_at)
               VALUES (?1, ?2, 'running', ?3, ?4, ?5, ?6, ?6)"#,
            params![child_id, name, input_hash, input_json, parent_id, now],
        )
        .map_err(journal_err)?;
        let payload = EventPayload::WorkflowStarted {
            name: name.to_string(),
            input_hash,
        };
        append_in_tx(&tx, child_id, &payload, now)?;
        tx.commit().map_err(journal_err)?;
        Ok(())
    }

    /// Atomically pick the oldest pending workflow, flip it to `running`,
    /// and append its [`EventPayload::WorkflowStarted`] event.
    ///
    /// Returns `None` if nothing is pending. Uses `BEGIN IMMEDIATE` so
    /// multiple scheduler workers (or multiple processes) cannot claim the
    /// same row.
    pub fn claim_next_pending(&mut self) -> Result<Option<ClaimedWorkflow>> {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(journal_err)?;

        let claimed: Option<(String, String, String, String)> = tx
            .query_row(
                r#"SELECT id, name, input_hash, input_json
                   FROM workflows
                   WHERE status = 'pending'
                   ORDER BY created_at ASC
                   LIMIT 1"#,
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .map_err(journal_err)?;

        let Some((id, name, input_hash, input_json)) = claimed else {
            tx.commit().map_err(journal_err)?;
            return Ok(None);
        };

        let now = now_ms();
        tx.execute(
            "UPDATE workflows SET status = 'running', updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )
        .map_err(journal_err)?;

        let payload = EventPayload::WorkflowStarted {
            name: name.clone(),
            input_hash,
        };
        append_in_tx(&tx, &id, &payload, now)?;

        tx.commit().map_err(journal_err)?;

        let input: serde_json::Value = if input_json.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_str(&input_json)?
        };

        Ok(Some(ClaimedWorkflow { id, name, input }))
    }

    /// Reset any workflow stuck in `running` back to `pending`. Called by
    /// the scheduler on startup to pick up workflows abandoned by a prior
    /// process crash.
    pub fn recover_abandoned(&mut self) -> Result<u64> {
        let now = now_ms();
        let affected = self
            .conn
            .execute(
                "UPDATE workflows SET status = 'pending', updated_at = ?1 WHERE status = 'running'",
                params![now],
            )
            .map_err(journal_err)?;
        Ok(affected as u64)
    }

    pub fn workflow_status(&self, workflow_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row(
                "SELECT status FROM workflows WHERE id = ?1",
                params![workflow_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(journal_err)
    }

    pub fn lookup_completed_tool_call(
        &self,
        workflow_id: &str,
        step_id: &str,
    ) -> Result<Option<serde_json::Value>> {
        let row: Option<String> = self
            .conn
            .query_row(
                r#"SELECT payload
                   FROM events
                   WHERE workflow_id = ?1
                     AND kind = 'tool_call_completed'
                     AND json_extract(payload, '$.step_id') = ?2
                   ORDER BY sequence ASC
                   LIMIT 1"#,
                params![workflow_id, step_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(journal_err)?;

        match row {
            Some(payload_json) => {
                let payload: EventPayload = serde_json::from_str(&payload_json)?;
                if let EventPayload::ToolCallCompleted { output, .. } = payload {
                    Ok(Some(output))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    pub fn lookup_completed_llm_call(
        &self,
        workflow_id: &str,
        prompt_hash: &str,
    ) -> Result<Option<clawnicle_core::LlmResponse>> {
        let row: Option<String> = self
            .conn
            .query_row(
                r#"SELECT payload
                   FROM events
                   WHERE workflow_id = ?1
                     AND kind = 'llm_call_completed'
                     AND json_extract(payload, '$.prompt_hash') = ?2
                   ORDER BY sequence ASC
                   LIMIT 1"#,
                params![workflow_id, prompt_hash],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(journal_err)?;

        match row {
            Some(payload_json) => {
                let payload: EventPayload = serde_json::from_str(&payload_json)?;
                if let EventPayload::LlmCallCompleted {
                    model,
                    response,
                    tokens_in,
                    tokens_out,
                    ..
                } = payload
                {
                    Ok(Some(clawnicle_core::LlmResponse {
                        model,
                        content: response,
                        tokens_in,
                        tokens_out,
                    }))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }

    pub fn read_all(&self, workflow_id: &str) -> Result<Vec<Event>> {
        let mut stmt = self
            .conn
            .prepare(
                r#"SELECT sequence, created_at, payload
                   FROM events
                   WHERE workflow_id = ?1
                   ORDER BY sequence ASC"#,
            )
            .map_err(journal_err)?;

        let rows = stmt
            .query_map(params![workflow_id], |row| {
                let sequence: i64 = row.get(0)?;
                let created_at_ms: i64 = row.get(1)?;
                let payload_json: String = row.get(2)?;
                Ok((sequence, created_at_ms, payload_json))
            })
            .map_err(journal_err)?;

        let mut out = Vec::new();
        for r in rows {
            let (sequence, created_at_ms, payload_json) = r.map_err(journal_err)?;
            let payload: EventPayload = serde_json::from_str(&payload_json)?;
            out.push(Event {
                sequence,
                workflow_id: workflow_id.to_string(),
                created_at_ms,
                payload,
            });
        }
        Ok(out)
    }
}

fn append_in_tx(
    tx: &Transaction<'_>,
    workflow_id: &str,
    payload: &EventPayload,
    now: i64,
) -> Result<i64> {
    let next_seq: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM events WHERE workflow_id = ?1",
            params![workflow_id],
            |r| r.get(0),
        )
        .map_err(journal_err)?;

    let kind = payload.kind();
    let json = serde_json::to_string(payload)?;

    tx.execute(
        r#"INSERT INTO events (workflow_id, sequence, kind, payload, created_at)
           VALUES (?1, ?2, ?3, ?4, ?5)"#,
        params![workflow_id, next_seq, kind, json, now],
    )
    .map_err(journal_err)?;

    Ok(next_seq)
}

fn journal_err(e: rusqlite::Error) -> Error {
    Error::Journal(e.to_string())
}

fn hash_input(input_json: &str) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    input_json.hash(&mut h);
    format!("{:016x}", h.finish())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_and_reads_two_events() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();

        journal
            .start_workflow("wf-1", "hello", "hash-0", None)
            .unwrap();
        journal
            .append(
                "wf-1",
                &EventPayload::WorkflowCompleted {
                    output: serde_json::json!({ "ok": true }),
                },
            )
            .unwrap();

        let events = journal.read_all("wf-1").unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events[0].payload,
            EventPayload::WorkflowStarted { .. }
        ));
        assert!(matches!(
            events[1].payload,
            EventPayload::WorkflowCompleted { .. }
        ));
        assert_eq!(events[0].sequence, 1);
        assert_eq!(events[1].sequence, 2);
    }

    #[test]
    fn sequence_is_per_workflow() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();

        journal.start_workflow("a", "a", "h", None).unwrap();
        journal.start_workflow("b", "b", "h", None).unwrap();
        journal
            .append(
                "a",
                &EventPayload::WorkflowCompleted {
                    output: serde_json::json!({}),
                },
            )
            .unwrap();
        journal
            .append(
                "b",
                &EventPayload::WorkflowCompleted {
                    output: serde_json::json!({}),
                },
            )
            .unwrap();

        let a = journal.read_all("a").unwrap();
        let b = journal.read_all("b").unwrap();
        assert_eq!(a.len(), 2);
        assert_eq!(b.len(), 2);
        assert_eq!(a[0].sequence, 1);
        assert_eq!(a[1].sequence, 2);
        assert_eq!(b[0].sequence, 1);
        assert_eq!(b[1].sequence, 2);
    }

    #[test]
    fn list_workflows_orders_by_updated_at_desc_with_event_counts() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();

        journal.start_workflow("a", "first", "h", None).unwrap();
        journal.start_workflow("b", "second", "h", None).unwrap();
        journal
            .append(
                "a",
                &EventPayload::ToolCallCompleted {
                    step_id: "s".into(),
                    output: serde_json::json!({}),
                    duration_ms: 1,
                },
            )
            .unwrap();

        let workflows = journal.list_workflows().unwrap();
        assert_eq!(workflows.len(), 2);
        // 'a' has 2 events (Started + ToolCallCompleted), 'b' has 1.
        let a = workflows.iter().find(|w| w.id == "a").unwrap();
        let b = workflows.iter().find(|w| w.id == "b").unwrap();
        assert_eq!(a.event_count, 2);
        assert_eq!(b.event_count, 1);
    }

    #[test]
    fn workflow_summary_returns_none_for_missing_id() {
        let dir = tempdir().unwrap();
        let journal = Journal::open(dir.path().join("j.db")).unwrap();
        assert!(journal.workflow_summary("nope").unwrap().is_none());
    }

    #[test]
    fn workflow_status_reflects_terminal_events() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();

        assert_eq!(journal.workflow_status("missing").unwrap(), None);

        journal.start_workflow("wf", "test", "h", None).unwrap();
        assert_eq!(
            journal.workflow_status("wf").unwrap().as_deref(),
            Some("running")
        );

        journal
            .append(
                "wf",
                &EventPayload::WorkflowCompleted {
                    output: serde_json::json!({}),
                },
            )
            .unwrap();
        assert_eq!(
            journal.workflow_status("wf").unwrap().as_deref(),
            Some("completed")
        );
    }

    #[test]
    fn lookup_completed_tool_call_returns_cached_output() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();
        journal.start_workflow("wf", "t", "h", None).unwrap();

        assert_eq!(
            journal.lookup_completed_tool_call("wf", "fetch-1").unwrap(),
            None
        );

        journal
            .append(
                "wf",
                &EventPayload::ToolCallCompleted {
                    step_id: "fetch-1".into(),
                    output: serde_json::json!({ "price": 42 }),
                    duration_ms: 11,
                },
            )
            .unwrap();

        let cached = journal.lookup_completed_tool_call("wf", "fetch-1").unwrap();
        assert_eq!(cached, Some(serde_json::json!({ "price": 42 })));

        assert_eq!(
            journal.lookup_completed_tool_call("wf", "fetch-2").unwrap(),
            None
        );
    }

    #[test]
    fn lookup_scoped_to_workflow() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();
        journal.start_workflow("a", "t", "h", None).unwrap();
        journal.start_workflow("b", "t", "h", None).unwrap();
        journal
            .append(
                "a",
                &EventPayload::ToolCallCompleted {
                    step_id: "shared-key".into(),
                    output: serde_json::json!("from-a"),
                    duration_ms: 1,
                },
            )
            .unwrap();

        assert_eq!(
            journal
                .lookup_completed_tool_call("a", "shared-key")
                .unwrap(),
            Some(serde_json::json!("from-a"))
        );
        assert_eq!(
            journal
                .lookup_completed_tool_call("b", "shared-key")
                .unwrap(),
            None
        );
    }

    #[test]
    fn lookup_completed_llm_call_returns_response() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();
        journal.start_workflow("wf", "t", "h", None).unwrap();

        assert!(
            journal
                .lookup_completed_llm_call("wf", "abc123")
                .unwrap()
                .is_none()
        );

        journal
            .append(
                "wf",
                &EventPayload::LlmCallCompleted {
                    step_id: "s1".into(),
                    model: "claude-haiku-4-5".into(),
                    prompt_hash: "abc123".into(),
                    response: "hello world".into(),
                    tokens_in: 42,
                    tokens_out: 7,
                },
            )
            .unwrap();

        let hit = journal
            .lookup_completed_llm_call("wf", "abc123")
            .unwrap()
            .unwrap();
        assert_eq!(hit.content, "hello world");
        assert_eq!(hit.tokens_in, 42);
        assert_eq!(hit.tokens_out, 7);
        assert_eq!(hit.model, "claude-haiku-4-5");

        assert!(
            journal
                .lookup_completed_llm_call("wf", "other-hash")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn submit_and_claim_pending_runs_workflow_lifecycle() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();

        // queue empty → claim returns None
        assert!(journal.claim_next_pending().unwrap().is_none());

        journal
            .submit_pending("scan", "wf-1", &serde_json::json!({ "mint": "abc" }))
            .unwrap();
        journal
            .submit_pending("scan", "wf-2", &serde_json::json!({ "mint": "def" }))
            .unwrap();

        // Both workflows show status='pending', with no events yet.
        assert_eq!(
            journal.workflow_status("wf-1").unwrap().as_deref(),
            Some("pending")
        );
        assert_eq!(journal.read_all("wf-1").unwrap().len(), 0);

        // First claim returns the oldest.
        let first = journal.claim_next_pending().unwrap().unwrap();
        assert_eq!(first.id, "wf-1");
        assert_eq!(first.input, serde_json::json!({ "mint": "abc" }));
        assert_eq!(
            journal.workflow_status("wf-1").unwrap().as_deref(),
            Some("running")
        );

        // WorkflowStarted event was emitted at claim time.
        let events = journal.read_all("wf-1").unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].payload,
            EventPayload::WorkflowStarted { .. }
        ));

        // Second claim returns wf-2; third returns None.
        let second = journal.claim_next_pending().unwrap().unwrap();
        assert_eq!(second.id, "wf-2");
        assert!(journal.claim_next_pending().unwrap().is_none());
    }

    #[test]
    fn recover_abandoned_moves_running_back_to_pending() {
        let dir = tempdir().unwrap();
        let mut journal = Journal::open(dir.path().join("j.db")).unwrap();
        journal
            .submit_pending("scan", "a", &serde_json::Value::Null)
            .unwrap();
        journal
            .submit_pending("scan", "b", &serde_json::Value::Null)
            .unwrap();
        journal.claim_next_pending().unwrap();
        journal.claim_next_pending().unwrap();

        assert_eq!(
            journal.workflow_status("a").unwrap().as_deref(),
            Some("running")
        );
        let recovered = journal.recover_abandoned().unwrap();
        assert_eq!(recovered, 2);
        assert_eq!(
            journal.workflow_status("a").unwrap().as_deref(),
            Some("pending")
        );

        // Completed workflows are untouched by a second recovery pass.
        journal
            .submit_pending("scan", "c", &serde_json::Value::Null)
            .unwrap();
        let claimed = journal.claim_next_pending().unwrap().unwrap();
        journal
            .append(
                &claimed.id,
                &EventPayload::WorkflowCompleted {
                    output: serde_json::Value::Null,
                },
            )
            .unwrap();

        // claimed is now 'completed'; the other two (b and c, or similar) sit
        // in 'pending'. Nothing is 'running', so recover_abandoned moves 0.
        assert_eq!(journal.recover_abandoned().unwrap(), 0);
        assert_eq!(
            journal.workflow_status(&claimed.id).unwrap().as_deref(),
            Some("completed")
        );
    }

    #[test]
    fn reopen_preserves_events() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("j.db");
        {
            let mut journal = Journal::open(&path).unwrap();
            journal.start_workflow("wf", "hello", "h", None).unwrap();
        }
        let journal = Journal::open(&path).unwrap();
        let events = journal.read_all("wf").unwrap();
        assert_eq!(events.len(), 1);
    }
}
