use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use clawnicle_core::Result;
use clawnicle_journal::Journal;
use clawnicle_runtime::Context;
use tempfile::tempdir;

async fn scan(
    cx: &mut Context,
    a_calls: Arc<AtomicU32>,
    b_calls: Arc<AtomicU32>,
    c_calls: Arc<AtomicU32>,
    b_should_fail: bool,
) -> Result<u32> {
    let a = cx
        .call::<u32, _, _, io::Error>("step-a", || {
            let a_calls = a_calls.clone();
            async move {
                a_calls.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            }
        })
        .await?;

    let b = cx
        .call::<u32, _, _, io::Error>("step-b", || {
            let b_calls = b_calls.clone();
            async move {
                b_calls.fetch_add(1, Ordering::SeqCst);
                if b_should_fail {
                    return Err(io::Error::other("simulated mid-workflow crash"));
                }
                Ok(2)
            }
        })
        .await?;

    let c = cx
        .call::<u32, _, _, io::Error>("step-c", || {
            let c_calls = c_calls.clone();
            async move {
                c_calls.fetch_add(1, Ordering::SeqCst);
                Ok(4)
            }
        })
        .await?;

    Ok(a + b + c)
}

#[tokio::test]
async fn crash_mid_workflow_resumes_from_last_successful_step() {
    let dir = tempdir().unwrap();
    let db = dir.path().join("j.db");
    let a = Arc::new(AtomicU32::new(0));
    let b = Arc::new(AtomicU32::new(0));
    let c = Arc::new(AtomicU32::new(0));

    // Run 1 — step-a succeeds, step-b fails, step-c never reached.
    {
        let journal = Journal::open(&db).unwrap();
        let mut cx = Context::open_or_start(journal, "scan-1", "scan", "h").unwrap();
        let result = scan(&mut cx, a.clone(), b.clone(), c.clone(), true).await;
        assert!(result.is_err(), "first run should fail");
    }
    assert_eq!(a.load(Ordering::SeqCst), 1, "step-a ran once");
    assert_eq!(b.load(Ordering::SeqCst), 1, "step-b ran once and failed");
    assert_eq!(c.load(Ordering::SeqCst), 0, "step-c never reached");

    // Run 2 — reopen the journal, step-b now succeeds. step-a must not re-run.
    let final_sum = {
        let journal = Journal::open(&db).unwrap();
        let mut cx = Context::open_or_start(journal, "scan-1", "scan", "h").unwrap();
        let result = scan(&mut cx, a.clone(), b.clone(), c.clone(), false)
            .await
            .unwrap();
        cx.complete(&result).unwrap();
        result
    };

    assert_eq!(final_sum, 7);
    assert_eq!(a.load(Ordering::SeqCst), 1, "step-a did NOT re-execute on resume");
    assert_eq!(b.load(Ordering::SeqCst), 2, "step-b re-executed after crash");
    assert_eq!(c.load(Ordering::SeqCst), 1, "step-c ran once after resume");

    // Run 3 — workflow already completed; cached final output returns immediately.
    let journal = Journal::open(&db).unwrap();
    let cx = Context::open_or_start(journal, "scan-1", "scan", "h").unwrap();
    assert_eq!(
        cx.cached_final_output().unwrap(),
        Some(serde_json::json!(7))
    );
}
