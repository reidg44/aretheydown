//! The shared status model every provider produces and every UI consumes.

use std::fmt;

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

/// How far back resolved incidents are kept: an incident whose end time is
/// within this long before the fetch is listed in [`PlatformStatus::resolved`].
pub const RESOLVED_WINDOW: TimeDelta = TimeDelta::hours(24);
/// [`RESOLVED_WINDOW`] in whole hours, for labels such as `in the last 24h`.
pub const RESOLVED_HOURS: i64 = RESOLVED_WINDOW.num_hours();

/// Whether an incident that ended at `ended_at` belongs in the resolved list
/// of a status fetched at `fetched_at`: within [`RESOLVED_WINDOW`] before it.
/// An end after the fetch (clock skew) counts.
#[must_use]
pub fn resolved_recently(ended_at: DateTime<Utc>, fetched_at: DateTime<Utc>) -> bool {
    ended_at >= fetched_at - RESOLVED_WINDOW
}

/// Sort resolved incidents newest `ended_at` first, the order of
/// [`PlatformStatus::resolved`]; ties keep their order.
pub fn sort_newest_end_first(incidents: &mut [Incident]) {
    incidents.sort_by_key(|incident| std::cmp::Reverse(incident.ended_at));
}

/// How healthy a platform is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Operational,
    Degraded,
    Outage,
    /// The status could not be fetched or understood.
    Unknown,
}

impl Level {
    /// Human-readable name, as shown in the text output.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Level::Operational => "Operational",
            Level::Degraded => "Degraded",
            Level::Outage => "Outage",
            Level::Unknown => "Unknown",
        }
    }

    /// How bad this level is, for picking the worst of several: Operational <
    /// Degraded < Unknown < Outage. Unknown outranks Degraded so an
    /// unrecognised status is never hidden behind a known minor one.
    #[must_use]
    pub fn severity(self) -> u8 {
        match self {
            Level::Operational => 0,
            Level::Degraded => 1,
            Level::Unknown => 2,
            Level::Outage => 3,
        }
    }

    /// The worst of `levels` by [`Level::severity`]; `Operational` when empty.
    #[must_use]
    pub fn worst(levels: impl IntoIterator<Item = Level>) -> Level {
        levels
            .into_iter()
            .max_by_key(|level| level.severity())
            .unwrap_or(Level::Operational)
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.label())
    }
}

/// Whether an active event is an unplanned incident or planned maintenance.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IncidentKind {
    #[default]
    Incident,
    /// Scheduled work that is in progress now (upcoming work is not listed).
    Maintenance,
}

/// An event the platform reports: an unresolved incident or maintenance in
/// progress, or (in [`PlatformStatus::resolved`]) one that has ended.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incident {
    #[serde(default)]
    pub kind: IncidentKind,
    pub title: String,
    /// Names of the affected components, possibly empty.
    #[serde(default)]
    pub components: Vec<String>,
    pub started_at: Option<DateTime<Utc>>,
    /// When the incident was resolved; `None` while it is active.
    pub ended_at: Option<DateTime<Utc>>,
    pub url: Option<String>,
    /// The vendor's own word for where the incident stands (`investigating`,
    /// `identified`, `monitoring`, `in_progress`, …), when the feed has one.
    pub stage: Option<String>,
    /// The vendor's newest update; the link has the full history.
    pub latest_update: Option<Update>,
}

/// An incident from a vendor's incident history (Statuspage or Google Cloud
/// `incidents.json`), resolved or not, with the level it gave the platform
/// while it was active (its `impact` or `severity`, mapped as for active
/// incidents). [`crate::history::backfill`] reads `title`, `components`, `url`,
/// `started_at` (incidents without one are skipped), and `ended_at` (`None`
/// while still active).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PastIncident {
    pub level: Level,
    pub incident: Incident,
}

/// One update the vendor posted on an incident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub text: String,
    pub posted_at: Option<DateTime<Utc>>,
}

impl Update {
    /// An update with `text` trimmed, or `None` when there is no text.
    /// Control characters are dropped (tabs become spaces, newlines stay), so
    /// feed text cannot send escape sequences to the terminal.
    #[must_use]
    pub fn new(text: Option<&str>, posted_at: Option<DateTime<Utc>>) -> Option<Self> {
        let text: String = text?
            .chars()
            .filter_map(|c| match c {
                '\t' => Some(' '),
                '\n' => Some('\n'),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect();
        let text = non_blank(Some(&text))?;
        Some(Self { text, posted_at })
    }

    /// The update with the newest `posted_at`; on a tie (or when none has a
    /// time) the first one wins, so pass the updates newest first when the
    /// feed has an order.
    #[must_use]
    pub fn latest(updates: impl IntoIterator<Item = Update>) -> Option<Self> {
        updates.into_iter().reduce(|best, next| {
            if next.posted_at > best.posted_at {
                next
            } else {
                best
            }
        })
    }
}

/// `value` trimmed and owned, or `None` when blank or missing.
#[must_use]
pub fn non_blank(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// One platform's status at the moment it was fetched.
///
/// It deserializes from the JSON it serializes to, so `--serve` history
/// (`crate::history`) can store it as is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformStatus {
    pub name: String,
    pub level: Level,
    /// The platform's own one-line summary, e.g. "All Systems Operational".
    pub description: String,
    #[serde(default)]
    pub incidents: Vec<Incident>,
    /// Incidents resolved within [`RESOLVED_WINDOW`] of `fetched_at`, newest
    /// end first. They never affect `level`.
    #[serde(default)]
    pub resolved: Vec<Incident>,
    pub fetched_at: DateTime<Utc>,
    /// Why the status is `Unknown`, when fetching or parsing failed.
    pub error: Option<String>,
    /// What one direct GET of the vendor's own service saw this fetch
    /// (`crate::providers::probe`). Shown beside the level, never part of
    /// it. Left out of the stored JSON when there is none, so history
    /// lines stay as they were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Probe>,
}

/// The result of a platform's probe: a GET of the vendor's own front door
/// (not its status feed), judged against the status code expected there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Probe {
    /// The URL that was probed.
    pub url: String,
    /// Whether it answered with the expected status.
    pub ok: bool,
    /// The status code it answered with; `None` without an answer.
    pub code: Option<u16>,
    /// How long the answer took; `None` without an answer.
    pub elapsed_ms: Option<u64>,
    /// `responding (HTTP 401 in 220 ms)`, `unexpected HTTP 503 (expected
    /// 4xx)`, or `no answer (could not connect)`.
    pub description: String,
}

impl PlatformStatus {
    /// A status for a platform that could not be fetched or parsed.
    #[must_use]
    pub fn unknown(
        name: impl Into<String>,
        error: impl Into<String>,
        fetched_at: DateTime<Utc>,
    ) -> Self {
        Self {
            name: name.into(),
            level: Level::Unknown,
            description: "Status unavailable".to_owned(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at,
            error: Some(error.into()),
            probe: None,
        }
    }
}

/// Sort `items` worst first by the [`Level`] that `level` gives each one
/// (Outage, Unknown, Degraded, Operational); equal levels keep their order.
pub fn sort_worst_first<T>(items: &mut [T], level: impl Fn(&T) -> Level) {
    items.sort_by_key(|item| std::cmp::Reverse(level(item).severity()));
}

/// `statuses` worst first, as [`sort_worst_first`] orders them.
#[must_use]
pub fn worst_first<'a>(
    statuses: impl IntoIterator<Item = &'a PlatformStatus>,
) -> Vec<&'a PlatformStatus> {
    let mut sorted: Vec<_> = statuses.into_iter().collect();
    sort_worst_first(&mut sorted, |status| status.level);
    sorted
}

/// The one-line answer to "is it them?": `All 7 operational`, or
/// `2 of 7 need attention: Cloudflare, AWS` with the names worst first.
/// Anything but Operational needs attention, Unknown included (a failed fetch
/// cannot rule a platform out). `None` when there are no statuses.
#[must_use]
pub fn verdict<'a>(statuses: impl IntoIterator<Item = &'a PlatformStatus>) -> Option<String> {
    let sorted = worst_first(statuses);
    let total = sorted.len();
    if total == 0 {
        return None;
    }
    let trouble: Vec<&str> = sorted
        .iter()
        .filter(|status| status.level != Level::Operational)
        .map(|status| status.name.as_str())
        .collect();
    Some(match trouble.len() {
        0 => format!("All {total} operational"),
        1 => format!("1 of {total} needs attention: {}", trouble[0]),
        n => format!("{n} of {total} need attention: {}", trouble.join(", ")),
    })
}

/// Whole seconds from `started_at` to `now`, never negative (a start in the
/// future, from clock skew, counts as zero).
#[must_use]
pub fn age_seconds(started_at: DateTime<Utc>, now: DateTime<Utc>) -> i64 {
    (now - started_at).num_seconds().max(0)
}

/// How long ago `started_at` was, as of `now`, in its two largest units with
/// a zero second unit dropped: `<1m ago`, `45m ago`, `2h 14m ago`, `2h ago`,
/// `3d 4h ago`. A start later than `now` (clock skew) is `just now`.
#[must_use]
pub fn age(started_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    if started_at > now {
        return "just now".to_owned();
    }
    format!("{} ago", elapsed(started_at, now))
}

/// [`age`] without the `ago`: `<1m`, `45m`, `2h 14m`, `3d 4h`. A start later
/// than `now` counts as `<1m`.
#[must_use]
pub fn elapsed(started_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let minutes = age_seconds(started_at, now) / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let (days, hours) = (hours / 24, hours % 24);
    match (days, hours, minutes) {
        (0, 0, 0) => "<1m".to_owned(),
        (0, 0, m) => format!("{m}m"),
        (0, h, 0) => format!("{h}h"),
        (0, h, m) => format!("{h}h {m}m"),
        (d, 0, _) => format!("{d}d"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone, Utc};

    use super::{
        Incident, IncidentKind, Level, PlatformStatus, Update, age, age_seconds, verdict,
        worst_first,
    };

    /// The age of a start `seconds` before a fixed reference time.
    fn age_of(seconds: i64) -> String {
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 19, 10, 0).unwrap();
        age(now - Duration::seconds(seconds), now)
    }

    #[test]
    fn status_round_trips_through_its_own_json() {
        let at = Utc.with_ymd_and_hms(2026, 11, 14, 2, 14, 0).unwrap();
        let status = PlatformStatus {
            name: "Cloudflare".to_owned(),
            level: Level::Degraded,
            description: "Partial outage".to_owned(),
            incidents: vec![
                Incident {
                    title: "Workers errors".to_owned(),
                    components: vec!["Workers".to_owned()],
                    started_at: Some(at - Duration::minutes(30)),
                    url: Some("https://example.com/i/1".to_owned()),
                    stage: Some("identified".to_owned()),
                    latest_update: Update::new(Some("We found it.\nFixing."), Some(at)),
                    ..Default::default()
                },
                Incident {
                    kind: IncidentKind::Maintenance,
                    title: "DB upgrade".to_owned(),
                    ..Default::default()
                },
            ],
            resolved: vec![Incident {
                title: "DNS blip".to_owned(),
                started_at: Some(at - Duration::hours(3)),
                ended_at: Some(at - Duration::hours(2)),
                stage: Some("resolved".to_owned()),
                ..Default::default()
            }],
            fetched_at: at,
            error: Some("partial".to_owned()),
            probe: None,
        };
        let json = serde_json::to_string(&status).unwrap();
        let back: PlatformStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(back, status);
    }

    #[test]
    fn age_under_a_minute() {
        assert_eq!(age_of(0), "<1m ago");
        assert_eq!(age_of(59), "<1m ago");
    }

    #[test]
    fn age_in_minutes_only() {
        assert_eq!(age_of(60), "1m ago");
        assert_eq!(age_of(45 * 60 + 30), "45m ago");
        assert_eq!(age_of(59 * 60 + 59), "59m ago");
    }

    #[test]
    fn age_in_hours_with_and_without_minutes() {
        assert_eq!(age_of(2 * 3600 + 14 * 60 + 5), "2h 14m ago");
        assert_eq!(age_of(2 * 3600 + 59), "2h ago");
        assert_eq!(age_of(23 * 3600 + 59 * 60), "23h 59m ago");
    }

    #[test]
    fn age_in_days_with_and_without_hours() {
        assert_eq!(age_of(3 * 86_400 + 4 * 3600 + 50 * 60), "3d 4h ago");
        assert_eq!(age_of(86_400 + 59 * 60), "1d ago");
        assert_eq!(age_of(400 * 86_400), "400d ago");
    }

    #[test]
    fn a_future_start_is_just_now() {
        assert_eq!(age_of(-1), "just now");
        assert_eq!(age_of(-3600), "just now");
    }

    #[test]
    fn age_seconds_is_whole_and_never_negative() {
        let now = Utc.with_ymd_and_hms(2026, 11, 13, 19, 10, 0).unwrap();
        assert_eq!(age_seconds(now - Duration::milliseconds(90_900), now), 90);
        assert_eq!(age_seconds(now + Duration::seconds(5), now), 0);
    }

    #[test]
    fn update_text_drops_control_characters_and_blank_text() {
        let update = Update::new(Some(" a\x1b]8;;x\x07b\tc\nd\r "), None).unwrap();
        assert_eq!(update.text, "a]8;;xb c\nd");
        assert_eq!(Update::new(Some(" \x1b "), None), None);
        assert_eq!(Update::new(None, None), None);
    }

    #[test]
    fn latest_update_is_the_newest_and_the_first_on_a_tie() {
        let at = |h| Some(Utc.with_ymd_and_hms(2026, 11, 13, h, 0, 0).unwrap());
        let update = |text: &str, posted_at| Update::new(Some(text), posted_at).unwrap();
        let newest = Update::latest([update("a", at(1)), update("b", at(3)), update("c", at(2))]);
        assert_eq!(newest.unwrap().text, "b");
        let tie = Update::latest([update("first", at(3)), update("second", at(3))]);
        assert_eq!(tie.unwrap().text, "first");
        let untimed = Update::latest([update("first", None), update("second", None)]);
        assert_eq!(untimed.unwrap().text, "first");
        assert_eq!(Update::latest([]), None);
    }

    fn status(name: &str, level: Level) -> PlatformStatus {
        let mut status = PlatformStatus::unknown(name, "", Utc::now());
        status.level = level;
        status.error = None;
        status
    }

    fn names(statuses: &[&PlatformStatus]) -> Vec<String> {
        statuses.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn worst_first_sorts_by_severity_and_keeps_ties_in_order() {
        use Level::{Degraded, Operational, Outage, Unknown};
        let statuses = [
            status("A", Operational),
            status("B", Degraded),
            status("C", Unknown),
            status("D", Operational),
            status("E", Outage),
            status("F", Degraded),
        ];
        assert_eq!(
            names(&worst_first(&statuses)),
            ["E", "C", "B", "F", "A", "D"]
        );
    }

    #[test]
    fn verdict_when_everything_is_operational() {
        let statuses = [
            status("GitHub", Level::Operational),
            status("AWS", Level::Operational),
        ];
        assert_eq!(verdict(&statuses).as_deref(), Some("All 2 operational"));
    }

    #[test]
    fn verdict_names_trouble_worst_first_and_counts_unknown() {
        let statuses = [
            status("GitHub", Level::Operational),
            status("Cloudflare", Level::Degraded),
            status("Slow", Level::Unknown),
            status("AWS", Level::Outage),
        ];
        assert_eq!(
            verdict(&statuses).as_deref(),
            Some("3 of 4 need attention: AWS, Slow, Cloudflare")
        );
    }

    #[test]
    fn verdict_for_one_platform_in_trouble_is_singular() {
        let statuses = [
            status("GitHub", Level::Operational),
            status("Slow", Level::Unknown),
        ];
        assert_eq!(
            verdict(&statuses).as_deref(),
            Some("1 of 2 needs attention: Slow")
        );
    }

    #[test]
    fn no_statuses_means_no_verdict() {
        assert_eq!(verdict(&[]), None);
    }

    #[test]
    fn worst_ranks_outage_then_unknown_then_degraded() {
        use Level::{Degraded, Operational, Outage, Unknown};
        assert_eq!(Level::worst([]), Operational);
        assert_eq!(Level::worst([Operational, Degraded]), Degraded);
        assert_eq!(Level::worst([Degraded, Unknown, Operational]), Unknown);
        assert_eq!(Level::worst([Unknown, Outage, Degraded]), Outage);
    }
}
