//! Microsoft 365 consumer status parsing, tested against a live capture and
//! small hand-written feeds.

use aretheydown::model::{IncidentKind, Level};
use aretheydown::providers::m365::{CONSUMER_FEED_URL, level_for_status, parse};
use aretheydown::providers::{Source, builtin};
use chrono::{DateTime, TimeZone, Utc};

/// Live capture of 2026-11-14: nine `Operational` services and one
/// `Service restored` (Microsoft 365 (Consumer)).
const CONSUMER: &[u8] = include_bytes!("fixtures/m365_consumer.json");

fn fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 14, 16, 0, 0).unwrap()
}

/// A feed of services, each given as `(name, status, title, message)`.
fn feed(services: &[(&str, &str, &str, &str)]) -> String {
    let items: Vec<String> = services
        .iter()
        .map(|(name, status, title, message)| {
            serde_json::json!({
                "ServiceDisplayName": name,
                "ServiceWorkloadName": name,
                "Id": null,
                "AuthoredDateTime": "0001-01-01T00:00:00+00:00",
                "InternalDateTime": "0001-01-01T00:00:00+00:00",
                "LastUpdatedTime": "2026-11-14T15:40:00.1862447+00:00",
                "Title": title,
                "Message": message,
                "Status": status,
            })
            .to_string()
        })
        .collect();
    format!("[{}]", items.join(","))
}

fn parse_feed(services: &[(&str, &str, &str, &str)]) -> aretheydown::model::PlatformStatus {
    parse("Microsoft 365", feed(services).as_bytes(), fetched_at()).unwrap()
}

#[test]
fn status_maps_to_level() {
    assert_eq!(level_for_status("Operational"), Level::Operational);
    assert_eq!(level_for_status("Available"), Level::Operational);
    assert_eq!(level_for_status("Service restored"), Level::Operational);
    assert_eq!(level_for_status(" service DEGRADATION "), Level::Degraded);
    assert_eq!(level_for_status("Unknown"), Level::Unknown);
    assert_eq!(level_for_status("Something new"), Level::Unknown);
}

#[test]
fn live_capture_lists_the_restored_service_but_stays_operational() {
    let status = parse("Microsoft 365", CONSUMER, fetched_at()).unwrap();
    assert_eq!(status.name, "Microsoft 365");
    assert_eq!(status.level, Level::Operational);
    assert_eq!(
        status.description,
        "Operational (1 restored service still in feed)"
    );
    assert_eq!(status.fetched_at, fetched_at());
    assert_eq!(status.error, None);
    assert_eq!(status.incidents.len(), 1);

    let incident = &status.incidents[0];
    assert_eq!(incident.kind, IncidentKind::Incident);
    assert_eq!(
        incident.title,
        "Microsoft 365 (Consumer): Users may be unable to access or perform several actions in Microsoft 365 services"
    );
    assert_eq!(incident.components, ["Microsoft 365 (Consumer)"]);
    assert_eq!(incident.started_at, None, "0001-01-01 is no start time");
    assert_eq!(incident.url, None);
    assert_eq!(incident.stage.as_deref(), Some("Service restored"));
    let update = incident.latest_update.as_ref().unwrap();
    assert!(
        update
            .text
            .starts_with("User impact: Users may have been unable")
    );
    assert!(
        update
            .text
            .ends_with("This is the final update for the event.")
    );
    assert!(!update.text.contains('\r'), "carriage returns are dropped");
    assert_eq!(
        update.posted_at,
        Some(
            Utc.with_ymd_and_hms(2026, 11, 14, 3, 51, 29).unwrap()
                + chrono::Duration::milliseconds(300)
        )
    );
}

#[test]
fn all_operational_lists_nothing() {
    let status = parse_feed(&[
        ("OneDrive", "Operational", "", ""),
        ("Outlook.com", "Available", "", ""),
    ]);
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.description, "All 2 services operational");
    assert_eq!(status.incidents, []);
}

#[test]
fn several_restored_services_are_counted() {
    let status = parse_feed(&[
        ("OneDrive", "Service restored", "Fixed", "Done"),
        ("Outlook.com", "Service restored", "Fixed", "Done"),
        ("Phone Link", "Operational", "", ""),
    ]);
    assert_eq!(status.level, Level::Operational);
    assert_eq!(
        status.description,
        "Operational (2 restored services still in feed)"
    );
    assert_eq!(status.incidents.len(), 2);
}

#[test]
fn a_degraded_service_makes_the_platform_degraded() {
    let status = parse_feed(&[
        ("OneDrive", "Operational", "", ""),
        (
            "Outlook.com",
            "Service degradation",
            "Mail is slow",
            "We're looking into it.",
        ),
        ("Phone Link", "Service restored", "Fixed", "Done"),
    ]);
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(
        status.description,
        "1 of 3 services not operational: Outlook.com (1 restored service still in feed)"
    );
    // Feed order is kept; the restored service is still listed.
    let titles: Vec<&str> = status.incidents.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Outlook.com: Mail is slow", "Phone Link: Fixed"]);
    assert_eq!(
        status.incidents[0].latest_update.as_ref().unwrap().text,
        "We're looking into it."
    );
}

#[test]
fn an_unknown_status_makes_the_platform_unknown() {
    let status = parse_feed(&[
        ("OneDrive", "Service degradation", "Slow", ""),
        ("Outlook.com", "Extended recovery", "", ""),
    ]);
    assert_eq!(status.level, Level::Unknown);
    assert_eq!(
        status.description,
        "2 of 2 services not operational: OneDrive, Outlook.com"
    );
    // A blank title leaves just the service name; a blank message gives no update.
    let outlook = &status.incidents[1];
    assert_eq!(outlook.title, "Outlook.com");
    assert_eq!(outlook.stage.as_deref(), Some("Extended recovery"));
    assert_eq!(outlook.latest_update, None);
}

#[test]
fn a_real_authored_time_is_the_start() {
    let body = r#"[{"ServiceDisplayName": "OneDrive", "Status": "Service degradation",
        "AuthoredDateTime": "2026-11-14T10:05:00+00:00", "Title": "Sync is slow"}]"#;
    let status = parse("Microsoft 365", body.as_bytes(), fetched_at()).unwrap();
    assert_eq!(
        status.incidents[0].started_at,
        Some(Utc.with_ymd_and_hms(2026, 11, 14, 10, 5, 0).unwrap())
    );
    assert_eq!(status.incidents[0].latest_update, None);
}

#[test]
fn an_empty_feed_is_operational() {
    let status = parse("Microsoft 365", b"[]", fetched_at()).unwrap();
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.description, "No services in the feed");
    assert_eq!(status.incidents, []);
}

#[test]
fn a_body_that_is_not_the_feed_is_an_error() {
    assert!(parse("Microsoft 365", b"<html></html>", fetched_at()).is_err());
    assert!(parse("Microsoft 365", br#"{"Status": "Available"}"#, fetched_at()).is_err());
}

#[test]
fn a_missing_or_blank_status_is_unknown() {
    let body = r#"[{"ServiceDisplayName": "OneDrive"},
        {"ServiceDisplayName": "Outlook.com", "Status": "  "}]"#;
    let status = parse("Microsoft 365", body.as_bytes(), fetched_at()).unwrap();
    assert_eq!(status.level, Level::Unknown);
    assert_eq!(status.incidents.len(), 2);
    assert_eq!(status.incidents[0].stage, None);
}

#[test]
fn microsoft_365_is_built_in_with_the_consumer_feed() {
    let m365 = builtin()
        .into_iter()
        .find(|p| p.name == "Microsoft 365")
        .expect("Microsoft 365 is a built-in platform");
    assert_eq!(
        m365.source,
        Source::Microsoft365 {
            url: CONSUMER_FEED_URL.to_owned()
        }
    );
    assert_eq!(
        CONSUMER_FEED_URL,
        "https://status.cloud.microsoft/api/posts/m365Consumer"
    );
}
