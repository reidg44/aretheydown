//! `--at` for a time before `--serve` first ran: the state comes from
//! records backfilled from the vendor's incident history, and says so.

use std::path::PathBuf;
use std::process::Command;

use aretheydown::history::{self, backfill};
use aretheydown::providers::statuspage;
use chrono::{TimeDelta, TimeZone, Utc};

const ANTHROPIC_INCIDENTS: &[u8] = include_bytes!("fixtures/anthropic_incidents.json");

/// Run the binary with `args` and return (exit code, stdout).
fn run(args: &[&str]) -> (Option<i32>, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_aretheydown"))
        .args(args)
        .output()
        .expect("run aretheydown");
    (
        output.status.code(),
        String::from_utf8(output.stdout).expect("utf-8 output"),
    )
}

#[test]
fn at_before_serve_ran_shows_the_backfilled_state() {
    // `--serve` first ran at 06:00 on 2026-11-14 and saw Anthropic Operational;
    // the backfill filled in everything before that.
    let first_poll = Utc.with_ymd_and_hms(2026, 11, 14, 6, 0, 0).unwrap();
    let past = statuspage::parse_past(ANTHROPIC_INCIDENTS, None, None).unwrap();
    let mut records = backfill(
        "Anthropic",
        &past,
        Some(first_poll),
        first_poll + TimeDelta::minutes(1),
    );
    let mut seen = records[0].clone();
    seen.recorded_at = first_poll;
    seen.status.description = "All Systems Operational".to_owned();
    seen.status.level = aretheydown::model::Level::Operational;
    seen.status.incidents.clear();
    seen.backfilled = false;
    records.push(seen);
    let text: String = records.iter().map(history::line).collect();

    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("at-backfilled.jsonl");
    std::fs::write(&path, text).unwrap();
    let path = path.to_str().unwrap();

    // During the 02:54 outage, before --serve ran.
    let (code, out) = run(&["--at", "2026-11-14T03:30:00Z", "--history", path]);
    assert_eq!(code, Some(1), "{out}");
    assert!(
        out.contains("Anthropic  Outage  1 active incident (from vendor history)"),
        "{out}"
    );
    assert!(
        out.contains("Elevated purchasing errors on claude.ai"),
        "{out}"
    );

    let (code, out) = run(&["--at", "2026-11-14T03:30:00Z", "--json", "--history", path]);
    assert_eq!(code, Some(1), "{out}");
    let json: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(json[0]["backfilled"], true);
    assert_eq!(json[0]["level"], "outage");

    // After --serve started: its own record, not marked.
    let (code, out) = run(&["--at", "2026-11-14T06:30:00Z", "--history", path]);
    assert_eq!(code, Some(0), "{out}");
    assert!(!out.contains("from vendor history"), "{out}");
    let (_, out) = run(&["--at", "2026-11-14T06:30:00Z", "--json", "--history", path]);
    assert!(!out.contains("backfilled"), "{out}");
}
