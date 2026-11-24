//! Turn statuses into terminal text or JSON. The web page is `crate::html`.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::history::Record;
use crate::model::{
    Incident, IncidentKind, PlatformStatus, RESOLVED_HOURS, Update, age, age_seconds, verdict,
    worst_first,
};

/// `--once` text cuts an update to about this many characters.
pub const UPDATE_TEXT_LIMIT: usize = 200;

/// The verdict on the first line, then one line per platform, worst first,
/// with its probe result, then its active incidents or error, indented
/// beneath it, then one line on
/// what it resolved in the last 24h, if anything. Incident ages are
/// measured against each platform's `fetched_at`, so the output is pure.
#[must_use]
pub fn text(statuses: &[PlatformStatus]) -> String {
    let name_width = column_width(statuses.iter().map(|s| s.name.as_str()));
    let level_width = column_width(statuses.iter().map(|s| s.level.label()));
    let mut out = String::new();
    if let Some(verdict) = verdict(statuses) {
        let _ = writeln!(out, "{verdict}");
    }
    for status in worst_first(statuses) {
        // Writing to a String cannot fail.
        let _ = writeln!(
            out,
            "{:name_width$}  {:level_width$}  {}",
            status.name, status.level, status.description
        );
        if let Some(line) = probe_line(status) {
            let _ = writeln!(out, "  probe: {line}");
        }
        if let Some(error) = &status.error {
            let _ = writeln!(out, "  error: {error}");
        }
        for incident in &status.incidents {
            write_incident(&mut out, incident, status.fetched_at);
        }
        if let Some(line) = resolved_line(status) {
            let _ = writeln!(out, "  {line}");
        }
    }
    out
}

/// What the platform's probe saw and where: `responding (HTTP 401 in 220
/// ms), GET https://api.openai.com/v1/models`; `None` without a probe. The
/// TUI's detail pane shows the same line.
#[must_use]
pub fn probe_line(status: &PlatformStatus) -> Option<String> {
    let probe = status.probe.as_ref()?;
    Some(format!("{}, GET {}", probe.description, probe.url))
}

/// `2 resolved in the last 24h, latest: <title> (1h 5m ago)`, or `1
/// resolved in the last 24h: <title> (…)`; `None` when nothing resolved.
fn resolved_line(status: &PlatformStatus) -> Option<String> {
    let latest = status.resolved.first()?;
    let ago = latest
        .ended_at
        .map(|ended_at| format!(" ({})", age(ended_at, status.fetched_at)))
        .unwrap_or_default();
    let head = match status.resolved.len() {
        1 => format!("1 resolved in the last {RESOLVED_HOURS}h:"),
        n => format!("{n} resolved in the last {RESOLVED_HOURS}h, latest:"),
    };
    Some(format!(
        "{head} {}{ago}",
        shorten(&latest.title, UPDATE_TEXT_LIMIT)
    ))
}

fn column_width<'a>(cells: impl Iterator<Item = &'a str>) -> usize {
    cells.map(|cell| cell.chars().count()).max().unwrap_or(0)
}

fn write_incident(out: &mut String, incident: &Incident, fetched_at: DateTime<Utc>) {
    let marker = match incident.kind {
        IncidentKind::Incident => "",
        IncidentKind::Maintenance => "[maintenance] ",
    };
    let _ = writeln!(out, "  - {marker}{}", incident.title);
    let details = incident_details(incident, fetched_at);
    if !details.is_empty() {
        let _ = writeln!(out, "    {}", details.join("; "));
    }
    if let Some(line) = update_line(incident, fetched_at) {
        let _ = writeln!(out, "    {line}");
    }
    if let Some(url) = &incident.url {
        let _ = writeln!(out, "    {url}");
    }
}

/// What the incident affects and when it started, as of `fetched_at`:
/// `affects Actions, Pages` and `since 2026-11-15 09:46 UTC (2h 14m ago)`,
/// each only when the feed has it. The text and the web page both use these.
#[must_use]
pub fn incident_details(incident: &Incident, fetched_at: DateTime<Utc>) -> Vec<String> {
    let mut details = Vec::new();
    if !incident.components.is_empty() {
        details.push(format!("affects {}", incident.components.join(", ")));
    }
    if let Some(started_at) = incident.started_at {
        details.push(format!(
            "since {} ({})",
            started_at.format("%Y-%m-%d %H:%M UTC"),
            age(started_at, fetched_at)
        ));
    }
    details
}

/// The stage and latest update on one line: `monitoring, updated 3h ago: A
/// fix has been…`, the text on one line and cut to [`UPDATE_TEXT_LIMIT`].
/// Just the stage without an update; `None` with neither.
fn update_line(incident: &Incident, fetched_at: DateTime<Utc>) -> Option<String> {
    let mut head: Vec<String> = incident.stage.iter().cloned().collect();
    let Some(Update { text, posted_at }) = &incident.latest_update else {
        return incident.stage.clone();
    };
    if let Some(posted_at) = posted_at {
        head.push(format!("updated {}", age(*posted_at, fetched_at)));
    }
    let head = if head.is_empty() {
        "update".to_owned()
    } else {
        head.join(", ")
    };
    Some(format!("{head}: {}", shorten(text, UPDATE_TEXT_LIMIT)))
}

/// `text` on one line (runs of whitespace, newlines included, become one
/// space), cut to at most `limit` characters plus `…`. The cut goes back to a
/// word break when one is near.
#[must_use]
pub fn shorten(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let cut: String = flat.chars().take(limit).collect();
    let at_word_break = flat.chars().nth(limit) == Some(' ');
    // Otherwise back up to the last space if it is within the last fifth.
    let cut = match cut.rfind(' ') {
        Some(space) if !at_word_break && cut[space..].chars().count() <= limit / 5 => &cut[..space],
        _ => cut.as_str(),
    };
    format!("{}…", cut.trim_end())
}

/// The statuses as a pretty-printed JSON array, in config order. Each incident
/// also carries `age_seconds`: whole seconds from `started_at` to the
/// platform's `fetched_at`, never negative, `null` without a start time. Its
/// `latest_update` (when there is one) gets an `age_seconds` the same way,
/// from `posted_at`. Incidents in `resolved` measure `age_seconds` from
/// `ended_at` instead. Each platform has a `probe`: the model's
/// [`crate::model::Probe`], or `null` without one.
///
/// # Panics
///
/// Never: every field of the model serializes cleanly.
#[must_use]
pub fn json(statuses: &[PlatformStatus]) -> String {
    let platforms = Value::Array(statuses.iter().map(platform_json).collect());
    // `{:#}` is serde_json's pretty printing.
    format!("{platforms:#}")
}

/// How `--at` text marks a platform taken from a backfilled record.
pub const BACKFILLED_MARK: &str = "(from vendor history)";

/// `--at` text: [`text`] of the recorded statuses, with
/// [`BACKFILLED_MARK`] after the description of each platform whose record
/// was rebuilt from the vendor's incident history.
#[must_use]
pub fn history_text(records: &[Record]) -> String {
    let statuses: Vec<PlatformStatus> = records
        .iter()
        .map(|record| {
            let mut status = record.status.clone();
            if record.backfilled {
                status.description = format!("{} {BACKFILLED_MARK}", status.description);
            }
            status
        })
        .collect();
    text(&statuses)
}

/// `--at` JSON: [`json`] of the recorded statuses, with `"backfilled":
/// true` on each platform rebuilt from the vendor's incident history.
#[must_use]
pub fn history_json(records: &[Record]) -> String {
    let platforms = Value::Array(
        records
            .iter()
            .map(|record| {
                let mut value = platform_json(&record.status);
                if record.backfilled {
                    value["backfilled"] = Value::Bool(true);
                }
                value
            })
            .collect(),
    );
    format!("{platforms:#}")
}

/// One platform as JSON: the model's own fields plus each incident's age.
fn platform_json(status: &PlatformStatus) -> Value {
    let mut value = serde_json::to_value(status).expect("the status model always serializes");
    // The model leaves `probe` out when there is none (history stays as it
    // was); `--json` always has the key.
    value["probe"] = serde_json::to_value(&status.probe).expect("a probe always serializes");
    let fetched_at = status.fetched_at;
    add_ages(
        &mut value["incidents"],
        &status.incidents,
        fetched_at,
        |i| i.started_at,
    );
    add_ages(&mut value["resolved"], &status.resolved, fetched_at, |i| {
        i.ended_at
    });
    value
}

/// Give each incident in the `json` array an `age_seconds` from the time
/// `from` picks, and its `latest_update` one from `posted_at`.
fn add_ages(
    json: &mut Value,
    incidents: &[Incident],
    fetched_at: DateTime<Utc>,
    from: fn(&Incident) -> Option<DateTime<Utc>>,
) {
    let since = |at: Option<DateTime<Utc>>| at.map(|at| age_seconds(at, fetched_at));
    let Some(json) = json.as_array_mut() else {
        return;
    };
    for (json, incident) in json.iter_mut().zip(incidents) {
        json["age_seconds"] = since(from(incident)).into();
        if let Some(update) = &incident.latest_update {
            json["latest_update"]["age_seconds"] = since(update.posted_at).into();
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;
    use crate::model::{Level, Probe};

    fn status(name: &str, level: Level, description: &str) -> PlatformStatus {
        PlatformStatus {
            name: name.to_owned(),
            level,
            description: description.to_owned(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: Utc.with_ymd_and_hms(2026, 11, 13, 19, 10, 0).unwrap(),
            error: None,
            probe: None,
            regions: Vec::new(),
            component_tree: Vec::new(),
        }
    }

    fn sample() -> Vec<PlatformStatus> {
        let mut cloudflare = status("Cloudflare", Level::Degraded, "Partially Degraded Service");
        cloudflare.incidents.push(Incident {
            kind: IncidentKind::Incident,
            title: "Workers API errors".to_owned(),
            components: vec!["Workers".to_owned(), "Queues".to_owned()],
            started_at: Some(Utc.with_ymd_and_hms(2026, 11, 13, 15, 31, 13).unwrap()),
            ended_at: None,
            url: Some("https://www.cloudflarestatus.com/incidents/e7ktq32gphhe".to_owned()),
            stage: Some("monitoring".to_owned()),
            latest_update: Some(Update {
                text: "A fix has been implemented and we are monitoring the results.".to_owned(),
                posted_at: Some(Utc.with_ymd_and_hms(2026, 11, 13, 15, 42, 43).unwrap()),
            }),
        });
        cloudflare.incidents.push(Incident {
            kind: IncidentKind::Maintenance,
            title: "HKG (Hong Kong) on 2026-11-13".to_owned(),
            components: Vec::new(),
            started_at: None,
            ended_at: None,
            url: None,
            stage: Some("in_progress".to_owned()),
            latest_update: None,
        });
        let mut slow = status("Slow", Level::Unknown, "Status unavailable");
        slow.error = Some("timed out after 10s".to_owned());
        slow.probe = Some(Probe {
            url: "https://slow.example/".to_owned(),
            ok: true,
            code: Some(401),
            elapsed_ms: Some(220),
            description: "responding (HTTP 401 in 220 ms)".to_owned(),
        });
        let mut github = status("GitHub", Level::Operational, "All Systems Operational");
        github.probe = Some(Probe {
            url: "https://github.com/".to_owned(),
            ok: false,
            code: None,
            elapsed_ms: None,
            description: "no answer (could not connect)".to_owned(),
        });
        vec![github, cloudflare, slow]
    }

    #[test]
    fn text_leads_with_the_verdict_and_lists_worst_first() {
        let expected = "\
2 of 3 need attention: Slow, Cloudflare
Slow        Unknown      Status unavailable
  probe: responding (HTTP 401 in 220 ms), GET https://slow.example/
  error: timed out after 10s
Cloudflare  Degraded     Partially Degraded Service
  - Workers API errors
    affects Workers, Queues; since 2026-11-13 15:31 UTC (3h 38m ago)
    monitoring, updated 3h 27m ago: A fix has been implemented and we are monitoring the results.
    https://www.cloudflarestatus.com/incidents/e7ktq32gphhe
  - [maintenance] HKG (Hong Kong) on 2026-11-13
    in_progress
GitHub      Operational  All Systems Operational
  probe: no answer (could not connect), GET https://github.com/
";
        assert_eq!(text(&sample()), expected);
    }

    #[test]
    fn text_shows_just_now_for_a_start_after_the_fetch() {
        let mut github = status("GitHub", Level::Degraded, "Minor Service Outage");
        github.incidents.push(Incident {
            kind: IncidentKind::Incident,
            title: "Actions delays".to_owned(),
            components: Vec::new(),
            started_at: Some(Utc.with_ymd_and_hms(2026, 11, 13, 19, 10, 30).unwrap()),
            url: None,
            ..Default::default()
        });
        assert!(
            text(&[github]).contains("    since 2026-11-13 19:10 UTC (just now)\n"),
            "clock skew must not show a negative age"
        );
    }

    #[test]
    fn text_puts_a_long_update_on_one_line_and_cuts_it() {
        let mut gcp = status("Google Cloud", Level::Outage, "1 active incident");
        let report = format!("# Incident Report\n## Summary\n{}", "word ".repeat(100));
        gcp.incidents.push(Incident {
            title: "us-west1 errors".to_owned(),
            latest_update: Some(Update {
                text: report,
                posted_at: None,
            }),
            ..Default::default()
        });
        let text = text(&[gcp]);
        let line = text
            .lines()
            .find(|line| line.starts_with("    update: "))
            .unwrap_or_else(|| panic!("no update line in {text}"));
        let body = line.strip_prefix("    update: ").unwrap();
        assert!(
            body.starts_with("# Incident Report ## Summary word word"),
            "{body}"
        );
        assert!(body.ends_with("word…"), "cut at a word break: {body}");
        assert!(body.chars().count() <= UPDATE_TEXT_LIMIT + 1, "{body}");
    }

    #[test]
    fn shorten_keeps_short_text_and_cuts_long_text() {
        assert_eq!(shorten("  a\n\n b  ", 10), "a b");
        assert_eq!(shorten("exactly ten", 11), "exactly ten");
        assert_eq!(shorten("one two three four", 13), "one two three…");
        // A cut that already ends a word keeps it.
        assert_eq!(shorten("aaaaaaaaaa bb cc", 13), "aaaaaaaaaa bb…");
        assert_eq!(shorten("aaaaaaaaaa bb cc", 14), "aaaaaaaaaa bb…");
        // A break in the last fifth of the cut is used; one further back is not.
        assert_eq!(shorten("one two three four", 15), "one two three…");
        assert_eq!(shorten("one two three four", 10), "one two th…");
        // No word break near the cut: cut mid-word.
        assert_eq!(shorten("abcdefghijklmnop", 10), "abcdefghij…");
        assert_eq!(shorten("ééééé", 3), "ééé…");
    }

    #[test]
    fn text_when_everything_is_operational() {
        let statuses = [
            status("GitHub", Level::Operational, "All Systems Operational"),
            status("AWS", Level::Operational, "No recent issues"),
        ];
        let expected = "\
All 2 operational
GitHub  Operational  All Systems Operational
AWS     Operational  No recent issues
";
        assert_eq!(text(&statuses), expected);
    }

    #[test]
    fn text_with_no_platforms_is_empty() {
        assert_eq!(text(&[]), "");
    }

    /// JSON keeps config order: only the text output sorts.
    #[test]
    fn json_matches_the_model() {
        let value: Value = serde_json::from_str(&json(&sample())).unwrap();
        let platforms = value.as_array().unwrap();
        assert_eq!(platforms.len(), 3);

        let github = &platforms[0];
        assert_eq!(github["name"], "GitHub");
        assert_eq!(github["level"], "operational");
        assert_eq!(github["description"], "All Systems Operational");
        assert_eq!(github["incidents"], Value::Array(Vec::new()));
        assert_eq!(github["fetched_at"], "2026-11-13T19:10:00Z");
        assert_eq!(github["error"], Value::Null);

        let incident = &platforms[1]["incidents"][0];
        assert_eq!(platforms[1]["level"], "degraded");
        assert_eq!(incident["kind"], "incident");
        assert_eq!(incident["title"], "Workers API errors");
        assert_eq!(incident["components"][1], "Queues");
        assert_eq!(incident["started_at"], "2026-11-13T15:31:13Z");
        // 15:31:13 to 19:10:00.
        assert_eq!(incident["age_seconds"], 3 * 3600 + 38 * 60 + 47);
        assert_eq!(
            incident["url"],
            "https://www.cloudflarestatus.com/incidents/e7ktq32gphhe"
        );
        assert_eq!(incident["stage"], "monitoring");
        let update = &incident["latest_update"];
        assert_eq!(
            update["text"],
            "A fix has been implemented and we are monitoring the results."
        );
        assert_eq!(update["posted_at"], "2026-11-13T15:42:43Z");
        // 15:42:43 to 19:10:00.
        assert_eq!(update["age_seconds"], 3 * 3600 + 27 * 60 + 17);

        let maintenance = &platforms[1]["incidents"][1];
        assert_eq!(maintenance["kind"], "maintenance");
        assert_eq!(maintenance["age_seconds"], Value::Null);
        assert_eq!(maintenance["stage"], "in_progress");
        assert_eq!(maintenance["latest_update"], Value::Null);

        assert_eq!(platforms[2]["level"], "unknown");
        assert_eq!(platforms[2]["error"], "timed out after 10s");
    }

    #[test]
    fn json_has_a_probe_object_or_null() {
        let value: Value = serde_json::from_str(&json(&sample())).unwrap();
        assert_eq!(
            value[0]["probe"],
            serde_json::json!({
                "url": "https://github.com/",
                "ok": false,
                "code": null,
                "elapsed_ms": null,
                "description": "no answer (could not connect)",
            })
        );
        assert_eq!(value[1]["probe"], Value::Null);
        assert_eq!(value[2]["probe"]["code"], 401);
        assert_eq!(value[2]["probe"]["elapsed_ms"], 220);
        assert_eq!(value[2]["probe"]["ok"], true);
    }

    /// GitHub with two incidents resolved earlier, newest first.
    fn with_resolved() -> PlatformStatus {
        let mut github = status("GitHub", Level::Operational, "All Systems Operational");
        github.resolved = vec![
            Incident {
                title: "Incident with Pull Requests".to_owned(),
                started_at: Some(Utc.with_ymd_and_hms(2026, 11, 13, 15, 47, 12).unwrap()),
                ended_at: Some(Utc.with_ymd_and_hms(2026, 11, 13, 18, 5, 0).unwrap()),
                url: Some("https://stspg.io/qzj7qjdcr8jc".to_owned()),
                stage: Some("resolved".to_owned()),
                latest_update: Some(Update {
                    text: "This incident has been resolved.".to_owned(),
                    posted_at: Some(Utc.with_ymd_and_hms(2026, 11, 13, 18, 5, 0).unwrap()),
                }),
                ..Default::default()
            },
            Incident {
                title: "Disruption with Email Delivery".to_owned(),
                ended_at: Some(Utc.with_ymd_and_hms(2026, 11, 13, 2, 0, 0).unwrap()),
                ..Default::default()
            },
        ];
        github
    }

    #[test]
    fn text_sums_up_resolved_incidents_on_one_line() {
        let expected = "\
All 1 operational
GitHub  Operational  All Systems Operational
  2 resolved in the last 24h, latest: Incident with Pull Requests (1h 5m ago)
";
        assert_eq!(text(&[with_resolved()]), expected);

        let mut one = with_resolved();
        one.resolved.truncate(1);
        assert!(
            text(&[one]).contains(
                "\n  1 resolved in the last 24h: Incident with Pull Requests (1h 5m ago)\n"
            ),
            "one resolved incident reads naturally"
        );
    }

    #[test]
    fn json_lists_resolved_incidents_with_their_age_since_the_end() {
        let value: Value = serde_json::from_str(&json(&[with_resolved()])).unwrap();
        let github = &value[0];
        assert_eq!(github["level"], "operational");
        assert_eq!(github["incidents"], Value::Array(Vec::new()));
        let resolved = &github["resolved"][0];
        assert_eq!(resolved["title"], "Incident with Pull Requests");
        assert_eq!(resolved["started_at"], "2026-11-13T15:47:12Z");
        assert_eq!(resolved["ended_at"], "2026-11-13T18:05:00Z");
        // 18:05 to 19:10.
        assert_eq!(resolved["age_seconds"], 65 * 60);
        assert_eq!(resolved["url"], "https://stspg.io/qzj7qjdcr8jc");
        assert_eq!(resolved["stage"], "resolved");
        assert_eq!(resolved["latest_update"]["age_seconds"], 65 * 60);
        assert_eq!(github["resolved"][1]["latest_update"], Value::Null);
    }

    #[test]
    fn active_incidents_have_a_null_end_in_json() {
        let value: Value = serde_json::from_str(&json(&sample())).unwrap();
        assert_eq!(value[1]["incidents"][0]["ended_at"], Value::Null);
        assert_eq!(value[1]["resolved"], Value::Array(Vec::new()));
    }
}
