use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use clawnicle_core::EventPayload;
use clawnicle_journal::Journal;

#[derive(Parser)]
#[command(
    name = "clawnicle",
    version,
    about = "Inspect and replay Clawnicle workflow journals"
)]
struct Cli {
    /// Path to the SQLite journal file.
    #[arg(short = 'd', long, default_value = "clawnicle.db")]
    db: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List all workflows in the journal.
    List,
    /// Show summary + key metrics for a single workflow.
    Show {
        /// Workflow id.
        id: String,
    },
    /// Dump the event timeline for a single workflow.
    Events {
        /// Workflow id.
        id: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let journal = Journal::open(&cli.db)
        .with_context(|| format!("opening journal at {}", cli.db.display()))?;

    match cli.command {
        Command::List => list(&journal),
        Command::Show { id } => show(&journal, &id),
        Command::Events { id } => events(&journal, &id),
    }
}

fn list(journal: &Journal) -> Result<()> {
    let workflows = journal.list_workflows()?;
    if workflows.is_empty() {
        println!("no workflows");
        return Ok(());
    }

    println!(
        "{:<28} {:<14} {:<10} {:>8}  updated",
        "id", "name", "status", "events"
    );
    println!("{}", "-".repeat(80));
    for w in workflows {
        println!(
            "{:<28} {:<14} {:<10} {:>8}  {}",
            truncate(&w.id, 28),
            truncate(&w.name, 14),
            w.status,
            w.event_count,
            format_ts(w.updated_at_ms),
        );
    }
    Ok(())
}

fn show(journal: &Journal, id: &str) -> Result<()> {
    let Some(w) = journal.workflow_summary(id)? else {
        anyhow::bail!("workflow not found: {id}");
    };

    println!("id:         {}", w.id);
    println!("name:       {}", w.name);
    println!("status:     {}", w.status);
    println!("created:    {}", format_ts(w.created_at_ms));
    println!("updated:    {}", format_ts(w.updated_at_ms));
    println!("events:     {}", w.event_count);

    let events = journal.read_all(id)?;
    let mut tool_calls = 0;
    let mut llm_calls = 0;
    let mut failures = 0;
    for ev in &events {
        match &ev.payload {
            EventPayload::ToolCallCompleted { .. } => tool_calls += 1,
            EventPayload::ToolCallFailed { .. } => failures += 1,
            EventPayload::LlmCallCompleted { .. } => llm_calls += 1,
            EventPayload::WorkflowFailed { .. } => failures += 1,
            _ => {}
        }
    }
    println!("tool calls: {tool_calls}");
    println!("llm calls:  {llm_calls}");
    println!("failures:   {failures}");

    if let Some(last) = events.last() {
        println!("last event: {}", last.payload.kind());
    }
    Ok(())
}

fn events(journal: &Journal, id: &str) -> Result<()> {
    let events = journal.read_all(id)?;
    if events.is_empty() {
        anyhow::bail!("no events for workflow {id} (does it exist?)");
    }
    for ev in events {
        let payload_str = match &ev.payload {
            EventPayload::WorkflowStarted { name, .. } => format!("name={name}"),
            EventPayload::ToolCallStarted {
                step_id, attempt, ..
            } => format!("step={step_id} attempt={attempt}"),
            EventPayload::ToolCallCompleted {
                step_id,
                duration_ms,
                ..
            } => format!("step={step_id} duration={duration_ms}ms"),
            EventPayload::ToolCallFailed {
                step_id,
                error,
                duration_ms,
            } => format!("step={step_id} duration={duration_ms}ms error={error}"),
            EventPayload::LlmCallCompleted {
                model,
                tokens_in,
                tokens_out,
                ..
            } => format!("model={model} tokens={tokens_in}→{tokens_out}"),
            EventPayload::StepCompleted { step_id, .. } => format!("step={step_id}"),
            EventPayload::WorkflowCompleted { .. } => "completed".to_string(),
            EventPayload::WorkflowFailed { error } => format!("error={error}"),
        };
        println!(
            "{:>4}  {}  {:<22}  {}",
            ev.sequence,
            format_ts(ev.created_at_ms),
            ev.payload.kind(),
            payload_str
        );
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n.saturating_sub(1)])
    }
}

fn format_ts(ms: i64) -> String {
    // Render as seconds-since-epoch with millisecond precision — avoids a
    // chrono dep for v0. Users can pipe through `date -r` if they want
    // human-readable timestamps.
    let secs = ms / 1000;
    let millis = (ms % 1000).abs();
    format!("{secs}.{millis:03}s")
}
