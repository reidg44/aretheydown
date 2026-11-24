//! AWS Health Dashboard public feed (`/public/currentevents`): a JSON array of the
//! events AWS is reporting right now, sent as UTF-16 with a byte-order mark.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

use crate::model::{Incident, IncidentKind, Level, PlatformStatus, Update};

/// The public feed of current AWS events.
pub const CURRENT_EVENTS_URL: &str = "https://health.aws.amazon.com/public/currentevents";

/// Where a person reads the details; the feed has no per-event public link.
pub const DASHBOARD_URL: &str = "https://health.aws.amazon.com/health/status";

#[derive(Deserialize)]
struct RawEvent {
    /// Event status code, sent as a string (`"3"`); numbers are accepted too.
    #[serde(default)]
    status: Value,
    /// Start time in Unix-epoch seconds, sent as a string (`"1772369485"`).
    #[serde(default)]
    date: Value,
    /// `arn:aws:health:<region-code>::event/...`
    #[serde(default)]
    arn: String,
    /// Human region name, e.g. `UAE` or `N. Virginia`.
    #[serde(default)]
    region_name: String,
    /// e.g. `Amazon Elastic Compute Cloud` or `Multiple services`.
    #[serde(default)]
    service_name: String,
    /// Short event title, e.g. `Region Availability`.
    #[serde(default)]
    summary: String,
    /// The event's updates, oldest first.
    #[serde(default)]
    event_log: Vec<RawLogEntry>,
}

#[derive(Deserialize)]
struct RawLogEntry {
    #[serde(default)]
    message: Option<String>,
    /// Unix-epoch seconds, as a string or a number.
    #[serde(default)]
    timestamp: Value,
}

/// An epoch-seconds JSON value (string or number) as a time.
fn epoch_time(value: &Value) -> Option<DateTime<Utc>> {
    as_integer(value).and_then(|seconds| DateTime::from_timestamp(seconds, 0))
}

/// Map an AWS Health Dashboard status code to a [`Level`].
///
/// The codes come from the dashboard's own front end: `0` resolved, `1`
/// informational ("Impacted"), `2` degraded, `3` disrupted. Informational counts as
/// `Degraded` so an active event never reads as all clear; any other code is
/// `Unknown`.
#[must_use]
pub fn level_for_status(code: i64) -> Level {
    match code {
        0 => Level::Operational,
        1 | 2 => Level::Degraded,
        3 => Level::Outage,
        _ => Level::Unknown,
    }
}

/// Decode a response body to text.
///
/// The live feed is UTF-16 with a byte-order mark, which also gives the byte
/// order. A body without a UTF-16 mark is read as UTF-8 (with or without its own
/// mark), in case AWS ever switches.
///
/// # Errors
///
/// Fails when the body is neither valid UTF-16 after a byte-order mark nor UTF-8.
pub fn decode(body: &[u8]) -> Result<String> {
    let from_bytes: fn([u8; 2]) -> u16 = match body {
        [0xFE, 0xFF, ..] => u16::from_be_bytes,
        [0xFF, 0xFE, ..] => u16::from_le_bytes,
        _ => {
            let utf8 = body.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(body);
            return String::from_utf8(utf8.to_vec())
                .context("response is neither UTF-16 with a byte-order mark nor UTF-8");
        }
    };
    let (pairs, leftover) = body[2..].as_chunks::<2>();
    if !leftover.is_empty() {
        bail!("UTF-16 response has an odd number of bytes");
    }
    let units: Vec<u16> = pairs.iter().copied().map(from_bytes).collect();
    String::from_utf16(&units).context("response is not valid UTF-16")
}

/// The AWS regions the TUI's filter picker lists, code and name, in the
/// order shown: the commercial regions (US first), then `GovCloud` and the
/// European Sovereign Cloud. From botocore's `endpoints.json` (2026-11-23),
/// without the China and ISO partitions, which the public Health Dashboard
/// does not cover. A code in the config or the live feed that is not here
/// is listed too, so a new region still shows up.
pub const REGIONS: &[(&str, &str)] = &[
    ("us-east-1", "US East (N. Virginia)"),
    ("us-east-2", "US East (Ohio)"),
    ("us-west-1", "US West (N. California)"),
    ("us-west-2", "US West (Oregon)"),
    ("af-south-1", "Africa (Cape Town)"),
    ("ap-east-1", "Asia Pacific (Hong Kong)"),
    ("ap-east-2", "Asia Pacific (Taipei)"),
    ("ap-northeast-1", "Asia Pacific (Tokyo)"),
    ("ap-northeast-2", "Asia Pacific (Seoul)"),
    ("ap-northeast-3", "Asia Pacific (Osaka)"),
    ("ap-south-1", "Asia Pacific (Mumbai)"),
    ("ap-south-2", "Asia Pacific (Hyderabad)"),
    ("ap-southeast-1", "Asia Pacific (Singapore)"),
    ("ap-southeast-2", "Asia Pacific (Sydney)"),
    ("ap-southeast-3", "Asia Pacific (Jakarta)"),
    ("ap-southeast-4", "Asia Pacific (Melbourne)"),
    ("ap-southeast-5", "Asia Pacific (Malaysia)"),
    ("ap-southeast-6", "Asia Pacific (New Zealand)"),
    ("ap-southeast-7", "Asia Pacific (Thailand)"),
    ("ca-central-1", "Canada (Central)"),
    ("ca-west-1", "Canada West (Calgary)"),
    ("eu-central-1", "Europe (Frankfurt)"),
    ("eu-central-2", "Europe (Zurich)"),
    ("eu-north-1", "Europe (Stockholm)"),
    ("eu-south-1", "Europe (Milan)"),
    ("eu-south-2", "Europe (Spain)"),
    ("eu-west-1", "Europe (Ireland)"),
    ("eu-west-2", "Europe (London)"),
    ("eu-west-3", "Europe (Paris)"),
    ("il-central-1", "Israel (Tel Aviv)"),
    ("me-central-1", "Middle East (UAE)"),
    ("me-south-1", "Middle East (Bahrain)"),
    ("mx-central-1", "Mexico (Central)"),
    ("sa-east-1", "South America (São Paulo)"),
    ("us-gov-east-1", "AWS GovCloud (US-East)"),
    ("us-gov-west-1", "AWS GovCloud (US-West)"),
    ("eusc-de-east-1", "AWS European Sovereign Cloud (Germany)"),
];

/// An AWS region code: a prefix of two or more letters, one or more words, and
/// a number, joined by hyphens (`us-east-1`, `us-gov-west-1`, `eusc-de-east-1`).
#[must_use]
pub fn is_region_code(code: &str) -> bool {
    let parts: Vec<&str> = code.split('-').collect();
    let [prefix, words @ .., number] = parts.as_slice() else {
        return false;
    };
    let letters = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase());
    prefix.len() >= 2
        && letters(prefix)
        && !words.is_empty()
        && words.iter().all(|word| letters(word))
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

/// A JSON value that is an integer or a string holding one.
fn as_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

/// The region code from an event ARN (`arn:aws:health:me-central-1::event/...`).
fn region_code(arn: &str) -> Option<&str> {
    arn.split(':').nth(3).filter(|code| !code.is_empty())
}

impl RawEvent {
    /// Whether the event counts for these region codes: it is in one of them
    /// (ignoring case), or it is global (no region code, or `global`).
    fn counts_for(&self, regions: &[String]) -> bool {
        match region_code(&self.arn) {
            None => true,
            Some(code) if code.eq_ignore_ascii_case("global") => true,
            Some(code) => regions.iter().any(|r| r.eq_ignore_ascii_case(code)),
        }
    }

    fn level(&self) -> Level {
        as_integer(&self.status).map_or(Level::Unknown, level_for_status)
    }

    /// e.g. `UAE (me-central-1): Region Availability`.
    fn title(&self) -> String {
        let region = match region_code(&self.arn) {
            Some(code) if !code.eq_ignore_ascii_case(&self.region_name) => {
                if self.region_name.is_empty() {
                    code.to_owned()
                } else {
                    format!("{} ({code})", self.region_name)
                }
            }
            _ => self.region_name.clone(),
        };
        let summary = if self.summary.is_empty() {
            "Untitled event"
        } else {
            &self.summary
        };
        if region.is_empty() {
            summary.to_owned()
        } else {
            format!("{region}: {summary}")
        }
    }

    fn into_incident(self) -> Incident {
        let title = self.title();
        let started_at = epoch_time(&self.date);
        // The newest entry by timestamp, not by position; the log is oldest
        // first, so reversed it settles ties.
        let latest_update = Update::latest(self.event_log.iter().rev().filter_map(|entry| {
            Update::new(entry.message.as_deref(), epoch_time(&entry.timestamp))
        }));
        let components = if self.service_name.is_empty() {
            Vec::new()
        } else {
            vec![self.service_name]
        };
        Incident {
            kind: IncidentKind::Incident,
            title,
            components,
            started_at,
            ended_at: None,
            url: Some(DASHBOARD_URL.to_owned()),
            stage: None,
            latest_update,
        }
    }
}

/// Parse a `currentevents` body (raw bytes, UTF-16 or UTF-8) into a [`PlatformStatus`],
/// counting every region.
///
/// An empty list is `Operational`. Each event that is not resolved becomes an
/// incident, and the platform takes the worst event's level.
///
/// # Errors
///
/// Fails when the body cannot be decoded or is not a JSON array of events.
pub fn parse(name: &str, body: &[u8], fetched_at: DateTime<Utc>) -> Result<PlatformStatus> {
    parse_in_regions(name, body, fetched_at, None)
}

/// [`parse`], keeping only events in `regions` (region codes such as
/// `us-east-1`, matched ignoring case) when given. Global events, whose ARN has no region code or the
/// code `global`, always count. The description says how many active events in
/// other regions were left out.
///
/// # Errors
///
/// Fails when the body cannot be decoded or is not a JSON array of events.
pub fn parse_in_regions(
    name: &str,
    body: &[u8],
    fetched_at: DateTime<Utc>,
    regions: Option<&[String]>,
) -> Result<PlatformStatus> {
    let text = decode(body)?;
    let events: Vec<RawEvent> =
        serde_json::from_str(&text).context("response is not an AWS current-events list")?;
    let (shown, elsewhere): (Vec<RawEvent>, Vec<RawEvent>) = events
        .into_iter()
        .filter(|event| event.level() != Level::Operational)
        .partition(|event| regions.is_none_or(|regions| event.counts_for(regions)));
    let level = Level::worst(shown.iter().map(RawEvent::level));
    let mut feed_regions: Vec<String> = Vec::new();
    for event in shown.iter().chain(&elsewhere) {
        if let Some(code) = region_code(&event.arn).map(str::to_ascii_lowercase)
            && is_region_code(&code)
            && !feed_regions.contains(&code)
        {
            feed_regions.push(code);
        }
    }
    let shown_text = match shown.len() {
        0 => "No current events".to_owned(),
        1 => "1 current event".to_owned(),
        n => format!("{n} current events"),
    };
    let description = match elsewhere.len() {
        0 => shown_text,
        n => format!("{shown_text} ({n} in other regions not shown)"),
    };
    Ok(PlatformStatus {
        name: name.to_owned(),
        level,
        description,
        incidents: shown.into_iter().map(RawEvent::into_incident).collect(),
        resolved: Vec::new(),
        fetched_at,
        error: None,
        probe: None,
        regions: feed_regions,
    })
}
