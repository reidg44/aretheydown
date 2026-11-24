//! Azure status RSS feed (`https://azure.status.microsoft/en-us/status/feed/`).
//!
//! Azure publishes no JSON API: the feed lists only active incidents, so an
//! empty feed means everything is operational.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use rss::{Channel, Item};

use crate::model::{Incident, IncidentKind, Level, PlatformStatus};

/// The status page a person reads; the feed is published alongside it.
pub const STATUS_PAGE_URL: &str = "https://azure.status.microsoft/en-us/status";

/// Parse the Azure status RSS body into a [`PlatformStatus`].
///
/// No items means `Operational`; each item becomes an incident and makes the
/// platform `Degraded` (the feed carries no severity to tell an outage apart).
///
/// # Errors
///
/// Fails when the body is not an RSS feed.
pub fn parse(name: &str, body: &[u8], fetched_at: DateTime<Utc>) -> Result<PlatformStatus> {
    let channel = Channel::read_from(body).context("response is not an RSS feed")?;
    let incidents: Vec<Incident> = channel.items().iter().map(incident).collect();
    let (level, description) = if incidents.is_empty() {
        (Level::Operational, "All Systems Operational")
    } else {
        (Level::Degraded, "Active incidents reported")
    };
    Ok(PlatformStatus {
        name: name.to_owned(),
        level,
        description: description.to_owned(),
        incidents,
        resolved: Vec::new(),
        fetched_at,
        error: None,
        probe: None,
        regions: Vec::new(),
        component_tree: Vec::new(),
    })
}

fn incident(item: &Item) -> Incident {
    Incident {
        kind: IncidentKind::Incident,
        title: non_blank(item.title())
            .unwrap_or("Untitled incident")
            .to_owned(),
        components: Vec::new(),
        // RFC 2822; chrono reads Azure's bare `Z` zone (`... 20:00:00 Z`) as UTC.
        started_at: item
            .pub_date()
            .and_then(|date| DateTime::parse_from_rfc2822(date.trim()).ok())
            .map(|date| date.with_timezone(&Utc)),
        ended_at: None,
        url: non_blank(item.link()).map(str::to_owned),
        // No stage in the feed; item text has no sample yet.
        stage: None,
        latest_update: None,
    }
}

fn non_blank(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}
