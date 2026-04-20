use std::io;

use clawnicle_core::Result;
use clawnicle_journal::Journal;
use clawnicle_runtime::Context;

#[tokio::main]
async fn main() -> Result<()> {
    let journal_path = "resume-demo.clawnicle.db";
    let workflow_id = "demo-1";
    let heal = std::env::var("CLAWNICLE_HEAL").is_ok();

    println!("clawnicle resume demo");
    println!("  journal:     {journal_path}");
    println!("  workflow:    {workflow_id}");
    println!("  heal mode:   {heal}");
    println!();

    let journal = Journal::open(journal_path)?;
    let mut cx = Context::open_or_start(journal, workflow_id, "demo", "h")?;

    if let Some(out) = cx.cached_final_output()? {
        println!("workflow already completed (from journal): {out}");
        println!("  tip: rm resume-demo.clawnicle.db* to start over");
        return Ok(());
    }

    match run(&mut cx, heal).await {
        Ok(sum) => {
            cx.complete(&sum)?;
            println!();
            println!("workflow completed: sum = {sum}");
        }
        Err(e) => {
            eprintln!();
            eprintln!("workflow failed: {e}");
            eprintln!("  rerun with CLAWNICLE_HEAL=1 to resume from the last successful step");
            std::process::exit(1);
        }
    }

    Ok(())
}

async fn run(cx: &mut Context, heal: bool) -> Result<u32> {
    let a = cx
        .call::<u32, _, _, io::Error>("step-a", || async {
            println!("  step-a: running (not cached)");
            Ok(10)
        })
        .await?;
    println!("  step-a result = {a}");

    let b = cx
        .call::<u32, _, _, io::Error>("step-b", || async {
            println!("  step-b: running (not cached)");
            if !heal {
                return Err(io::Error::other("step-b exploded"));
            }
            Ok(20)
        })
        .await?;
    println!("  step-b result = {b}");

    let c = cx
        .call::<u32, _, _, io::Error>("step-c", || async {
            println!("  step-c: running (not cached)");
            Ok(30)
        })
        .await?;
    println!("  step-c result = {c}");

    Ok(a + b + c)
}
