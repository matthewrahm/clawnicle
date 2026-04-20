//! Narrative crash-and-resume demo.
//!
//! Simulates an "AI research agent" that runs four steps: search for
//! articles, extract key points, write a summary, review the summary.
//! Step 3 crashes unless `CLAWNICLE_HEAL=1` is set. Rerun with the env
//! var and the first two steps short-circuit from the journal (the
//! demo prints "skipped (already done)" for each), step 3 succeeds,
//! step 4 runs.
//!
//! The goal is to make the durability story legible without requiring
//! the reader to understand Rust, Tokio, or SQLite.

use std::io::{self, Write};
use std::time::Duration;

use clawnicle_core::Result;
use clawnicle_journal::Journal;
use clawnicle_runtime::Context;

const JOURNAL_PATH: &str = "research.clawnicle.db";
const WORKFLOW_ID: &str = "research-1";

#[tokio::main]
async fn main() -> Result<()> {
    let heal = std::env::var("CLAWNICLE_HEAL").is_ok();
    let journal = Journal::open(JOURNAL_PATH)?;
    let mut cx = Context::open_or_start(journal, WORKFLOW_ID, "research", "v1")?;

    print_header();

    if cx.cached_final_output()?.is_some() {
        println!("  already done in a previous run.");
        println!();
        return Ok(());
    }

    match run(&mut cx, heal).await {
        Ok(skipped) => {
            cx.complete(&"done")?;
            println!();
            if skipped > 0 {
                let noun = if skipped == 1 { "step" } else { "steps" };
                println!("  done. {skipped} {noun} skipped from the previous run.");
            } else {
                println!("  done.");
            }
        }
        Err(e) => {
            println!();
            println!("  the workflow crashed: {e}");
            println!("  rerun with CLAWNICLE_HEAL=1 to resume.");
            println!("  steps that already succeeded will be skipped.");
            std::process::exit(1);
        }
    }
    Ok(())
}

fn print_header() {
    println!("╭─ research agent ─────────────────────────────╮");
    println!("│ topic: space travel                          │");
    println!("╰──────────────────────────────────────────────╯");
    println!();
}

async fn run(cx: &mut Context, heal: bool) -> Result<u32> {
    let mut skipped = 0;

    skipped += do_step(cx, "1/4", "Searching for articles", "search-articles", || async {
        tokio::time::sleep(Duration::from_millis(450)).await;
        Ok::<_, io::Error>("found 3 articles".to_string())
    })
    .await?;

    skipped += do_step(cx, "2/4", "Extracting key points", "extract-points", || async {
        tokio::time::sleep(Duration::from_millis(450)).await;
        Ok::<_, io::Error>("extracted 5 points".to_string())
    })
    .await?;

    skipped += do_step(cx, "3/4", "Writing summary", "write-summary", move || async move {
        tokio::time::sleep(Duration::from_millis(450)).await;
        if !heal {
            return Err(io::Error::other("writer service returned 503"));
        }
        Ok("wrote 220-word summary".to_string())
    })
    .await?;

    skipped += do_step(cx, "4/4", "Reviewing summary", "review-summary", || async {
        tokio::time::sleep(Duration::from_millis(450)).await;
        Ok::<_, io::Error>("looks good".to_string())
    })
    .await?;

    Ok(skipped)
}

async fn do_step<F, Fut>(
    cx: &mut Context,
    num: &str,
    label: &str,
    step_id: &str,
    tool: F,
) -> Result<u32>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<String, io::Error>>,
{
    let cached = cx.has_cached_tool_call(step_id)?;
    print!("  {num}  {label:<26}  ");
    io::stdout().flush().ok();

    let msg = match cx.call::<String, _, _, io::Error>(step_id, tool).await {
        Ok(m) => m,
        Err(e) => {
            println!("FAILED");
            return Err(e);
        }
    };

    if cached {
        println!("skipped (already done)");
        Ok(1)
    } else {
        println!("{msg}");
        Ok(0)
    }
}
