// swisha must never send a second payout instruction for one reference. A resubmission needs a
// fresh payoutInstructionUUID, which Swish cannot tie back to the original, so it can debit
// twice. The guarantee is that only one call site exists, and no type can express that.

use std::fs;
use std::path::{Path, PathBuf};

fn sources() -> Vec<PathBuf> {
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).expect("read the source directory") {
            let path = entry.expect("read a directory entry").path();
            if path.is_dir() {
                walk(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut found);
    assert!(!found.is_empty(), "found no sources to scan, the test would pass vacuously");
    found
}

fn hits(needle: &str, skip_definition: bool) -> Vec<String> {
    let mut hits = Vec::new();
    for path in sources() {
        let text = fs::read_to_string(&path).expect("read a source file");
        for (index, line) in text.lines().enumerate() {
            if !line.contains(needle) {
                continue;
            }
            if skip_definition && line.contains("pub async fn submit_payout") {
                continue;
            }
            let name = path.file_name().unwrap_or_default().to_string_lossy().to_string();
            hits.push(format!("{name}:{}", index + 1));
        }
    }
    hits
}

#[test]
fn only_the_payout_route_can_submit_to_swish() {
    let callers = hits("submit_payout(", true);
    assert_eq!(
        callers.len(),
        1,
        "exactly one call site may submit a payout to Swish, found: {callers:?}"
    );
    assert!(
        callers[0].starts_with("payout.rs:"),
        "the only submit call site must be the payout route, found: {callers:?}"
    );
}

// Proves the sanity of the check above: if the scan were broken it would report zero for
// everything, and the assertion on one call site would be the only thing that noticed.
#[test]
fn the_source_scan_actually_finds_things() {
    assert!(!hits("poll_payout_status(", false).is_empty());
    assert!(!hits("PayoutStore", false).is_empty());
}

#[test]
fn no_resubmission_machinery_survives() {
    for banned in [
        "retry_payout",
        "sweep_retry",
        "claim_retry",
        "retry_context",
        "RetryContext",
        "execute_payout",
        "AUTO_RETRY_CODES",
        "is_auto_retryable",
        "may_sweep_retry",
        "sweep_stalled",
        "retry_count",
        "FAILED_RETRY",
        "FailedRetry",
    ] {
        let found = hits(banned, false);
        assert!(found.is_empty(), "`{banned}` should be gone, found at {found:?}");
    }
}
