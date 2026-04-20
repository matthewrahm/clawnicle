//! Durable LLM-summary agent.
//!
//! Reads text from stdin, asks the model for a 3-bullet summary, prints the
//! result. Every LLM call is journaled; rerun the same workflow id and the
//! cached response comes back without hitting the provider.
//!
//! If `ANTHROPIC_API_KEY` is set and the binary was built with the
//! `anthropic` feature, the real Anthropic Messages API is used. Otherwise
//! the agent falls back to a MockProvider returning a canned bullet list —
//! so the demo always runs end-to-end, no key required.

use std::io::Read;

use anyhow::{Context as _, Result};
use clawnicle_core::{LlmMessage, LlmRequest, LlmResponse};
use clawnicle_journal::Journal;
use clawnicle_llm::MockProvider;
use clawnicle_runtime::Context;

const MODEL: &str = "claude-haiku-4-5";
const SYSTEM_PROMPT: &str = "You are a terse summarizer. Given input text, reply \
with EXACTLY three one-line bullet points, each starting with '- '. \
No preamble, no trailing text, no markdown beyond the bullets.";

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let workflow_id = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| format!("summarize-{}", now_ms()));

    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .context("reading stdin")?;
    let input = input.trim();

    if input.is_empty() {
        anyhow::bail!("no input on stdin — pipe some text: echo 'hello' | summarize-agent");
    }

    let journal_path = "summarize.clawnicle.db";
    println!("journal:  {journal_path}");
    println!("workflow: {workflow_id}");

    let journal = Journal::open(journal_path)?;
    let mut cx = Context::open_or_start(journal, &workflow_id, "summarize", "v1")?;

    if let Some(out) = cx.cached_final_output()? {
        println!();
        println!("workflow already completed — cached output:");
        println!("{}", out.as_str().unwrap_or(""));
        return Ok(());
    }

    let request = LlmRequest {
        model: MODEL.into(),
        messages: vec![LlmMessage::system(SYSTEM_PROMPT), LlmMessage::user(input)],
        temperature: Some(0.2),
        max_tokens: Some(256),
    };

    let response = complete(&mut cx, &request).await?;

    println!();
    println!("summary:");
    println!("{}", response.content);

    println!();
    println!(
        "tokens: in={} out={}  session_total={}",
        response.tokens_in,
        response.tokens_out,
        cx.usage().tokens
    );

    cx.complete(&response.content)?;
    Ok(())
}

async fn complete(cx: &mut Context, req: &LlmRequest) -> Result<LlmResponse> {
    #[cfg(feature = "anthropic")]
    {
        if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
            println!("provider: anthropic");
            let provider = clawnicle_llm::AnthropicProvider::new(key)?;
            let resp = cx.complete_llm("summarize", &provider, req).await?;
            return Ok(resp);
        }
    }
    println!("provider: mock (set ANTHROPIC_API_KEY + --features anthropic for real calls)");
    let provider = MockProvider::new(vec![LlmResponse {
        model: MODEL.into(),
        content: "- first key point from the text\n- second observation\n- closing note".into(),
        tokens_in: estimate_tokens(req),
        tokens_out: 24,
    }]);
    Ok(cx.complete_llm("summarize", &provider, req).await?)
}

fn estimate_tokens(req: &LlmRequest) -> u32 {
    // Dumb estimate: 1 token per 4 chars. Good enough for the mock path.
    let chars: usize = req.messages.iter().map(|m| m.content.len()).sum();
    (chars / 4).min(u32::MAX as usize) as u32
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
