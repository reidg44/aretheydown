//! Azure status RSS parsing, tested against saved feeds.

use aretheydown::model::{IncidentKind, Level};
use aretheydown::providers::azure::parse;
use chrono::{DateTime, TimeZone, Utc};

/// Captured from the live feed: no `<item>`s, meaning no active incidents.
const EMPTY: &[u8] = include_bytes!("fixtures/azure_feed_empty.xml");
/// Modeled on the live feed's channel and item structure.
const WITH_ITEMS: &[u8] = include_bytes!("fixtures/azure_feed_items.xml");

fn fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 13, 20, 0, 43).unwrap()
}

#[test]
fn empty_feed_is_operational() {
    let status = parse("Azure", EMPTY, fetched_at()).unwrap();
    assert_eq!(status.name, "Azure");
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.description, "All Systems Operational");
    assert_eq!(status.incidents, []);
    assert_eq!(status.fetched_at, fetched_at());
    assert_eq!(status.error, None);
}

#[test]
fn each_item_is_an_incident_and_the_level_is_degraded() {
    let status = parse("Azure", WITH_ITEMS, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.description, "Active incidents reported");
    assert_eq!(status.error, None);
    assert_eq!(status.incidents.len(), 2);

    let front_door = &status.incidents[0];
    assert_eq!(front_door.kind, IncidentKind::Incident);
    assert_eq!(
        front_door.title,
        "Active - Azure Front Door - Intermittent connectivity issues"
    );
    assert_eq!(front_door.components, Vec::<String>::new());
    // The feed writes the zone as a bare "Z".
    assert_eq!(
        front_door.started_at,
        Some(Utc.with_ymd_and_hms(2026, 11, 13, 18, 15, 27).unwrap())
    );
    assert_eq!(
        front_door.url.as_deref(),
        Some("https://azure.status.microsoft/en-us/status/")
    );
    // The feed has no stage, and updates have no sample yet.
    for incident in &status.incidents {
        assert_eq!(incident.stage, None);
        assert_eq!(incident.latest_update, None);
    }

    assert_eq!(
        status.incidents[1].started_at,
        Some(Utc.with_ymd_and_hms(2026, 11, 13, 16, 2, 9).unwrap())
    );
}

#[test]
fn item_with_missing_or_bad_date_and_link_is_still_listed() {
    let body = br#"<?xml version="1.0" encoding="utf-8"?>
        <rss version="2.0"><channel>
            <title>Azure Status</title>
            <link>https://azure.status.microsoft/en-us/status/</link>
            <description>Azure Status</description>
            <item><title>No date here</title></item>
            <item><title>Bad date</title><pubDate>sometime today</pubDate><link>  </link></item>
        </channel></rss>"#;
    let status = parse("Azure", body, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.incidents.len(), 2);
    assert_eq!(status.incidents[0].title, "No date here");
    assert_eq!(status.incidents[0].started_at, None);
    assert_eq!(status.incidents[0].url, None);
    assert_eq!(status.incidents[1].started_at, None);
    assert_eq!(status.incidents[1].url, None);
}

#[test]
fn non_rss_body_is_an_error() {
    assert!(
        parse(
            "Azure",
            b"<html><body>Service Unavailable</body></html>",
            fetched_at()
        )
        .is_err()
    );
    assert!(parse("Azure", b"{\"not\": \"xml\"}", fetched_at()).is_err());
}
