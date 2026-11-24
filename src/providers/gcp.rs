//! Google Cloud (`https://status.cloud.google.com/incidents.json`): a JSON array of
//! recent incidents, newest first. Closed incidents carry an `end` time; active ones
//! have none.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::model::{
    Incident, IncidentKind, Level, PastIncident, PlatformStatus, Update, resolved_recently,
    sort_newest_end_first,
};

/// The status page; incident `uri`s (e.g. `incidents/<id>`) are relative to it.
pub const STATUS_ROOT: &str = "https://status.cloud.google.com/";

#[derive(Deserialize)]
struct RawIncident {
    begin: Option<DateTime<Utc>>,
    /// Absent (or null) while the incident is still active.
    end: Option<DateTime<Utc>>,
    #[serde(default)]
    severity: String,
    #[serde(default)]
    external_desc: String,
    uri: Option<String>,
    #[serde(default)]
    affected_products: Vec<RawProduct>,
    most_recent_update: Option<RawUpdate>,
}

#[derive(Deserialize)]
struct RawUpdate {
    /// Often a long markdown report; shown as-is.
    text: Option<String>,
    when: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
struct RawProduct {
    title: String,
}

/// Map a Google Cloud incident `severity` to a [`Level`].
///
/// `high` is an outage; `medium`, `low`, and any value Google adds later count as
/// `Degraded`, since an active incident always means something is wrong.
#[must_use]
pub fn level_for_severity(severity: &str) -> Level {
    match severity {
        "high" => Level::Outage,
        _ => Level::Degraded,
    }
}

fn description(active: usize) -> String {
    match active {
        0 => "No active incidents".to_owned(),
        1 => "1 active incident".to_owned(),
        n => format!("{n} active incidents"),
    }
}

/// Parse an `incidents.json` body into a [`PlatformStatus`]. Incidents
/// without an `end` are active and set the level; those whose `end` is within
/// [`RESOLVED_WINDOW`](crate::model::RESOLVED_WINDOW) of `fetched_at` go in
/// `resolved`, newest end first.
///
/// # Errors
///
/// Fails when the body is not a JSON array of Google Cloud incidents.
pub fn parse(name: &str, body: &[u8], fetched_at: DateTime<Utc>) -> Result<PlatformStatus> {
    let raw: Vec<RawIncident> =
        serde_json::from_slice(body).context("response is not a Google Cloud incident list")?;
    let (active, closed): (Vec<RawIncident>, Vec<RawIncident>) =
        raw.into_iter().partition(|i| i.end.is_none());
    // The worst active incident decides; no active incidents is Operational.
    let level = Level::worst(active.iter().map(|i| level_for_severity(&i.severity)));
    let description = description(active.len());
    let mut resolved: Vec<Incident> = closed
        .into_iter()
        .filter(|i| i.end.is_some_and(|end| resolved_recently(end, fetched_at)))
        .map(incident)
        .collect();
    sort_newest_end_first(&mut resolved);
    Ok(PlatformStatus {
        name: name.to_owned(),
        level,
        description,
        incidents: active.into_iter().map(incident).collect(),
        resolved,
        fetched_at,
        error: None,
        probe: None,
        regions: Vec::new(),
        component_tree: Vec::new(),
    })
}

/// Every incident in an `incidents.json` body, closed or active, for history
/// backfill ([`crate::history::backfill`]), each with its `severity` as a
/// level ([`level_for_severity`]).
///
/// # Errors
///
/// Fails when the body is not a JSON array of Google Cloud incidents.
pub fn parse_past(body: &[u8]) -> Result<Vec<PastIncident>> {
    let raw: Vec<RawIncident> =
        serde_json::from_slice(body).context("response is not a Google Cloud incident list")?;
    Ok(raw
        .into_iter()
        .map(|raw| PastIncident {
            level: level_for_severity(&raw.severity),
            incident: incident(raw),
        })
        .collect())
}

/// One feed incident in the model, active or closed.
fn incident(raw: RawIncident) -> Incident {
    Incident {
        kind: IncidentKind::Incident,
        title: if raw.external_desc.trim().is_empty() {
            "Untitled incident".to_owned()
        } else {
            raw.external_desc
        },
        components: raw.affected_products.into_iter().map(|p| p.title).collect(),
        started_at: raw.begin,
        ended_at: raw.end,
        url: raw
            .uri
            .map(|uri| format!("{STATUS_ROOT}{}", uri.trim_start_matches('/'))),
        // Google gives no stage word; `most_recent_update.status` is the
        // product's availability (`AVAILABLE`), not the incident's stage.
        stage: None,
        latest_update: raw
            .most_recent_update
            .and_then(|u| Update::new(u.text.as_deref(), u.when)),
    }
}
