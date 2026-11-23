//! Draw an [`App`] into a ratatui frame. Reads the status model only: no
//! fetching or parsing happens here.

use chrono::{DateTime, Local, TimeDelta, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use super::REFRESH_INTERVAL;
use super::app::{App, DetailViewport, History, STALE_LIMIT, Stale};
use super::link;
use super::wrap::wrap;
use crate::history::{self, Change, TIMELINE_WINDOW};
use crate::model::{
    Incident, IncidentKind, Level, PlatformStatus, RESOLVED_HOURS, age, elapsed, resolved_recently,
    verdict,
};
use crate::render::{self, UPDATE_TEXT_LIMIT, shorten};

/// Width of the level column: the longest label, "Operational".
const LEVEL_WIDTH: usize = 11;
/// Width of the last column: an `HH:MM:SS` clock, or a stale tag such as
/// `stale 10m`. A row is kept stale for at most [`STALE_LIMIT`], and the tag
/// can run a refresh interval past it (`stale 11m`), still two digits.
const TIME_WIDTH: usize = 9;
const HIGHLIGHT_SYMBOL: &str = "> ";
/// Incident lines start "• " (or "> " when highlighted) or "  "; wrapped
/// lines line up after that.
const INCIDENT_INDENT: usize = 2;

/// Draw the whole screen: header, platform list, detail pane, key help, or,
/// while it is open, the timeline in place of the list and detail pane.
/// Incident ages are measured against `now`, the time of this draw.
/// Returns the scrolling pane's viewport, which the app needs for scrolling.
pub fn draw(frame: &mut Frame, app: &App, now: DateTime<Utc>) -> DetailViewport {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(Paragraph::new(header_line(app)), header);
    frame.render_widget(Paragraph::new(footer_line(app)), footer);

    if app.timeline_open() {
        return draw_timeline(frame, app, body, now);
    }
    let [list_area, detail_area] =
        Layout::horizontal([Constraint::Length(list_width(app)), Constraint::Min(20)]).areas(body);
    draw_list(frame, app, list_area, now);
    draw_detail(frame, app, detail_area, now)
}

fn header_line(app: &App) -> Line<'static> {
    let mut spans = vec![Span::from(" Are they down?").bold()];
    // Only platforms with data count; before the first fetch there is no verdict.
    let fetched: Vec<&PlatformStatus> = app.rows().filter_map(|(_, status)| status).collect();
    if let Some(verdict) = verdict(fetched.iter().copied()) {
        let worst = Level::worst(fetched.iter().map(|status| status.level));
        spans.push(
            Span::from(format!("   {verdict}"))
                .fg(level_color(worst))
                .bold(),
        );
    }
    if let Some(at) = app.last_refresh() {
        spans.push(Span::from(format!("   updated {}", clock(at))).dark_gray());
    }
    if app.is_refreshing() {
        spans.push(Span::from("   refreshing…").yellow());
    }
    Line::from(spans)
}

/// The key help, or a notice (e.g. a link that would not open) while one shows.
fn footer_line(app: &App) -> Line<'static> {
    if let Some(notice) = app.notice() {
        return Line::from(format!(" {notice}")).yellow();
    }
    let refresh = REFRESH_INTERVAL.as_secs();
    if app.timeline_open() {
        return Line::from(format!(
            " ↑/↓ j/k PgUp/PgDn J/K scroll  t/Esc back  r refresh  q quit  (auto-refresh every {refresh}s)"
        ))
        .dark_gray();
    }
    Line::from(format!(
        " ↑/↓ j/k select  PgUp/PgDn J/K scroll  Tab/⇧Tab incident  o open  s sort: {}  t timeline  r refresh  q quit  (auto-refresh every {refresh}s)",
        app.sort_order().label(),
    ))
    .dark_gray()
}

fn name_width(app: &App) -> usize {
    app.rows()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
}

/// Just wide enough for the widest row, its highlight marker, and the borders.
fn list_width(app: &App) -> u16 {
    // "● " + name + "  " + level + "  " + probe + " " + "HH:MM:SS" or "stale 10m"
    let row = 2 + name_width(app) + 2 + LEVEL_WIDTH + 2 + 2 + TIME_WIDTH;
    u16::try_from(row + HIGHLIGHT_SYMBOL.len() + 2).unwrap_or(u16::MAX)
}

fn draw_list(frame: &mut Frame, app: &App, area: Rect, now: DateTime<Utc>) {
    let width = name_width(app);
    let items: Vec<ListItem> = app
        .rows()
        .enumerate()
        .map(|(row, (name, status))| {
            ListItem::new(list_row(name, status, app.row_stale(row), width, now))
        })
        .collect();
    let list = List::new(items)
        .block(Block::bordered().title(" Platforms "))
        .highlight_symbol(HIGHLIGHT_SYMBOL)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    let mut state = ListState::default().with_selected(Some(app.selected()));
    frame.render_stateful_widget(list, area, &mut state);
}

/// A platform's row: its level, then one column for its probe (`✓`
/// responding, `✗` not, blank without one), then the fetch time. A stale
/// one shows how old its data is in place of the clock.
fn list_row(
    name: &str,
    status: Option<&PlatformStatus>,
    stale: Option<&Stale>,
    name_width: usize,
    now: DateTime<Utc>,
) -> Line<'static> {
    let Some(status) = status else {
        return Line::from(vec![
            Span::from("○ ").dark_gray(),
            Span::from(format!("{name:name_width$}  ")),
            Span::from("loading…").dark_gray(),
        ]);
    };
    let color = level_color(status.level);
    Line::from(vec![
        Span::from("● ").fg(color),
        Span::from(format!("{name:name_width$}  ")),
        Span::from(format!("{:LEVEL_WIDTH$}  ", status.level.label())).fg(color),
        probe_mark(status),
        Span::from(" "),
        Span::from(if stale.is_some() {
            stale_tag(status.fetched_at, now)
        } else {
            clock(status.fetched_at)
        })
        .dark_gray(),
    ])
}

/// The probe column: a green `✓`, a red `✗`, or a blank.
fn probe_mark(status: &PlatformStatus) -> Span<'static> {
    match &status.probe {
        Some(probe) if probe.ok => Span::from("✓").green(),
        Some(_) => Span::from("✗").red(),
        None => Span::from(" "),
    }
}

/// `stale 4m`: how long ago the shown data was fetched.
fn stale_tag(fetched_at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    format!("stale {}", elapsed(fetched_at, now))
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect, now: DateTime<Utc>) -> DetailViewport {
    let (title, detail) = match app.selected_row() {
        None => (
            String::new(),
            Detail::plain(vec![Line::from("No platforms configured.")]),
        ),
        Some((name, None)) => (
            name.to_owned(),
            Detail::plain(vec![Line::from("Loading…").dark_gray()]),
        ),
        Some((name, Some(status))) => {
            let mut detail = detail_lines(
                status,
                app.row_stale(app.selected()),
                app.highlighted_incident(),
                now,
            );
            push_history(&mut detail, app.history(), name, now);
            (name.to_owned(), detail)
        }
    };
    let inner_width = usize::from(area.width.saturating_sub(2));
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut incident_lines = Vec::new();
    let mut link_rows: Vec<LinkRow> = Vec::new();
    for (index, (line, indent)) in detail.lines.iter().enumerate() {
        if detail.incident_starts.contains(&index) {
            incident_lines.push(u16::try_from(lines.len()).unwrap_or(u16::MAX));
        }
        let wrapped = wrap(line, inner_width, *indent);
        if let Some(link) = detail.links.iter().find(|link| link.index == index) {
            for (offset, row) in wrapped.iter().enumerate() {
                link_rows.push(LinkRow {
                    line: lines.len() + offset,
                    start: leading_spaces(row),
                    width: row.width(),
                    url: &link.url,
                });
            }
        }
        lines.extend(wrapped);
    }
    let viewport = DetailViewport {
        content_lines: u16::try_from(lines.len()).unwrap_or(u16::MAX),
        height: area.height.saturating_sub(2),
        incident_lines,
    };
    // The app stores this scroll after the draw, via `set_viewport`.
    let scroll = app.scroll_for(&viewport);

    let block = with_more_hints(
        Block::bordered().title(format!(" {title} ")),
        scroll,
        &viewport,
    );
    let inner = block.inner(area);
    frame.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), area);
    link_visible_rows(frame, inner, scroll, &link_rows);
    viewport
}

/// Draw the timeline of every platform's level changes over `area`, scrolled
/// as the app says, and return its viewport.
fn draw_timeline(frame: &mut Frame, app: &App, area: Rect, now: DateTime<Utc>) -> DetailViewport {
    let names: Vec<&str> = app.names().collect();
    let (lines, count) = timeline_lines(app.history(), &names, now);
    let inner_width = usize::from(area.width.saturating_sub(2));
    let lines: Vec<Line<'static>> = lines
        .iter()
        .flat_map(|(line, indent)| wrap(line, inner_width, *indent))
        .collect();
    let viewport = DetailViewport {
        content_lines: u16::try_from(lines.len()).unwrap_or(u16::MAX),
        height: area.height.saturating_sub(2),
        incident_lines: Vec::new(),
    };
    let scroll = app.timeline_scroll_for(&viewport);
    let mut block = Block::bordered().title(format!(
        " Timeline (last {} days) ",
        TIMELINE_WINDOW.num_days()
    ));
    if let Some(count) = count {
        let noun = if count == 1 { "change" } else { "changes" };
        block = block.title_top(Line::from(format!(" {count} {noun} ")).right_aligned());
    }
    let block = with_more_hints(block, scroll, &viewport);
    frame.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), area);
    viewport
}

/// `block` with ` ↑ more ` on its top border when `scroll` hides lines above,
/// and ` ↓ more ` on its bottom border when lines below are out of view.
fn with_more_hints<'a>(mut block: Block<'a>, scroll: u16, viewport: &DetailViewport) -> Block<'a> {
    if scroll > 0 {
        block = block.title_top(Line::from(" ↑ more ").right_aligned().dark_gray());
    }
    if scroll < viewport.max_scroll() {
        block = block.title_bottom(Line::from(" ↓ more ").right_aligned().dark_gray());
    }
    block
}

/// The timeline's lines before wrapping, each with its hanging indent, and
/// how many changes they list (`None` while there is no history to count):
/// the changes of every platform in `names` (config order) from
/// [`history::timeline`], merged newest first (ties in config order) under a
/// local-date heading per day. A missing, unreadable, or empty history file
/// shows the detail pane's message instead.
fn timeline_lines(
    history: Option<&History>,
    names: &[&str],
    now: DateTime<Utc>,
) -> (Vec<(Line<'static>, usize)>, Option<usize>) {
    let records = match history {
        None => {
            return (
                vec![(Line::from("Reading the history file…").dark_gray(), 0)],
                None,
            );
        }
        Some(History::Records(records)) => records,
        Some(other) => return (no_history_lines(other), None),
    };
    let mut changes: Vec<(&str, Change)> = names
        .iter()
        .flat_map(|&name| {
            history::timeline(records, name, now)
                .into_iter()
                .map(move |change| (name, change))
        })
        .collect();
    if changes.is_empty() {
        return (no_history_lines(&History::Missing), Some(0));
    }
    // Stable, so changes at the same time keep platform order.
    changes.sort_by_key(|(_, change)| std::cmp::Reverse(change.at));
    let width = changes
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines = Vec::new();
    let mut day = None;
    for (name, change) in &changes {
        let local = change.at.with_timezone(&Local);
        if day != Some(local.date_naive()) {
            if day.is_some() {
                lines.push((Line::default(), 0));
            }
            day = Some(local.date_naive());
            lines.push((Line::from(local.format("%a %-d %b").to_string()).bold(), 0));
        }
        let lead = format!("  {}  {name:width$}  ", local.format("%H:%M"));
        lines.extend(change_entry(lead, change));
    }
    (lines, Some(changes.len()))
}

/// What the history views say when there is no history to show: how to
/// record it, and for an unreadable file why it could not be read.
fn no_history_lines(history: &History) -> Vec<(Line<'static>, usize)> {
    let mut lines = vec![(
        Line::from(
            "No history: run aretheydown --serve (or brew services start aretheydown) to record it.",
        )
        .dark_gray(),
        0,
    )];
    if let History::Unreadable(error) = history {
        lines.push((Line::from(error.clone()).yellow(), 0));
    }
    lines
}

/// One wrapped row of an incident URL.
struct LinkRow<'a> {
    /// The row's index in the wrapped content.
    line: usize,
    /// The column where the URL starts, after the row's indent.
    start: usize,
    /// The row's width in columns, indent included.
    width: usize,
    url: &'a str,
}

/// Turn the on-screen part of each URL row into an OSC 8 hyperlink. A URL row
/// holds only the URL after its indent (see [`url_line`]).
fn link_visible_rows(frame: &mut Frame, inner: Rect, scroll: u16, link_rows: &[LinkRow]) {
    let buffer = frame.buffer_mut();
    for &LinkRow {
        line,
        start,
        width,
        url,
    } in link_rows
    {
        let Some(row) = line.checked_sub(usize::from(scroll)) else {
            continue;
        };
        let Ok(row) = u16::try_from(row) else {
            continue;
        };
        if row >= inner.height {
            continue;
        }
        let start = inner
            .x
            .saturating_add(u16::try_from(start).unwrap_or(u16::MAX));
        let end = inner
            .x
            .saturating_add(u16::try_from(width).unwrap_or(u16::MAX))
            .min(inner.right());
        link::link_cells(buffer, inner.y + row, start, end, url);
    }
}

/// How many columns of spaces `row` starts with.
fn leading_spaces(row: &Line) -> usize {
    row.spans
        .iter()
        .flat_map(|span| span.content.chars())
        .take_while(|&c| c == ' ')
        .count()
}

/// The detail pane's content before wrapping.
struct Detail {
    /// Each line with the indent its wrapped continuation gets.
    lines: Vec<(Line<'static>, usize)>,
    /// The index in `lines` where each incident starts, in incident order.
    incident_starts: Vec<usize>,
    /// Each incident URL line, for linking once wrapped.
    links: Vec<DetailLink>,
}

/// An incident URL line in [`Detail::lines`].
struct DetailLink {
    /// The line's index in `lines`.
    index: usize,
    url: String,
}

impl Detail {
    /// Lines with no incidents and no hanging indent.
    fn plain(lines: Vec<Line<'static>>) -> Self {
        Self {
            lines: lines.into_iter().map(|line| (line, 0)).collect(),
            incident_starts: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Add an incident's URL line, recorded for linking; nothing for `None`.
    fn push_url(&mut self, url: Option<&str>) {
        let Some(url) = url else {
            return;
        };
        self.links.push(DetailLink {
            index: self.lines.len(),
            url: url.to_owned(),
        });
        self.lines.push((url_line(url), INCIDENT_INDENT));
    }
}

/// The detail pane's lines for `status`, with incident `highlighted` drawn
/// distinctly. With `stale` set, `status` is older good data and the failed
/// refresh shows above it.
fn detail_lines(
    status: &PlatformStatus,
    stale: Option<&Stale>,
    highlighted: Option<usize>,
    now: DateTime<Utc>,
) -> Detail {
    let color = level_color(status.level);
    let mut lines = vec![
        Line::from(vec![
            Span::from(status.level.label()).fg(color).bold(),
            Span::from(format!("  {}", status.description)),
        ]),
        Line::from(format!("Updated {}", clock(status.fetched_at))).dark_gray(),
    ];
    if let Some(line) = render::probe_line(status) {
        let mark = probe_mark(status);
        lines.push(Line::from(vec![
            Span::from("Probe: "),
            mark,
            Span::from(format!(" {line}")),
        ]));
    }
    if let Some(error) = &status.error {
        lines.push(Line::from(format!("Error: {error}")).red());
    }
    if let Some(stale) = stale {
        lines.push(
            Line::from(format!(
                "Refresh failed at {}: {}",
                clock(stale.failed_at),
                stale.error
            ))
            .yellow(),
        );
        lines.push(
            Line::from(format!(
                "Showing data from {} (shown as Unknown after {}m).",
                age(status.fetched_at, now),
                STALE_LIMIT.num_minutes()
            ))
            .yellow(),
        );
    }
    if status.incidents.is_empty() {
        // With an error there is no incident list to speak of.
        if status.error.is_none() {
            lines.push(Line::default());
            lines.push(Line::from("No active incidents."));
        }
        let mut detail = Detail::plain(lines);
        push_resolved(&mut detail, status, now);
        return detail;
    }
    lines.push(Line::default());
    lines.push(Line::from(format!("Active incidents ({})", status.incidents.len())).bold());
    let mut detail = Detail::plain(lines);
    for (index, incident) in status.incidents.iter().enumerate() {
        detail.lines.push((Line::default(), 0));
        detail.incident_starts.push(detail.lines.len());
        let is_highlighted = highlighted == Some(index);
        detail.lines.extend(
            incident_lines(incident, is_highlighted, now)
                .into_iter()
                .map(|line| (line, INCIDENT_INDENT)),
        );
        detail.push_url(incident.url.as_deref());
    }
    push_resolved(&mut detail, status, now);
    detail
}

/// Append the `Resolved in the last 24h (N)` section: each incident's title,
/// components, when it was resolved and how long it lasted, and its link.
/// Nothing when no incident resolved. These are not in `incident_starts`, so
/// Tab and `o` skip them.
fn push_resolved(detail: &mut Detail, status: &PlatformStatus, now: DateTime<Utc>) {
    // Kept stale data was filtered at its own fetch; drop what has since aged
    // out of the window so the heading stays true.
    let resolved: Vec<&Incident> = status
        .resolved
        .iter()
        .filter(|i| i.ended_at.is_none_or(|at| resolved_recently(at, now)))
        .collect();
    if resolved.is_empty() {
        return;
    }
    detail.lines.push((Line::default(), 0));
    detail.lines.push((
        Line::from(format!(
            "Resolved in the last {RESOLVED_HOURS}h ({})",
            resolved.len()
        ))
        .bold(),
        0,
    ));
    for incident in resolved {
        detail.lines.push((Line::default(), 0));
        let mut lines = vec![Line::from(format!("• {}", incident.title))];
        if !incident.components.is_empty() {
            lines.push(Line::from(format!(
                "  Affects: {}",
                incident.components.join(", ")
            )));
        }
        if let Some(when) = resolved_when(incident, now) {
            lines.push(Line::from(format!("  {when}")).dark_gray());
        }
        detail
            .lines
            .extend(lines.into_iter().map(|line| (line, INCIDENT_INDENT)));
        detail.push_url(incident.url.as_deref());
    }
}

/// Append the `History (last 7 days)` section for platform `name`: its
/// level changes from the history file, newest first, each with the titles
/// of the incidents active then. Nothing before the first read of the file;
/// a missing or unreadable file shows how to record history instead, and an
/// unreadable one also why it could not be read.
fn push_history(detail: &mut Detail, history: Option<&History>, name: &str, now: DateTime<Utc>) {
    let records = match history {
        None => return,
        Some(History::Records(records)) => records,
        Some(other) => {
            detail.lines.push((Line::default(), 0));
            detail.lines.extend(no_history_lines(other));
            return;
        }
    };
    detail.lines.push((Line::default(), 0));
    detail.lines.push((
        Line::from(format!(
            "History (last {} days)",
            TIMELINE_WINDOW.num_days()
        ))
        .bold(),
        0,
    ));
    let changes = history::timeline(records, name, now);
    if changes.is_empty() {
        detail.lines.push((
            Line::from("  No history recorded for this platform yet.").dark_gray(),
            INCIDENT_INDENT,
        ));
    }
    for change in &changes {
        detail.lines.extend(change_lines(change, now));
    }
}

/// One history entry: `  Sat 02:14  Operational → Outage, lasted 39m`, then
/// its incident titles on a line of their own, indented under the levels.
fn change_lines(change: &Change, now: DateTime<Utc>) -> Vec<(Line<'static>, usize)> {
    change_entry(format!("  {}  ", change_time(change.at, now)), change)
}

/// A change after `lead` (its time, and in the timeline its platform):
/// `from → to, lasted 39m`, then the incident titles on a line of their own,
/// indented to line up after `lead`.
fn change_entry(lead: String, change: &Change) -> Vec<(Line<'static>, usize)> {
    let indent = lead.chars().count();
    let mut spans = vec![Span::from(lead)];
    if let Some(from) = change.from {
        spans.push(Span::from(from.label()).fg(level_color(from)));
        spans.push(Span::from(" → "));
    }
    spans.push(
        Span::from(change.to.label())
            .fg(level_color(change.to))
            .bold(),
    );
    spans.push(Span::from(match change.until {
        Some(until) => format!(", lasted {}", elapsed(change.at, until)),
        None => ", ongoing".to_owned(),
    }));
    let mut lines = vec![(Line::from(spans), indent)];
    if !change.incidents.is_empty() {
        let titles = shorten(&change.incidents.join("; "), UPDATE_TEXT_LIMIT);
        lines.push((
            Line::from(format!("{}{titles}", " ".repeat(indent))).dark_gray(),
            indent,
        ));
    }
    lines
}

/// When a history entry happened, in local time: `Sat 02:14`, or with the
/// date (`Sat Nov 8 02:14`) once it is a day short of the window or older,
/// where a weekday alone could mean two days.
fn change_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let format = if now - at < TIMELINE_WINDOW - TimeDelta::days(1) {
        "%a %H:%M"
    } else {
        "%a %b %-d %H:%M"
    };
    at.with_timezone(&Local).format(format).to_string()
}

/// `Resolved 2h 14m ago, lasted 1h 5m`; without a start time just the first
/// part, and `None` without an end time.
fn resolved_when(incident: &Incident, now: DateTime<Utc>) -> Option<String> {
    let ended_at = incident.ended_at?;
    let resolved = format!("Resolved {}", age(ended_at, now));
    Some(match incident.started_at {
        Some(started_at) => format!("{resolved}, lasted {}", elapsed(started_at, ended_at)),
        None => resolved,
    })
}

/// An incident's lines, except its URL (see [`url_line`]). The highlighted
/// one starts with `> ` (like the selected platform) and shows its title
/// reversed.
fn incident_lines(
    incident: &Incident,
    highlighted: bool,
    now: DateTime<Utc>,
) -> Vec<Line<'static>> {
    let mut title = vec![Span::from(if highlighted { "> " } else { "• " })];
    if incident.kind == IncidentKind::Maintenance {
        title.push(Span::from("[maintenance] ").cyan());
    }
    let mut name = Span::from(incident.title.clone()).bold();
    if highlighted {
        name = name.reversed();
    }
    title.push(name);
    let mut lines = vec![Line::from(title)];
    if !incident.components.is_empty() {
        lines.push(Line::from(format!(
            "  Affects: {}",
            incident.components.join(", ")
        )));
    }
    if let Some(started_at) = incident.started_at {
        lines.push(Line::from(format!(
            "  Started: {} ({})",
            timestamp(started_at),
            age(started_at, now)
        )));
    }
    lines.extend(update_lines(incident, now));
    lines
}

/// The stage and latest update: a heading line (`Latest update (monitoring,
/// 3h ago):`, or `Stage: monitoring` with no update), then the update's text
/// as written, one line per line of text, each indented like the other
/// incident lines so `wrap` hangs it there too.
fn update_lines(incident: &Incident, now: DateTime<Utc>) -> Vec<Line<'static>> {
    let Some(update) = &incident.latest_update else {
        return incident
            .stage
            .iter()
            .map(|stage| Line::from(format!("  Stage: {stage}")).dark_gray())
            .collect();
    };
    let about: Vec<String> = incident
        .stage
        .iter()
        .cloned()
        .chain(update.posted_at.map(|posted_at| age(posted_at, now)))
        .collect();
    let heading = if about.is_empty() {
        "  Latest update:".to_owned()
    } else {
        format!("  Latest update ({}):", about.join(", "))
    };
    let mut lines = vec![Line::from(heading).dark_gray()];
    lines.extend(
        update
            .text
            .lines()
            .map(|text| Line::from(format!("  {}", text.trim_end()))),
    );
    lines
}

/// An incident's URL, underlined, after `INCIDENT_INDENT` spaces. Nothing
/// else goes on this line: `link_visible_rows` links every cell after the
/// indent.
fn url_line(url: &str) -> Line<'static> {
    Line::from(vec![
        Span::from(" ".repeat(INCIDENT_INDENT)),
        Span::from(url.to_owned()).underlined(),
    ])
}

fn level_color(level: Level) -> Color {
    match level {
        Level::Operational => Color::Green,
        Level::Degraded => Color::Yellow,
        Level::Outage => Color::Red,
        Level::Unknown => Color::Magenta,
    }
}

/// Local wall-clock time, e.g. "14:03:22".
fn clock(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local).format("%H:%M:%S").to_string()
}

/// Local date and time with the zone offset, e.g. "2026-11-13 07:31 -08:00".
fn timestamp(at: DateTime<Utc>) -> String {
    at.with_timezone(&Local)
        .format("%Y-%m-%d %H:%M %:z")
        .to_string()
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    use super::*;
    use crate::model::{Probe, Update};
    use crate::tui::app::Action;

    fn at(hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 13, hour, minute, 0).unwrap()
    }

    fn status(name: &str, level: Level, description: &str) -> PlatformStatus {
        PlatformStatus {
            name: name.to_owned(),
            level,
            description: description.to_owned(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: at(19, 10),
            error: None,
            probe: None,
        }
    }

    fn loaded_app() -> App {
        let mut cloudflare = status("Cloudflare", Level::Degraded, "Partially Degraded Service");
        cloudflare.incidents = vec![
            Incident {
                kind: IncidentKind::Incident,
                title: "Workers API errors".to_owned(),
                components: vec!["Workers".to_owned(), "Queues".to_owned()],
                started_at: Some(at(15, 31)),
                url: Some("https://www.cloudflarestatus.com/incidents/e7k".to_owned()),
                ..Default::default()
            },
            Incident {
                kind: IncidentKind::Maintenance,
                title: "HKG datacenter".to_owned(),
                components: Vec::new(),
                started_at: None,
                url: None,
                ..Default::default()
            },
        ];
        let mut slow = PlatformStatus::unknown("Slow", "timed out after 10s", at(19, 10));
        slow.description = "Status unavailable".to_owned();
        let mut app = App::new(vec!["GitHub".into(), "Cloudflare".into(), "Slow".into()]);
        app.start_refresh();
        app.finish_refresh(
            vec![
                status("GitHub", Level::Operational, "All Systems Operational"),
                cloudflare,
                slow,
            ],
            at(19, 10),
        );
        app
    }

    fn render(app: &App) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, app, at(19, 10));
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// Draw like the event loop does: the app learns the pane's viewport.
    fn render_sized(app: &mut App, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let mut viewport = DetailViewport::default();
        terminal
            .draw(|frame| viewport = draw(frame, app, at(19, 10)))
            .unwrap();
        app.set_viewport(viewport);
        terminal.backend().buffer().clone()
    }

    /// A platform with 12 incidents (52 lines of detail), then an operational one.
    fn busy_app() -> App {
        let mut busy = status("Busy", Level::Outage, "Major Outage");
        busy.incidents = (1..=12)
            .map(|n| Incident {
                kind: IncidentKind::Incident,
                title: format!("Incident {n:02}"),
                components: vec![format!("Service {n:02}")],
                started_at: None,
                url: Some(format!("https://status.example.com/{n:02}")),
                ..Default::default()
            })
            .collect();
        let mut app = App::new(vec!["Busy".into(), "Calm".into()]);
        app.start_refresh();
        app.finish_refresh(
            vec![busy, status("Calm", Level::Operational, "All good")],
            at(19, 10),
        );
        app
    }

    /// The detail pane's rows (inside its borders), right-trimmed.
    fn pane_rows(buffer: &Buffer) -> Vec<String> {
        let rows = rows(buffer);
        let top: Vec<char> = rows[1].chars().collect();
        let left = top.iter().rposition(|&c| c == '┌').unwrap();
        rows[2..rows.len() - 2]
            .iter()
            .map(|row| {
                let inner: String = row.chars().skip(left + 1).collect();
                inner.trim_end_matches(['│', ' ']).to_owned()
            })
            .collect()
    }

    /// The buffer as plain text, one string per row.
    fn rows(buffer: &Buffer) -> Vec<String> {
        let width = usize::from(buffer.area.width);
        buffer
            .content
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|cell| link::visible(cell.symbol()))
                    .collect()
            })
            .collect()
    }

    fn screen(buffer: &Buffer) -> String {
        rows(buffer).join("\n")
    }

    #[test]
    fn list_shows_every_platform_with_level_and_update_time() {
        let screen = screen(&render(&loaded_app()));
        let updated = clock(at(19, 10));
        assert!(screen.contains(&format!("● GitHub      Operational    {updated}")));
        assert!(screen.contains(&format!("● Cloudflare  Degraded       {updated}")));
        assert!(
            screen.contains(&format!("> ● Slow        Unknown        {updated}")),
            "the first results select the top row: {screen}"
        );
        assert!(screen.contains(&format!("updated {updated}")));
    }

    #[test]
    fn level_colors_the_status_dot() {
        let buffer = render(&loaded_app());
        let rows = rows(&buffer);
        let dot = |row_text: &str| {
            let y = rows.iter().position(|r| r.contains(row_text)).unwrap();
            let x = rows[y].chars().position(|c| c == '●').unwrap();
            buffer[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].fg
        };
        assert_eq!(dot("● GitHub "), Color::Green);
        assert_eq!(dot("● Cloudflare "), Color::Yellow);
        assert_eq!(dot("● Slow "), Color::Magenta);
    }

    #[test]
    fn detail_pane_follows_the_selection() {
        // Worst first: Slow (selected), Cloudflare, GitHub.
        let mut app = loaded_app();
        app.apply(Action::Down);
        app.apply(Action::Down);
        let screen_text = screen(&render(&app));
        assert!(screen_text.contains("Operational  All Systems Operational"));
        assert!(screen_text.contains("No active incidents."));

        app.apply(Action::Up);
        let screen_text = screen(&render(&app));
        assert!(screen_text.contains("> ● Cloudflare"));
        assert!(screen_text.contains("┌ Cloudflare ─"), "{screen_text}");
        assert!(screen_text.contains("Degraded  Partially Degraded Service"));
        assert!(screen_text.contains("Active incidents (2)"));
        assert!(
            screen_text.contains("> Workers API errors"),
            "the first incident is highlighted"
        );
        assert!(screen_text.contains("Affects: Workers, Queues"));
        // Drawn at 19:10, so the 15:31 start is 3h 39m old.
        assert!(screen_text.contains(&format!("Started: {} (3h 39m ago)", timestamp(at(15, 31)))));
        assert!(screen_text.contains("https://www.cloudflarestatus.com/incidents/e7k"));
        assert!(screen_text.contains("• [maintenance] HKG datacenter"));
    }

    fn probe(ok: bool, description: &str) -> Probe {
        Probe {
            url: "https://api.example.com/v1/models".to_owned(),
            ok,
            code: None,
            elapsed_ms: None,
            description: description.to_owned(),
        }
    }

    /// GitHub responding, Cloudflare not, Slow with no probe.
    fn probed_app() -> App {
        let mut github = status("GitHub", Level::Operational, "All Systems Operational");
        github.probe = Some(probe(true, "responding (HTTP 401 in 220 ms)"));
        let mut cloudflare = status("Cloudflare", Level::Operational, "All Systems Operational");
        cloudflare.probe = Some(probe(false, "no answer (could not connect)"));
        let mut app = App::new(vec!["GitHub".into(), "Cloudflare".into(), "Slow".into()]);
        app.start_refresh();
        app.finish_refresh(
            vec![
                github,
                cloudflare,
                status("Slow", Level::Operational, "All Systems Operational"),
            ],
            at(19, 10),
        );
        app
    }

    #[test]
    fn the_probe_column_marks_responding_failed_and_none() {
        let buffer = render(&probed_app());
        let screen = screen(&buffer);
        let updated = clock(at(19, 10));
        assert!(
            screen.contains(&format!("● GitHub      Operational  ✓ {updated}")),
            "{screen}"
        );
        assert!(
            screen.contains(&format!("● Cloudflare  Operational  ✗ {updated}")),
            "{screen}"
        );
        assert!(
            screen.contains(&format!("● Slow        Operational    {updated}")),
            "{screen}"
        );
        assert!(
            screen.contains("All 3 operational"),
            "a probe never changes the verdict"
        );
        let rows = rows(&buffer);
        let mark = |name: &str, symbol: &str| {
            let row = format!("● {name}");
            let y = rows.iter().position(|r| r.contains(&row)).unwrap();
            let x = rows[y]
                .chars()
                .position(|c| c.to_string() == symbol)
                .unwrap();
            buffer[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].fg
        };
        assert_eq!(mark("GitHub", "✓"), Color::Green);
        assert_eq!(mark("Cloudflare", "✗"), Color::Red);
    }

    #[test]
    fn the_detail_pane_shows_the_probe_result_and_url() {
        let mut app = probed_app();
        let pane = pane_rows(&render_sized(&mut app, 140, 24));
        assert_eq!(
            pane[2],
            "Probe: ✓ responding (HTTP 401 in 220 ms), GET https://api.example.com/v1/models"
        );
        app.apply(Action::Down);
        let pane = pane_rows(&render_sized(&mut app, 140, 24));
        assert_eq!(
            pane[2],
            "Probe: ✗ no answer (could not connect), GET https://api.example.com/v1/models"
        );
        app.apply(Action::Down);
        let pane = pane_rows(&render_sized(&mut app, 140, 24));
        assert!(pane.iter().all(|row| !row.contains("Probe")), "{pane:?}");
    }

    /// `loaded_app` after a refresh at 19:14 in which Cloudflare fails. Its
    /// 19:10 data is 4 minutes old, so it is kept and marked stale.
    fn stale_app() -> App {
        let mut app = loaded_app();
        let failed = at(19, 14);
        let mut slow = PlatformStatus::unknown("Slow", "timed out after 10s", failed);
        slow.description = "Status unavailable".to_owned();
        let mut github = status("GitHub", Level::Operational, "All Systems Operational");
        github.fetched_at = failed;
        app.start_refresh();
        app.finish_refresh(
            vec![
                github,
                PlatformStatus::unknown("Cloudflare", "HTTP 503 Service Unavailable", failed),
                slow,
            ],
            failed,
        );
        app
    }

    fn render_at(app: &App, now: DateTime<Utc>) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(110, 24)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, app, now);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn a_stale_row_keeps_its_level_and_shows_the_data_age() {
        let buffer = render_at(&stale_app(), at(19, 14));
        let screen = screen(&buffer);
        assert!(
            screen.contains("● Cloudflare  Degraded       stale 4m"),
            "{screen}"
        );
        let updated = clock(at(19, 14));
        assert!(screen.contains(&format!("● GitHub      Operational    {updated}")));
        assert!(
            screen.contains("2 of 3 need attention: Slow, Cloudflare"),
            "the verdict counts the kept Degraded: {screen}"
        );
        let rows = rows(&buffer);
        let y = rows.iter().position(|r| r.contains("stale 4m")).unwrap();
        let x = rows[y].find("stale 4m").unwrap();
        let x = rows[y][..x].chars().count();
        let cell = &buffer[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())];
        assert_eq!(cell.fg, Color::DarkGray, "the tag is dim");
    }

    #[test]
    fn the_stale_tag_counts_data_from_the_future_as_under_a_minute() {
        assert_eq!(stale_tag(at(19, 10), at(19, 14)), "stale 4m");
        assert_eq!(stale_tag(at(19, 10), at(19, 21)), "stale 11m");
        assert_eq!(stale_tag(at(19, 14), at(19, 10)), "stale <1m");
    }

    #[test]
    fn a_stale_platform_shows_the_error_and_age_above_the_kept_incidents() {
        // Worst first: Slow (Unknown), Cloudflare (Degraded), GitHub.
        let mut app = stale_app();
        app.apply(Action::Down);
        let screen_text = screen(&render_at(&app, at(19, 14)));
        assert!(screen_text.contains("┌ Cloudflare ─"), "{screen_text}");
        assert!(screen_text.contains("Degraded  Partially Degraded Service"));
        let pane = pane_rows(&render_at(&app, at(19, 14)));
        let failed = pane
            .iter()
            .position(|row| {
                row.contains(&format!(
                    "Refresh failed at {}: HTTP 503 Service Unavailable",
                    clock(at(19, 14))
                ))
            })
            .unwrap_or_else(|| panic!("{pane:#?}"));
        let showing = pane
            .iter()
            .position(|row| row.contains("Showing data from 4m ago"))
            .unwrap_or_else(|| panic!("{pane:#?}"));
        let incidents = pane
            .iter()
            .position(|row| row.contains("Active incidents (2)"))
            .unwrap();
        assert!(failed < showing && showing < incidents, "{pane:#?}");
        assert!(screen_text.contains("> Workers API errors"));
    }

    #[test]
    fn unknown_platform_shows_its_error() {
        let screen_text = screen(&render(&loaded_app()));
        assert!(screen_text.contains("Unknown  Status unavailable"));
        assert!(screen_text.contains("Error: timed out after 10s"));
        assert!(!screen_text.contains("No active incidents."));
    }

    #[test]
    fn before_the_first_fetch_rows_show_loading() {
        let mut app = App::new(vec!["GitHub".into(), "Cloudflare".into()]);
        app.start_refresh();
        let screen_text = screen(&render(&app));
        assert!(screen_text.contains("○ GitHub      loading…"));
        assert!(screen_text.contains("○ Cloudflare  loading…"));
        assert!(screen_text.contains("Loading…"));
        assert!(screen_text.contains("refreshing…"));
        assert!(!screen_text.contains("updated "));
    }

    #[test]
    fn footer_lists_the_keys_and_the_current_order() {
        let mut app = loaded_app();
        let screen_text = screen(&render_sized(&mut app, 140, 24));
        assert!(
            screen_text.contains(
                "↑/↓ j/k select  PgUp/PgDn J/K scroll  Tab/⇧Tab incident  o open  s sort: worst first  t timeline  r refresh  q quit"
            ),
            "{screen_text}"
        );
        app.apply(Action::ToggleSort);
        assert!(screen(&render_sized(&mut app, 140, 24)).contains("s sort: config order"));
    }

    /// The header row, right-trimmed.
    fn header(app: &App) -> String {
        rows(&render(app))[0].trim_end().to_owned()
    }

    #[test]
    fn header_says_which_platforms_need_attention_counting_unknown() {
        let app = loaded_app();
        let updated = clock(at(19, 10));
        assert_eq!(
            header(&app),
            format!(
                " Are they down?   2 of 3 need attention: Slow, Cloudflare   updated {updated}"
            )
        );
        let buffer = render(&app);
        let x = rows(&buffer)[0].find("2 of 3").unwrap();
        assert_eq!(
            buffer[(u16::try_from(x).unwrap(), 0)].fg,
            Color::Magenta,
            "colored by the worst level (Unknown)"
        );
    }

    #[test]
    fn header_says_all_operational() {
        let mut app = App::new(vec!["GitHub".into(), "AWS".into()]);
        app.start_refresh();
        app.finish_refresh(
            vec![
                status("GitHub", Level::Operational, "All Systems Operational"),
                status("AWS", Level::Operational, "No recent issues"),
            ],
            at(19, 10),
        );
        let text = header(&app);
        assert!(
            text.contains("Are they down?   All 2 operational   updated"),
            "{text}"
        );
    }

    #[test]
    fn header_has_no_verdict_before_the_first_fetch() {
        let mut app = App::new(vec!["GitHub".into()]);
        app.start_refresh();
        assert_eq!(header(&app), " Are they down?   refreshing…");
    }

    #[test]
    fn list_shows_trouble_first_and_s_restores_config_order() {
        let order = |app: &App| -> Vec<String> {
            rows(&render(app))
                .iter()
                .filter_map(|row| {
                    let after = row.split("● ").nth(1)?;
                    Some(after.split_whitespace().next()?.to_owned())
                })
                .collect()
        };
        let mut app = loaded_app();
        assert_eq!(order(&app), ["Slow", "Cloudflare", "GitHub"]);
        app.apply(Action::ToggleSort);
        assert_eq!(order(&app), ["GitHub", "Cloudflare", "Slow"]);
    }

    #[test]
    fn long_detail_scrolls_by_page_with_hints() {
        let mut app = busy_app();
        // 110x16: the pane holds 12 of the 52 lines; a page is 11.
        let buffer = render_sized(&mut app, 110, 16);
        let text = screen(&buffer);
        assert!(text.contains("↓ more"), "{text}");
        assert!(!text.contains("↑ more"), "already at the top");
        assert_eq!(pane_rows(&buffer)[0], "Outage  Major Outage");

        app.apply(Action::PageDown);
        let buffer = render_sized(&mut app, 110, 16);
        let text = screen(&buffer);
        assert!(text.contains("↑ more") && text.contains("↓ more"), "{text}");
        let pane = pane_rows(&buffer);
        // Line 12 (index 11) of the content is now at the top.
        assert_eq!(pane[0], "  https://status.example.com/02", "{pane:#?}");
        assert!(!text.contains("Major Outage"));
        assert_eq!(app.selected(), 0, "paging does not move the selection");
    }

    #[test]
    fn scrolling_stops_at_the_last_line() {
        let mut app = busy_app();
        render_sized(&mut app, 110, 16);
        for _ in 0..20 {
            app.apply(Action::PageDown);
        }
        let buffer = render_sized(&mut app, 110, 16);
        let text = screen(&buffer);
        let pane = pane_rows(&buffer);
        assert_eq!(app.detail_scroll(), 40, "52 lines - 12 rows");
        assert_eq!(
            pane.last().unwrap(),
            "  https://status.example.com/12",
            "the last line fills the bottom row: {pane:#?}"
        );
        assert!(
            text.contains("↑ more") && !text.contains("↓ more"),
            "{text}"
        );

        app.apply(Action::PageUp);
        render_sized(&mut app, 110, 16);
        assert_eq!(app.detail_scroll(), 29, "one press scrolls straight back");
    }

    #[test]
    fn changing_the_selection_shows_the_top_again() {
        let mut app = busy_app();
        render_sized(&mut app, 110, 16);
        app.apply(Action::PageDown);
        app.apply(Action::PageDown);
        render_sized(&mut app, 110, 16);
        app.apply(Action::Down);
        let text = screen(&render_sized(&mut app, 110, 16));
        assert!(text.contains("Operational  All good"));
        assert!(!text.contains("more"), "short content: no hints: {text}");

        app.apply(Action::Up);
        let buffer = render_sized(&mut app, 110, 16);
        assert_eq!(pane_rows(&buffer)[0], "Outage  Major Outage");
    }

    #[test]
    fn short_detail_has_no_scroll_hints() {
        let mut app = loaded_app();
        app.apply(Action::Down);
        app.apply(Action::PageDown);
        let text = screen(&render_sized(&mut app, 110, 24));
        assert!(
            !text.contains("↑ more") && !text.contains("↓ more"),
            "{text}"
        );
        assert!(text.contains("Degraded  Partially Degraded Service"));
    }

    #[test]
    fn wrapped_incident_titles_indent_under_the_bullet() {
        let mut cloudflare = status("Cloudflare", Level::Degraded, "Minor");
        cloudflare.incidents = vec![Incident {
            kind: IncidentKind::Incident,
            title: "Elevated errors on the Workers API in Europe".to_owned(),
            components: vec!["Workers".to_owned(), "Queues".to_owned()],
            started_at: None,
            url: Some("https://www.cloudflarestatus.com/incidents/e7k".to_owned()),
            ..Default::default()
        }];
        let mut app = App::new(vec!["Cloudflare".into()]);
        app.start_refresh();
        app.finish_refresh(vec![cloudflare], at(19, 10));
        // Narrow enough that the pane (inner width 28) wraps the title and URL.
        let pane = pane_rows(&render_sized(&mut app, 72, 24));
        let start = pane.iter().position(|row| row.starts_with("> ")).unwrap();
        assert_eq!(
            pane[start..start + 3],
            [
                "> Elevated errors on the",
                "  Workers API in Europe",
                "  Affects: Workers, Queues"
            ],
            "{pane:#?}"
        );
        let url = pane
            .iter()
            .position(|row| row.contains("https://"))
            .unwrap();
        assert_eq!(
            pane[url..url + 2],
            ["  https://www.cloudflarestat", "  us.com/incidents/e7k"],
            "a long URL splits with no blank row before it: {pane:#?}"
        );
    }

    /// Each row's linked cells: the row, their visible text, and the URLs
    /// they link to (deduplicated). Rows with no links are left out.
    fn linked_rows(buffer: &Buffer) -> Vec<(usize, String, Vec<String>)> {
        let width = usize::from(buffer.area.width);
        buffer
            .content
            .chunks(width)
            .enumerate()
            .filter_map(|(y, row)| {
                let cells: Vec<_> = row
                    .iter()
                    .filter_map(|cell| Some((link::target(cell.symbol())?, cell)))
                    .collect();
                if cells.is_empty() {
                    return None;
                }
                let text = cells
                    .iter()
                    .map(|(_, cell)| link::visible(cell.symbol()))
                    .collect();
                let mut urls: Vec<String> =
                    cells.iter().map(|(url, _)| (*url).to_owned()).collect();
                urls.dedup();
                Some((y, text, urls))
            })
            .collect()
    }

    /// One Cloudflare incident linking to `url`.
    fn one_incident_app(url: &str) -> App {
        let mut cloudflare = status("Cloudflare", Level::Degraded, "Minor");
        cloudflare.incidents = vec![Incident {
            kind: IncidentKind::Incident,
            title: "Workers".to_owned(),
            components: Vec::new(),
            started_at: None,
            url: Some(url.to_owned()),
            ..Default::default()
        }];
        let mut app = App::new(vec!["Cloudflare".into()]);
        app.start_refresh();
        app.finish_refresh(vec![cloudflare], at(19, 10));
        app
    }

    #[test]
    fn a_wrapped_url_links_the_full_url_from_every_row() {
        let url = "https://www.cloudflarestatus.com/incidents/e7k";
        let mut app = one_incident_app(url);
        // The pane's inner width is 28, so the URL wraps onto two rows.
        let buffer = render_sized(&mut app, 72, 24);
        let linked = linked_rows(&buffer);
        assert_eq!(
            linked
                .iter()
                .map(|(_, text, urls)| (text.as_str(), urls.clone()))
                .collect::<Vec<_>>(),
            [
                ("https://www.cloudflarestat", vec![url.to_owned()]),
                ("us.com/incidents/e7k", vec![url.to_owned()]),
            ],
            "only the URL is linked, all of it to the full URL"
        );
        let joined: String = linked.iter().map(|(_, text, _)| text.as_str()).collect();
        assert_eq!(joined, url, "the visible text is the plain URL");

        // Every linked cell keeps a one-column width and the URL's underline.
        let (y, _, _) = linked[0];
        let row = &buffer.content[y * 70..(y + 1) * 70];
        for cell in row
            .iter()
            .filter(|cell| link::target(cell.symbol()).is_some())
        {
            assert_eq!(
                cell.diff_option,
                ratatui::buffer::CellDiffOption::ForcedWidth(std::num::NonZeroU16::MIN)
            );
            assert!(cell.modifier.contains(Modifier::UNDERLINED));
        }
    }

    #[test]
    fn every_url_character_is_linked_at_any_width() {
        let url = "https://x.test/incidents/abc";
        // At width 4 the pane is 2 columns: wrapping drops the hanging indent,
        // and the links must follow. The first row's two leading spaces fill
        // it, so its `h` is clipped (a wrap quirk, the same without links).
        for width in 4..=60 {
            let mut app = one_incident_app(url);
            let buffer = render_sized(&mut app, width, 200);
            let joined: String = linked_rows(&buffer)
                .iter()
                .map(|(_, text, _)| text.as_str())
                .collect();
            let expected = if width == 4 { &url[1..] } else { url };
            assert_eq!(joined, expected, "width {width}: {:#?}", pane_rows(&buffer));
        }
    }

    #[test]
    fn links_follow_the_scroll_and_stay_inside_the_pane() {
        let mut app = busy_app();
        render_sized(&mut app, 110, 16);
        app.apply(Action::PageDown);
        let buffer = render_sized(&mut app, 110, 16);
        let pane = pane_rows(&buffer);
        let linked = linked_rows(&buffer);
        // Pane rows start at screen row 2 (header, then the top border).
        let expected: Vec<(usize, String, Vec<String>)> = pane
            .iter()
            .enumerate()
            .filter_map(|(row, text)| {
                let url = text.strip_prefix("  https://")?;
                let url = format!("https://{url}");
                Some((row + 2, url.clone(), vec![url]))
            })
            .collect();
        assert_eq!(expected.len(), 3, "{pane:#?}");
        assert_eq!(linked, expected);
    }

    #[test]
    fn a_url_with_control_characters_stays_plain() {
        let app = one_incident_app("https://x.test/\x1b]0;title\x07a");
        let buffer = render(&app);
        assert_eq!(linked_rows(&buffer), []);
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| !cell.symbol().contains('\x1b')),
            "no escape reaches the screen"
        );
    }

    /// The style of the cell where `text` first appears on screen.
    fn style_at(buffer: &Buffer, text: &str) -> Style {
        let rows = rows(buffer);
        let y = rows.iter().position(|row| row.contains(text)).unwrap();
        let x = rows[y].find(text).unwrap();
        let x = rows[y][..x].chars().count();
        buffer[(u16::try_from(x).unwrap(), u16::try_from(y).unwrap())].style()
    }

    #[test]
    fn the_highlighted_incident_is_marked_and_reversed() {
        let mut app = loaded_app();
        app.apply(Action::Down);
        let buffer = render(&app);
        let text = screen(&buffer);
        assert!(text.contains("> Workers API errors"), "{text}");
        assert!(text.contains("• [maintenance] HKG datacenter"), "{text}");
        let reversed = |title: &str| {
            style_at(&buffer, title)
                .add_modifier
                .contains(Modifier::REVERSED)
        };
        assert!(reversed("Workers API errors"));
        assert!(!reversed("HKG datacenter"));

        app.apply(Action::NextIncident);
        let buffer = render(&app);
        let text = screen(&buffer);
        assert!(text.contains("• Workers API errors"), "{text}");
        assert!(text.contains("> [maintenance] HKG datacenter"), "{text}");
        assert!(
            style_at(&buffer, "HKG datacenter")
                .add_modifier
                .contains(Modifier::REVERSED)
        );
    }

    #[test]
    fn the_latest_update_wraps_under_its_incident() {
        let mut cloudflare = status("Cloudflare", Level::Degraded, "Minor");
        cloudflare.incidents = vec![Incident {
            title: "Workers API".to_owned(),
            url: Some("https://example.com/i".to_owned()),
            stage: Some("monitoring".to_owned()),
            latest_update: Some(Update {
                text: "A fix has been implemented and we are monitoring the results.\n\n## Next\nMore soon."
                    .to_owned(),
                posted_at: Some(at(15, 42)),
            }),
            ..Default::default()
        }];
        let mut app = App::new(vec!["Cloudflare".into()]);
        app.start_refresh();
        app.finish_refresh(vec![cloudflare], at(19, 10));
        // Inner width 28, as in the tests above.
        let pane = pane_rows(&render_sized(&mut app, 72, 24));
        let start = pane.iter().position(|row| row.starts_with("> ")).unwrap();
        assert_eq!(
            pane[start + 1..start + 11],
            [
                "  Latest update (monitoring,",
                "  3h 28m ago):",
                "  A fix has been implemented",
                "  and we are monitoring the",
                "  results.",
                "",
                "  ## Next",
                "  More soon.",
                "  https://example.com/i",
                "",
            ],
            "{pane:#?}"
        );
    }

    #[test]
    fn a_stage_without_an_update_still_shows() {
        let mut cloudflare = status("Cloudflare", Level::Degraded, "Minor");
        cloudflare.incidents = vec![Incident {
            title: "HKG".to_owned(),
            kind: IncidentKind::Maintenance,
            stage: Some("in_progress".to_owned()),
            ..Default::default()
        }];
        let mut app = App::new(vec!["Cloudflare".into()]);
        app.start_refresh();
        app.finish_refresh(vec![cloudflare], at(19, 10));
        let pane = pane_rows(&render_sized(&mut app, 110, 24));
        let start = pane.iter().position(|row| row.starts_with("> ")).unwrap();
        assert_eq!(pane[start + 1], "  Stage: in_progress", "{pane:#?}");
    }

    #[test]
    fn tab_scrolls_past_long_updates_to_each_incident() {
        let mut busy = status("Busy", Level::Outage, "Major Outage");
        busy.incidents = (1..=4)
            .map(|n| Incident {
                title: format!("Incident {n:02}"),
                latest_update: Some(Update {
                    text: (1..=10)
                        .map(|l| format!("line {l}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    posted_at: None,
                }),
                ..Default::default()
            })
            .collect();
        let mut app = App::new(vec!["Busy".into()]);
        app.start_refresh();
        app.finish_refresh(vec![busy], at(19, 10));
        render_sized(&mut app, 110, 16);
        for n in 2..=4 {
            app.apply(Action::NextIncident);
            let pane = pane_rows(&render_sized(&mut app, 110, 16));
            assert!(
                pane.contains(&format!("> Incident {n:02}")),
                "incident {n} in view: {pane:#?}"
            );
        }
    }

    #[test]
    fn tab_keeps_the_highlighted_incident_in_view() {
        let mut app = busy_app();
        // 110x16: the pane holds 12 of the 52 lines. Incident n starts on
        // content line 5 + 4 * (n - 1).
        render_sized(&mut app, 110, 16);
        for _ in 0..3 {
            app.apply(Action::NextIncident);
        }
        let pane = pane_rows(&render_sized(&mut app, 110, 16));
        assert_eq!(
            pane[0], "> Incident 04",
            "line 17 scrolls to the top: {pane:#?}"
        );
        assert_eq!(app.detail_scroll(), 17);

        app.apply(Action::NextIncident);
        let pane = pane_rows(&render_sized(&mut app, 110, 16));
        assert_eq!(pane[4], "> Incident 05", "already in view: {pane:#?}");

        for _ in 0..8 {
            app.apply(Action::NextIncident);
        }
        let pane = pane_rows(&render_sized(&mut app, 110, 16));
        assert_eq!(pane[0], "> Incident 01", "wrapped back up: {pane:#?}");
        assert_eq!(app.detail_scroll(), 5);
    }

    #[test]
    fn paging_moves_the_highlight_onto_the_screen() {
        let mut app = busy_app();
        render_sized(&mut app, 110, 16);
        app.apply(Action::PageDown);
        // Rows 11..23: incident 03 (line 13) is the first that starts on screen.
        let pane = pane_rows(&render_sized(&mut app, 110, 16));
        assert_eq!(pane[2], "> Incident 03", "{pane:#?}");
        assert!(!pane.iter().any(|row| row == "> Incident 01"), "{pane:#?}");
        assert_eq!(
            app.open_target().as_deref(),
            Some("https://status.example.com/03")
        );
    }

    #[test]
    fn a_notice_replaces_the_footer_keys() {
        let mut app = loaded_app();
        app.set_notice("Could not open https://example.com: no browser".to_owned());
        let buffer = render(&app);
        let footer = rows(&buffer).last().unwrap().trim_end().to_owned();
        assert_eq!(footer, " Could not open https://example.com: no browser");
        assert_eq!(style_at(&buffer, "Could not open").fg, Some(Color::Yellow));
        app.clear_notice();
        assert!(screen(&render(&app)).contains("o open"));
    }

    #[test]
    fn empty_platform_list_renders() {
        let screen_text = screen(&render(&App::new(Vec::new())));
        assert!(screen_text.contains("No platforms configured."));
    }

    /// GitHub (operational, selected) with two resolved incidents, and Cloudflare.
    fn app_with_resolved() -> App {
        let mut github = status("GitHub", Level::Operational, "All Systems Operational");
        github.resolved = vec![
            Incident {
                title: "Incident with Pull Requests".to_owned(),
                components: vec!["Pull Requests".to_owned()],
                started_at: Some(at(15, 47)),
                ended_at: Some(at(16, 56)),
                url: Some("https://stspg.io/qzj7qjdcr8jc".to_owned()),
                ..Default::default()
            },
            Incident {
                title: "Disruption with Email Delivery".to_owned(),
                ended_at: Some(at(2, 0)),
                ..Default::default()
            },
        ];
        let mut app = App::new(vec!["GitHub".into(), "Cloudflare".into()]);
        app.start_refresh();
        app.finish_refresh(
            vec![github, status("Cloudflare", Level::Operational, "All good")],
            at(19, 10),
        );
        app
    }

    #[test]
    fn resolved_incidents_show_below_the_active_ones() {
        let mut app = app_with_resolved();
        app.apply(Action::NextIncident);
        assert_eq!(
            app.highlighted_incident(),
            None,
            "Tab skips resolved incidents"
        );
        // Config order on a tie: GitHub is the top row and selected.
        let buffer = render(&app);
        let pane = pane_rows(&buffer);
        let pane: Vec<&str> = pane.iter().map(|row| row.trim_end()).collect();
        let at_row = |text: &str| {
            pane.iter()
                .position(|row| row.contains(text))
                .unwrap_or_else(|| panic!("no {text:?} in {pane:#?}"))
        };
        let none = at_row("No active incidents.");
        let heading = at_row("Resolved in the last 24h (2)");
        assert!(heading > none, "below the active incidents");
        assert_eq!(pane[heading + 2], "• Incident with Pull Requests");
        assert_eq!(pane[heading + 3], "  Affects: Pull Requests");
        assert_eq!(pane[heading + 4], "  Resolved 2h 14m ago, lasted 1h 9m");
        assert_eq!(pane[heading + 5], "  https://stspg.io/qzj7qjdcr8jc");
        assert_eq!(pane[heading + 7], "• Disruption with Email Delivery");
        assert_eq!(pane[heading + 8], "  Resolved 17h 10m ago");

        let rows = rows(&buffer);
        let y = rows
            .iter()
            .position(|r| r.contains("https://stspg.io"))
            .unwrap();
        let x = rows[y].find("https://stspg.io").unwrap();
        let x = u16::try_from(rows[y][..x].chars().count()).unwrap();
        let cell = &buffer[(x, u16::try_from(y).unwrap())];
        assert_eq!(
            link::target(cell.symbol()),
            Some("https://stspg.io/qzj7qjdcr8jc"),
            "the resolved link is clickable"
        );
    }

    #[test]
    fn no_resolved_incidents_means_no_resolved_heading() {
        let screen = screen(&render(&loaded_app()));
        assert!(!screen.contains("Resolved in the last"), "{screen}");
    }

    #[test]
    fn resolved_incidents_drop_out_once_older_than_the_window_at_draw_time() {
        // Drawn at 02:05 the next day: the 16:56 end is still in, the 02:00 one is not.
        let next_day = Utc.with_ymd_and_hms(2026, 11, 14, 2, 5, 0).unwrap();
        let screen = screen(&render_at(&app_with_resolved(), next_day));
        assert!(screen.contains("Resolved in the last 24h (1)"), "{screen}");
        assert!(
            !screen.contains("Disruption with Email Delivery"),
            "{screen}"
        );
    }

    /// GitHub's records: Operational since 11-10, an outage 15:47 to 16:56
    /// on 11-13, and a restart re-recording Operational at 18:00. Cloudflare
    /// has none.
    fn github_history() -> History {
        let record = |at: DateTime<Utc>, level: Level, titles: &[&str]| {
            let mut status = status("GitHub", level, "");
            status.incidents = titles
                .iter()
                .map(|title| Incident {
                    title: (*title).to_owned(),
                    ..Default::default()
                })
                .collect();
            history::Record {
                recorded_at: at,
                status,
                backfilled: false,
            }
        };
        let long = "Elevated error rates ".repeat(12);
        History::Records(vec![
            record(
                Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap(),
                Level::Operational,
                &[],
            ),
            record(
                at(15, 47),
                Level::Outage,
                &["Incident with Pull Requests", long.trim_end()],
            ),
            record(at(16, 56), Level::Operational, &[]),
            record(at(18, 0), Level::Operational, &[]),
        ])
    }

    fn local(at: DateTime<Utc>) -> String {
        at.with_timezone(&Local).format("%a %H:%M").to_string()
    }

    #[test]
    fn history_lists_level_changes_newest_first_after_the_resolved_section() {
        let mut app = app_with_resolved();
        app.set_history(github_history());
        let buffer = render_sized(&mut app, 140, 40);
        let pane = pane_rows(&buffer);
        let at_row = |text: &str| {
            pane.iter()
                .position(|row| row.contains(text))
                .unwrap_or_else(|| panic!("no {text:?} in {pane:#?}"))
        };
        let heading = at_row("History (last 7 days)");
        assert!(heading > at_row("Resolved in the last 24h"), "{pane:#?}");
        let when = local(at(15, 47));
        let indent = " ".repeat(2 + when.chars().count() + 2);
        assert_eq!(
            pane[heading + 1],
            format!("  {}  Outage → Operational, ongoing", local(at(16, 56)))
        );
        assert_eq!(
            pane[heading + 2],
            format!("  {when}  Operational → Outage, lasted 1h 9m")
        );
        // Titles joined and cut like `--once` updates, wrapped under the levels.
        assert!(
            pane[heading + 3]
                .starts_with(&format!("{indent}Incident with Pull Requests; Elevated")),
            "{pane:#?}"
        );
        let titles: String = pane[heading + 3..]
            .iter()
            .take_while(|row| row.starts_with(&indent))
            .map(|row| row.trim())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(titles.ends_with('…'), "{titles}");
        let oldest = pane
            .iter()
            .position(|row| row.contains(", lasted 3d 15h"))
            .unwrap_or_else(|| panic!("{pane:#?}"));
        assert_eq!(
            pane[oldest],
            format!(
                "  {}  Operational, lasted 3d 15h",
                local(Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap())
            ),
            "the oldest entry shows only its level"
        );
        assert_eq!(pane.iter().filter(|r| r.contains("ongoing")).count(), 1);
    }

    #[test]
    fn a_platform_with_no_records_says_so() {
        let mut app = app_with_resolved();
        app.set_history(github_history());
        app.apply(Action::Down);
        let screen = screen(&render_sized(&mut app, 140, 40));
        assert!(screen.contains("History (last 7 days)"), "{screen}");
        assert!(
            screen.contains("  No history recorded for this platform yet."),
            "{screen}"
        );
    }

    #[test]
    fn a_missing_or_unreadable_history_file_replaces_the_section() {
        let mut app = app_with_resolved();
        let before = screen(&render_sized(&mut app, 140, 40));
        assert!(!before.contains("History"), "nothing before the first read");
        assert!(!before.contains("No history"), "{before}");

        app.set_history(History::Missing);
        let missing = screen(&render_sized(&mut app, 140, 40));
        assert!(!missing.contains("History (last"), "{missing}");
        assert!(
            missing.contains(
                "No history: run aretheydown --serve (or brew services start aretheydown) to record it."
            ),
            "{missing}"
        );

        app.set_history(History::Unreadable(
            "could not read the history file /h.jsonl: Permission denied".to_owned(),
        ));
        let unreadable = screen(&render_sized(&mut app, 140, 40));
        assert!(
            unreadable.contains("No history: run aretheydown --serve"),
            "{unreadable}"
        );
        assert!(
            unreadable.contains("could not read the history file /h.jsonl: Permission denied"),
            "{unreadable}"
        );
        assert!(
            unreadable.contains("Resolved in the last 24h"),
            "the rest still shows"
        );
    }

    #[test]
    fn an_old_entry_shows_its_date() {
        let now = at(19, 10);
        let old = Utc.with_ymd_and_hms(2026, 11, 5, 9, 0, 0).unwrap();
        assert_eq!(
            change_time(old, now),
            old.with_timezone(&Local)
                .format("%a %b %-d %H:%M")
                .to_string()
        );
        assert_eq!(change_time(at(2, 0), now), local(at(2, 0)));
    }

    fn record(name: &str, at: DateTime<Utc>, level: Level, titles: &[&str]) -> history::Record {
        let mut status = status(name, level, "");
        status.incidents = titles
            .iter()
            .map(|title| Incident {
                title: (*title).to_owned(),
                ..Default::default()
            })
            .collect();
        history::Record {
            recorded_at: at,
            status,
            backfilled: false,
        }
    }

    /// GitHub: Operational since 11-10, an outage 15:47 to 16:56 on 11-13,
    /// re-recorded Operational at 18:00. Cloudflare: Operational since 11-12
    /// 08:00, Degraded from 16:56. Slow has none.
    fn two_platform_history() -> History {
        History::Records(vec![
            record(
                "GitHub",
                Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap(),
                Level::Operational,
                &[],
            ),
            record(
                "Cloudflare",
                Utc.with_ymd_and_hms(2026, 11, 12, 8, 0, 0).unwrap(),
                Level::Operational,
                &[],
            ),
            record(
                "GitHub",
                at(15, 47),
                Level::Outage,
                &["Incident with Pull Requests"],
            ),
            record("GitHub", at(16, 56), Level::Operational, &[]),
            record(
                "Cloudflare",
                at(16, 56),
                Level::Degraded,
                &["Workers API errors"],
            ),
            record("GitHub", at(18, 0), Level::Operational, &[]),
        ])
    }

    /// `loaded_app` (Slow, Cloudflare, GitHub worst first) with the timeline open.
    fn timeline_app(history: History) -> App {
        let mut app = loaded_app();
        app.set_history(history);
        app.apply(Action::Timeline);
        app
    }

    /// The timeline's rows (inside its borders), right-trimmed.
    fn timeline_rows(rows: &[String]) -> Vec<String> {
        rows[2..rows.len() - 2]
            .iter()
            .map(|row| {
                let inner: String = row.chars().skip(1).collect();
                inner.trim_end_matches(['│', ' ']).to_owned()
            })
            .collect()
    }

    #[test]
    fn the_timeline_merges_every_platform_newest_first_under_day_headings() {
        let mut app = timeline_app(two_platform_history());
        let buffer = render_sized(&mut app, 100, 30);
        let all = rows(&buffer);
        assert!(all[1].starts_with("┌ Timeline (last 7 days) "), "{all:#?}");
        assert!(all[1].ends_with(" 5 changes ┐"), "{all:#?}");
        assert!(!screen(&buffer).contains(" Platforms "), "the list is gone");

        // (time, platform and change) newest first; titles indent after the name.
        let indent = " ".repeat(2 + 5 + 2 + "Cloudflare".len() + 2);
        // At 16:56, GitHub before Cloudflare: config order, though the list
        // shows Cloudflare (Degraded) above GitHub (Operational).
        let entries = [
            (
                at(16, 56),
                "GitHub      Outage → Operational, ongoing",
                None,
            ),
            (
                at(16, 56),
                "Cloudflare  Operational → Degraded, ongoing",
                Some("Workers API errors"),
            ),
            (
                at(15, 47),
                "GitHub      Operational → Outage, lasted 1h 9m",
                Some("Incident with Pull Requests"),
            ),
            (
                Utc.with_ymd_and_hms(2026, 11, 12, 8, 0, 0).unwrap(),
                "Cloudflare  Operational, lasted 1d 8h",
                None,
            ),
            (
                Utc.with_ymd_and_hms(2026, 11, 10, 0, 0, 0).unwrap(),
                "GitHub      Operational, lasted 3d 15h",
                None,
            ),
        ];
        let mut expected: Vec<String> = Vec::new();
        let mut day = None;
        for (when, text, titles) in entries {
            let local = when.with_timezone(&Local);
            if day != Some(local.date_naive()) {
                if day.is_some() {
                    expected.push(String::new());
                }
                day = Some(local.date_naive());
                expected.push(local.format("%a %-d %b").to_string());
            }
            expected.push(format!("  {}  {text}", local.format("%H:%M")));
            if let Some(titles) = titles {
                expected.push(format!("{indent}{titles}"));
            }
        }
        let pane = timeline_rows(&rows(&buffer));
        assert_eq!(pane[..expected.len()], expected[..], "{pane:#?}");
        assert!(pane[expected.len()..].iter().all(String::is_empty));
    }

    #[test]
    fn the_day_heading_names_the_weekday_day_and_month() {
        let mut app = timeline_app(History::Records(vec![record(
            "GitHub",
            at(12, 0),
            Level::Operational,
            &[],
        )]));
        let pane = timeline_rows(&rows(&render_sized(&mut app, 100, 10)));
        let local = at(12, 0).with_timezone(&Local);
        assert_eq!(pane[0], local.format("%a %-d %b").to_string());
        // 13 Nov 2026 is a Friday; noon UTC is still the 13th from -11h to +11h.
        assert!(pane[0].ends_with(" Nov"), "{}", pane[0]);
        assert!(
            rows(&render_sized(&mut app, 100, 10))[1].ends_with(" 1 change ┐"),
            "singular"
        );
    }

    #[test]
    fn the_timeline_scrolls_with_hints() {
        let records = (0..20)
            .map(|n| {
                let level = if n % 2 == 0 {
                    Level::Outage
                } else {
                    Level::Operational
                };
                record("GitHub", at(0, 0) + TimeDelta::minutes(n * 30), level, &[])
            })
            .collect();
        let mut app = timeline_app(History::Records(records));
        let top = rows(&render_sized(&mut app, 100, 12));
        assert!(top[top.len() - 2].contains(" ↓ more "), "{top:#?}");
        assert!(!top[1].contains("more"), "{top:#?}");
        app.apply(Action::Down);
        let middle = rows(&render_sized(&mut app, 100, 12));
        assert!(middle[1].contains(" ↑ more "), "{middle:#?}");
        assert!(middle[middle.len() - 2].contains(" ↓ more "), "{middle:#?}");
        assert_eq!(timeline_rows(&middle)[0], timeline_rows(&top)[1]);
        for _ in 0..5 {
            app.apply(Action::PageDown);
        }
        let bottom = rows(&render_sized(&mut app, 100, 12));
        assert!(bottom[1].contains(" ↑ more "), "{bottom:#?}");
        assert!(!bottom[bottom.len() - 2].contains("more"), "{bottom:#?}");
        let last = timeline_rows(&bottom);
        assert!(
            last.last().unwrap().contains("GitHub  Outage, lasted 30m"),
            "the oldest entry ends the list: {last:#?}"
        );
    }

    #[test]
    fn a_missing_empty_or_unreadable_history_shows_how_to_record_it() {
        let message = "No history: run aretheydown --serve (or brew services start aretheydown) to record it.";
        let mut app = loaded_app();
        app.apply(Action::Timeline);
        let before = timeline_rows(&rows(&render_sized(&mut app, 120, 10)));
        assert_eq!(before[0], "Reading the history file…");

        for history in [History::Missing, History::Records(Vec::new())] {
            app.set_history(history);
            let buffer = render_sized(&mut app, 120, 10);
            assert_eq!(timeline_rows(&rows(&buffer))[0], message);
        }
        assert!(
            rows(&render_sized(&mut app, 120, 10))[1].contains(" 0 changes "),
            "an empty file counts none"
        );

        app.set_history(History::Unreadable(
            "could not read the history file /h.jsonl: Permission denied".to_owned(),
        ));
        let buffer = render_sized(&mut app, 120, 10);
        let pane = timeline_rows(&rows(&buffer));
        assert_eq!(pane[0], message);
        assert_eq!(
            pane[1],
            "could not read the history file /h.jsonl: Permission denied"
        );
        assert!(!rows(&buffer)[1].contains("change"), "nothing to count");
    }

    #[test]
    fn the_footer_says_how_to_leave_the_timeline() {
        let mut app = timeline_app(History::Missing);
        let footer = rows(&render(&app)).last().unwrap().trim_end().to_owned();
        assert_eq!(
            footer,
            " ↑/↓ j/k PgUp/PgDn J/K scroll  t/Esc back  r refresh  q quit  (auto-refresh every 60s)"
        );
        app.apply(Action::Back);
        let screen = screen(&render_sized(&mut app, 140, 24));
        assert!(screen.contains(" Platforms "), "back to the list");
        assert!(screen.contains("t timeline"), "{screen}");
    }
}
