//! The TUI's state: which platform and incident are selected, the latest
//! statuses, and whether a refresh is in flight. Pure, so it is tested
//! without a terminal.

use chrono::{DateTime, TimeDelta, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::picker::{Item, Outcome, Picker};
use crate::history::Record;
use crate::model::{ComponentNode, Incident, Level, PlatformStatus, name_key, sort_worst_first};
use crate::providers::aws;

/// Something the user asked for with a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Up,
    Down,
    /// Scroll the detail pane down one page.
    PageDown,
    /// Scroll the detail pane up one page.
    PageUp,
    /// Highlight the selected platform's next incident, wrapping to the first.
    NextIncident,
    /// Highlight the previous incident, wrapping to the last.
    PrevIncident,
    /// Open the highlighted incident's page, or the platform's status page.
    Open,
    /// Switch between worst-first and config order.
    ToggleSort,
    Refresh,
    /// Open or close the full-screen timeline of every platform.
    Timeline,
    /// Open the selected platform's filter picker.
    Filter,
    /// Close the timeline when it is open; otherwise quit.
    Back,
    Quit,
}

/// Work an action asks the event loop to do outside the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Start a refresh now.
    Refresh,
    /// Open this URL in the default browser.
    Open(String),
    /// Show this message in the footer for a few seconds.
    Notice(String),
    /// Write a picker's choices to the config file, then refetch with them.
    Save(Pick),
}

/// What a filter picker saved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// For `[aws] regions` (none removes it).
    AwsRegions(Vec<String>),
    /// For `platform`'s `[components]` entry (none removes it).
    Components {
        platform: String,
        names: Vec<String>,
    },
}

impl Pick {
    /// The chosen keys, whichever filter they are for.
    #[must_use]
    pub fn keys(&self) -> &[String] {
        match self {
            Self::AwsRegions(codes) => codes,
            Self::Components { names, .. } => names,
        }
    }
}

/// What `f` can filter on a platform.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Filter {
    /// Nothing: `f` says so in the footer.
    #[default]
    None,
    /// AWS, with the `[aws] regions` in force (`None`: every region).
    AwsRegions(Option<Vec<String>>),
    /// A Statuspage platform, with its `[components]` entry in force
    /// (`None`: every component).
    Components(Option<Vec<String>>),
}

/// The order of the platform list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    /// Worst level first (Outage, Unknown, Degraded, Operational); ties in config order.
    #[default]
    WorstFirst,
    /// The order of the config (built-ins, then `[[statuspage]]` vendors).
    Config,
}

impl SortOrder {
    /// How the footer names this order.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SortOrder::WorstFirst => "worst first",
            SortOrder::Config => "config order",
        }
    }

    /// The other order.
    #[must_use]
    pub fn toggled(self) -> Self {
        match self {
            SortOrder::WorstFirst => SortOrder::Config,
            SortOrder::Config => SortOrder::WorstFirst,
        }
    }
}

impl Action {
    /// The action bound to `key`, if any. Only presses count, not releases or repeats.
    #[must_use]
    pub fn from_key(key: KeyEvent) -> Option<Self> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return (key.code == KeyCode::Char('c')).then_some(Action::Quit);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => Some(Action::Up),
            KeyCode::Down | KeyCode::Char('j') => Some(Action::Down),
            KeyCode::PageDown | KeyCode::Char('J') => Some(Action::PageDown),
            KeyCode::PageUp | KeyCode::Char('K') => Some(Action::PageUp),
            KeyCode::Tab => Some(Action::NextIncident),
            KeyCode::BackTab => Some(Action::PrevIncident),
            KeyCode::Char('o') => Some(Action::Open),
            KeyCode::Char('s') => Some(Action::ToggleSort),
            KeyCode::Char('r') => Some(Action::Refresh),
            KeyCode::Char('t') => Some(Action::Timeline),
            KeyCode::Char('f') => Some(Action::Filter),
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Char('q') => Some(Action::Quit),
            _ => None,
        }
    }
}

/// The detail pane's size, content length, and where each incident starts, as
/// last drawn. Scrolling and the incident highlight need it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DetailViewport {
    /// Lines of content, after wrapping to the pane's width.
    pub content_lines: u16,
    /// Lines that fit inside the pane.
    pub height: u16,
    /// The content line where each incident starts, in incident order.
    pub incident_lines: Vec<u16>,
}

impl DetailViewport {
    /// The largest scroll offset that still fills the pane: the last line sits
    /// at the bottom, never above blank space. A pane with no room never scrolls.
    #[must_use]
    pub fn max_scroll(&self) -> u16 {
        if self.height == 0 {
            return 0;
        }
        self.content_lines.saturating_sub(self.height)
    }

    /// How far one page scroll moves: the pane's height less one line, so the
    /// line at the edge stays in view for context.
    fn page_lines(&self) -> u16 {
        self.height.saturating_sub(1).max(1)
    }

    /// Whether content line `line` is on screen at `scroll`.
    fn shows(&self, line: u16, scroll: u16) -> bool {
        line >= scroll && line < scroll.saturating_add(self.height)
    }
}

/// How long after its last good fetch a platform's data stays on screen
/// while refreshes fail. Past it, the row shows the failure as `Unknown`.
pub const STALE_LIMIT: TimeDelta = TimeDelta::minutes(10);

/// A failed refresh of a platform whose last good data is still shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stale {
    /// Why the latest refresh failed.
    pub error: String,
    /// When the latest refresh failed.
    pub failed_at: DateTime<Utc>,
}

/// What the TUI last read of the history file `--serve` writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum History {
    /// There is no file at the history path.
    Missing,
    /// The file, or where it lives, could not be read: why.
    Unreadable(String),
    /// The file's readable records, in file order.
    Records(Vec<Record>),
}

/// One platform as the TUI knows it.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    /// The page `o` opens when there is no incident link.
    status_page: Option<String>,
    /// The latest status, once fetched; while `stale` is set, the last good one.
    status: Option<PlatformStatus>,
    /// Set while refreshes fail and `status` is older good data.
    stale: Option<Stale>,
    /// What `f` filters.
    filter: Filter,
}

impl Entry {
    /// Take a refresh's result. A failure (a status with `error` set) keeps
    /// the last good status, marked stale, while it is at most
    /// [`STALE_LIMIT`] old at `now`; any other result replaces it.
    fn update(&mut self, status: PlatformStatus, now: DateTime<Utc>) {
        let keep = status.error.is_some()
            && self
                .status
                .as_ref()
                .is_some_and(|old| old.error.is_none() && now - old.fetched_at <= STALE_LIMIT);
        if keep {
            // The probe is this refresh's own observation, so it replaces the
            // kept one even though the feed's data is older.
            if let Some(old) = &mut self.status {
                old.probe = status.probe;
            }
            self.stale = Some(Stale {
                error: status.error.unwrap_or_default(),
                failed_at: status.fetched_at,
            });
        } else {
            self.status = Some(status);
            self.stale = None;
        }
    }
}

/// The filter picker while it is open, and what it saves to.
#[derive(Debug, Clone)]
struct OpenPicker {
    picker: Picker,
    /// The Statuspage platform whose `[components]` it picks; `None` for
    /// the AWS region picker.
    components_of: Option<String>,
}

/// Everything the TUI shows, independent of how it is drawn.
#[derive(Debug, Clone)]
pub struct App {
    /// The platforms in config order, known before any data arrives.
    entries: Vec<Entry>,
    /// How the rows are ordered.
    sort: SortOrder,
    /// Display order: `order[row]` is the index into `entries` shown at `row`.
    order: Vec<usize>,
    /// The selected row (a position in `order`, not in `entries`).
    selected: usize,
    /// Lines scrolled off the top of the detail pane.
    detail_scroll: u16,
    detail_viewport: DetailViewport,
    /// The highlighted incident of the selected platform (an index into its
    /// incidents; clamped when read, since a refresh can shorten the list).
    highlighted: usize,
    /// Scroll the highlighted incident into view on the next draw.
    reveal_highlight: bool,
    /// A short message for the footer, e.g. a link that would not open.
    notice: Option<String>,
    refreshing: bool,
    last_refresh: Option<DateTime<Utc>>,
    /// The history file as last read; `None` until the first read lands.
    history: Option<History>,
    /// While the full-screen timeline replaces the list and detail pane,
    /// the lines scrolled off its top; `None` while it is closed.
    timeline: Option<u16>,
    /// The timeline as last drawn, for its page size and end.
    timeline_viewport: DetailViewport,
    /// The filter picker, while it replaces the list and detail pane.
    picker: Option<OpenPicker>,
    quit: bool,
}

impl App {
    /// A fresh app for platforms with these names and no known status pages,
    /// with no data yet and the first one selected.
    #[must_use]
    pub fn new(names: Vec<String>) -> Self {
        Self::from_entries(names.into_iter().map(|name| (name, None)))
    }

    /// A fresh app for `(name, status page)` pairs, in config order.
    #[must_use]
    pub fn with_status_pages(platforms: Vec<(String, String)>) -> Self {
        Self::from_entries(platforms.into_iter().map(|(name, page)| (name, Some(page))))
    }

    fn from_entries(entries: impl Iterator<Item = (String, Option<String>)>) -> Self {
        let entries: Vec<Entry> = entries
            .map(|(name, status_page)| Entry {
                name,
                status_page,
                status: None,
                stale: None,
                filter: Filter::None,
            })
            .collect();
        let order = (0..entries.len()).collect();
        Self {
            entries,
            sort: SortOrder::default(),
            order,
            selected: 0,
            detail_scroll: 0,
            detail_viewport: DetailViewport::default(),
            highlighted: 0,
            reveal_highlight: false,
            notice: None,
            refreshing: false,
            last_refresh: None,
            history: None,
            timeline: None,
            timeline_viewport: DetailViewport::default(),
            picker: None,
            quit: false,
        }
    }

    /// React to a key press: the picker takes every key while it is open;
    /// otherwise the key's [`Action`] is applied.
    pub fn key(&mut self, key: KeyEvent) -> Option<Effect> {
        let Some(open) = &mut self.picker else {
            return Action::from_key(key).and_then(|action| self.apply(action));
        };
        match open.picker.key(key) {
            Outcome::Stay => None,
            Outcome::Cancel => {
                self.picker = None;
                None
            }
            Outcome::Save(keys) => {
                let components_of = self.picker.take().and_then(|open| open.components_of);
                Some(Effect::Save(match components_of {
                    Some(platform) => Pick::Components {
                        platform,
                        names: keys,
                    },
                    None => Pick::AwsRegions(keys),
                }))
            }
            Outcome::Quit => {
                self.quit = true;
                None
            }
        }
    }

    /// Apply a user action, returning any work it leaves for the event loop.
    pub fn apply(&mut self, action: Action) -> Option<Effect> {
        match action {
            Action::Refresh => return self.start_refresh().then_some(Effect::Refresh),
            Action::Quit => self.quit = true,
            _ if self.timeline.is_some() => self.apply_in_timeline(action),
            _ => return self.apply_in_list(action),
        }
        None
    }

    /// Apply `action` to the platform list and detail pane.
    fn apply_in_list(&mut self, action: Action) -> Option<Effect> {
        match action {
            Action::Up => self.select(self.selected.saturating_sub(1)),
            Action::Down => {
                self.select((self.selected + 1).min(self.entries.len().saturating_sub(1)));
            }
            Action::PageDown => {
                let viewport = &self.detail_viewport;
                let scroll = self.detail_scroll.saturating_add(viewport.page_lines());
                self.scroll_to(scroll.min(viewport.max_scroll()));
            }
            Action::PageUp => {
                let page = self.detail_viewport.page_lines();
                self.scroll_to(self.detail_scroll.saturating_sub(page));
            }
            Action::NextIncident => self.move_highlight(1),
            Action::PrevIncident => self.move_highlight(-1),
            Action::Open => return self.open_target().map(Effect::Open),
            Action::ToggleSort => {
                self.sort = self.sort.toggled();
                self.reorder();
            }
            Action::Timeline => self.timeline = Some(0),
            Action::Filter => return self.open_picker(),
            Action::Back => self.quit = true,
            // Handled in `apply`, before the timeline and the list split.
            Action::Refresh | Action::Quit => {}
        }
        None
    }

    /// Apply `action` while the timeline is open: the arrow and page keys
    /// scroll it, `t` and Esc close it, and keys about the list (`s`, Tab,
    /// `o`) do nothing, since it is not on screen.
    fn apply_in_timeline(&mut self, action: Action) {
        let scroll = self.timeline_scroll();
        let viewport = &self.timeline_viewport;
        let max = viewport.max_scroll();
        let page = viewport.page_lines();
        match action {
            Action::Up => self.timeline = Some(scroll.saturating_sub(1)),
            Action::Down => self.timeline = Some(scroll.saturating_add(1).min(max)),
            Action::PageUp => self.timeline = Some(scroll.saturating_sub(page)),
            Action::PageDown => self.timeline = Some(scroll.saturating_add(page).min(max)),
            Action::Timeline | Action::Back => self.timeline = None,
            Action::ToggleSort
            | Action::NextIncident
            | Action::PrevIncident
            | Action::Open
            | Action::Filter
            | Action::Refresh
            | Action::Quit => {}
        }
    }

    /// Open the selected platform's filter picker, or say in the footer that
    /// it has none.
    fn open_picker(&mut self) -> Option<Effect> {
        let entry = self.selected_entry()?;
        match &entry.filter {
            Filter::None => Some(Effect::Notice(format!(
                "{} can't be filtered (only AWS regions and Statuspage components can be picked)",
                entry.name
            ))),
            Filter::AwsRegions(regions) => {
                let feed = entry
                    .status
                    .as_ref()
                    .map_or(&[][..], |status| &status.regions);
                let items = region_items(regions.as_deref(), feed);
                self.picker = Some(OpenPicker {
                    picker: Picker::new("AWS regions", "regions", items),
                    components_of: None,
                });
                None
            }
            Filter::Components(names) => {
                let name = entry.name.clone();
                let status = match &entry.status {
                    None => {
                        return Some(Effect::Notice(format!(
                            "{name}'s components aren't known yet: its first fetch is still running"
                        )));
                    }
                    Some(status) if status.error.is_some() => {
                        return Some(Effect::Notice(format!(
                            "{name}'s components aren't known: its last fetch failed (r to retry)"
                        )));
                    }
                    Some(status) => status,
                };
                let items = component_items(names.as_deref(), &status.component_tree);
                if items.is_empty() {
                    return Some(Effect::Notice(format!(
                        "{name}'s feed lists no components to pick"
                    )));
                }
                self.picker = Some(OpenPicker {
                    picker: Picker::new(&format!("{name} components"), "components", items),
                    components_of: Some(name),
                });
                None
            }
        }
    }

    /// The filter picker, while it is open.
    #[must_use]
    pub fn picker(&self) -> Option<&Picker> {
        self.picker.as_ref().map(|open| &open.picker)
    }

    /// Set what `f` filters on the platform named `name`.
    pub fn set_filter(&mut self, name: &str, filter: Filter) {
        if let Some(entry) = self.entries.iter_mut().find(|entry| entry.name == name) {
            entry.filter = filter;
        }
    }

    /// Whether the full-screen timeline is showing.
    #[must_use]
    pub fn timeline_open(&self) -> bool {
        self.timeline.is_some()
    }

    /// Lines scrolled off the top of the timeline; 0 while it is closed.
    #[must_use]
    pub fn timeline_scroll(&self) -> u16 {
        self.timeline.unwrap_or(0)
    }

    /// The scroll to draw the timeline with: the current one, pulled back if
    /// the content got shorter or the screen taller.
    #[must_use]
    pub fn timeline_scroll_for(&self, viewport: &DetailViewport) -> u16 {
        self.timeline_scroll().min(viewport.max_scroll())
    }

    /// Select row `index`. A new selection starts its detail pane at the top,
    /// with its first incident highlighted.
    fn select(&mut self, index: usize) {
        if index != self.selected {
            self.selected = index;
            self.detail_scroll = 0;
            self.highlighted = 0;
            self.reveal_highlight = false;
        }
    }

    /// Scroll the detail pane to `scroll`. If that moves the highlighted
    /// incident off screen, the highlight follows: to the first incident that
    /// starts on screen, else the one whose body fills it. So `o` always opens
    /// an incident you can see.
    fn scroll_to(&mut self, scroll: u16) {
        self.detail_scroll = scroll;
        let viewport = &self.detail_viewport;
        let starts = &viewport.incident_lines;
        let Some(current) = self.highlighted_incident() else {
            return;
        };
        if starts
            .get(current)
            .is_none_or(|&line| viewport.shows(line, scroll))
        {
            return;
        }
        let on_screen = starts.iter().position(|&line| viewport.shows(line, scroll));
        let above = starts.iter().rposition(|&line| line < scroll);
        if let Some(index) = on_screen.or(above) {
            self.highlighted = index;
        }
    }

    /// Move the incident highlight `step` places, wrapping at either end, and
    /// scroll it into view.
    fn move_highlight(&mut self, step: isize) {
        let Some(current) = self.highlighted_incident() else {
            return;
        };
        let count = self.selected_incidents().len();
        self.highlighted = current
            .checked_add_signed(step)
            .map_or(count - 1, |i| i % count);
        self.reveal_highlight = true;
    }

    /// The selected platform's latest status, once fetched.
    fn selected_status(&self) -> Option<&PlatformStatus> {
        self.selected_entry()?.status.as_ref()
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entry_at(self.selected)
    }

    /// The platform at display row `row`.
    fn entry_at(&self, row: usize) -> Option<&Entry> {
        self.entries.get(*self.order.get(row)?)
    }

    /// The selected platform's incidents; empty before its first fetch.
    fn selected_incidents(&self) -> &[Incident] {
        self.selected_status()
            .map_or(&[], |status| status.incidents.as_slice())
    }

    /// The highlighted incident (an index into the selected platform's
    /// incidents), or `None` when it has none.
    #[must_use]
    pub fn highlighted_incident(&self) -> Option<usize> {
        let count = self.selected_incidents().len();
        (count > 0).then(|| self.highlighted.min(count - 1))
    }

    /// What `o` opens: the highlighted incident's link, else the selected
    /// platform's status page. `None` when there is neither.
    #[must_use]
    pub fn open_target(&self) -> Option<String> {
        let incident_url = self
            .highlighted_incident()
            .and_then(|index| self.selected_incidents()[index].url.clone());
        incident_url.or_else(|| self.selected_entry()?.status_page.clone())
    }

    /// Recompute the display order, keeping the same platform selected
    /// (and its detail scroll) wherever it moves to.
    fn reorder(&mut self) {
        let selected_platform = self.order.get(self.selected).copied();
        self.order = (0..self.entries.len()).collect();
        if self.sort == SortOrder::WorstFirst {
            // No data yet ranks with Operational, so it stays in config order.
            let entries = &self.entries;
            sort_worst_first(&mut self.order, |&i| {
                entries[i]
                    .status
                    .as_ref()
                    .map_or(Level::Operational, |status| status.level)
            });
        }
        if let Some(platform) = selected_platform {
            self.selected = self.order.iter().position(|&i| i == platform).unwrap_or(0);
        }
    }

    /// Record the scrolling pane as just drawn (the timeline while it is
    /// open, else the detail pane), and keep the scroll it was drawn with (see
    /// [`App::scroll_for`] and [`App::timeline_scroll_for`]).
    pub fn set_viewport(&mut self, viewport: DetailViewport) {
        if self.picker.is_some() {
            // The picker covers the panes, so they keep their scroll.
            return;
        }
        if self.timeline.is_some() {
            self.timeline = Some(self.timeline_scroll_for(&viewport));
            self.timeline_viewport = viewport;
            return;
        }
        self.detail_scroll = self.scroll_for(&viewport);
        self.detail_viewport = viewport;
        self.reveal_highlight = false;
    }

    /// The scroll to draw `viewport` with: the current one, pulled back if the
    /// content got shorter or the pane taller. Right after the highlight
    /// moves, a highlighted incident out of view scrolls to the top of the pane.
    #[must_use]
    pub fn scroll_for(&self, viewport: &DetailViewport) -> u16 {
        let scroll = self.detail_scroll.min(viewport.max_scroll());
        let highlight_line = self
            .highlighted_incident()
            .and_then(|index| viewport.incident_lines.get(index).copied());
        match highlight_line {
            Some(line) if self.reveal_highlight && !viewport.shows(line, scroll) => {
                line.min(viewport.max_scroll())
            }
            _ => scroll,
        }
    }

    /// Lines scrolled off the top of the detail pane.
    #[must_use]
    pub fn detail_scroll(&self) -> u16 {
        self.detail_scroll
    }

    /// Mark a refresh as started. Returns `false` (and changes nothing) when one
    /// is already in flight, so refreshes never overlap.
    pub fn start_refresh(&mut self) -> bool {
        if self.refreshing {
            return false;
        }
        self.refreshing = true;
        true
    }

    /// Store a refresh's results, which come back in platform order. A
    /// platform whose fetch failed keeps recent good data, marked stale (see
    /// [`App::row_stale`]).
    ///
    /// The first results select the top row, so the worst platform's
    /// incidents show at once; later ones keep the selected platform, and its
    /// highlighted incident (found by title and link) when it is still active.
    pub fn finish_refresh(&mut self, statuses: Vec<PlatformStatus>, now: DateTime<Utc>) {
        let first = self.last_refresh.is_none();
        let highlighted = self
            .highlighted_incident()
            .map(|index| self.selected_incidents()[index].clone());
        self.refreshing = false;
        self.last_refresh = Some(now);
        for (entry, status) in self.entries.iter_mut().zip(statuses) {
            entry.update(status, now);
        }
        self.reorder();
        if first {
            self.select(0);
        } else if let Some(was) = highlighted {
            let still_active = self
                .selected_incidents()
                .iter()
                .position(|i| i.title == was.title && i.url == was.url);
            if let Some(index) = still_active {
                self.highlighted = index;
            }
        }
    }

    /// Platform names in config order, whatever the row order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|entry| entry.name.as_str())
    }

    /// Platform names with their latest status (`None` until the first fetch
    /// lands), in display order.
    pub fn rows(&self) -> impl Iterator<Item = (&str, Option<&PlatformStatus>)> {
        self.order.iter().map(|&i| {
            let entry = &self.entries[i];
            (entry.name.as_str(), entry.status.as_ref())
        })
    }

    /// Why the platform at display row `row` shows stale data, if it does:
    /// its last refresh failed and its status is the last good one.
    #[must_use]
    pub fn row_stale(&self, row: usize) -> Option<&Stale> {
        self.entry_at(row)?.stale.as_ref()
    }

    /// The current order of the platform list.
    #[must_use]
    pub fn sort_order(&self) -> SortOrder {
        self.sort
    }

    /// The selected row, in display order.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The selected platform's name and latest status, if there are any platforms.
    #[must_use]
    pub fn selected_row(&self) -> Option<(&str, Option<&PlatformStatus>)> {
        self.rows().nth(self.selected)
    }

    #[must_use]
    pub fn is_refreshing(&self) -> bool {
        self.refreshing
    }

    /// When the last refresh finished.
    #[must_use]
    pub fn last_refresh(&self) -> Option<DateTime<Utc>> {
        self.last_refresh
    }

    #[must_use]
    pub fn should_quit(&self) -> bool {
        self.quit
    }

    /// Show `message` in the footer until [`App::clear_notice`].
    pub fn set_notice(&mut self, message: String) {
        self.notice = Some(message);
    }

    /// Take the footer message down, showing the key help again.
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// The footer message, if one is showing.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Store the latest read of the history file, replacing the one before.
    pub fn set_history(&mut self, history: History) {
        self.history = Some(history);
    }

    /// The history file as last read, `None` before the first read lands.
    #[must_use]
    pub fn history(&self) -> Option<&History> {
        self.history.as_ref()
    }
}

/// The AWS region picker's items: the built-in regions ([`aws::REGIONS`]),
/// then any code in `configured` or `feed` that is not one of them. The codes
/// in `configured` start ticked.
#[must_use]
pub fn region_items(configured: Option<&[String]>, feed: &[String]) -> Vec<Item> {
    let configured = configured.unwrap_or_default();
    let ticked = |code: &str| configured.iter().any(|c| c.eq_ignore_ascii_case(code));
    let mut items: Vec<Item> = aws::REGIONS
        .iter()
        .map(|&(code, name)| Item::new(code, name, ticked(code)))
        .collect();
    let extras = configured
        .iter()
        .map(|code| (code, "(in your config)"))
        .chain(feed.iter().map(|code| (code, "(in the live feed)")));
    for (code, label) in extras {
        let code = code.to_ascii_lowercase();
        if !items.iter().any(|item| item.key == code) {
            items.push(Item::new(&code, label, ticked(&code)));
        }
    }
    items
}

/// A Statuspage platform's component picker items: any name in `configured`
/// that names nothing in `tree` first (hint `not in feed`, ticked, so a save
/// keeps it), then `tree` in feed order, each group a heading with its
/// components under it. Names in `configured` start ticked, matched as the
/// `[components]` matcher does (trimmed, any case); a group's name covers
/// its components.
#[must_use]
pub fn component_items(configured: Option<&[String]>, tree: &[ComponentNode]) -> Vec<Item> {
    let normalize = name_key;
    let configured: Vec<(&String, String)> = configured
        .unwrap_or_default()
        .iter()
        .map(|name| (name, normalize(name)))
        .collect();
    let ticked = |name: &str| {
        let name = normalize(name);
        configured.iter().any(|(_, c)| *c == name)
    };
    let in_feed = |name: &str| {
        tree.iter()
            .flat_map(ComponentNode::names)
            .any(|n| normalize(n) == name)
    };
    let mut items: Vec<Item> = Vec::new();
    for (name, normalized) in &configured {
        let listed = items.iter().any(|item| normalize(&item.key) == *normalized);
        if !listed && !in_feed(normalized) {
            let mut item = Item::new(name.trim(), "", true);
            "not in feed".clone_into(&mut item.hint);
            items.push(item);
        }
    }
    for node in tree {
        match node {
            ComponentNode::Group { name, components } => {
                let heading = items.len();
                let mut item = Item::new(name, "", ticked(name));
                item.heading = true;
                item.hint = match components.len() {
                    1 => "1 component".to_owned(),
                    n => format!("{n} components"),
                };
                items.push(item);
                for component in components {
                    let mut item = Item::new(component, "", ticked(component));
                    item.parent = Some(heading);
                    items.push(item);
                }
            }
            ComponentNode::Component(name) => items.push(Item::new(name, "", ticked(name))),
        }
    }
    items
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use ratatui::crossterm::event::KeyEventState;

    use super::*;

    fn app() -> App {
        App::new(vec!["GitHub".into(), "Cloudflare".into(), "AWS".into()])
    }

    fn status(name: &str, level: Level) -> PlatformStatus {
        PlatformStatus {
            name: name.to_owned(),
            level,
            description: String::new(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: Utc.with_ymd_and_hms(2026, 11, 13, 19, 10, 0).unwrap(),
            error: None,
            probe: None,
            regions: Vec::new(),
            component_tree: Vec::new(),
        }
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn keys_map_to_actions() {
        let none = KeyModifiers::NONE;
        let cases = [
            (KeyCode::Up, Some(Action::Up)),
            (KeyCode::Char('k'), Some(Action::Up)),
            (KeyCode::Down, Some(Action::Down)),
            (KeyCode::Char('j'), Some(Action::Down)),
            (KeyCode::PageDown, Some(Action::PageDown)),
            (KeyCode::Char('J'), Some(Action::PageDown)),
            (KeyCode::PageUp, Some(Action::PageUp)),
            (KeyCode::Char('K'), Some(Action::PageUp)),
            (KeyCode::Tab, Some(Action::NextIncident)),
            (KeyCode::BackTab, Some(Action::PrevIncident)),
            (KeyCode::Char('o'), Some(Action::Open)),
            (KeyCode::Char('s'), Some(Action::ToggleSort)),
            (KeyCode::Char('r'), Some(Action::Refresh)),
            (KeyCode::Char('t'), Some(Action::Timeline)),
            (KeyCode::Char('q'), Some(Action::Quit)),
            (KeyCode::Esc, Some(Action::Back)),
            (KeyCode::Char('c'), None),
            (KeyCode::Char('x'), None),
        ];
        for (code, expected) in cases {
            assert_eq!(Action::from_key(press(code, none)), expected, "{code:?}");
        }
        let ctrl_c = press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(Action::from_key(ctrl_c), Some(Action::Quit));
        let ctrl_r = press(KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(Action::from_key(ctrl_r), None);
        // Terminals report Shift-J as an uppercase 'J', usually with SHIFT set.
        let shift_j = press(KeyCode::Char('J'), KeyModifiers::SHIFT);
        assert_eq!(Action::from_key(shift_j), Some(Action::PageDown));
        // Shift-Tab arrives as BackTab with SHIFT set.
        let shift_tab = press(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(Action::from_key(shift_tab), Some(Action::PrevIncident));
    }

    #[test]
    fn key_releases_are_ignored() {
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            state: KeyEventState::NONE,
            ..press(KeyCode::Char('q'), KeyModifiers::NONE)
        };
        assert_eq!(Action::from_key(release), None);
    }

    #[test]
    fn selection_moves_and_stays_in_bounds() {
        let mut app = app();
        assert_eq!(app.selected(), 0);
        app.apply(Action::Up);
        assert_eq!(app.selected(), 0, "cannot go above the first row");
        app.apply(Action::Down);
        app.apply(Action::Down);
        assert_eq!(app.selected(), 2);
        app.apply(Action::Down);
        assert_eq!(app.selected(), 2, "cannot go below the last row");
        app.apply(Action::Up);
        assert_eq!(app.selected_row().unwrap().0, "Cloudflare");
    }

    /// 30 lines of content in a 10-line pane: pages move 9 lines, the end is at 20.
    fn thirty_lines_in_ten() -> App {
        let mut app = app();
        app.set_viewport(DetailViewport {
            content_lines: 30,
            height: 10,
            incident_lines: Vec::new(),
        });
        app
    }

    #[test]
    fn page_keys_scroll_the_detail_pane_and_stop_at_the_ends() {
        let mut app = thirty_lines_in_ten();
        assert_eq!(app.detail_scroll(), 0);
        app.apply(Action::PageUp);
        assert_eq!(app.detail_scroll(), 0, "cannot scroll above the top");
        app.apply(Action::PageDown);
        assert_eq!(app.detail_scroll(), 9);
        app.apply(Action::PageDown);
        app.apply(Action::PageDown);
        assert_eq!(
            app.detail_scroll(),
            20,
            "stops with the last line at the bottom"
        );
        app.apply(Action::PageUp);
        assert_eq!(app.detail_scroll(), 11, "one press moves back at once");
        assert_eq!(app.selected(), 0, "scrolling leaves the selection alone");
    }

    #[test]
    fn content_that_fits_does_not_scroll() {
        let mut app = app();
        app.apply(Action::PageDown);
        assert_eq!(app.detail_scroll(), 0, "nothing drawn yet");
        app.set_viewport(DetailViewport {
            content_lines: 5,
            height: 10,
            incident_lines: Vec::new(),
        });
        app.apply(Action::PageDown);
        assert_eq!(app.detail_scroll(), 0);
    }

    #[test]
    fn changing_the_selection_scrolls_back_to_the_top() {
        let mut app = thirty_lines_in_ten();
        app.apply(Action::PageDown);
        app.apply(Action::Down);
        assert_eq!(app.detail_scroll(), 0);

        app.apply(Action::PageDown);
        app.apply(Action::Up);
        assert_eq!(app.detail_scroll(), 0);
    }

    #[test]
    fn staying_on_the_same_row_keeps_the_scroll() {
        let mut app = thirty_lines_in_ten();
        app.apply(Action::PageDown);
        app.apply(Action::Up);
        assert_eq!(app.selected(), 0);
        assert_eq!(
            app.detail_scroll(),
            9,
            "Up on the first row changes nothing"
        );
    }

    #[test]
    fn a_pane_with_no_room_does_not_scroll() {
        let mut app = app();
        app.set_viewport(DetailViewport {
            content_lines: 30,
            height: 0,
            incident_lines: Vec::new(),
        });
        app.apply(Action::PageDown);
        assert_eq!(app.detail_scroll(), 0);
    }

    #[test]
    fn shorter_content_pulls_the_scroll_back() {
        let mut app = thirty_lines_in_ten();
        app.apply(Action::PageDown);
        app.apply(Action::PageDown);
        app.set_viewport(DetailViewport {
            content_lines: 12,
            height: 10,
            incident_lines: Vec::new(),
        });
        assert_eq!(app.detail_scroll(), 2);
    }

    #[test]
    fn selection_is_safe_with_no_platforms() {
        let mut app = App::new(Vec::new());
        app.apply(Action::Down);
        app.apply(Action::Up);
        assert_eq!(app.selected(), 0);
        assert!(app.selected_row().is_none());
    }

    #[test]
    fn refreshes_never_overlap() {
        let mut app = app();
        assert_eq!(app.apply(Action::Refresh), Some(Effect::Refresh));
        assert!(app.is_refreshing());
        assert_eq!(
            app.apply(Action::Refresh),
            None,
            "a refresh is already running"
        );
        assert!(!app.start_refresh());
        app.finish_refresh(Vec::new(), Utc::now());
        assert!(!app.is_refreshing());
        assert!(app.start_refresh());
    }

    fn names(app: &App) -> Vec<&str> {
        app.rows().map(|(name, _)| name).collect()
    }

    /// GitHub, Cloudflare, AWS at these levels, in that order.
    fn refresh(app: &mut App, levels: [Level; 3]) {
        app.start_refresh();
        let statuses = ["GitHub", "Cloudflare", "AWS"]
            .into_iter()
            .zip(levels)
            .map(|(name, level)| status(name, level))
            .collect();
        app.finish_refresh(statuses, Utc::now());
    }

    #[test]
    fn rows_sort_worst_first_with_ties_in_config_order() {
        use Level::{Degraded, Operational, Outage};
        let mut app = app();
        assert_eq!(app.sort_order(), SortOrder::WorstFirst);
        assert_eq!(names(&app), ["GitHub", "Cloudflare", "AWS"], "no data yet");
        refresh(&mut app, [Degraded, Operational, Outage]);
        assert_eq!(names(&app), ["AWS", "GitHub", "Cloudflare"]);
        refresh(&mut app, [Degraded, Operational, Degraded]);
        assert_eq!(
            names(&app),
            ["GitHub", "AWS", "Cloudflare"],
            "ties keep config order"
        );
    }

    #[test]
    fn s_toggles_config_order_and_back() {
        use Level::{Operational, Outage, Unknown};
        let mut app = app();
        refresh(&mut app, [Operational, Unknown, Outage]);
        assert_eq!(names(&app), ["AWS", "Cloudflare", "GitHub"]);
        assert_eq!(app.apply(Action::ToggleSort), None);
        assert_eq!(app.sort_order(), SortOrder::Config);
        assert_eq!(names(&app), ["GitHub", "Cloudflare", "AWS"]);
        refresh(&mut app, [Outage, Operational, Operational]);
        assert_eq!(
            names(&app),
            ["GitHub", "Cloudflare", "AWS"],
            "a refresh keeps config order"
        );
        app.apply(Action::ToggleSort);
        assert_eq!(app.sort_order(), SortOrder::WorstFirst);
        assert_eq!(names(&app), ["GitHub", "Cloudflare", "AWS"]);
    }

    #[test]
    fn the_first_results_select_the_worst_platform() {
        use Level::{Operational, Outage};
        let mut app = app();
        app.apply(Action::Down);
        assert_eq!(app.selected_row().unwrap().0, "Cloudflare", "no data yet");
        refresh(&mut app, [Operational, Operational, Outage]);
        assert_eq!(app.selected(), 0);
        assert_eq!(app.selected_row().unwrap().0, "AWS");
    }

    #[test]
    fn selection_follows_the_platform_when_rows_reorder() {
        use Level::{Degraded, Operational, Outage};
        let mut app = thirty_lines_in_ten();
        refresh(&mut app, [Operational, Operational, Operational]);
        app.apply(Action::Down);
        app.apply(Action::PageDown);
        assert_eq!(app.selected_row().unwrap().0, "Cloudflare");

        refresh(&mut app, [Operational, Outage, Degraded]);
        assert_eq!(app.selected(), 0, "Cloudflare moved to the top");
        let (name, status) = app.selected_row().unwrap();
        assert_eq!(name, "Cloudflare");
        assert_eq!(
            status.unwrap().level,
            Outage,
            "the detail shows its new data"
        );
        assert_eq!(app.detail_scroll(), 9, "same platform, same scroll");

        app.apply(Action::ToggleSort);
        assert_eq!(app.selected(), 1);
        assert_eq!(app.selected_row().unwrap().0, "Cloudflare");
    }

    #[test]
    fn finished_refresh_fills_rows_in_order() {
        let mut app = app();
        app.apply(Action::ToggleSort);
        assert!(app.rows().all(|(_, status)| status.is_none()));
        assert_eq!(app.last_refresh(), None);

        let now = Utc.with_ymd_and_hms(2026, 11, 13, 19, 11, 0).unwrap();
        app.start_refresh();
        app.finish_refresh(
            vec![
                status("GitHub", Level::Operational),
                status("Cloudflare", Level::Degraded),
                status("AWS", Level::Unknown),
            ],
            now,
        );
        let levels: Vec<_> = app.rows().map(|(_, s)| s.unwrap().level).collect();
        assert_eq!(
            levels,
            [Level::Operational, Level::Degraded, Level::Unknown]
        );
        assert_eq!(app.last_refresh(), Some(now));
    }

    /// 19:10, the time every `status` was fetched at.
    fn good_fetch() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 13, 19, 10, 0).unwrap()
    }

    /// GitHub Degraded with one incident, fetched at 19:10; Cloudflare and AWS
    /// Operational. Config order.
    fn app_with_good_data() -> App {
        let mut app = app();
        app.apply(Action::ToggleSort);
        app.start_refresh();
        let mut github = status("GitHub", Level::Degraded);
        github.description = "Partial outage".to_owned();
        github.incidents = vec![incident("Actions delayed", None)];
        github.resolved = vec![incident("Webhooks delayed", None)];
        app.finish_refresh(
            vec![
                github,
                status("Cloudflare", Level::Operational),
                status("AWS", Level::Operational),
            ],
            good_fetch(),
        );
        app
    }

    /// A refresh at `now` in which GitHub fails and the others succeed.
    fn github_fails(app: &mut App, now: DateTime<Utc>) {
        app.start_refresh();
        let mut cloudflare = status("Cloudflare", Level::Operational);
        cloudflare.fetched_at = now;
        let mut aws = status("AWS", Level::Operational);
        aws.fetched_at = now;
        app.finish_refresh(
            vec![
                PlatformStatus::unknown("GitHub", "timed out after 10s", now),
                cloudflare,
                aws,
            ],
            now,
        );
    }

    #[test]
    fn a_failed_refresh_keeps_recent_good_data_marked_stale() {
        let mut app = app_with_good_data();
        assert!(app.row_stale(0).is_none());
        let now = good_fetch() + TimeDelta::minutes(4);
        github_fails(&mut app, now);

        let (name, status) = app.selected_row().unwrap();
        assert_eq!(name, "GitHub");
        let status = status.unwrap();
        assert_eq!(status.level, Level::Degraded, "keeps the last good level");
        assert_eq!(status.description, "Partial outage");
        assert_eq!(status.incidents.len(), 1);
        assert_eq!(status.resolved.len(), 1, "keeps the resolved list too");
        assert_eq!(status.fetched_at, good_fetch(), "the data's own time");
        assert!(status.error.is_none());
        let stale = app.row_stale(0).expect("marked stale");
        assert_eq!(stale.error, "timed out after 10s");
        assert_eq!(stale.failed_at, now);
        assert!(app.row_stale(1).is_none(), "Cloudflare fetched fine");
    }

    #[test]
    fn kept_stale_data_shows_the_failed_refreshs_probe() {
        let mut app = app_with_good_data();
        let now = good_fetch() + TimeDelta::minutes(4);
        app.start_refresh();
        let mut failed = PlatformStatus::unknown("GitHub", "HTTP 502", now);
        failed.probe = Some(crate::model::Probe {
            url: "https://github.com/".to_owned(),
            ok: false,
            code: None,
            elapsed_ms: None,
            description: "no answer (timed out after 9s)".to_owned(),
        });
        let probe = failed.probe.clone();
        app.finish_refresh(
            vec![
                failed,
                status("Cloudflare", Level::Operational),
                status("AWS", Level::Operational),
            ],
            now,
        );
        let status = app.selected_row().unwrap().1.unwrap();
        assert!(app.row_stale(0).is_some());
        assert_eq!(status.level, Level::Degraded, "the kept level");
        assert_eq!(status.probe, probe, "the new probe");
    }

    #[test]
    fn repeated_failures_keep_the_data_up_to_the_stale_limit() {
        let mut app = app_with_good_data();
        github_fails(&mut app, good_fetch() + TimeDelta::minutes(4));
        let at_limit = good_fetch() + STALE_LIMIT;
        github_fails(&mut app, at_limit);
        let status = app.selected_row().unwrap().1.unwrap();
        assert_eq!(status.level, Level::Degraded, "still kept at the limit");
        assert_eq!(app.row_stale(0).unwrap().failed_at, at_limit);

        github_fails(&mut app, at_limit + TimeDelta::seconds(1));
        let status = app.selected_row().unwrap().1.unwrap();
        assert_eq!(status.level, Level::Unknown, "too old: falls back");
        assert_eq!(status.error.as_deref(), Some("timed out after 10s"));
        assert!(app.row_stale(0).is_none());
    }

    #[test]
    fn a_failure_after_the_stale_limit_shows_unknown() {
        let mut app = app_with_good_data();
        github_fails(&mut app, good_fetch() + TimeDelta::minutes(11));
        let status = app.selected_row().unwrap().1.unwrap();
        assert_eq!(status.level, Level::Unknown);
        assert_eq!(status.incidents, []);
        assert!(app.row_stale(0).is_none());
    }

    #[test]
    fn a_good_fetch_clears_the_stale_mark() {
        let mut app = app_with_good_data();
        let failed = good_fetch() + TimeDelta::minutes(2);
        github_fails(&mut app, failed);
        assert!(app.row_stale(0).is_some());

        let now = failed + TimeDelta::minutes(1);
        let mut github = status("GitHub", Level::Operational);
        github.fetched_at = now;
        app.start_refresh();
        app.finish_refresh(
            vec![
                github,
                status("Cloudflare", Level::Operational),
                status("AWS", Level::Operational),
            ],
            now,
        );
        assert!(app.row_stale(0).is_none());
        let status = app.selected_row().unwrap().1.unwrap();
        assert_eq!(status.level, Level::Operational);
        assert_eq!(status.fetched_at, now);
    }

    #[test]
    fn a_failure_with_no_earlier_good_data_shows_unknown() {
        let mut app = app();
        app.apply(Action::ToggleSort);
        let now = good_fetch();
        github_fails(&mut app, now);
        let status = app.selected_row().unwrap().1.unwrap();
        assert_eq!(status.level, Level::Unknown);
        assert!(app.row_stale(0).is_none());

        // Unknown followed by another failure stays Unknown, not "stale Unknown".
        github_fails(&mut app, now + TimeDelta::minutes(1));
        assert!(app.row_stale(0).is_none());
    }

    #[test]
    fn a_stale_row_sorts_by_its_kept_level() {
        use Level::{Degraded, Operational};
        let mut app = app();
        refresh(&mut app, [Operational, Degraded, Operational]);
        assert_eq!(names(&app), ["Cloudflare", "GitHub", "AWS"]);
        // `refresh` stamps every status 19:10, so a failure a minute later is
        // well inside the limit.
        let now = good_fetch() + TimeDelta::minutes(1);
        app.start_refresh();
        app.finish_refresh(
            vec![
                PlatformStatus::unknown("GitHub", "timed out after 10s", now),
                status("Cloudflare", Degraded),
                status("AWS", Operational),
            ],
            now,
        );
        assert_eq!(
            names(&app),
            ["Cloudflare", "GitHub", "AWS"],
            "GitHub ranks as its kept Operational, not as Unknown"
        );
        assert!(app.row_stale(1).is_some(), "the stale mark follows the row");
        assert!(app.row_stale(0).is_none());
    }

    fn incident(title: &str, url: Option<&str>) -> crate::model::Incident {
        crate::model::Incident {
            kind: crate::model::IncidentKind::Incident,
            title: title.to_owned(),
            components: Vec::new(),
            started_at: None,
            url: url.map(str::to_owned),
            ..Default::default()
        }
    }

    /// With status pages: GitHub Degraded with three incidents (the second
    /// has no link), Cloudflare Operational, AWS Unknown. Worst first: AWS,
    /// GitHub, Cloudflare; AWS is selected.
    fn app_with_incidents() -> App {
        let mut app = App::with_status_pages(
            [
                ("GitHub", "https://www.githubstatus.com"),
                ("Cloudflare", "https://www.cloudflarestatus.com"),
                ("AWS", "https://health.aws.amazon.com/health/status"),
            ]
            .map(|(name, page)| (name.to_owned(), page.to_owned()))
            .to_vec(),
        );
        let mut github = status("GitHub", Level::Degraded);
        github.incidents = vec![
            incident(
                "Actions delays",
                Some("https://www.githubstatus.com/incidents/a"),
            ),
            incident("Pages builds", None),
            incident(
                "API errors",
                Some("https://www.githubstatus.com/incidents/c"),
            ),
        ];
        let mut aws = status("AWS", Level::Unknown);
        aws.error = Some("timed out after 10s".to_owned());
        app.start_refresh();
        app.finish_refresh(
            vec![github, status("Cloudflare", Level::Operational), aws],
            Utc::now(),
        );
        app
    }

    /// `app_with_incidents` with GitHub selected.
    fn github_selected() -> App {
        let mut app = app_with_incidents();
        app.apply(Action::Down);
        assert_eq!(app.selected_row().unwrap().0, "GitHub");
        app
    }

    #[test]
    fn tab_and_shift_tab_move_the_highlight_and_wrap() {
        let mut app = github_selected();
        assert_eq!(app.highlighted_incident(), Some(0), "starts on the first");
        app.apply(Action::NextIncident);
        app.apply(Action::NextIncident);
        assert_eq!(app.highlighted_incident(), Some(2));
        app.apply(Action::NextIncident);
        assert_eq!(app.highlighted_incident(), Some(0), "wraps past the last");
        app.apply(Action::PrevIncident);
        assert_eq!(
            app.highlighted_incident(),
            Some(2),
            "wraps before the first"
        );
        app.apply(Action::PrevIncident);
        assert_eq!(app.highlighted_incident(), Some(1));
    }

    #[test]
    fn the_highlight_resets_when_the_platform_changes() {
        let mut app = github_selected();
        app.apply(Action::NextIncident);
        app.apply(Action::Down);
        assert_eq!(app.highlighted_incident(), None, "Cloudflare has none");
        app.apply(Action::Up);
        assert_eq!(app.highlighted_incident(), Some(0));
    }

    #[test]
    fn the_highlight_stays_on_the_platform_through_a_reorder() {
        let mut app = github_selected();
        app.apply(Action::NextIncident);
        app.apply(Action::ToggleSort);
        assert_eq!(app.selected_row().unwrap().0, "GitHub");
        assert_eq!(app.highlighted_incident(), Some(1));
    }

    #[test]
    fn with_no_incidents_nothing_is_highlighted() {
        let mut app = app_with_incidents();
        assert_eq!(app.selected_row().unwrap().0, "AWS");
        app.apply(Action::NextIncident);
        app.apply(Action::PrevIncident);
        assert_eq!(app.highlighted_incident(), None);
    }

    #[test]
    fn a_shorter_incident_list_clamps_the_highlight() {
        let mut app = github_selected();
        app.apply(Action::PrevIncident);
        let mut github = status("GitHub", Level::Degraded);
        github.incidents = vec![incident("Only one", None)];
        app.start_refresh();
        app.finish_refresh(
            vec![
                github,
                status("Cloudflare", Level::Operational),
                status("AWS", Level::Unknown),
            ],
            Utc::now(),
        );
        assert_eq!(app.selected_row().unwrap().0, "GitHub");
        assert_eq!(app.highlighted_incident(), Some(0));
    }

    /// 40 lines in a 10-line pane, with incidents starting on these lines.
    fn pane_with_incidents_at(incident_lines: &[u16]) -> DetailViewport {
        DetailViewport {
            content_lines: 40,
            height: 10,
            incident_lines: incident_lines.to_vec(),
        }
    }

    #[test]
    fn tab_scrolls_an_incident_out_of_view_to_the_top() {
        let mut app = github_selected();
        let pane = pane_with_incidents_at(&[5, 14, 36]);
        app.set_viewport(pane.clone());
        assert_eq!(app.detail_scroll(), 0);

        app.apply(Action::NextIncident);
        assert_eq!(
            app.scroll_for(&pane),
            14,
            "the view draws with the new scroll"
        );
        app.set_viewport(pane.clone());
        assert_eq!(app.detail_scroll(), 14);

        app.apply(Action::NextIncident);
        app.set_viewport(pane.clone());
        assert_eq!(app.detail_scroll(), 30, "no further than the end");

        app.apply(Action::NextIncident);
        app.set_viewport(pane);
        assert_eq!(app.detail_scroll(), 5, "wrapping scrolls back up");
    }

    #[test]
    fn tab_leaves_the_scroll_alone_when_the_incident_is_in_view() {
        let mut app = github_selected();
        let pane = pane_with_incidents_at(&[2, 7, 36]);
        app.set_viewport(pane.clone());
        app.apply(Action::NextIncident);
        app.set_viewport(pane);
        assert_eq!(app.detail_scroll(), 0, "line 7 is on screen (0..10)");
    }

    #[test]
    fn paging_moves_the_highlight_to_an_incident_on_screen() {
        let mut app = github_selected();
        app.set_viewport(pane_with_incidents_at(&[5, 14, 36]));
        let mut page = |action, scroll, highlighted| {
            app.apply(action);
            app.set_viewport(pane_with_incidents_at(&[5, 14, 36]));
            assert_eq!(
                (app.detail_scroll(), app.highlighted_incident()),
                (scroll, Some(highlighted)),
                "{action:?}"
            );
        };
        // Rows 9..19: incident 0 (line 5) is off screen; 1 (line 14) is the first on it.
        page(Action::PageDown, 9, 1);
        // Rows 18..28: no incident starts on screen; incident 1's body fills it.
        page(Action::PageDown, 18, 1);
        page(Action::PageDown, 27, 2);
        page(Action::PageUp, 18, 1);
        page(Action::PageUp, 9, 1);
        page(Action::PageUp, 0, 0);
    }

    #[test]
    fn page_keys_do_not_pull_the_view_back_to_the_highlight() {
        let mut app = github_selected();
        let pane = pane_with_incidents_at(&[5, 14, 36]);
        app.set_viewport(pane.clone());
        app.apply(Action::NextIncident);
        app.set_viewport(pane.clone());
        assert_eq!(app.detail_scroll(), 14);
        app.apply(Action::PageUp);
        app.set_viewport(pane);
        assert_eq!(app.detail_scroll(), 5, "one page up from 14");
    }

    #[test]
    fn a_refresh_keeps_the_same_incident_highlighted() {
        let mut app = github_selected();
        app.apply(Action::NextIncident);
        let mut github = status("GitHub", Level::Degraded);
        github.incidents = vec![
            incident("New outage", None),
            incident(
                "Actions delays",
                Some("https://www.githubstatus.com/incidents/a"),
            ),
            incident("Pages builds", None),
        ];
        app.start_refresh();
        app.finish_refresh(
            vec![
                github,
                status("Cloudflare", Level::Operational),
                status("AWS", Level::Unknown),
            ],
            Utc::now(),
        );
        assert_eq!(app.highlighted_incident(), Some(2), "still Pages builds");
    }

    #[test]
    fn o_opens_the_highlighted_incident() {
        let mut app = github_selected();
        assert_eq!(
            app.apply(Action::Open),
            Some(Effect::Open(
                "https://www.githubstatus.com/incidents/a".to_owned()
            ))
        );
        app.apply(Action::PrevIncident);
        assert_eq!(
            app.apply(Action::Open),
            Some(Effect::Open(
                "https://www.githubstatus.com/incidents/c".to_owned()
            ))
        );
    }

    #[test]
    fn o_opens_the_status_page_for_an_incident_without_a_link() {
        let mut app = github_selected();
        app.apply(Action::NextIncident);
        assert_eq!(
            app.apply(Action::Open),
            Some(Effect::Open("https://www.githubstatus.com".to_owned()))
        );
    }

    #[test]
    fn o_opens_the_status_page_for_operational_and_unknown_platforms() {
        let mut app = app_with_incidents();
        assert_eq!(
            app.apply(Action::Open),
            Some(Effect::Open(
                "https://health.aws.amazon.com/health/status".to_owned()
            )),
            "AWS is Unknown"
        );
        app.apply(Action::Down);
        app.apply(Action::Down);
        assert_eq!(
            app.selected_row().unwrap().1.unwrap().level,
            Level::Operational
        );
        assert_eq!(
            app.apply(Action::Open),
            Some(Effect::Open("https://www.cloudflarestatus.com".to_owned()))
        );
    }

    #[test]
    fn o_opens_the_status_page_before_data_arrives() {
        let mut app = App::with_status_pages(vec![(
            "GitHub".to_owned(),
            "https://www.githubstatus.com".to_owned(),
        )]);
        assert_eq!(
            app.apply(Action::Open),
            Some(Effect::Open("https://www.githubstatus.com".to_owned()))
        );
    }

    #[test]
    fn o_does_nothing_with_nothing_to_open() {
        assert_eq!(App::new(Vec::new()).apply(Action::Open), None);
        assert_eq!(app().apply(Action::Open), None, "no status pages known");
    }

    #[test]
    fn notices_show_until_cleared() {
        let mut app = app();
        assert_eq!(app.notice(), None);
        app.set_notice("Could not open".to_owned());
        assert_eq!(app.notice(), Some("Could not open"));
        app.clear_notice();
        assert_eq!(app.notice(), None);
    }

    #[test]
    fn esc_quits_from_the_list() {
        let mut app = app();
        assert_eq!(app.apply(Action::Back), None);
        assert!(app.should_quit());
    }

    #[test]
    fn t_opens_the_timeline_and_t_or_esc_closes_it_keeping_the_selection() {
        let mut app = app();
        app.apply(Action::Down);
        assert!(!app.timeline_open());
        app.apply(Action::Timeline);
        assert!(app.timeline_open());
        app.apply(Action::Timeline);
        assert!(!app.timeline_open());
        app.apply(Action::Timeline);
        app.apply(Action::Back);
        assert!(!app.timeline_open(), "Esc closes it");
        assert!(!app.should_quit(), "Esc does not quit while it is open");
        assert_eq!(app.selected_row().unwrap().0, "Cloudflare");
    }

    #[test]
    fn q_quits_from_the_timeline() {
        let mut app = app();
        app.apply(Action::Timeline);
        app.apply(Action::Quit);
        assert!(app.should_quit());
    }

    /// `app` with the timeline open, drawn as 30 lines on a 10-line screen.
    fn timeline_thirty_in_ten() -> App {
        let mut app = app();
        app.apply(Action::Timeline);
        app.set_viewport(DetailViewport {
            content_lines: 30,
            height: 10,
            incident_lines: Vec::new(),
        });
        app
    }

    #[test]
    fn arrow_and_page_keys_scroll_the_timeline_not_the_list() {
        let mut app = timeline_thirty_in_ten();
        app.apply(Action::Up);
        assert_eq!(app.timeline_scroll(), 0, "cannot scroll above the top");
        app.apply(Action::Down);
        app.apply(Action::Down);
        assert_eq!(app.timeline_scroll(), 2);
        app.apply(Action::PageDown);
        assert_eq!(app.timeline_scroll(), 11);
        app.apply(Action::PageDown);
        app.apply(Action::PageDown);
        assert_eq!(app.timeline_scroll(), 20, "stops at the end");
        app.apply(Action::Down);
        assert_eq!(app.timeline_scroll(), 20);
        app.apply(Action::PageUp);
        assert_eq!(app.timeline_scroll(), 11);
        app.apply(Action::Up);
        assert_eq!(app.timeline_scroll(), 10);
        assert_eq!(app.selected(), 0, "the selection stays put");
        assert_eq!(app.detail_scroll(), 0, "so does the detail pane");
    }

    #[test]
    fn list_keys_do_nothing_in_the_timeline() {
        let mut app = github_selected();
        app.apply(Action::Timeline);
        assert_eq!(app.apply(Action::Open), None);
        app.apply(Action::ToggleSort);
        assert_eq!(app.sort_order(), SortOrder::WorstFirst);
        app.apply(Action::NextIncident);
        app.apply(Action::Timeline);
        assert_eq!(app.highlighted_incident(), Some(0));
    }

    #[test]
    fn a_refresh_in_the_timeline_keeps_its_scroll() {
        let mut app = timeline_thirty_in_ten();
        app.apply(Action::PageDown);
        assert_eq!(app.apply(Action::Refresh), Some(Effect::Refresh));
        app.finish_refresh(Vec::new(), Utc::now());
        app.set_history(History::Records(Vec::new()));
        app.set_viewport(DetailViewport {
            content_lines: 32,
            height: 10,
            incident_lines: Vec::new(),
        });
        assert_eq!(app.timeline_scroll(), 9, "same place in longer content");
        app.set_viewport(DetailViewport {
            content_lines: 14,
            height: 10,
            incident_lines: Vec::new(),
        });
        assert_eq!(app.timeline_scroll(), 4, "pulled back to the end");
    }

    #[test]
    fn reopening_the_timeline_starts_at_the_top() {
        let mut app = timeline_thirty_in_ten();
        app.apply(Action::PageDown);
        app.apply(Action::Back);
        app.apply(Action::Timeline);
        assert_eq!(app.timeline_scroll(), 0);
    }

    #[test]
    fn the_timeline_viewport_leaves_the_detail_scroll_alone() {
        let mut app = thirty_lines_in_ten();
        app.apply(Action::PageDown);
        app.apply(Action::Timeline);
        app.set_viewport(DetailViewport::default());
        app.apply(Action::Timeline);
        assert_eq!(app.detail_scroll(), 9);
    }

    #[test]
    fn quit_sets_the_flag() {
        let mut app = app();
        assert!(!app.should_quit());
        assert_eq!(app.apply(Action::Quit), None);
        assert!(app.should_quit());
    }

    fn key(code: KeyCode) -> KeyEvent {
        press(code, KeyModifiers::NONE)
    }

    /// GitHub, Cloudflare, AWS (limited to us-west-2 and a code the built-in
    /// list lacks), with AWS selected and an event in another new region.
    fn aws_app() -> App {
        let mut app = app();
        app.set_filter(
            "AWS",
            Filter::AwsRegions(Some(vec!["us-west-2".into(), "xx-config-1".into()])),
        );
        let mut aws = status("AWS", Level::Degraded);
        aws.regions = vec!["us-west-2".into(), "xx-feed-1".into()];
        app.finish_refresh(
            vec![
                status("GitHub", Level::Operational),
                status("Cloudflare", Level::Operational),
                aws,
            ],
            Utc::now(),
        );
        assert_eq!(app.selected_row().unwrap().0, "AWS");
        app
    }

    #[test]
    fn f_maps_to_the_filter_action() {
        assert_eq!(
            Action::from_key(key(KeyCode::Char('f'))),
            Some(Action::Filter)
        );
    }

    #[test]
    fn f_on_aws_opens_the_region_picker_with_the_configured_regions_ticked() {
        let mut app = aws_app();
        assert_eq!(app.key(key(KeyCode::Char('f'))), None);
        let picker = app.picker().expect("the picker is open");
        assert_eq!(picker.title(), "AWS regions");
        let ticked: Vec<&str> = picker
            .items()
            .iter()
            .filter(|item| item.ticked)
            .map(|item| item.key.as_str())
            .collect();
        assert_eq!(ticked, ["us-west-2", "xx-config-1"]);
        let keys: Vec<&str> = picker
            .items()
            .iter()
            .map(|item| item.key.as_str())
            .collect();
        assert_eq!(keys.len(), aws::REGIONS.len() + 2);
        assert_eq!(&keys[keys.len() - 2..], ["xx-config-1", "xx-feed-1"]);
    }

    #[test]
    fn f_on_a_platform_without_a_filter_shows_a_notice() {
        let mut app = aws_app();
        app.apply(Action::Down);
        let name = app.selected_row().unwrap().0.to_owned();
        let effect = app.key(key(KeyCode::Char('f')));
        assert_eq!(
            effect,
            Some(Effect::Notice(format!(
                "{name} can't be filtered (only AWS regions and Statuspage components can be picked)"
            )))
        );
        assert!(app.picker().is_none());
    }

    #[test]
    fn picker_keys_tick_and_save_or_cancel() {
        let mut app = aws_app();
        app.key(key(KeyCode::Char('f')));
        // us-east-1 is first: tick it. Untick us-west-2, the fourth.
        app.key(key(KeyCode::Char(' ')));
        for _ in 0..3 {
            app.key(key(KeyCode::Char('j')));
        }
        app.key(key(KeyCode::Char(' ')));
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            Some(Effect::Save(Pick::AwsRegions(vec![
                "us-east-1".into(),
                "xx-config-1".into()
            ])))
        );
        assert!(app.picker().is_none(), "saving closes the picker");

        app.key(key(KeyCode::Char('f')));
        app.key(key(KeyCode::Char(' ')));
        assert_eq!(app.key(key(KeyCode::Esc)), None);
        assert!(app.picker().is_none(), "Esc closes it");
        assert!(!app.should_quit(), "Esc in the picker does not quit");
    }

    #[test]
    fn list_keys_do_nothing_while_the_picker_is_open() {
        let mut app = aws_app();
        app.key(key(KeyCode::Char('f')));
        for code in [
            KeyCode::Char('r'),
            KeyCode::Char('o'),
            KeyCode::Tab,
            KeyCode::Char('s'),
        ] {
            assert_eq!(app.key(key(code)), None, "{code:?}");
        }
        assert_eq!(app.sort_order(), SortOrder::WorstFirst);
        assert!(app.picker().is_some());
        app.key(key(KeyCode::Char('q')));
        assert!(app.should_quit());
    }

    #[test]
    fn f_does_nothing_in_the_timeline() {
        let mut app = aws_app();
        app.apply(Action::Timeline);
        assert_eq!(app.key(key(KeyCode::Char('f'))), None);
        assert!(app.picker().is_none());
        assert!(app.timeline_open());
    }

    #[test]
    fn the_picker_leaves_the_detail_scroll_alone() {
        let mut app = aws_app();
        app.set_viewport(DetailViewport {
            content_lines: 30,
            height: 10,
            incident_lines: Vec::new(),
        });
        app.apply(Action::PageDown);
        app.key(key(KeyCode::Char('f')));
        app.set_viewport(DetailViewport::default());
        app.key(key(KeyCode::Esc));
        assert_eq!(app.detail_scroll(), 9);
    }

    #[test]
    fn region_items_add_unknown_codes_once_after_the_built_ins() {
        let configured = vec!["US-WEST-2".to_owned(), "xx-new-1".to_owned()];
        let feed = vec![
            "xx-new-1".to_owned(),
            "us-east-1".to_owned(),
            "yy-new-2".to_owned(),
        ];
        let items = region_items(Some(&configured), &feed);
        assert_eq!(items.len(), aws::REGIONS.len() + 2);
        let extras: Vec<(&str, &str, bool)> = items[aws::REGIONS.len()..]
            .iter()
            .map(|item| (item.key.as_str(), item.label.as_str(), item.ticked))
            .collect();
        assert_eq!(
            extras,
            [
                ("xx-new-1", "(in your config)", true),
                ("yy-new-2", "(in the live feed)", false)
            ]
        );
        assert!(
            items
                .iter()
                .find(|item| item.key == "us-west-2")
                .unwrap()
                .ticked
        );
        assert!(region_items(None, &[]).iter().all(|item| !item.ticked));
    }

    fn fixture(name: &str, body: &[u8], watch: Option<&[String]>) -> PlatformStatus {
        crate::providers::statuspage::parse_watching(name, body, Utc::now(), watch).unwrap()
    }

    const GITHUB: &[u8] = include_bytes!("../../tests/fixtures/github_summary.json");
    const CLOUDFLARE: &[u8] = include_bytes!("../../tests/fixtures/cloudflare_summary.json");

    /// GitHub and Cloudflare from their fixtures, GitHub watching `watch`.
    fn components_app(watch: &[&str]) -> App {
        let watch: Vec<String> = watch.iter().map(|&n| n.to_owned()).collect();
        let watch = (!watch.is_empty()).then_some(watch);
        let mut app = App::new(vec!["GitHub".into(), "Cloudflare".into()]);
        app.set_filter("GitHub", Filter::Components(watch.clone()));
        app.set_filter("Cloudflare", Filter::Components(None));
        app.finish_refresh(
            vec![
                fixture("GitHub", GITHUB, watch.as_deref()),
                fixture("Cloudflare", CLOUDFLARE, None),
            ],
            Utc::now(),
        );
        app.sort = SortOrder::Config;
        app.reorder();
        app.select(0);
        assert_eq!(app.selected_row().unwrap().0, "GitHub");
        app
    }

    fn keys(app: &App) -> Vec<(String, bool)> {
        let picker = app.picker().unwrap();
        picker
            .visible()
            .iter()
            .map(|&i| (picker.items()[i].key.clone(), picker.items()[i].ticked))
            .collect()
    }

    #[test]
    fn f_on_github_opens_its_components_with_the_configured_names_ticked() {
        let mut app = components_app(&["webhooks", " Actions ", "Gone Away"]);
        assert_eq!(app.key(key(KeyCode::Char('f'))), None);
        let picker = app.picker().unwrap();
        assert_eq!(picker.title(), "GitHub components");
        let shown = keys(&app);
        assert_eq!(shown.len(), 13, "12 components and one not in the feed");
        assert_eq!(shown[0], ("Gone Away".to_owned(), true));
        assert_eq!(picker.items()[0].hint, "not in feed");
        assert_eq!(shown[1], ("Git Operations".to_owned(), false));
        assert_eq!(shown[2], ("Webhooks".to_owned(), true));
        assert!(shown.contains(&("Actions".to_owned(), true)));

        // Untick Webhooks, tick Git Operations: the feed's spelling, tree order.
        for code in [
            KeyCode::Down,
            KeyCode::Char(' '),
            KeyCode::Down,
            KeyCode::Char(' '),
        ] {
            app.key(key(code));
        }
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            Some(Effect::Save(Pick::Components {
                platform: "GitHub".into(),
                names: vec![
                    "Gone Away".into(),
                    "Git Operations".into(),
                    "Actions".into()
                ],
            }))
        );
        assert!(app.picker().is_none());
    }

    #[test]
    fn a_name_not_in_the_feed_is_dropped_only_when_unticked() {
        let mut app = components_app(&["Gone Away"]);
        app.key(key(KeyCode::Char('f')));
        app.key(key(KeyCode::Char(' ')));
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            Some(Effect::Save(Pick::Components {
                platform: "GitHub".into(),
                names: Vec::new(),
            }))
        );
    }

    #[test]
    fn cloudflare_groups_are_headings_and_the_filter_keeps_their_names() {
        let mut app = components_app(&[]);
        app.apply(Action::Down);
        app.key(key(KeyCode::Char('f')));
        let shown = keys(&app);
        assert_eq!(shown.len(), 18, "2 collapsed groups and 16 ungrouped");
        assert_eq!(shown[0].0, "Cloudflare Sites and Services");
        assert_eq!(shown[17].0, "North America");
        let picker = app.picker().unwrap();
        assert!(picker.items()[0].heading);
        assert_eq!(picker.items()[0].hint, "3 components");

        for c in "/boston".chars() {
            app.key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            keys(&app)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>(),
            ["North America", "Boston, United States - (BOS)"]
        );
        app.key(key(KeyCode::Enter));
        app.key(key(KeyCode::Char(' ')));
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            Some(Effect::Save(Pick::Components {
                platform: "Cloudflare".into(),
                names: vec!["North America".into()],
            })),
            "ticking a group saves the group's name"
        );
    }

    #[test]
    fn a_configured_group_starts_ticked_and_covers_its_components() {
        let mut app = App::new(vec!["Cloudflare".into()]);
        let watch = vec!["north america".to_owned()];
        app.set_filter("Cloudflare", Filter::Components(Some(watch.clone())));
        app.finish_refresh(
            vec![fixture("Cloudflare", CLOUDFLARE, Some(&watch))],
            Utc::now(),
        );
        app.key(key(KeyCode::Char('f')));
        let picker = app.picker().unwrap();
        let heading = picker
            .items()
            .iter()
            .position(|item| item.key == "North America")
            .unwrap();
        assert!(picker.items()[heading].ticked);
        assert!(picker.covered(heading + 1));
        assert_eq!(picker.picked(), ["North America"]);
    }

    #[test]
    fn f_without_a_good_summary_shows_a_notice() {
        let mut app = App::new(vec!["GitHub".into()]);
        app.set_filter("GitHub", Filter::Components(None));
        assert_eq!(
            app.key(key(KeyCode::Char('f'))),
            Some(Effect::Notice(
                "GitHub's components aren't known yet: its first fetch is still running".into()
            ))
        );
        app.finish_refresh(
            vec![PlatformStatus::unknown("GitHub", "timed out", Utc::now())],
            Utc::now(),
        );
        assert_eq!(
            app.key(key(KeyCode::Char('f'))),
            Some(Effect::Notice(
                "GitHub's components aren't known: its last fetch failed (r to retry)".into()
            ))
        );
        assert!(app.picker().is_none());
    }

    #[test]
    fn kept_stale_data_still_gives_the_component_tree() {
        let mut app = App::new(vec!["GitHub".into()]);
        app.set_filter("GitHub", Filter::Components(None));
        let now = Utc::now();
        app.finish_refresh(vec![fixture("GitHub", GITHUB, None)], now);
        app.finish_refresh(
            vec![PlatformStatus::unknown("GitHub", "timed out", now)],
            now,
        );
        assert!(app.row_stale(0).is_some());
        assert_eq!(app.key(key(KeyCode::Char('f'))), None);
        assert_eq!(app.picker().unwrap().items().len(), 12);
    }
}
