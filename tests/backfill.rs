//! History backfill from the vendors' incident feeds, over captured fixtures.

use aretheydown::history::{self, Record, backfill};
use aretheydown::model::Level;
use aretheydown::providers::{gcp, statuspage};
use chrono::{DateTime, TimeZone, Utc};

const ANTHROPIC_INCIDENTS: &[u8] = include_bytes!("fixtures/anthropic_incidents.json");
const ANTHROPIC_SUMMARY: &[u8] = include_bytes!("fixtures/anthropic_summary.json");
const GCP_CLOSED: &[u8] = include_bytes!("fixtures/gcp_incidents_closed.json");

fn utc(month: u32, day: u32, hour: u32, minute: u32, second: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, month, day, hour, minute, second)
        .unwrap()
}

/// Each record as (time to the second, level, number of active incidents).
fn rows(records: &[Record]) -> Vec<(String, Level, usize)> {
    records
        .iter()
        .map(|r| {
            assert!(r.backfilled, "every backfilled record says so");
            assert_eq!(r.status.fetched_at, r.recorded_at);
            (
                r.recorded_at.format("%m-%d %H:%M:%S").to_string(),
                r.status.level,
                r.status.incidents.len(),
            )
        })
        .collect()
}

fn row(at: &str, level: Level, incidents: usize) -> (String, Level, usize) {
    (at.to_owned(), level, incidents)
}

#[test]
fn statuspage_incidents_become_a_record_at_each_change() {
    let past = statuspage::parse_past(ANTHROPIC_INCIDENTS, None, None).unwrap();
    let now = utc(11, 14, 8, 0, 0);
    let records = backfill("Anthropic", &past, None, now);
    // Every incident in the fixture is `major`. The Microsoft 365 connector
    // incident (11-12 17:52 to 11-13 21:49) overlaps two others, so the set
    // of active incidents changes inside one long outage.
    assert_eq!(
        rows(&records),
        [
            row("11-12 17:52:44", Level::Outage, 1),
            row("11-13 09:58:58", Level::Outage, 2),
            row("11-13 11:58:45", Level::Outage, 1),
            row("11-13 13:09:50", Level::Outage, 2),
            row("11-13 15:38:49", Level::Outage, 1),
            row("11-13 21:49:44", Level::Operational, 0),
            row("11-14 02:54:33", Level::Outage, 1),
            row("11-14 04:23:05", Level::Operational, 0),
            // Still unresolved: active through `now`.
            row("11-14 07:11:36", Level::Outage, 1),
        ]
    );
    let first = &records[0].status;
    assert_eq!(first.name, "Anthropic");
    assert_eq!(first.description, "1 active incident");
    let incident = &first.incidents[0];
    assert_eq!(
        incident.title,
        "Elevated errors affecting the Microsoft 365 connector"
    );
    assert_eq!(incident.components, ["claude.ai", "Claude Cowork"]);
    assert!(
        incident
            .url
            .as_deref()
            .is_some_and(|u| u.starts_with("https://"))
    );
    // As it stood then: active, with no later stage or update.
    assert_eq!(incident.ended_at, None);
    assert_eq!(incident.stage, None);
    assert_eq!(incident.latest_update, None);
    // Newest start first, as the feeds list them.
    let titles: Vec<&str> = records[1]
        .status
        .incidents
        .iter()
        .map(|i| i.title.as_str())
        .collect();
    assert_eq!(
        titles,
        [
            "Elevated errors on claude.ai, Claude Code, and platform.claude.com",
            "Elevated errors affecting the Microsoft 365 connector",
        ]
    );
    assert_eq!(records[5].status.description, "No active incidents");
}

#[test]
fn only_records_before_the_platforms_earliest_record_are_given() {
    let past = statuspage::parse_past(ANTHROPIC_INCIDENTS, None, None).unwrap();
    let now = utc(11, 14, 8, 0, 0);
    let until = utc(11, 14, 2, 54, 33);
    let records = backfill("Anthropic", &past, Some(until), now);
    // The 02:54:33.303 start is just after `until`: strictly before only.
    assert_eq!(records.len(), 6);
    assert!(records.iter().all(|r| r.recorded_at < until));
}

#[test]
fn a_second_backfill_adds_nothing() {
    let past = statuspage::parse_past(ANTHROPIC_INCIDENTS, None, None).unwrap();
    let now = utc(11, 14, 8, 0, 0);
    let first = backfill("Anthropic", &past, None, now);
    assert_eq!(first.len(), 9);
    // The next start finds the backfilled records as the earliest.
    let later = now + chrono::TimeDelta::hours(1);
    let again = backfill("Anthropic", &past, history::earliest(&first), later);
    assert_eq!(again, []);
}

#[test]
fn components_filter_applies_to_statuspage_backfill() {
    let watch = ["claude console (platform.claude.com)".to_owned()];
    let past = statuspage::parse_past(ANTHROPIC_INCIDENTS, None, Some(&watch)).unwrap();
    let titles: Vec<&str> = past.iter().map(|p| p.incident.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Elevated errors on claude.ai, Claude Code, and platform.claude.com"]
    );
    let records = backfill("Anthropic", &past, None, utc(11, 14, 8, 0, 0));
    assert_eq!(
        rows(&records),
        [
            row("11-13 09:58:58", Level::Outage, 1),
            row("11-13 11:58:45", Level::Operational, 0),
        ]
    );
}

#[test]
fn a_group_name_matches_through_the_summary() {
    // Incident components carry `group_id` but the list has no group rows.
    let incidents = br#"{"page": {"url": "https://status.example.com"}, "incidents": [
        {"id": "a", "name": "In the group", "impact": "minor",
         "started_at": "2026-11-13T10:00:00Z", "resolved_at": "2026-11-13T11:00:00Z",
         "components": [{"id": "c1", "name": "Bedrock", "group_id": "g1"}]},
        {"id": "b", "name": "Elsewhere", "impact": "major",
         "started_at": "2026-11-13T12:00:00Z", "resolved_at": "2026-11-13T13:00:00Z",
         "components": [{"id": "c2", "name": "Website"}]}
    ]}"#;
    let summary = br#"{"status": {"indicator": "none", "description": "ok"},
        "components": [{"id": "g1", "name": "Cloud Platforms", "group": true}]}"#;
    let watch = ["Cloud Platforms".to_owned()];
    let past = statuspage::parse_past(incidents, Some(summary), Some(&watch)).unwrap();
    assert_eq!(past.len(), 1);
    assert_eq!(past[0].level, Level::Degraded);
    assert_eq!(
        past[0].incident.url.as_deref(),
        Some("https://status.example.com/incidents/a")
    );
    // Without the summary the group name matches nothing.
    let past = statuspage::parse_past(incidents, None, Some(&watch)).unwrap();
    assert_eq!(past.len(), 0);
}

#[test]
fn the_anthropic_summary_parses_as_the_group_source() {
    let watch = ["Claude on Cloud Platforms".to_owned()];
    let past =
        statuspage::parse_past(ANTHROPIC_INCIDENTS, Some(ANTHROPIC_SUMMARY), Some(&watch)).unwrap();
    // No fixture incident touches that group.
    assert_eq!(past.len(), 0);
}

#[test]
fn google_cloud_severity_sets_the_level_within_retention() {
    let past = gcp::parse_past(GCP_CLOSED).unwrap();
    assert_eq!(past.len(), 4);
    let now = utc(10, 21, 0, 0, 0);
    let records = backfill("Google Cloud", &past, None, now);
    // The `high` incident of 08-20 and the `low` one of 02-27 ended before
    // the 30-day window (from 09-21): not written.
    assert_eq!(
        rows(&records),
        [
            row("10-08 11:00:00", Level::Degraded, 1),
            row("10-09 16:46:00", Level::Operational, 0),
            row("10-20 06:00:00", Level::Degraded, 1),
            row("10-20 12:08:00", Level::Operational, 0),
        ]
    );
    let incident = &records[0].status.incidents[0];
    assert_eq!(incident.components, ["Google Cloud Storage"]);
    assert_eq!(
        incident.url.as_deref(),
        Some("https://status.cloud.google.com/incidents/mSFVt6PiP557bjRyYsut")
    );
    // Within 30 days of the `high` one, it shows as Outage.
    let records = backfill("Google Cloud", &past, None, utc(9, 1, 0, 0, 0));
    assert_eq!(
        rows(&records),
        [
            row("08-20 15:40:00", Level::Outage, 1),
            row("08-20 19:20:00", Level::Operational, 0),
        ]
    );
}

#[test]
fn unparseable_feeds_are_errors() {
    assert!(statuspage::parse_past(b"[]", None, None).is_err());
    assert!(statuspage::parse_past(ANTHROPIC_INCIDENTS, Some(b"nope"), None).is_err());
    assert!(gcp::parse_past(b"{}").is_err());
}
