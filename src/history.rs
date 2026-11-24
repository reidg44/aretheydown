//! `--serve` history: a JSON Lines file of status changes, the lookups
//! `--at <time>` runs on it, and the level timeline the TUI shows from it
//! ([`timeline`], [`read`]).
//!
//! Each line is a [`Record`]: `{"recorded_at": <RFC 3339>, "status":
//! <PlatformStatus>}`, the model itself (no `age_seconds`). `--serve` appends
//! a platform's status only when it differs from the last one it wrote
//! ([`changed`]), and drops records older than [`RETENTION`] when it starts
//! ([`prune`]). Everything but the file functions at the bottom is pure.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::hash::BuildHasher;
use std::io::{self, Read as _, Seek as _, SeekFrom, Write as _};
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDateTime, NaiveTime, TimeDelta, TimeZone, Utc};
use serde::{Deserialize, Serialize};

use crate::model::{Incident, Level, PastIncident, PlatformStatus};
use crate::providers::check;

/// How long records are kept: older ones are dropped when `--serve` starts.
pub const RETENTION: TimeDelta = TimeDelta::days(30);

/// One line of the history file: a platform's status as `--serve` saw it,
/// or, with `backfilled`, as rebuilt from the vendor's incident history
/// ([`backfill`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub recorded_at: DateTime<Utc>,
    pub status: PlatformStatus,
    /// Rebuilt from the vendor's incident feed when `--serve` started, not
    /// seen by a poll. Left out of the JSON when false, so older files and
    /// readers still work.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub backfilled: bool,
}

/// The default history file: `$XDG_STATE_HOME/aretheydown/history.jsonl`,
/// else `~/.local/state/aretheydown/history.jsonl`. `None` when neither
/// variable is set to an absolute path. `var` reads an environment variable
/// (`std::env::var_os` outside tests).
#[must_use]
pub fn default_path(var: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name| var(name).map(PathBuf::from).filter(|p| p.is_absolute());
    let state = absolute("XDG_STATE_HOME")
        .or_else(|| absolute("HOME").map(|home| home.join(".local").join("state")))?;
    Some(state.join("aretheydown").join("history.jsonl"))
}

/// Whether two statuses of a platform say the same thing: everything but
/// `fetched_at`, `resolved`, `probe`, and a check's response time is equal.
fn same_state(a: &PlatformStatus, b: &PlatformStatus) -> bool {
    a.name == b.name
        && a.level == b.level
        && check::without_latency(&a.description) == check::without_latency(&b.description)
        && a.error == b.error
        && a.incidents == b.incidents
}

/// The statuses in `statuses` to write, given the last status written for
/// each platform (by name): those with no earlier record or a different
/// [`same_state`]. In `statuses` order.
#[must_use]
pub fn changed<'a, S: BuildHasher>(
    last: &HashMap<String, PlatformStatus, S>,
    statuses: &'a [PlatformStatus],
) -> Vec<&'a PlatformStatus> {
    statuses
        .iter()
        .filter(|status| {
            last.get(&status.name)
                .is_none_or(|previous| !same_state(previous, status))
        })
        .collect()
}

/// Decides what each poll writes and remembers what was written.
///
/// `write` appends records (a file in `--serve`, a fake in tests). A failed
/// write leaves the memory as it was, so the next poll tries those changes again.
pub struct Recorder<W> {
    last: HashMap<String, PlatformStatus>,
    write: W,
}

impl<W: FnMut(&[Record]) -> io::Result<()>> Recorder<W> {
    /// A recorder that has written nothing yet, so its first poll writes every platform.
    pub fn new(write: W) -> Self {
        Self {
            last: HashMap::new(),
            write,
        }
    }

    /// Write the statuses of this poll that changed, stamped `recorded_at`.
    /// Returns how many records were written (0 calls no write).
    ///
    /// # Errors
    ///
    /// Whatever `write` returns; nothing is remembered then.
    pub fn record(
        &mut self,
        statuses: &[PlatformStatus],
        recorded_at: DateTime<Utc>,
    ) -> io::Result<usize> {
        let records: Vec<Record> = changed(&self.last, statuses)
            .into_iter()
            .map(|status| Record {
                recorded_at,
                // A probe is what this poll saw, not the platform's state;
                // records leave it out (and `same_state` ignores it).
                status: PlatformStatus {
                    probe: None,
                    ..status.clone()
                },
                backfilled: false,
            })
            .collect();
        if records.is_empty() {
            return Ok(0);
        }
        (self.write)(&records)?;
        for record in &records {
            self.last
                .insert(record.status.name.clone(), record.status.clone());
        }
        Ok(records.len())
    }
}

/// The records in a history file's text, in file order, and how many
/// non-blank lines could not be read.
#[must_use]
pub fn parse(text: &str) -> (Vec<Record>, usize) {
    let mut records = Vec::new();
    let mut skipped = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(_) => skipped += 1,
        }
    }
    (records, skipped)
}

/// One record as a line of the file, newline included.
///
/// # Panics
///
/// Never: every field of the model serializes cleanly.
#[must_use]
pub fn line(record: &Record) -> String {
    let mut line = serde_json::to_string(record).expect("a record always serializes");
    line.push('\n');
    line
}

/// What each platform looked like at `time`: per platform, the latest record
/// with `recorded_at` at or before `time` (a later line wins a tie). Platforms
/// come in the order they first appear in `records`; one with no record by
/// `time` is left out.
#[must_use]
pub fn at_records(records: &[Record], time: DateTime<Utc>) -> Vec<&Record> {
    let mut order: Vec<&str> = Vec::new();
    let mut latest: HashMap<&str, &Record> = HashMap::new();
    for record in records.iter().filter(|r| r.recorded_at <= time) {
        let name = record.status.name.as_str();
        match latest.get(name) {
            None => {
                order.push(name);
                latest.insert(name, record);
            }
            Some(best) if record.recorded_at >= best.recorded_at => {
                latest.insert(name, record);
            }
            Some(_) => {}
        }
    }
    order.into_iter().map(|name| latest[name]).collect()
}

/// The earliest `recorded_at` of platform `name` in `records`, where
/// [`backfill`] stops.
#[must_use]
pub fn earliest_for(records: &[Record], name: &str) -> Option<DateTime<Utc>> {
    earliest(records.iter().filter(|r| r.status.name == name))
}

/// The earliest `recorded_at` in `records`, for "no history before …" messages.
#[must_use]
pub fn earliest<'a>(records: impl IntoIterator<Item = &'a Record>) -> Option<DateTime<Utc>> {
    records.into_iter().map(|r| r.recorded_at).min()
}

/// The lines of a history file's text to keep at `now`, in order, each with
/// its newline: records within [`RETENTION`], and unreadable lines (perhaps
/// from another version; `--at` skips them). Blank lines go.
#[must_use]
pub fn retain(text: &str, now: DateTime<Utc>) -> String {
    let cutoff = now - RETENTION;
    let mut kept = String::new();
    for raw in text.lines().filter(|line| !line.trim().is_empty()) {
        let keep = serde_json::from_str::<Record>(raw).map_or(true, |r| r.recorded_at >= cutoff);
        if keep {
            kept.push_str(raw);
            kept.push('\n');
        }
    }
    kept
}

/// Read an `--at` time. Accepted, ignoring case and extra spaces:
///
/// - RFC 3339 (`2026-11-14T02:14:00Z`), in its own offset.
/// - `now`, and relative times: `<number><m|h|d>` groups then `ago`
///   (`30m ago`, `1d 4h ago`, `1h30m ago`).
/// - Clock forms, in the zone of `now`: `YYYY-MM-DD HH:MM`, `today HH:MM`,
///   `yesterday HH:MM`, and a bare `HH:MM`, which is the most recent past
///   one (yesterday's when that time has not come yet today). Ending a
///   clock form in ` UTC` or `Z` reads it in UTC instead, dates included.
///
/// # Errors
///
/// A message naming the accepted formats, or saying that a local time does
/// not exist (a daylight-saving gap). An ambiguous local time takes the earlier.
pub fn parse_time<Tz: TimeZone>(input: &str, now: &DateTime<Tz>) -> Result<DateTime<Utc>, String> {
    let trimmed = input.trim();
    if let Ok(at) = DateTime::parse_from_rfc3339(trimmed) {
        return Ok(at.with_timezone(&Utc));
    }
    let lower = trimmed.to_ascii_lowercase();
    let words: Vec<&str> = lower.split_whitespace().collect();
    let invalid = || {
        format!(
            "invalid --at time {trimmed:?}: use RFC 3339 (2026-11-14T02:14:00Z), \"now\", \"2h ago\" or \"1d 4h ago\" (m, h, d), \
             or \"HH:MM\", \"today HH:MM\", \"yesterday HH:MM\", \"YYYY-MM-DD HH:MM\" (local time; end in \" UTC\" or \"Z\" for UTC)"
        )
    };
    match words.as_slice() {
        ["now"] => return Ok(now.with_timezone(&Utc)),
        [groups @ .., "ago"] if !groups.is_empty() => {
            return ago(groups)
                .and_then(|back| now.with_timezone(&Utc).checked_sub_signed(back))
                .ok_or_else(invalid);
        }
        _ => {}
    }
    let (words, in_utc) = match words.as_slice() {
        [rest @ .., "utc" | "z"] => (rest.to_vec(), true),
        [rest @ .., last] if last.len() > 1 && last.ends_with('z') => {
            let mut rest = rest.to_vec();
            rest.push(&last[..last.len() - 1]);
            (rest, true)
        }
        _ => (words, false),
    };
    let found = if in_utc {
        clock(&words, &now.with_timezone(&Utc))
    } else {
        clock(&words, now)
    };
    match found {
        Clock::At(at) => Ok(at),
        Clock::Missing => Err(format!(
            "--at time {trimmed:?} does not exist in local time"
        )),
        Clock::Other => Err(invalid()),
    }
}

/// What [`clock`] made of the words.
enum Clock {
    /// A clock form, at this instant.
    At(DateTime<Utc>),
    /// A clock form naming a local time that does not exist (a
    /// daylight-saving gap).
    Missing,
    /// Not a clock form.
    Other,
}

/// The span `groups` (each one or more `<number><m|h|d>`) adds up to, or
/// `None` when a group is malformed or the span overflows.
fn ago(groups: &[&str]) -> Option<TimeDelta> {
    let mut total = TimeDelta::zero();
    for group in groups {
        let mut rest = *group;
        if rest.is_empty() {
            return None;
        }
        while !rest.is_empty() {
            let digits = rest.find(|c: char| !c.is_ascii_digit())?;
            if digits == 0 {
                return None;
            }
            let count: i64 = rest[..digits].parse().ok()?;
            let unit = rest[digits..].chars().next()?;
            let span = match unit {
                'm' => TimeDelta::try_minutes(count)?,
                'h' => TimeDelta::try_hours(count)?,
                'd' => TimeDelta::try_days(count)?,
                _ => return None,
            };
            total = total.checked_add(&span)?;
            rest = &rest[digits + 1..];
        }
    }
    Some(total)
}

/// A clock form (`HH:MM`, `today HH:MM`, `yesterday HH:MM`,
/// `YYYY-MM-DD HH:MM`) read in `now`'s zone.
fn clock<Tz: TimeZone>(words: &[&str], now: &DateTime<Tz>) -> Clock {
    let Some(naive) = clock_naive(words, now) else {
        return Clock::Other;
    };
    now.timezone()
        .from_local_datetime(&naive)
        .earliest()
        .map_or(Clock::Missing, |at| Clock::At(at.with_timezone(&Utc)))
}

/// The wall-clock date and time a clock form names on `now`'s calendar, or
/// `None` when `words` is not one. A bare `HH:MM` later than `now` is
/// yesterday's.
fn clock_naive<Tz: TimeZone>(words: &[&str], now: &DateTime<Tz>) -> Option<NaiveDateTime> {
    let hm = |text: &str| NaiveTime::parse_from_str(text, "%H:%M").ok();
    let today = now.date_naive();
    match words {
        [time] => {
            let time = hm(time)?;
            let at = today.and_time(time);
            if at > now.naive_local() {
                Some(today.pred_opt()?.and_time(time))
            } else {
                Some(at)
            }
        }
        ["today", time] => Some(today.and_time(hm(time)?)),
        ["yesterday", time] => Some(today.pred_opt()?.and_time(hm(time)?)),
        [date, time] => {
            NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M").ok()
        }
        _ => None,
    }
}

/// How far back the TUI's history section reaches.
pub const TIMELINE_WINDOW: TimeDelta = TimeDelta::days(7);

/// A change of a platform's level, as the TUI's history section lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// When the record with the new level was written.
    pub at: DateTime<Utc>,
    /// The level before; `None` for the oldest change shown.
    pub from: Option<Level>,
    pub to: Level,
    /// When the next change came; `None` while this level is still in force.
    pub until: Option<DateTime<Utc>>,
    /// The titles of the incidents active in the record that made the change.
    pub incidents: Vec<String>,
}

/// The level changes of platform `name` in `records`, newest first: the
/// changes within [`TIMELINE_WINDOW`] before `now`, plus the change in force
/// when the window starts (so an outage that began before it still shows).
/// Records keeping the level of the one before are folded into it; the oldest
/// change shown has no `from`. Records later than `now` are left out.
#[must_use]
pub fn timeline(records: &[Record], name: &str, now: DateTime<Utc>) -> Vec<Change> {
    let mut mine: Vec<&Record> = records
        .iter()
        .filter(|r| r.status.name == name && r.recorded_at <= now)
        .collect();
    // Stable: records written at the same time keep file order.
    mine.sort_by_key(|r| r.recorded_at);
    let mut changes: Vec<Change> = Vec::new();
    for record in mine {
        let level = record.status.level;
        let previous = changes.last().map(|c| c.to);
        if previous == Some(level) {
            continue;
        }
        if let Some(last) = changes.last_mut() {
            last.until = Some(record.recorded_at);
        }
        changes.push(Change {
            at: record.recorded_at,
            from: previous,
            to: level,
            until: None,
            incidents: record
                .status
                .incidents
                .iter()
                .map(|i| i.title.clone())
                .collect(),
        });
    }
    let start = now - TIMELINE_WINDOW;
    let first = changes
        .iter()
        .rposition(|c| c.at <= start)
        .unwrap_or_default();
    let mut shown = changes.split_off(first);
    if let Some(oldest) = shown.first_mut() {
        oldest.from = None;
    }
    shown.reverse();
    shown
}

/// The [`backfill`] records of platform `name`, rebuilt from its `past`
/// incidents at `now`: a record (with `backfilled` set) at each incident start
/// or end where the platform's level or its set of active incidents changes,
/// the level being the worst of the incidents active then (Operational once
/// none is). Only records strictly before `until` (the platform's earliest
/// record in the file, else `now`) and within [`RETENTION`] of `now` are
/// given, so they never overlap what `--serve` recorded and a second run adds
/// nothing (`prune` at the next start only drops records the window has
/// left). An incident that began before the window shows only in the
/// changes after it. Oldest first.
#[must_use]
pub fn backfill(
    name: &str,
    past: &[PastIncident],
    until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Vec<Record> {
    let until = until.unwrap_or(now).min(now);
    let cutoff = now - RETENTION;
    // Each incident's active span; an end before the start counts as none.
    let spans: Vec<Span> = past
        .iter()
        .filter_map(|past| {
            let start = past.incident.started_at?;
            let end = past.incident.ended_at.map(|end| end.max(start));
            Some(Span { start, end, past })
        })
        .collect();
    let mut times: Vec<DateTime<Utc>> = spans
        .iter()
        .flat_map(|span| std::iter::once(span.start).chain(span.end))
        .filter(|t| *t < until)
        .collect();
    times.sort_unstable();
    times.dedup();

    let mut records: Vec<Record> = Vec::new();
    for time in times {
        // Active at `time`: started by then and not yet ended. Newest start
        // first, as the feeds list them.
        let mut active: Vec<&Span> = spans
            .iter()
            .filter(|span| span.start <= time && span.end.is_none_or(|end| end > time))
            .collect();
        active.sort_by_key(|span| std::cmp::Reverse(span.start));
        let level = Level::worst(active.iter().map(|span| span.past.level));
        let incidents: Vec<Incident> = active
            .iter()
            .map(|span| Incident {
                ended_at: None,
                // The stage and latest update are the vendor's last word,
                // not what it said at `time`.
                stage: None,
                latest_update: None,
                ..span.past.incident.clone()
            })
            .collect();
        if records
            .last()
            .is_some_and(|r| r.status.level == level && r.status.incidents == incidents)
        {
            continue;
        }
        records.push(Record {
            recorded_at: time,
            status: PlatformStatus {
                name: name.to_owned(),
                level,
                description: active_description(incidents.len()),
                incidents,
                resolved: Vec::new(),
                fetched_at: time,
                error: None,
                probe: None,
                regions: Vec::new(),
                component_tree: Vec::new(),
            },
            backfilled: true,
        });
    }
    // Changes before the window still set the state the first one in it
    // starts from; they are not written (`prune` would drop them).
    records.retain(|r| r.recorded_at >= cutoff && r.recorded_at < until);
    records
}

/// When a past incident was active: from `start` until `end` (`None` while
/// it still is).
struct Span<'a> {
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
    past: &'a PastIncident,
}

/// `No active incidents`, `1 active incident`, `3 active incidents`.
fn active_description(active: usize) -> String {
    match active {
        0 => "No active incidents".to_owned(),
        1 => "1 active incident".to_owned(),
        n => format!("{n} active incidents"),
    }
}

/// Append `records` to the file at `path` in one write, creating it and
/// its directory when missing. When the file does not end in a newline (an
/// earlier write was cut short), a newline goes first, so the cut line stays
/// one unreadable line and the new records stay readable.
///
/// # Errors
///
/// When the directory or file cannot be created or written.
pub fn append(path: &Path, records: &[Record]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)?;
    let mut text = String::new();
    if file.metadata()?.len() > 0 {
        let mut last = [0];
        file.seek(SeekFrom::End(-1))?;
        file.read_exact(&mut last)?;
        if last != *b"\n" {
            text.push('\n');
        }
    }
    text.extend(records.iter().map(line));
    file.write_all(text.as_bytes())
}

/// Drop records older than [`RETENTION`] from the file at `path` (see
/// [`retain`]), rewriting it through a temporary file. A missing file is fine.
///
/// # Errors
///
/// When the file cannot be read or rewritten.
pub fn prune(path: &Path, now: DateTime<Utc>) -> io::Result<()> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let kept = retain(&text, now);
    if kept == text {
        return Ok(());
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, kept)?;
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// The readable records in the file at `path` ([`parse`]; a line cut short
/// by a running `--serve` is skipped), or `None` when there is no file.
///
/// # Errors
///
/// When the file exists but cannot be read.
pub fn read(path: &Path) -> io::Result<Option<Vec<Record>>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(parse(&text).0)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{FixedOffset, TimeZone};

    use super::*;
    use crate::model::Update;

    fn t(hour: u32, minute: u32) -> DateTime<Utc> {
        utc(14, hour, minute)
    }

    fn status(name: &str, level: Level) -> PlatformStatus {
        PlatformStatus {
            name: name.to_owned(),
            level,
            description: "desc".to_owned(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: t(0, 0),
            error: None,
            probe: None,
            regions: Vec::new(),
            component_tree: Vec::new(),
        }
    }

    fn record(at: DateTime<Utc>, name: &str, level: Level) -> Record {
        Record {
            recorded_at: at,
            status: status(name, level),
            backfilled: false,
        }
    }

    fn written(statuses: &[PlatformStatus]) -> HashMap<String, PlatformStatus> {
        statuses
            .iter()
            .map(|s| (s.name.clone(), s.clone()))
            .collect()
    }

    fn names(statuses: &[&PlatformStatus]) -> Vec<String> {
        statuses.iter().map(|s| s.name.clone()).collect()
    }

    #[test]
    fn first_poll_writes_every_platform() {
        let poll = [status("A", Level::Operational), status("B", Level::Outage)];
        assert_eq!(names(&changed(&HashMap::new(), &poll)), ["A", "B"]);
    }

    #[test]
    fn unchanged_poll_writes_nothing() {
        let poll = [status("A", Level::Operational), status("B", Level::Outage)];
        assert_eq!(names(&changed(&written(&poll), &poll)), [""; 0]);
    }

    #[test]
    fn fetched_at_or_resolved_alone_is_no_change() {
        let before = [status("A", Level::Operational)];
        let mut after = before.clone();
        after[0].fetched_at = t(5, 0);
        after[0].resolved = vec![Incident {
            title: "ended".to_owned(),
            ended_at: Some(t(4, 0)),
            ..Default::default()
        }];
        assert_eq!(names(&changed(&written(&before), &after)), [""; 0]);
    }

    #[test]
    fn a_checks_response_time_alone_is_no_change() {
        let mut before = status("API", Level::Operational);
        before.description = "Responding: HTTP 200 in 142 ms".to_owned();
        let mut after = before.clone();
        after.description = "Responding: HTTP 200 in 9 ms".to_owned();
        let last = written(&[before]);
        assert_eq!(
            names(&changed(&last, std::slice::from_ref(&after))),
            [""; 0]
        );
        after.description = "Responding: HTTP 204 in 9 ms".to_owned();
        assert_eq!(names(&changed(&last, &[after])), ["API"]);
    }

    fn probed(status: &PlatformStatus, ok: bool, ms: u64) -> PlatformStatus {
        PlatformStatus {
            probe: Some(crate::model::Probe {
                url: "https://api.example.com/".to_owned(),
                ok,
                code: ok.then_some(401),
                elapsed_ms: ok.then_some(ms),
                description: format!("responding (HTTP 401 in {ms} ms)"),
            }),
            ..status.clone()
        }
    }

    #[test]
    fn a_probe_is_never_a_change_and_never_recorded() {
        let before = status("A", Level::Operational);
        let last = written(&[probed(&before, true, 220)]);
        for after in [
            probed(&before, true, 9),
            probed(&before, false, 0),
            before.clone(),
        ] {
            assert_eq!(names(&changed(&last, &[after])), [""; 0]);
        }
        let mut writes: Vec<Record> = Vec::new();
        let mut recorder = Recorder::new(|records: &[Record]| {
            writes.extend_from_slice(records);
            Ok(())
        });
        recorder
            .record(&[probed(&before, false, 0)], t(1, 0))
            .unwrap();
        drop(recorder);
        assert_eq!(writes[0].status, before, "the record has no probe");
        assert!(!line(&writes[0]).contains("probe"), "{}", line(&writes[0]));
    }

    #[test]
    fn each_kind_of_change_writes_that_platform_only() {
        let incident = Incident {
            title: "Errors".to_owned(),
            stage: Some("investigating".to_owned()),
            ..Default::default()
        };
        let mut with_incident = status("B", Level::Degraded);
        with_incident.incidents = vec![incident.clone()];
        let before = [status("A", Level::Operational), with_incident.clone()];
        let last = written(&before);

        let mut level = before.clone();
        level[0].level = Level::Degraded;
        assert_eq!(names(&changed(&last, &level)), ["A"]);

        let mut new_incident = before.clone();
        new_incident[0].incidents = vec![incident.clone()];
        assert_eq!(names(&changed(&last, &new_incident)), ["A"]);

        let mut update = before.clone();
        update[1].incidents[0].latest_update = Update::new(Some("Fix deployed"), Some(t(1, 0)));
        assert_eq!(names(&changed(&last, &update)), ["B"]);

        let mut stage = before.clone();
        stage[1].incidents[0].stage = Some("monitoring".to_owned());
        assert_eq!(names(&changed(&last, &stage)), ["B"]);

        let mut error = before.clone();
        error[0].error = Some("timed out".to_owned());
        assert_eq!(names(&changed(&last, &error)), ["A"]);
    }

    #[test]
    fn recorder_writes_changes_and_retries_after_a_failed_write() {
        let mut writes: Vec<Vec<Record>> = Vec::new();
        let mut fail = true;
        let mut recorder = Recorder::new(|records: &[Record]| {
            if std::mem::take(&mut fail) {
                return Err(io::Error::other("disk full"));
            }
            writes.push(records.to_vec());
            Ok(())
        });
        let poll = [status("A", Level::Operational), status("B", Level::Outage)];
        assert!(recorder.record(&poll, t(1, 0)).is_err());
        // The failed write is retried in full on the next poll.
        assert_eq!(recorder.record(&poll, t(1, 1)).unwrap(), 2);
        assert_eq!(recorder.record(&poll, t(1, 2)).unwrap(), 0);
        let mut next = poll.clone();
        next[1].level = Level::Operational;
        assert_eq!(recorder.record(&next, t(1, 3)).unwrap(), 1);
        drop(recorder);
        assert_eq!(writes.len(), 2);
        assert!(writes[0].iter().all(|r| r.recorded_at == t(1, 1)));
        assert_eq!(writes[1][0].status.name, "B");
        assert_eq!(writes[1][0].recorded_at, t(1, 3));
    }

    fn records() -> Vec<Record> {
        vec![
            record(t(1, 0), "A", Level::Operational),
            record(t(1, 0), "B", Level::Operational),
            record(t(2, 0), "A", Level::Outage),
            record(t(3, 0), "C", Level::Degraded),
            record(t(4, 0), "A", Level::Operational),
        ]
    }

    /// Each platform [`at_records`] gives, with its level.
    fn levels(records: &[&Record]) -> Vec<(String, Level)> {
        records
            .iter()
            .map(|r| (r.status.name.clone(), r.status.level))
            .collect()
    }

    #[test]
    fn at_before_the_first_record_is_empty() {
        assert_eq!(levels(&at_records(&records(), t(0, 59))), []);
    }

    #[test]
    fn at_exactly_on_a_record_includes_it() {
        assert_eq!(
            levels(&at_records(&records(), t(2, 0))),
            [
                ("A".to_owned(), Level::Outage),
                ("B".to_owned(), Level::Operational)
            ]
        );
    }

    #[test]
    fn at_between_records_takes_the_earlier_and_later_platforms_appear() {
        assert_eq!(
            levels(&at_records(&records(), t(2, 30))),
            [
                ("A".to_owned(), Level::Outage),
                ("B".to_owned(), Level::Operational)
            ]
        );
        assert_eq!(
            levels(&at_records(&records(), t(5, 0))),
            [
                ("A".to_owned(), Level::Operational),
                ("B".to_owned(), Level::Operational),
                ("C".to_owned(), Level::Degraded)
            ]
        );
    }

    #[test]
    fn earliest_is_the_first_recorded_time() {
        assert_eq!(earliest(&records()), Some(t(1, 0)));
        assert_eq!(earliest(&[]), None);
        assert_eq!(earliest_for(&records(), "C"), Some(t(3, 0)));
        assert_eq!(earliest_for(&records(), "D"), None);
    }

    #[test]
    fn parse_skips_bad_lines_and_blank_ones() {
        let text: String = records().iter().take(2).map(line).collect();
        let text = format!("{text}\nnot json\n{{\"recorded_at\": 1}}\n");
        let (parsed, skipped) = parse(&text);
        assert_eq!(parsed, records()[..2]);
        assert_eq!(skipped, 2);
    }

    #[test]
    fn retention_drops_records_older_than_30_days_and_keeps_order() {
        let now = t(12, 0);
        let old = record(now - RETENTION - TimeDelta::seconds(1), "A", Level::Outage);
        let edge = record(now - RETENTION, "B", Level::Degraded);
        let new = record(now - TimeDelta::hours(1), "A", Level::Operational);
        let text = [&old, &edge, &new].map(line).concat() + "\ngarbage\n";
        let kept = retain(&text, now);
        // Unreadable lines stay (`--at` skips them); blank lines go.
        assert_eq!(kept, [&edge, &new].map(line).concat() + "garbage\n");
    }

    #[test]
    fn append_and_prune_on_a_real_file() {
        let dir = std::env::temp_dir().join(format!("aretheydown-history-{}", std::process::id()));
        let path = dir.join("nested").join("history.jsonl");
        let _ = fs::remove_dir_all(&dir);
        let now = t(12, 0);
        let old = record(now - RETENTION - TimeDelta::hours(1), "A", Level::Outage);
        let new = record(now, "A", Level::Operational);

        append(&path, std::slice::from_ref(&old)).unwrap();
        // A write cut short leaves a partial line with no newline.
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"recorded_at\":")
            .unwrap();
        append(&path, std::slice::from_ref(&new)).unwrap();
        let (records, skipped) = parse(&fs::read_to_string(&path).unwrap());
        assert_eq!(records, [old, new.clone()]);
        assert_eq!(skipped, 1);

        prune(&path, now).unwrap();
        let (records, skipped) = parse(&fs::read_to_string(&path).unwrap());
        assert_eq!(records, [new]);
        assert_eq!(skipped, 1);
        prune(&dir.join("missing.jsonl"), now).unwrap();
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn parse_time_accepts_rfc_3339_and_dated_clock_times() {
        let zone = FixedOffset::west_opt(8 * 3600).unwrap();
        let now = zone.with_ymd_and_hms(2026, 11, 14, 15, 0, 0).unwrap();
        assert_eq!(parse_time("2026-11-14T02:14:00Z", &now), Ok(t(2, 14)));
        assert_eq!(parse_time("2026-11-14T02:14:00+01:00", &now), Ok(t(1, 14)));
        // Local times are in `now`'s zone (UTC-8 here).
        assert_eq!(parse_time("2026-11-13 18:14", &now), Ok(t(2, 14)));
        assert_eq!(parse_time(" 09:30 ", &now), Ok(t(17, 30)));
    }

    /// 2026-11-14 18:00 at UTC-8, which is 2026-11-15 02:00 UTC: the local
    /// and UTC dates differ, so the `UTC` suffix tests can tell them apart.
    fn evening() -> DateTime<FixedOffset> {
        let zone = FixedOffset::west_opt(8 * 3600).unwrap();
        zone.with_ymd_and_hms(2026, 11, 14, 18, 0, 0).unwrap()
    }

    fn utc(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, day, hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn parse_time_accepts_phrases() {
        let now = evening();
        assert_eq!(parse_time("now", &now), Ok(utc(15, 2, 0)));
        // Local 02:14 at UTC-8 is 10:14 UTC.
        assert_eq!(parse_time("today 02:14", &now), Ok(utc(14, 10, 14)));
        assert_eq!(parse_time("yesterday 02:14", &now), Ok(utc(13, 10, 14)));
        assert_eq!(parse_time("30m ago", &now), Ok(utc(15, 1, 30)));
        assert_eq!(parse_time("2h ago", &now), Ok(utc(15, 0, 0)));
        assert_eq!(parse_time("1d 4h ago", &now), Ok(utc(13, 22, 0)));
        assert_eq!(parse_time("1h30m ago", &now), Ok(utc(15, 0, 30)));
        assert_eq!(parse_time("0m ago", &now), Ok(utc(15, 2, 0)));
    }

    #[test]
    fn parse_time_ignores_case_and_extra_spaces() {
        let now = evening();
        assert_eq!(parse_time("YESTERDAY  02:14", &now), Ok(utc(13, 10, 14)));
        assert_eq!(parse_time(" 2H AGO ", &now), Ok(utc(15, 0, 0)));
        assert_eq!(parse_time("Now", &now), Ok(utc(15, 2, 0)));
        assert_eq!(parse_time("1d   4h  ago", &now), Ok(utc(13, 22, 0)));
    }

    #[test]
    fn bare_clock_time_is_the_most_recent_past_one() {
        let now = evening();
        // 19:00 local has not come yet today, so it is yesterday's.
        assert_eq!(parse_time("19:00", &now), Ok(utc(14, 3, 0)));
        // The current minute and earlier stay today.
        assert_eq!(parse_time("18:00", &now), Ok(utc(15, 2, 0)));
        assert_eq!(parse_time("17:59", &now), Ok(utc(15, 1, 59)));
        // `today` always means today, even in the future.
        assert_eq!(parse_time("today 19:00", &now), Ok(utc(15, 3, 0)));
        // Seconds into the current minute: that minute is still today.
        let now = now + TimeDelta::seconds(30);
        assert_eq!(parse_time("18:00", &now), Ok(utc(15, 2, 0)));
        assert_eq!(parse_time("18:01", &now), Ok(utc(14, 2, 1)));
    }

    #[test]
    fn utc_suffix_reads_clock_forms_in_utc() {
        let now = evening(); // 2026-11-15 02:00 UTC
        // 02:14 UTC has not come yet on the UTC date, so it is yesterday's.
        assert_eq!(parse_time("02:14 UTC", &now), Ok(utc(14, 2, 14)));
        assert_eq!(parse_time("01:30Z", &now), Ok(utc(15, 1, 30)));
        assert_eq!(parse_time("02:14Z", &now), Ok(utc(14, 2, 14)));
        assert_eq!(parse_time("01:30 Z", &now), Ok(utc(15, 1, 30)));
        assert_eq!(parse_time("01:30z", &now), Ok(utc(15, 1, 30)));
        assert_eq!(parse_time("today 02:14 UTC", &now), Ok(utc(15, 2, 14)));
        assert_eq!(parse_time("yesterday 02:14 utc", &now), Ok(utc(14, 2, 14)));
        assert_eq!(parse_time("2026-11-14 02:14 UTC", &now), Ok(utc(14, 2, 14)));
        assert_eq!(parse_time("2026-11-14 02:14Z", &now), Ok(utc(14, 2, 14)));
        assert!(parse_time("2h ago UTC", &now).is_err());
        assert!(parse_time("now UTC", &now).is_err());
    }

    /// UTC-8, except that local 2026-11-14 02:00 to 02:59 does not exist, like
    /// a spring-forward gap.
    #[derive(Debug, Clone, Copy)]
    struct GapZone;

    impl TimeZone for GapZone {
        type Offset = FixedOffset;

        fn from_offset(_: &FixedOffset) -> Self {
            Self
        }

        fn offset_from_local_date(
            &self,
            _: &chrono::NaiveDate,
        ) -> chrono::LocalResult<FixedOffset> {
            chrono::LocalResult::Single(FixedOffset::west_opt(8 * 3600).unwrap())
        }

        fn offset_from_local_datetime(
            &self,
            local: &NaiveDateTime,
        ) -> chrono::LocalResult<FixedOffset> {
            let gap = chrono::NaiveDate::from_ymd_opt(2026, 11, 14)
                .unwrap()
                .and_hms_opt(2, 0, 0)
                .unwrap();
            if (gap..gap + TimeDelta::hours(1)).contains(local) {
                chrono::LocalResult::None
            } else {
                chrono::LocalResult::Single(FixedOffset::west_opt(8 * 3600).unwrap())
            }
        }

        fn offset_from_utc_date(&self, _: &chrono::NaiveDate) -> FixedOffset {
            FixedOffset::west_opt(8 * 3600).unwrap()
        }

        fn offset_from_utc_datetime(&self, _: &NaiveDateTime) -> FixedOffset {
            FixedOffset::west_opt(8 * 3600).unwrap()
        }
    }

    #[test]
    fn parse_time_reports_a_local_time_that_does_not_exist() {
        let now = GapZone.with_ymd_and_hms(2026, 11, 15, 10, 0, 0).unwrap();
        for gap in ["yesterday 02:30", "2026-11-14 02:30"] {
            let error = parse_time(gap, &now).unwrap_err();
            assert!(error.contains("does not exist"), "{gap:?}: {error}");
        }
        assert!(parse_time("yesterday 03:00", &now).is_ok());
        let error = parse_time("yesterday 02:30", &now).unwrap_err();
        assert!(error.contains("\"yesterday 02:30\""), "{error}");
        // `today` on the gap day.
        let now = GapZone.with_ymd_and_hms(2026, 11, 14, 10, 0, 0).unwrap();
        assert!(
            parse_time("today 02:30", &now)
                .unwrap_err()
                .contains("does not exist")
        );
    }

    #[test]
    fn parse_time_rejects_other_input() {
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 15, 0, 0).unwrap();
        for bad in [
            "",
            "yesterday",
            "today",
            "2h",
            "ago",
            "0x ago",
            "-2h ago",
            "2 h ago",
            "h ago",
            "99999999999999999999h ago",
            "999999999d ago",
            "tomorrow 02:14",
            "yesterday 02:14 extra",
            "2026-11-14",
            "25:00",
            "25:00 UTC",
            "UTC",
            "2026-13-01 10:00",
            "10:00:00",
        ] {
            let error = parse_time(bad, &now).unwrap_err();
            assert!(error.contains("RFC 3339"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn default_path_prefers_xdg_state_home_then_home() {
        let env = |pairs: &'static [(&str, &str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| OsString::from(v))
            }
        };
        assert_eq!(
            default_path(env(&[("XDG_STATE_HOME", "/s"), ("HOME", "/h")])),
            Some(PathBuf::from("/s/aretheydown/history.jsonl"))
        );
        assert_eq!(
            default_path(env(&[("XDG_STATE_HOME", "relative"), ("HOME", "/h")])),
            Some(PathBuf::from("/h/.local/state/aretheydown/history.jsonl"))
        );
        assert_eq!(default_path(env(&[])), None);
    }

    fn with_incidents(mut record: Record, titles: &[&str]) -> Record {
        record.status.incidents = titles
            .iter()
            .map(|title| Incident {
                title: (*title).to_owned(),
                ..Default::default()
            })
            .collect();
        record
    }

    /// (at, from, to, until) of each change, for compact asserts.
    type Row = (DateTime<Utc>, Option<Level>, Level, Option<DateTime<Utc>>);

    fn rows(changes: &[Change]) -> Vec<Row> {
        changes
            .iter()
            .map(|c| (c.at, c.from, c.to, c.until))
            .collect()
    }

    #[test]
    fn timeline_collapses_same_level_records_newest_first() {
        let now = utc(15, 12, 0);
        let mut restarted = record(utc(15, 3, 0), "A", Level::Outage);
        restarted.status.description = "restart re-recorded it".to_owned();
        let records = [
            record(utc(15, 1, 0), "A", Level::Operational),
            record(utc(15, 1, 30), "B", Level::Outage),
            with_incidents(
                record(utc(15, 2, 14), "A", Level::Outage),
                &["Actions errors"],
            ),
            restarted,
            record(utc(15, 2, 53), "A", Level::Outage), // out of order, same level
            record(utc(15, 4, 0), "A", Level::Operational),
        ];
        let changes = timeline(&records, "A", now);
        assert_eq!(
            rows(&changes),
            [
                (utc(15, 4, 0), Some(Level::Outage), Level::Operational, None),
                (
                    utc(15, 2, 14),
                    Some(Level::Operational),
                    Level::Outage,
                    Some(utc(15, 4, 0))
                ),
                (
                    utc(15, 1, 0),
                    None,
                    Level::Operational,
                    Some(utc(15, 2, 14))
                ),
            ]
        );
        assert_eq!(changes[1].incidents, ["Actions errors"]);
        assert_eq!(changes[0].incidents, Vec::<String>::new());
        assert_eq!(timeline(&records, "Nobody", now), []);
    }

    #[test]
    fn timeline_keeps_the_change_in_force_when_the_window_starts() {
        let now = utc(20, 12, 0);
        let records = [
            record(utc(1, 0, 0), "A", Level::Operational),
            record(utc(12, 0, 0), "A", Level::Outage), // 8 days before now
            record(utc(13, 0, 0), "A", Level::Outage),
            record(utc(14, 0, 0), "A", Level::Degraded),
            record(utc(21, 0, 0), "A", Level::Operational), // after now
        ];
        assert_eq!(
            rows(&timeline(&records, "A", now)),
            [
                (utc(14, 0, 0), Some(Level::Outage), Level::Degraded, None),
                (utc(12, 0, 0), None, Level::Outage, Some(utc(14, 0, 0))),
            ]
        );
        // Nothing changed within the window: the level in force still shows.
        let quiet = [record(utc(1, 0, 0), "A", Level::Operational)];
        assert_eq!(
            rows(&timeline(&quiet, "A", now)),
            [(utc(1, 0, 0), None, Level::Operational, None)]
        );
    }

    #[test]
    fn read_skips_a_cut_short_line_and_reports_a_missing_file() {
        let dir = std::env::temp_dir().join(format!("aretheydown-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("history.jsonl");
        assert!(read(&path).unwrap().is_none());
        let kept = record(t(1, 0), "A", Level::Outage);
        append(&path, std::slice::from_ref(&kept)).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"recorded_at\":")
            .unwrap();
        assert_eq!(read(&path).unwrap(), Some(vec![kept]));
        // A directory exists but cannot be read as a file.
        assert!(read(&dir).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

    fn past(level: Level, start: DateTime<Utc>, end: Option<DateTime<Utc>>) -> PastIncident {
        PastIncident {
            level,
            incident: Incident {
                title: format!("{level} from {start}"),
                started_at: Some(start),
                ended_at: end,
                ..Default::default()
            },
        }
    }

    fn levels_at(records: &[Record]) -> Vec<(DateTime<Utc>, Level)> {
        records
            .iter()
            .map(|r| (r.recorded_at, r.status.level))
            .collect()
    }

    #[test]
    fn only_changes_within_retention_are_given() {
        let now = utc(30, 0, 0);
        let cutoff = now - RETENTION;
        let incidents = [
            past(
                Level::Outage,
                cutoff - TimeDelta::days(10),
                Some(cutoff + TimeDelta::days(1)),
            ),
            // Inside the outage: the set changes after the window starts.
            past(
                Level::Degraded,
                cutoff - TimeDelta::days(5),
                Some(cutoff + TimeDelta::hours(2)),
            ),
        ];
        let records = backfill("A", &incidents, None, now);
        assert_eq!(
            levels_at(&records),
            [
                (cutoff + TimeDelta::hours(2), Level::Outage),
                (cutoff + TimeDelta::days(1), Level::Operational)
            ]
        );
        // The state before the window carried into the first change in it.
        assert_eq!(records[0].status.incidents.len(), 1);
        // An earliest record at or before the window start leaves nothing.
        assert_eq!(backfill("A", &incidents, Some(cutoff), now), []);
    }

    #[test]
    fn overlapping_incidents_take_the_worst_level_and_same_state_folds() {
        let now = utc(20, 0, 0);
        let incidents = [
            past(Level::Degraded, t(1, 0), Some(t(5, 0))),
            past(Level::Outage, t(2, 0), Some(t(3, 0))),
            // Same level as the first, but a different incident: a record.
            past(Level::Degraded, t(5, 0), Some(t(6, 0))),
            // No start: skipped. An end before the start: zero length.
            PastIncident {
                level: Level::Outage,
                incident: Incident::default(),
            },
            past(Level::Outage, t(8, 0), Some(t(7, 0))),
        ];
        let records = backfill("A", &incidents, None, now);
        assert_eq!(
            levels_at(&records),
            [
                (t(1, 0), Level::Degraded),
                (t(2, 0), Level::Outage),
                (t(3, 0), Level::Degraded),
                // The first ends as the third starts: one record.
                (t(5, 0), Level::Degraded),
                (t(6, 0), Level::Operational),
            ]
        );
        assert_eq!(records[1].status.description, "2 active incidents");
    }

    #[test]
    fn backfilled_is_left_out_of_the_json_when_false() {
        let plain = record(t(1, 0), "A", Level::Outage);
        assert!(!line(&plain).contains("backfilled"));
        let mut marked = plain.clone();
        marked.backfilled = true;
        assert!(line(&marked).contains("\"backfilled\":true"));
        let (parsed, skipped) = parse(&(line(&plain) + &line(&marked)));
        assert_eq!(parsed, [plain, marked]);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn at_records_gives_the_records_themselves() {
        let mut all = records();
        all[2].backfilled = true;
        let found = at_records(&all, t(2, 30));
        assert_eq!(found.len(), 2);
        assert!(found[0].backfilled);
        assert!(!found[1].backfilled);
    }
}
