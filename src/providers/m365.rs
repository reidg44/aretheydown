//! Microsoft 365 consumer services (`https://status.cloud.microsoft/api/posts/m365Consumer`):
//! a JSON array with one entry per service (`Outlook.com`, `OneDrive`, `Teams Free`, …)
//! and its current `Status`. Business tenant health needs a sign-in and is not read.

use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Utc};
use serde::Deserialize;

use crate::model::{Incident, IncidentKind, Level, PlatformStatus, Update, non_blank};

/// The feed of consumer services.
pub const CONSUMER_FEED_URL: &str = "https://status.cloud.microsoft/api/posts/m365Consumer";

/// The status page a person reads.
pub const STATUS_PAGE_URL: &str = "https://status.cloud.microsoft/";

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawService {
    service_display_name: String,
    #[serde(default)]
    status: String,
    title: Option<String>,
    message: Option<String>,
    /// `0001-01-01T00:00:00+00:00` when Microsoft gives no time.
    authored_date_time: Option<DateTime<Utc>>,
    last_updated_time: Option<DateTime<Utc>>,
}

/// What a service's `Status` means, read once per service.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Health {
    /// `Operational` or `Available`: not listed.
    Fine,
    /// `Service restored`: Microsoft leaves it up after a fix; listed, not a problem.
    Restored,
    /// `Service degradation`, or anything unknown: listed, and raises the level.
    Trouble(Level),
}

impl Health {
    fn of(status: &str) -> Self {
        match status.trim().to_lowercase().as_str() {
            "operational" | "available" => Self::Fine,
            "service restored" => Self::Restored,
            "service degradation" => Self::Trouble(Level::Degraded),
            _ => Self::Trouble(Level::Unknown),
        }
    }

    fn level(self) -> Level {
        match self {
            Self::Fine | Self::Restored => Level::Operational,
            Self::Trouble(level) => level,
        }
    }
}

/// Map a service `Status` (trimmed, any case) to a [`Level`].
///
/// `Operational`, `Available`, and `Service restored` are fine; `Service
/// degradation` is `Degraded`; anything else, `Unknown` and a blank status
/// included, is `Unknown`. The feed has no outage value.
#[must_use]
pub fn level_for_status(status: &str) -> Level {
    Health::of(status).level()
}

/// One feed entry with its status read.
struct Service {
    raw: RawService,
    health: Health,
}

impl Service {
    fn name(&self) -> &str {
        self.raw.service_display_name.trim()
    }
}

/// A real time, or `None` for Microsoft's year-1 placeholder.
fn real_time(time: Option<DateTime<Utc>>) -> Option<DateTime<Utc>> {
    time.filter(|t| t.year() > 1)
}

fn restored_note(restored: usize) -> String {
    match restored {
        0 => String::new(),
        1 => " (1 restored service still in feed)".to_owned(),
        n => format!(" ({n} restored services still in feed)"),
    }
}

fn summarize(services: &[Service]) -> String {
    let restored = services
        .iter()
        .filter(|s| s.health == Health::Restored)
        .count();
    let troubled: Vec<&str> = services
        .iter()
        .filter(|s| matches!(s.health, Health::Trouble(_)))
        .map(Service::name)
        .collect();
    match (services.len(), troubled.len(), restored) {
        (0, _, _) => "No services in the feed".to_owned(),
        (n, 0, 0) => format!("All {n} services operational"),
        (_, 0, r) => format!("Operational{}", restored_note(r)),
        (n, k, r) => format!(
            "{k} of {n} services not operational: {}{}",
            troubled.join(", "),
            restored_note(r)
        ),
    }
}

fn incident(service: &Service) -> Incident {
    let name = service.name().to_owned();
    let raw = &service.raw;
    let title = match non_blank(raw.title.as_deref()) {
        Some(title) => format!("{name}: {title}"),
        None => name.clone(),
    };
    Incident {
        kind: IncidentKind::Incident,
        title,
        components: vec![name],
        started_at: real_time(raw.authored_date_time),
        ended_at: None,
        url: None,
        stage: non_blank(Some(&raw.status)),
        latest_update: Update::new(raw.message.as_deref(), real_time(raw.last_updated_time)),
    }
}

/// Parse the consumer services feed into a [`PlatformStatus`].
///
/// The platform takes the worst service level; restored services are listed
/// but count as operational. An empty feed is `Operational`.
///
/// # Errors
///
/// Fails when the body is not a JSON array of Microsoft 365 services.
pub fn parse(name: &str, body: &[u8], fetched_at: DateTime<Utc>) -> Result<PlatformStatus> {
    let raw: Vec<RawService> =
        serde_json::from_slice(body).context("response is not a Microsoft 365 service list")?;
    let services: Vec<Service> = raw
        .into_iter()
        .map(|raw| Service {
            health: Health::of(&raw.status),
            raw,
        })
        .collect();
    let level = Level::worst(services.iter().map(|s| s.health.level()));
    let description = summarize(&services);
    let incidents = services
        .iter()
        .filter(|s| s.health != Health::Fine)
        .map(incident)
        .collect();
    Ok(PlatformStatus {
        name: name.to_owned(),
        level,
        description,
        incidents,
        resolved: Vec::new(),
        fetched_at,
        error: None,
        probe: None,
        regions: Vec::new(),
    })
}
