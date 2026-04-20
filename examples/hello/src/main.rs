use std::time::{SystemTime, UNIX_EPOCH};

use clawnicle_core::EventPayload;
use clawnicle_journal::Journal;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "hello.clawnicle.db".to_string());

    println!("Opening journal: {db_path}");
    let mut journal = Journal::open(&db_path)?;

    let workflow_id = format!("hello-{}", now_ms());

    journal.start_workflow(&workflow_id, "hello", "no-input", None)?;
    journal.append(
        &workflow_id,
        &EventPayload::WorkflowCompleted {
            output: serde_json::json!({ "message": "hello, clawnicle" }),
        },
    )?;

    let events = journal.read_all(&workflow_id)?;
    println!(
        "wrote + read {} events for workflow {workflow_id}:",
        events.len()
    );
    for ev in events {
        println!(
            "  seq={} kind={:<20} at={}ms",
            ev.sequence,
            ev.payload.kind(),
            ev.created_at_ms
        );
    }

    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
