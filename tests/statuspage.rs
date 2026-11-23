//! Statuspage parsing, tested against summaries captured from the live endpoints.

use aretheydown::model::{IncidentKind, Level, PlatformStatus};
use aretheydown::providers::statuspage::{
    level_for_indicator, parse, parse_resolved, parse_watching, parse_with_resolved,
};
use chrono::{DateTime, TimeZone, Utc};

const GITHUB: &[u8] = include_bytes!("fixtures/github_summary.json");
const CLOUDFLARE: &[u8] = include_bytes!("fixtures/cloudflare_summary.json");
const OPENAI: &[u8] = include_bytes!("fixtures/openai_summary.json");
const ANTHROPIC: &[u8] = include_bytes!("fixtures/anthropic_summary.json");
/// Anthropic's `incidents.json` of 2026-11-14, trimmed to its first five
/// incidents: one unresolved, then resolved at 2026-11-14 04:23, 2026-11-13
/// 21:49, 15:38, and 11:58.
const ANTHROPIC_INCIDENTS: &[u8] = include_bytes!("fixtures/anthropic_incidents.json");

fn fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 13, 19, 10, 0).unwrap()
}

#[test]
fn indicator_maps_to_level() {
    assert_eq!(level_for_indicator("none"), Level::Operational);
    assert_eq!(level_for_indicator("minor"), Level::Degraded);
    assert_eq!(level_for_indicator("major"), Level::Outage);
    assert_eq!(level_for_indicator("critical"), Level::Outage);
    assert_eq!(level_for_indicator("maintenance"), Level::Degraded);
    assert_eq!(level_for_indicator("something-new"), Level::Unknown);
}

#[test]
fn operational_summary_has_no_incidents() {
    let status = parse("GitHub", GITHUB, fetched_at()).unwrap();
    assert_eq!(status.name, "GitHub");
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.description, "All Systems Operational");
    assert_eq!(status.incidents, []);
    assert_eq!(status.fetched_at, fetched_at());
    assert_eq!(status.error, None);
}

#[test]
fn degraded_summary_lists_active_incidents() {
    let status = parse("Cloudflare", CLOUDFLARE, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.description, "Partially Degraded Service");
    assert_eq!(status.incidents.len(), 3);

    let workers = &status.incidents[0];
    assert_eq!(
        workers.title,
        "Cloudflare is seeing an issue with Workers API availability"
    );
    assert_eq!(workers.components, vec!["Workers"]);
    assert_eq!(
        workers.started_at,
        Some("2026-11-13T15:31:13.772Z".parse().unwrap())
    );
    assert_eq!(
        workers.url.as_deref(),
        Some("https://www.cloudflarestatus.com/incidents/e7ktq32gphhe")
    );
    assert_eq!(status.incidents[2].components, vec!["Workers", "Queues"]);
}

#[test]
fn incidents_carry_their_stage_and_newest_update() {
    let status = parse("Cloudflare", CLOUDFLARE, fetched_at()).unwrap();
    let workers = &status.incidents[0];
    assert_eq!(workers.stage.as_deref(), Some("monitoring"));
    let update = workers.latest_update.as_ref().unwrap();
    assert_eq!(
        update.text,
        "A fix has been implemented and we are monitoring the results."
    );
    assert_eq!(
        update.posted_at,
        Some("2026-11-13T15:42:43.996Z".parse().unwrap())
    );
    assert_eq!(status.incidents[1].stage.as_deref(), Some("identified"));
}

/// One incident with three updates listed out of time order.
const SHUFFLED_UPDATES: &[u8] = br#"{
    "status": {"indicator": "minor", "description": "Minor Service Outage"},
    "incidents": [{
        "name": "API errors",
        "status": "identified",
        "created_at": "2026-11-13T10:00:00Z",
        "components": [],
        "incident_updates": [
            {"status": "investigating", "body": "Looking into it.", "created_at": "2026-11-13T10:00:00Z"},
            {"status": "identified", "body": "  Fix rolling out.\n", "created_at": "2026-11-13T12:00:00Z"},
            {"status": "investigating", "body": "Still looking.", "created_at": "2026-11-13T11:00:00Z"}
        ]
    }]
}"#;

#[test]
fn the_newest_update_wins_whatever_the_list_order() {
    let status = parse("Example", SHUFFLED_UPDATES, fetched_at()).unwrap();
    let update = status.incidents[0].latest_update.as_ref().unwrap();
    assert_eq!(update.text, "Fix rolling out.");
    assert_eq!(
        update.posted_at,
        Some("2026-11-13T12:00:00Z".parse().unwrap())
    );
}

#[test]
fn no_updates_means_no_latest_update() {
    let body = br#"{
        "status": {"indicator": "minor", "description": "Minor Service Outage"},
        "incidents": [
            {"name": "A", "status": "investigating", "incident_updates": []},
            {"name": "B", "status": "", "incident_updates": [{"body": " ", "created_at": null}]},
            {"name": "C"}
        ]
    }"#;
    let status = parse("Example", body, fetched_at()).unwrap();
    assert_eq!(status.incidents[0].stage.as_deref(), Some("investigating"));
    for incident in &status.incidents {
        assert_eq!(incident.latest_update, None, "{}", incident.title);
    }
    assert_eq!(status.incidents[1].stage, None);
    assert_eq!(status.incidents[2].stage, None);
}

#[test]
fn upcoming_maintenance_is_not_listed() {
    // The Cloudflare fixture holds one maintenance in status "scheduled" (future).
    let status = parse("Cloudflare", CLOUDFLARE, fetched_at()).unwrap();
    assert!(
        status
            .incidents
            .iter()
            .all(|i| i.kind == IncidentKind::Incident)
    );
}

#[test]
fn in_progress_maintenance_is_listed_after_incidents() {
    let body = br#"{
        "status": {"indicator": "maintenance", "description": "Service Under Maintenance"},
        "incidents": [],
        "scheduled_maintenances": [
            {
                "name": "HKG (Hong Kong) on 2026-11-13",
                "status": "in_progress",
                "started_at": null,
                "scheduled_for": "2026-11-13T21:00:00.000Z",
                "shortlink": "https://www.cloudflarestatus.com/incidents/6nik2rihsm8r",
                "components": [{"name": "Hong Kong - (HKG)"}],
                "incident_updates": [
                    {"status": "scheduled", "body": "Work is planned.", "created_at": "2026-11-12T09:00:00Z"},
                    {"status": "in_progress", "body": "Work has started.", "created_at": "2026-11-13T21:00:05Z"}
                ]
            },
            {
                "name": "Later work",
                "status": "scheduled",
                "scheduled_for": "2026-11-20T21:00:00.000Z",
                "components": []
            }
        ]
    }"#;
    let status = parse("Cloudflare", body, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.incidents.len(), 1);
    let maintenance = &status.incidents[0];
    assert_eq!(maintenance.kind, IncidentKind::Maintenance);
    assert_eq!(maintenance.title, "HKG (Hong Kong) on 2026-11-13");
    assert_eq!(maintenance.components, vec!["Hong Kong - (HKG)"]);
    assert_eq!(
        maintenance.started_at,
        Some("2026-11-13T21:00:00Z".parse().unwrap())
    );
    assert_eq!(maintenance.stage.as_deref(), Some("in_progress"));
    let update = maintenance.latest_update.as_ref().unwrap();
    assert_eq!(update.text, "Work has started.");
}

#[test]
fn openai_summary_parses() {
    // OpenAI's feed (a Statuspage-compatible host) has no `scheduled_maintenances`
    // key, and its incident has no shortlink or components.
    let status = parse("OpenAI", OPENAI, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.description, "Partial System Degradation");
    let titles: Vec<&str> = status.incidents.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Delayed Costs Data in the Compliance API"]);
    let incident = &status.incidents[0];
    assert_eq!(
        incident.started_at,
        Some("2026-11-13T16:10:07Z".parse().unwrap())
    );
    assert_eq!(incident.url, None);
    assert_eq!(incident.stage.as_deref(), Some("monitoring"));
    let update = incident.latest_update.as_ref().unwrap();
    assert_eq!(
        update.text,
        "We are monitoring the affected services to confirm recovery. \
         Delays in cost data are still expected."
    );
    assert_eq!(
        update.posted_at,
        Some("2026-11-14T05:31:20Z".parse().unwrap())
    );
}

#[test]
fn anthropic_summary_parses() {
    let status = parse("Anthropic", ANTHROPIC, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(status.description, "Partially Degraded Service");
    let titles: Vec<&str> = status.incidents.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Some users unable to sign up with Apple on the Claude iOS app"]
    );
    let incident = &status.incidents[0];
    assert_eq!(incident.components, vec!["claude.ai"]);
    assert_eq!(
        incident.url.as_deref(),
        Some("https://stspg.io/4858wf40dx89")
    );
    assert_eq!(incident.stage.as_deref(), Some("investigating"));
    let update = incident.latest_update.as_ref().unwrap();
    assert!(
        update
            .text
            .starts_with("We are investigating an issue with Sign in with Apple"),
        "{update:?}"
    );
    assert_eq!(
        update.posted_at,
        Some("2026-11-14T07:11:36.218Z".parse().unwrap())
    );
}

#[test]
fn malformed_body_is_an_error() {
    assert!(parse("Broken", b"<html>502</html>", fetched_at()).is_err());
}

#[test]
fn missing_started_at_falls_back_to_created_at() {
    let body = br#"{
        "status": {"indicator": "major", "description": "Major Outage"},
        "incidents": [{
            "name": "API down",
            "started_at": null,
            "created_at": "2026-11-13T10:00:00Z",
            "shortlink": null,
            "components": []
        }]
    }"#;
    let status = parse("Example", body, fetched_at()).unwrap();
    assert_eq!(status.level, Level::Outage);
    let incident = &status.incidents[0];
    assert_eq!(
        incident.started_at,
        Some("2026-11-13T10:00:00Z".parse().unwrap())
    );
    assert_eq!(incident.url, None);
    assert_eq!(incident.components, Vec::<String>::new());
}

// --- Component filter (`[components]` in the config) ---

fn watching(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn parse_for(name: &str, body: &[u8], names: &[&str]) -> PlatformStatus {
    parse_watching(name, body, fetched_at(), Some(&watching(names))).unwrap()
}

fn titles(status: &PlatformStatus) -> Vec<&str> {
    status.incidents.iter().map(|i| i.title.as_str()).collect()
}

#[test]
fn no_filter_matches_parse() {
    for (name, body) in [
        ("GitHub", GITHUB),
        ("Cloudflare", CLOUDFLARE),
        ("OpenAI", OPENAI),
        ("Anthropic", ANTHROPIC),
    ] {
        assert_eq!(
            parse_watching(name, body, fetched_at(), None).unwrap(),
            parse(name, body, fetched_at()).unwrap()
        );
    }
}

#[test]
fn watching_workers_counts_only_workers_incidents() {
    let status = parse_for("Cloudflare", CLOUDFLARE, &["workers"]);
    // Degraded from the Workers incidents (minor) and Workers' own status,
    // not Outage-ish datacenter trouble.
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(
        titles(&status),
        [
            "Cloudflare is seeing an issue with Workers API availability",
            "Changes to Cloudflare Workers are not producing Event Subscription events",
        ]
    );
    assert_eq!(
        status.description,
        "1 of 1 watched components not operational: Workers (1 incident on other components not shown)"
    );
}

#[test]
fn datacenter_outages_do_not_raise_an_unrelated_watch() {
    // A datacenter in major outage (and its incident) doesn't touch API.
    let body = br#"{
        "status": {"indicator": "minor", "description": "Partially Degraded Service"},
        "components": [
            {"id": "a", "name": "API", "status": "operational", "group_id": null, "group": false},
            {"id": "b", "name": "Albuquerque - (ABQ)", "status": "major_outage", "group_id": null, "group": false}
        ],
        "incidents": [{
            "name": "ABQ down", "impact": "major", "started_at": null, "created_at": null,
            "shortlink": null,
            "components": [{"id": "b", "name": "Albuquerque - (ABQ)", "status": "major_outage", "group_id": null}]
        }]
    }"#;
    let status = parse_for("Example", body, &["API"]);
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.incidents, []);
    assert_eq!(
        status.description,
        "1 watched component operational (1 incident on other components not shown)"
    );
}

#[test]
fn an_operational_watch_is_operational_despite_a_minor_indicator() {
    let body = br#"{
        "status": {"indicator": "minor", "description": "Partially Degraded Service"},
        "components": [{"id": "x", "name": "Pages", "status": "operational", "group_id": null}],
        "incidents": [{
            "name": "Workers slow", "impact": "minor", "started_at": null, "created_at": null,
            "shortlink": null,
            "components": [{"id": "w", "name": "Workers", "status": "degraded_performance", "group_id": null}]
        }]
    }"#;
    let status = parse_for("Cloudflare", body, &["Pages"]);
    assert_eq!(status.level, Level::Operational);
    assert!(
        status
            .description
            .starts_with("1 watched component operational")
    );
}

#[test]
fn watching_a_group_covers_its_components() {
    let status = parse_for("Cloudflare", CLOUDFLARE, &["North America"]);
    assert_eq!(status.level, Level::Degraded);
    // No incident lists a North America datacenter, so all 3 are left out.
    assert_eq!(status.incidents, []);
    assert_eq!(
        status.description,
        "2 of 2 watched components not operational: Albuquerque, United States - (ABQ), \
         Boston, United States - (BOS) (3 incidents on other components not shown)"
    );
}

#[test]
fn a_component_in_another_group_does_not_count() {
    let body = br#"{
        "status": {"indicator": "major", "description": "Partial System Outage"},
        "components": [
            {"id": "na", "name": "North America", "status": "operational", "group_id": null, "group": true},
            {"id": "eu", "name": "Europe", "status": "major_outage", "group_id": null, "group": true},
            {"id": "bos", "name": "Boston", "status": "operational", "group_id": "na", "group": false},
            {"id": "ams", "name": "Amsterdam", "status": "major_outage", "group_id": "eu", "group": false}
        ],
        "incidents": [{
            "name": "AMS down", "impact": "critical", "started_at": null, "created_at": null,
            "shortlink": null,
            "components": [{"id": "ams", "name": "Amsterdam", "status": "major_outage", "group_id": "eu"}]
        }]
    }"#;
    let status = parse_for("Example", body, &["North America"]);
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.incidents, []);

    let status = parse_for("Example", body, &["europe"]);
    assert_eq!(status.level, Level::Outage);
    assert_eq!(titles(&status), ["AMS down"]);
}

#[test]
fn an_incident_without_components_counts_under_any_filter() {
    let status = parse_for("OpenAI", OPENAI, &["Some Component Not There"]);
    assert_eq!(
        titles(&status),
        ["Delayed Costs Data in the Compliance API"]
    );
    assert_eq!(status.level, Level::Degraded);
}

fn one_component(status: &str, impact: &str) -> Vec<u8> {
    format!(
        r#"{{
            "status": {{"indicator": "none", "description": "All Systems Operational"}},
            "components": [{{"id": "a", "name": "API", "status": "{status}", "group_id": null}}],
            "incidents": [{{
                "name": "Something", "impact": "{impact}", "started_at": null, "created_at": null,
                "shortlink": null,
                "components": [{{"id": "a", "name": "API", "status": "{status}", "group_id": null}}]
            }}]
        }}"#
    )
    .into_bytes()
}

#[test]
fn component_status_and_incident_impact_set_the_level() {
    for (status, impact, expected) in [
        ("operational", "none", Level::Operational),
        ("degraded_performance", "none", Level::Degraded),
        ("partial_outage", "none", Level::Degraded),
        ("under_maintenance", "none", Level::Degraded),
        ("major_outage", "none", Level::Outage),
        ("operational", "minor", Level::Degraded),
        ("operational", "major", Level::Outage),
        ("operational", "critical", Level::Outage),
        ("something_new", "none", Level::Unknown),
        ("operational", "something_new", Level::Unknown),
    ] {
        let parsed = parse_for("Example", &one_component(status, impact), &["API"]);
        assert_eq!(parsed.level, expected, "{status} / {impact}");
    }
}

#[test]
fn a_name_missing_from_the_feed_is_shown_in_the_description() {
    let status = parse_for("Cloudflare", CLOUDFLARE, &["Workers", " Pakages "]);
    assert!(
        status.description.ends_with(r#"(not in feed: "Pakages")"#),
        "{}",
        status.description
    );
}

#[test]
fn in_progress_maintenance_on_a_watched_component_is_degraded() {
    let body = br#"{
        "status": {"indicator": "maintenance", "description": "Service Under Maintenance"},
        "components": [
            {"id": "h", "name": "Hong Kong - (HKG)", "status": "operational", "group_id": null},
            {"id": "s", "name": "Sydney - (SYD)", "status": "operational", "group_id": null}
        ],
        "incidents": [],
        "scheduled_maintenances": [
            {"name": "HKG work", "status": "in_progress", "scheduled_for": null,
             "components": [{"id": "h", "name": "Hong Kong - (HKG)", "status": "operational", "group_id": null}]},
            {"name": "SYD later", "status": "scheduled", "scheduled_for": null,
             "components": [{"id": "s", "name": "Sydney - (SYD)", "status": "operational", "group_id": null}]}
        ]
    }"#;
    let status = parse_for("Cloudflare", body, &["Hong Kong - (HKG)"]);
    assert_eq!(status.level, Level::Degraded);
    assert_eq!(titles(&status), ["HKG work"]);

    let status = parse_for("Cloudflare", body, &["Sydney - (SYD)"]);
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.incidents, []);
    // The upcoming Sydney work is not counted; the HKG work is left out.
    assert_eq!(
        status.description,
        "1 watched component operational (1 incident on other components not shown)"
    );
}

#[test]
fn several_operational_watches_and_none_in_feed_read_naturally() {
    let body = br#"{
        "status": {"indicator": "none", "description": "All Systems Operational"},
        "components": [
            {"id": "a", "name": "API", "status": "operational", "group_id": null},
            {"id": "b", "name": "Web", "status": "operational", "group_id": null}
        ],
        "incidents": []
    }"#;
    assert_eq!(
        parse_for("Example", body, &["API", "Web"]).description,
        "All 2 watched components operational"
    );
    assert_eq!(
        parse_for("Example", body, &["Gone"]).description,
        r#"No watched components in the feed (not in feed: "Gone")"#
    );
}

/// 2026-11-14 20:00 UTC: two of the fixture's resolved incidents ended in the
/// 24 hours before.
fn history_fetched_at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 14, 20, 0, 0).unwrap()
}

#[test]
fn resolved_keeps_the_last_24h_newest_first() {
    let resolved = parse_resolved(ANTHROPIC_INCIDENTS, history_fetched_at(), None).unwrap();
    let titles: Vec<&str> = resolved.iter().map(|i| i.title.as_str()).collect();
    // The unresolved one and the two older than 24h are gone.
    assert_eq!(
        titles,
        [
            "Elevated purchasing errors on claude.ai",
            "Elevated errors affecting the Microsoft 365 connector",
        ]
    );
    let purchasing = &resolved[0];
    assert_eq!(purchasing.kind, IncidentKind::Incident);
    assert_eq!(purchasing.components, ["claude.ai"]);
    assert_eq!(
        purchasing.started_at,
        Some("2026-11-14T02:54:33.303Z".parse().unwrap())
    );
    assert_eq!(
        purchasing.ended_at,
        Some("2026-11-14T04:23:05.193Z".parse().unwrap())
    );
    assert_eq!(
        purchasing.url.as_deref(),
        Some("https://stspg.io/qqf00d9r7pm4")
    );
    assert_eq!(purchasing.stage.as_deref(), Some("resolved"));
    assert!(purchasing.latest_update.is_some());
}

#[test]
fn resolved_applies_the_component_watch() {
    let watch = watching(&["Claude Cowork"]);
    let resolved = parse_resolved(ANTHROPIC_INCIDENTS, history_fetched_at(), Some(&watch)).unwrap();
    let titles: Vec<&str> = resolved.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Elevated errors affecting the Microsoft 365 connector"]
    );
}

/// Out of order, no shortlinks, a page URL with a trailing slash (like
/// `OpenAI`'s feed), one resolved exactly 24h before the fetch and one just
/// past it.
const UNORDERED_HISTORY: &[u8] = br#"{
    "page": {"url": "https://status.example.com/"},
    "incidents": [
        {"id": "a", "name": "Edge of the window", "status": "resolved", "resolved_at": "2026-11-13T20:00:00Z"},
        {"id": "b", "name": "Newest", "status": "postmortem", "resolved_at": "2026-11-14T19:00:00Z"},
        {"id": "c", "name": "Too old", "status": "resolved", "resolved_at": "2026-11-13T19:59:59Z"},
        {"id": "d", "name": "Middle", "status": "resolved", "resolved_at": "2026-11-14T10:00:00Z", "components": []}
    ]
}"#;

#[test]
fn resolved_sorts_by_end_and_links_to_the_page_without_a_shortlink() {
    let resolved = parse_resolved(UNORDERED_HISTORY, history_fetched_at(), None).unwrap();
    let titles: Vec<&str> = resolved.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Newest", "Middle", "Edge of the window"]);
    assert_eq!(
        resolved[0].url.as_deref(),
        Some("https://status.example.com/incidents/b")
    );
    assert_eq!(resolved[0].stage.as_deref(), Some("postmortem"));
}

#[test]
fn a_bad_incident_list_is_an_error() {
    assert!(parse_resolved(b"<html>502</html>", history_fetched_at(), None).is_err());
}

#[test]
fn a_failed_history_request_leaves_the_summary_status_alone() {
    let fetched_at = history_fetched_at();
    for incidents in [None, Some(&b"<html>502</html>"[..])] {
        for (summary, watch) in [
            (ANTHROPIC, None),
            (CLOUDFLARE, Some(watching(&["Workers"]))),
        ] {
            let watch = watch.as_deref();
            let with = parse_with_resolved("X", summary, incidents, fetched_at, watch).unwrap();
            let without = parse_watching("X", summary, fetched_at, watch).unwrap();
            assert_eq!(with, without);
            assert_eq!(with.resolved, []);
            assert_eq!(with.error, None);
        }
    }
}

#[test]
fn resolved_incidents_never_raise_the_level() {
    let status = parse_with_resolved(
        "Anthropic",
        GITHUB,
        Some(ANTHROPIC_INCIDENTS),
        history_fetched_at(),
        None,
    )
    .unwrap();
    assert_eq!(status.level, Level::Operational);
    assert_eq!(status.description, "All Systems Operational");
    assert_eq!(status.incidents, []);
    assert_eq!(status.resolved.len(), 2);
}

#[test]
fn a_watched_group_covers_resolved_incidents_on_its_components() {
    // incidents.json lists components with a `group_id` but no group rows;
    // the summary knows the group's name.
    let summary = br#"{
        "status": {"indicator": "none", "description": "All Systems Operational"},
        "components": [
            {"id": "g1", "name": "Developer Platform", "status": "operational", "group": true},
            {"id": "w1", "name": "Workers", "status": "operational", "group_id": "g1"},
            {"id": "d1", "name": "DNS", "status": "operational"}
        ]
    }"#;
    let history = br#"{
        "page": {"url": "https://status.example.com"},
        "incidents": [
            {"id": "a", "name": "Workers errors", "resolved_at": "2026-11-14T18:00:00Z",
             "components": [{"id": "w1", "name": "Workers", "group_id": "g1"}]},
            {"id": "b", "name": "DNS errors", "resolved_at": "2026-11-14T17:00:00Z",
             "components": [{"id": "d1", "name": "DNS"}]}
        ]
    }"#;
    let watch = watching(&["developer platform"]);
    let status = parse_with_resolved(
        "Example",
        summary,
        Some(history),
        history_fetched_at(),
        Some(&watch),
    )
    .unwrap();
    let titles: Vec<&str> = status.resolved.iter().map(|i| i.title.as_str()).collect();
    assert_eq!(titles, ["Workers errors"]);
}
