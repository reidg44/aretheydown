//! Google Cloud parsing, tested against incident lists captured from the live endpoint.

use aretheydown::model::{IncidentKind, Level};
use aretheydown::providers::gcp::{level_for_severity, parse};
use chrono::{DateTime, TimeZone, Utc};

/// Four closed incidents (one of them `high`), trimmed from the live feed.
const CLOSED: &[u8] = include_bytes!("fixtures/gcp_incidents_closed.json");
/// A real `high` incident with `end` removed, followed by two closed ones.
const ACTIVE: &[u8] = include_bytes!("fixtures/gcp_incidents_active.json");

fn fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 13, 20, 0, 0).unwrap()
}

/// A one-incident list with the given severity and, optionally, an end time.
fn one_incident(severity: &str, end: Option<&str>) -> String {
    let end = end.map_or(String::new(), |end| format!(r#""end": "{end}","#));
    format!(
        r#"[{{
            "id": "abc",
            "begin": "2026-11-13T10:00:00+00:00",
            {end}
            "severity": "{severity}",
            "external_desc": "Something is wrong",
            "uri": "incidents/abc",
            "affected_products": [{{"title": "Cloud Run", "id": "x"}}]
        }}]"#
    )
}

#[test]
fn severity_maps_to_level() {
    assert_eq!(level_for_severity("high"), Level::Outage);
    assert_eq!(level_for_severity("medium"), Level::Degraded);
    assert_eq!(level_for_severity("low"), Level::Degraded);
    assert_eq!(level_for_severity("something-new"), Level::Degraded);
}

#[test]
fn closed_incidents_are_ignored() {
    let status = parse("Google Cloud", CLOSED, fetched_at()).unwrap();
    assert_eq!(status.name, "Google Cloud");
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.description, "No active incidents");
    assert_eq!(status.incidents, []);
    assert_eq!(status.fetched_at, fetched_at());
    assert_eq!(status.error, None);
}

#[test]
fn active_high_incident_is_an_outage() {
    let status = parse("Google Cloud", ACTIVE, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Outage);
    assert_eq!(status.description, "1 active incident");
    assert_eq!(status.incidents.len(), 1);

    let incident = &status.incidents[0];
    assert_eq!(incident.kind, IncidentKind::Incident);
    assert!(
        incident
            .title
            .starts_with("We are investigating an issue where customers may experience timeouts"),
        "title was {:?}",
        incident.title
    );
    assert_eq!(incident.components.len(), 27);
    assert_eq!(incident.components[0], "AlloyDB for PostgreSQL");
    assert_eq!(incident.components[26], "Persistent Disk");
    assert_eq!(
        incident.started_at,
        Some("2026-08-20T15:40:00Z".parse().unwrap())
    );
    assert_eq!(
        incident.url.as_deref(),
        Some("https://status.cloud.google.com/incidents/utF3FMFdQfwBzJcGG6vf")
    );
    assert_eq!(incident.stage, None);
    let update = incident.latest_update.as_ref().unwrap();
    // Shown as-is: a markdown report, newlines included.
    assert!(
        update
            .text
            .starts_with("# Incident Report\n## Summary\nOn Thursday, 20 August 2026"),
        "{update:?}"
    );
    assert_eq!(
        update.posted_at,
        Some("2026-08-27T21:45:02Z".parse().unwrap())
    );
}

#[test]
fn an_incident_without_a_recent_update_has_none() {
    let status = parse(
        "Google Cloud",
        one_incident("low", None).as_bytes(),
        fetched_at(),
    )
    .unwrap();
    assert_eq!(status.incidents[0].latest_update, None);
    assert_eq!(status.incidents[0].stage, None);
}

#[test]
fn active_medium_or_low_incident_is_degraded() {
    for severity in ["medium", "low"] {
        let body = one_incident(severity, None);
        let status = parse("Google Cloud", body.as_bytes(), fetched_at()).unwrap();
        assert_eq!(status.level, Level::Degraded, "severity {severity}");
        assert_eq!(status.incidents.len(), 1);
        assert_eq!(status.incidents[0].components, vec!["Cloud Run"]);
    }
}

#[test]
fn null_end_counts_as_active() {
    let body = r#"[{"id": "a", "end": null, "severity": "medium", "external_desc": "Ongoing", "uri": "incidents/a"}]"#;
    let status = parse("Google Cloud", body.as_bytes(), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    let incident = &status.incidents[0];
    assert_eq!(incident.started_at, None);
    assert_eq!(incident.components, Vec::<String>::new());
}

#[test]
fn closed_high_incident_does_not_count() {
    let body = one_incident("high", Some("2026-11-13T11:00:00+00:00"));
    let status = parse("Google Cloud", body.as_bytes(), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.incidents, []);
}

#[test]
fn worst_active_incident_decides_the_level() {
    let body = r#"[
        {"id": "a", "severity": "low", "external_desc": "Minor", "uri": "incidents/a"},
        {"id": "b", "severity": "high", "external_desc": "Major", "uri": "incidents/b"},
        {"id": "c", "severity": "medium", "external_desc": "Middling", "uri": "incidents/c"}
    ]"#;
    let status = parse("Google Cloud", body.as_bytes(), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Outage);
    assert_eq!(status.description, "3 active incidents");
    let titles: Vec<_> = status.incidents.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Minor", "Major", "Middling"]);
}

#[test]
fn empty_list_is_operational() {
    let status = parse("Google Cloud", b"[]", fetched_at()).unwrap();
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.incidents, []);
}

#[test]
fn malformed_body_is_an_error() {
    assert!(parse("Google Cloud", b"<html>502</html>", fetched_at()).is_err());
    assert!(parse("Google Cloud", br#"{"incidents": []}"#, fetched_at()).is_err());
}

#[test]
fn blank_description_gets_a_fallback_title() {
    let body = one_incident("medium", None).replace("Something is wrong", "  ");
    let status = parse("Google Cloud", body.as_bytes(), fetched_at()).unwrap();
    assert_eq!(status.incidents[0].title, "Untitled incident");
}

#[test]
fn closed_incidents_inside_24h_are_resolved_and_older_ones_are_dropped() {
    // The newest closed incident ended 2026-10-20 12:08; the next one 11 days earlier.
    let fetched_at = Utc.with_ymd_and_hms(2026, 10, 21, 0, 0, 0).unwrap();
    let status = parse("Google Cloud", CLOSED, fetched_at).unwrap();
    assert_eq!(
        status.level,
        Level::Operational,
        "resolved never raises the level"
    );
    assert_eq!(status.incidents, []);
    assert_eq!(status.resolved.len(), 1);
    let resolved = &status.resolved[0];
    assert!(
        resolved
            .title
            .starts_with("Multiple services were experiencing issu"),
        "{:?}",
        resolved.title
    );
    assert_eq!(
        resolved.started_at,
        Some("2026-10-20T06:00:00Z".parse().unwrap())
    );
    assert_eq!(
        resolved.ended_at,
        Some("2026-10-20T12:08:00Z".parse().unwrap())
    );
    assert!(
        resolved
            .url
            .as_deref()
            .is_some_and(|url| url.starts_with("https://status.cloud.google.com/incidents/")),
        "{:?}",
        resolved.url
    );
    assert_eq!(resolved.stage, None);
}

#[test]
fn active_incidents_stay_out_of_resolved() {
    // The GCS incident ended 2026-10-09 16:46, inside the window.
    let fetched_at = Utc.with_ymd_and_hms(2026, 10, 10, 0, 0, 0).unwrap();
    let status = parse("Google Cloud", ACTIVE, fetched_at).unwrap();
    assert_eq!(status.level, Level::Outage);
    assert_eq!(status.incidents.len(), 1);
    assert_eq!(status.incidents[0].ended_at, None);
    assert_eq!(status.resolved.len(), 1);
    assert!(status.resolved[0].title.starts_with("Google Cloud Storage"));
}

#[test]
fn resolved_incidents_are_newest_end_first() {
    let body = r#"[
        {"id": "a", "begin": "2026-11-13T08:00:00Z", "end": "2026-11-13T09:00:00Z", "external_desc": "Older end"},
        {"id": "b", "begin": "2026-11-13T07:00:00Z", "end": "2026-11-13T18:00:00Z", "external_desc": "Newer end"},
        {"id": "c", "begin": "2026-11-11T07:00:00Z", "end": "2026-11-12T19:59:59Z", "external_desc": "Just too old"}
    ]"#;
    let status = parse("Google Cloud", body.as_bytes(), fetched_at()).unwrap();
    let titles: Vec<_> = status.resolved.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Newer end", "Older end"]);
    assert_eq!(status.level, Level::Operational);
}
