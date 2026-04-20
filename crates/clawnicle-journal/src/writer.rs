use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use clawnicle_core::{Error, Event, EventPayload, Result};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::schema::SCHEMA_SQL;

pub struct Journal {
    conn: Connection,
}

impl Journal {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(journal_err)?;
        conn.execute_batch(SCHEMA_SQL).map_err(journal_err)?;
        Ok(Self { conn })
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
