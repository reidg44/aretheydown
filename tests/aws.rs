//! AWS Health Dashboard parsing, tested against `currentevents` bodies saved as the
//! endpoint sends them: UTF-16 with a byte-order mark.

use aretheydown::model::{IncidentKind, Level};
use aretheydown::providers::aws::{
    DASHBOARD_URL, decode, level_for_status, parse, parse_in_regions,
};
use aretheydown::providers::{self, Source};
use chrono::{DateTime, TimeZone, Utc};

/// Two live events (UAE and Bahrain region disruptions), trimmed: each event keeps
/// its first and last log entries and three impacted services.
const EVENTS: &[u8] = include_bytes!("fixtures/aws_currentevents.json.utf16");
/// No current events: a byte-order mark and `[]`.
const EMPTY: &[u8] = include_bytes!("fixtures/aws_currentevents_empty.json.utf16");

fn fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 13, 20, 0, 0).unwrap()
}

/// `text` as UTF-16 big-endian with a byte-order mark, like the live endpoint.
fn utf16be(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFE, 0xFF];
    bytes.extend(text.encode_utf16().flat_map(u16::to_be_bytes));
    bytes
}

/// `text` as UTF-16 little-endian with a byte-order mark.
fn utf16le(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    bytes
}

fn event(status: &str, summary: &str) -> String {
    format!(
        r#"{{
            "date": "1772369485",
            "arn": "arn:aws:health:us-east-1::event/EC2/AWS_EC2_OPERATIONAL_ISSUE/X",
            "region_name": "N. Virginia",
            "status": {status},
            "service": "ec2-us-east-1",
            "service_name": "Amazon Elastic Compute Cloud",
            "summary": "{summary}",
            "event_log": []
        }}"#
    )
}

#[test]
fn status_code_maps_to_level() {
    assert_eq!(level_for_status(0), Level::Operational);
    assert_eq!(level_for_status(1), Level::Degraded);
    assert_eq!(level_for_status(2), Level::Degraded);
    assert_eq!(level_for_status(3), Level::Outage);
    assert_eq!(level_for_status(-1), Level::Unknown);
    assert_eq!(level_for_status(7), Level::Unknown);
}

#[test]
fn decodes_utf16_in_either_byte_order() {
    assert_eq!(decode(&utf16be("[\"Zürich\"]")).unwrap(), "[\"Zürich\"]");
    assert_eq!(decode(&utf16le("[\"Zürich\"]")).unwrap(), "[\"Zürich\"]");
}

#[test]
fn decodes_utf8_with_or_without_a_byte_order_mark() {
    assert_eq!(decode(b"[]").unwrap(), "[]");
    assert_eq!(decode(b"\xEF\xBB\xBF[]").unwrap(), "[]");
}

#[test]
fn rejects_bodies_that_are_not_text() {
    // A BOM followed by an odd number of bytes cannot be UTF-16.
    assert!(decode(&[0xFE, 0xFF, 0x00, 0x5B, 0x00]).is_err());
    // An unpaired surrogate.
    assert!(decode(&[0xFE, 0xFF, 0xD8, 0x00]).is_err());
    // No BOM and not UTF-8.
    assert!(decode(&[0x5B, 0xFF, 0x5D]).is_err());
}

#[test]
fn empty_event_list_is_operational() {
    let status = parse("AWS", EMPTY, fetched_at()).unwrap();
    assert_eq!(status.name, "AWS");
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.description, "No current events");
    assert_eq!(status.incidents, []);
    assert_eq!(status.fetched_at, fetched_at());
    assert_eq!(status.error, None);
}

#[test]
fn each_live_event_becomes_an_incident() {
    let status = parse("AWS", EVENTS, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Outage);
    assert_eq!(status.description, "2 current events");
    assert_eq!(status.incidents.len(), 2);

    let uae = &status.incidents[0];
    assert_eq!(uae.kind, IncidentKind::Incident);
    assert_eq!(uae.title, "UAE (me-central-1): Region Availability");
    assert_eq!(uae.components, vec!["Multiple services"]);
    assert_eq!(
        uae.started_at,
        Some("2026-03-01T12:51:25Z".parse().unwrap())
    );
    assert_eq!(uae.url.as_deref(), Some(DASHBOARD_URL));

    assert_eq!(uae.stage, None);
    let update = uae.latest_update.as_ref().unwrap();
    assert!(
        update.text.starts_with(
            "We are providing an update on the disruption affecting the Middle East (UAE)"
        ),
        "{update:?}"
    );
    assert_eq!(update.posted_at, DateTime::from_timestamp(1_789_468_030, 0));

    let bahrain = &status.incidents[1];
    assert_eq!(bahrain.title, "Bahrain (me-south-1): Region Availability");
    assert_eq!(
        bahrain.started_at,
        Some("2026-03-02T05:56:27Z".parse().unwrap())
    );
}

#[test]
fn the_latest_log_entry_is_picked_by_timestamp_not_position() {
    let body = event("3", "Region Availability").replace(
        r#""event_log": []"#,
        r#""event_log": [
            {"summary": "s", "message": "Second.", "status": 3, "timestamp": "1772369600"},
            {"summary": "s", "message": "Newest.", "status": 3, "timestamp": 1772369700},
            {"summary": "s", "message": "First.", "status": 1, "timestamp": "1772369500"}
        ]"#,
    );
    let status = parse("AWS", &utf16be(&format!("[{body}]")), fetched_at()).unwrap();
    let incident = &status.incidents[0];
    let update = incident.latest_update.as_ref().unwrap();
    assert_eq!(update.text, "Newest.");
    assert_eq!(update.posted_at, DateTime::from_timestamp(1_772_369_700, 0));
    assert_eq!(incident.stage, None);
}

#[test]
fn an_empty_event_log_means_no_latest_update() {
    let body = format!("[{}]", event("2", "Increased errors"));
    let status = parse("AWS", &utf16be(&body), fetched_at()).unwrap();
    assert_eq!(status.incidents[0].latest_update, None);
}

#[test]
fn level_is_the_worst_current_event() {
    let body = format!("[{}, {}]", event("\"1\"", "Info"), event("\"2\"", "Slow"));
    let status = parse("AWS", &utf16be(&body), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.incidents.len(), 2);

    let body = format!("[{}, {}]", event("\"2\"", "Slow"), event("\"3\"", "Down"));
    let status = parse("AWS", &utf16be(&body), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Outage);
}

#[test]
fn unrecognised_status_is_unknown_unless_another_event_is_an_outage() {
    let body = format!("[{}, {}]", event("\"2\"", "Slow"), event("\"9\"", "New"));
    let status = parse("AWS", &utf16be(&body), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Unknown);

    let body = format!("[{}, {}]", event("\"9\"", "New"), event("\"3\"", "Down"));
    let status = parse("AWS", &utf16be(&body), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Outage);
}

#[test]
fn resolved_events_are_not_listed() {
    let body = format!("[{}, {}]", event("\"0\"", "Fixed"), event("2", "Slow"));
    let status = parse("AWS", &utf16le(&body), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.description, "1 current event");
    assert_eq!(status.incidents.len(), 1);
    assert_eq!(status.incidents[0].title, "N. Virginia (us-east-1): Slow");
    assert_eq!(
        status.incidents[0].components,
        vec!["Amazon Elastic Compute Cloud"]
    );
}

#[test]
fn missing_or_bad_fields_do_not_fail_the_event() {
    let body = r#"[
        {"status": "3", "service_name": "AWS IAM", "summary": "Sign-in errors", "date": "soon"},
        {"status": "2", "region_name": "Global", "arn": "arn:aws:health:global::event/X",
         "summary": "Console latency", "date": 1772369485}
    ]"#;
    let status = parse("AWS", &utf16be(body), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Outage);

    let iam = &status.incidents[0];
    assert_eq!(iam.title, "Sign-in errors");
    assert_eq!(iam.components, vec!["AWS IAM"]);
    assert_eq!(iam.started_at, None);

    let console = &status.incidents[1];
    // The ARN's region repeats the region name, so it is not shown twice.
    assert_eq!(console.title, "Global: Console latency");
    assert_eq!(console.components, Vec::<String>::new());
    assert_eq!(
        console.started_at,
        Some("2026-03-01T12:51:25Z".parse().unwrap())
    );
}

#[test]
fn malformed_body_is_an_error() {
    assert!(parse("AWS", &utf16be("<html>502</html>"), fetched_at()).is_err());
    assert!(parse("AWS", &utf16be("{\"events\": []}"), fetched_at()).is_err());
}

fn regions(codes: &[&str]) -> Vec<String> {
    codes.iter().map(|&code| code.to_owned()).collect()
}

#[test]
fn no_region_filter_counts_every_region() {
    let status = parse_in_regions("AWS", EVENTS, fetched_at(), None).unwrap();
    assert_eq!(status, parse("AWS", EVENTS, fetched_at()).unwrap());
}

#[test]
fn events_outside_the_chosen_regions_are_dropped() {
    let chosen = regions(&["us-east-1", "us-west-2"]);
    let status = parse_in_regions("AWS", EVENTS, fetched_at(), Some(&chosen)).unwrap();
    assert_eq!(status.level, Level::Operational);
    assert_eq!(
        status.description,
        "No current events (2 in other regions not shown)"
    );
    assert_eq!(status.incidents, []);
}

#[test]
fn events_in_a_chosen_region_still_count() {
    let chosen = regions(&["me-south-1"]);
    let status = parse_in_regions("AWS", EVENTS, fetched_at(), Some(&chosen)).unwrap();
    assert_eq!(status.level, Level::Outage);
    assert_eq!(
        status.description,
        "1 current event (1 in other regions not shown)"
    );
    assert_eq!(status.incidents.len(), 1);
    assert_eq!(
        status.incidents[0].title,
        "Bahrain (me-south-1): Region Availability"
    );
}

#[test]
fn region_codes_match_case_insensitively() {
    let chosen = regions(&["ME-CENTRAL-1"]);
    let status = parse_in_regions("AWS", EVENTS, fetched_at(), Some(&chosen)).unwrap();
    assert_eq!(status.incidents.len(), 1);
    assert_eq!(
        status.incidents[0].title,
        "UAE (me-central-1): Region Availability"
    );
}

#[test]
fn global_events_always_count() {
    let body = r#"[
        {"status": "3", "summary": "No ARN at all"},
        {"status": "2", "region_name": "Global", "arn": "arn:aws:health:global::event/X",
         "summary": "Console latency"},
        {"status": "2", "arn": "arn:aws:health:::event/Y", "summary": "Empty region"},
        {"status": "3", "arn": "arn:aws:health:eu-west-1::event/Z", "summary": "Ireland"}
    ]"#;
    let chosen = regions(&["us-east-1"]);
    let status = parse_in_regions("AWS", &utf16be(body), fetched_at(), Some(&chosen)).unwrap();
    let titles: Vec<&str> = status.incidents.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(
        titles,
        ["No ARN at all", "Global: Console latency", "Empty region"]
    );
    assert_eq!(status.level, Level::Outage);
}

#[test]
fn resolved_events_elsewhere_are_not_counted_as_hidden() {
    let body = format!(
        "[{}, {}]",
        event("\"0\"", "Fixed"),
        event("\"2\"", "Slow").replace("us-east-1", "eu-west-1")
    );
    let chosen = regions(&["us-west-2"]);
    let status = parse_in_regions("AWS", &utf16be(&body), fetched_at(), Some(&chosen)).unwrap();
    assert_eq!(
        status.description,
        "No current events (1 in other regions not shown)"
    );
}

#[test]
fn aws_is_a_builtin_platform() {
    let aws = providers::builtin()
        .into_iter()
        .find(|p| p.name == "AWS")
        .expect("AWS is built in");
    assert_eq!(
        aws.source,
        Source::Aws {
            url: "https://health.aws.amazon.com/public/currentevents".to_owned(),
            regions: None,
        }
    );
}
