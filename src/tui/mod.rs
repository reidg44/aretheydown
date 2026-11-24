//! The interactive terminal UI: a platform list, a detail pane for the
//! selected platform, and a refresh every [`REFRESH_INTERVAL`].
//!
//! Fetching, reading the history file, and opening links run in background
//! tasks; the UI only reads the [`PlatformStatus`] values and history records
//! they hand back.

pub mod app;
mod link;
pub mod picker;
pub mod view;
mod wrap;

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use ratatui::Terminal;
use ratatui::backend::Backend;
use ratatui::crossterm::event::{self, Event};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until};

use crate::config::{self, Saved};
use crate::model::PlatformStatus;
use crate::providers::{Platform, Source};
use crate::{fetch, history};
use app::{App, DetailViewport, Effect, Filter, History, Pick};

/// How often the statuses refresh on their own.
pub use crate::fetch::REFRESH_INTERVAL;

/// How long a footer notice (e.g. a link that would not open) stays up.
const NOTICE_DURATION: Duration = Duration::from_secs(5);

/// How long the input thread waits for a key before checking whether the UI has quit.
const INPUT_POLL: Duration = Duration::from_millis(200);

/// Run the TUI over `platforms` until the user quits. `history` is the
/// history file `--serve` writes (or why there is none), read on each refresh
/// for the detail pane's history section. `config` is the config file the
/// filter picker saves to (`None` when no location is known).
///
/// Takes over the terminal (raw mode, alternate screen) and restores it on
/// return, on error, and on panic.
///
/// # Errors
///
/// Fails when the terminal cannot be set up, drawn to, or read from.
pub async fn run(
    client: reqwest::Client,
    platforms: Vec<Platform>,
    history: Result<PathBuf, String>,
    config: Option<PathBuf>,
) -> Result<()> {
    // `try_init` also installs a panic hook that restores the terminal.
    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(error) => {
            ratatui::restore();
            return Err(error).context("could not set up the terminal");
        }
    };
    // `fetch_all` turns a panic in one platform's fetch into an Unknown
    // status, so the TUI keeps running: the hook must not restore the
    // terminal (or print over the screen) for those panics.
    let restore_on_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !fetch::in_fetch_task() {
            restore_on_panic(info);
        }
    }));

    let (events_tx, mut events) = unbounded_channel();
    let input = spawn_input_thread(events_tx);
    let fetch = move |platforms: Vec<Platform>| {
        let client = client.clone();
        async move { fetch::fetch_all_live(&client, &platforms).await }
    };
    // `open` waits for the launcher (`/usr/bin/open` on macOS), so it runs
    // off the async threads; its output goes nowhere, not onto the screen.
    let open = |url: String| async move {
        tokio::task::spawn_blocking(move || open::that_detached(&url))
            .await
            .context("the opener stopped")?
            .map_err(anyhow::Error::from)
    };
    // The file can be large and the disk slow: read it off the async threads.
    let load_history = move || {
        let path = history.clone();
        async move {
            let path = match path {
                Ok(path) => path,
                Err(error) => return History::Unreadable(error),
            };
            tokio::task::spawn_blocking(move || read_history(&path))
                .await
                .unwrap_or_else(|error| read_failed(&error))
        }
    };
    // A small file, but the disk may be slow: write it off the async threads.
    let save = move |pick: Pick| {
        let path = config.clone();
        async move {
            let path = path.ok_or_else(|| {
                anyhow!("there is no config file location (set HOME or XDG_CONFIG_HOME, or pass --config)")
            })?;
            tokio::task::spawn_blocking(move || {
                let saved = match &pick {
                    Pick::AwsRegions(codes) => config::save_aws_regions(&path, codes)?,
                    Pick::Components { platform, names } => {
                        config::save_components(&path, platform, names)?
                    }
                };
                Ok(saved_notice(&path, saved, &pick))
            })
            .await
            .context("the save stopped")?
        }
    };
    let result = event_loop(
        &mut terminal,
        platforms,
        &mut events,
        fetch,
        open,
        load_history,
        save,
    )
    .await;

    // Closing the channel stops the input thread; wait so it is not still
    // reading the terminal once it is handed back to the shell.
    drop(events);
    let _ = input.join();
    ratatui::restore();
    result
}

/// The footer notice after `pick` was saved to `path`.
fn saved_notice(path: &Path, saved: Saved, pick: &Pick) -> String {
    match pick {
        Pick::AwsRegions(codes) => saved_regions_notice(path, saved, codes.len()),
        Pick::Components { platform, names } => {
            saved_components_notice(path, saved, platform, names.len())
        }
    }
}

/// The footer notice after [`config::save_components`] wrote `count` names
/// to `platform`'s `[components]` entry in `path`.
fn saved_components_notice(path: &Path, saved: Saved, platform: &str, count: usize) -> String {
    let path = path.display();
    let names = if count == 1 {
        format!("1 {platform} component")
    } else {
        format!("{count} {platform} components")
    };
    match (saved, count) {
        (Saved::Unchanged, 0) => {
            format!(
                "Every {platform} component already counts: {path} has no [components] entry for it"
            )
        }
        (Saved::Unchanged, _) => format!("{path} already watches these {names}"),
        (_, 0) => format!("Removed {platform} from [components] in {path}: every component counts"),
        (Saved::Created, _) => format!("Created {path} watching {names}"),
        (Saved::Updated, _) => format!("Saved {names} to {path}"),
    }
}

/// The footer notice after [`config::save_aws_regions`] wrote `count`
/// region codes to `path`.
fn saved_regions_notice(path: &Path, saved: Saved, count: usize) -> String {
    let path = path.display();
    let regions = if count == 1 {
        "1 AWS region".to_owned()
    } else {
        format!("{count} AWS regions")
    };
    match (saved, count) {
        (Saved::Unchanged, 0) => {
            format!("Every AWS region already counts: {path} has no [aws] regions")
        }
        (Saved::Unchanged, _) => format!("{path} already has these {regions}"),
        (_, 0) => format!("Removed [aws] regions from {path}: every region counts"),
        (Saved::Created, _) => format!("Created {path} with {regions}"),
        (Saved::Updated, _) => format!("Saved {regions} to {path}"),
    }
}

/// Act on the outcome of saving a picker's `pick`, returning the footer
/// notice and whether to refetch. On success, every AWS platform takes the
/// new regions (none: every region), or the picked Statuspage platform its
/// new components (none: every component), and the refetch starts at once,
/// even if a refresh is running. On failure, the old filter stays and the
/// notice says why.
fn apply_saved(
    app: &mut App,
    platforms: &mut [Platform],
    pick: Pick,
    saved: Result<String>,
) -> (String, bool) {
    let message = match saved {
        Ok(message) => message,
        Err(error) => {
            let what = match &pick {
                Pick::AwsRegions(_) => "[aws] regions".to_owned(),
                Pick::Components { platform, .. } => format!("{platform}'s [components]"),
            };
            return (format!("Could not save {what}: {error:#}"), false);
        }
    };
    let (platform_name, keys) = match pick {
        Pick::AwsRegions(codes) => (None, codes),
        Pick::Components { platform, names } => (Some(platform), names),
    };
    let keys = (!keys.is_empty()).then_some(keys);
    for platform in platforms {
        let changed = match (&mut platform.source, &platform_name) {
            (Source::Aws { regions, .. }, None) => {
                regions.clone_from(&keys);
                true
            }
            (Source::Statuspage { components, .. }, Some(name)) if platform.name == *name => {
                components.clone_from(&keys);
                true
            }
            _ => false,
        };
        if changed {
            app.set_filter(&platform.name, filter_for(platform));
        }
    }
    app.start_refresh();
    (message, true)
}

/// What the footer says when opening a link failed; `None` when it opened.
fn open_failure(opened: Result<(String, Result<()>), tokio::task::JoinError>) -> Option<String> {
    match opened {
        Ok((_, Ok(()))) => None,
        Ok((url, Err(error))) => Some(format!("Could not open {url}: {error:#}")),
        Err(error) => Some(format!("Could not open the link: {error}")),
    }
}

/// On quit: drop a running fetch and history read, but let a save finish,
/// so quitting right after Enter still writes it.
async fn stop<T, H, S>(
    pending: Option<tokio::task::JoinHandle<T>>,
    reading: Option<tokio::task::JoinHandle<H>>,
    saving: Option<tokio::task::JoinHandle<S>>,
) {
    if let Some(task) = pending {
        task.abort();
    }
    if let Some(task) = reading {
        task.abort();
    }
    if let Some(task) = saving {
        let _ = task.await;
    }
}

/// Show `message` in the footer for [`NOTICE_DURATION`] from now.
fn notify(app: &mut App, until: &mut Option<Instant>, message: String) {
    app.set_notice(message);
    *until = Some(Instant::now() + NOTICE_DURATION);
}

/// A fresh app for `platforms`, each with its status page and filter.
fn new_app(platforms: &[Platform]) -> App {
    let mut app = App::with_status_pages(
        platforms
            .iter()
            .map(|p| (p.name.clone(), p.source.status_page()))
            .collect(),
    );
    for platform in platforms {
        app.set_filter(&platform.name, filter_for(platform));
    }
    app
}

/// What `f` filters on `platform`.
fn filter_for(platform: &Platform) -> Filter {
    match &platform.source {
        Source::Aws { regions, .. } => Filter::AwsRegions(regions.clone()),
        Source::Statuspage { components, .. } => Filter::Components(components.clone()),
        _ => Filter::None,
    }
}

/// What the history file at `path` holds, for [`App::set_history`].
fn read_history(path: &Path) -> History {
    match history::read(path) {
        Ok(Some(records)) => History::Records(records),
        Ok(None) => History::Missing,
        Err(error) => History::Unreadable(format!(
            "could not read the history file {}: {error}",
            path.display()
        )),
    }
}

/// The history to show when the task reading the file panicked or was cancelled.
fn read_failed(error: &tokio::task::JoinError) -> History {
    History::Unreadable(format!("reading the history file failed: {error}"))
}

/// Forward terminal events to the UI loop until the receiver is dropped.
fn spawn_input_thread(tx: UnboundedSender<io::Result<Event>>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !tx.is_closed() {
            match event::poll(INPUT_POLL) {
                Ok(false) => {}
                Ok(true) => {
                    let event = event::read();
                    let failed = event.is_err();
                    if tx.send(event).is_err() || failed {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(Err(error));
                    break;
                }
            }
        }
    })
}

/// Draw, react to keys, and refresh until the user quits.
///
/// `fetch` gets the full platform list and runs in its own task, so a slow
/// platform never blocks input or drawing. `open` gets a URL to show in the
/// browser and also runs in its own task; when it fails, the footer says so
/// for [`NOTICE_DURATION`]. `load_history` reads the history file; it runs
/// in its own task alongside each refresh's fetch. `save` writes the AWS
/// region picker's codes to the config file and returns the footer notice;
/// it runs in its own task, one at a time. After it succeeds, the platforms
/// take the new regions and a refresh starts at once (replacing one in
/// flight). A failure keeps the old regions and shows why in the footer.
async fn event_loop<B, F, Fut, O, OFut, H, HFut, S, SFut>(
    terminal: &mut Terminal<B>,
    mut platforms: Vec<Platform>,
    events: &mut UnboundedReceiver<io::Result<Event>>,
    fetch: F,
    open: O,
    load_history: H,
    save: S,
) -> Result<()>
where
    B: Backend,
    B::Error: Send + Sync + 'static,
    F: Fn(Vec<Platform>) -> Fut,
    Fut: Future<Output = Vec<PlatformStatus>> + Send + 'static,
    O: Fn(String) -> OFut,
    OFut: Future<Output = Result<()>> + Send + 'static,
    H: Fn() -> HFut,
    HFut: Future<Output = History> + Send + 'static,
    S: Fn(Pick) -> SFut,
    SFut: Future<Output = Result<String>> + Send + 'static,
{
    let mut app = new_app(&platforms);
    // The first tick fires at once, so data loads on startup.
    let mut ticker = interval_at(Instant::now(), REFRESH_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut pending: Option<tokio::task::JoinHandle<Vec<PlatformStatus>>> = None;
    let mut reading: Option<tokio::task::JoinHandle<History>> = None;
    let mut opening: JoinSet<(String, Result<()>)> = JoinSet::new();
    let mut saving: Option<tokio::task::JoinHandle<(Pick, Result<String>)>> = None;
    let mut notice_until: Option<Instant> = None;

    loop {
        let mut viewport = DetailViewport::default();
        terminal.draw(|frame| viewport = view::draw(frame, &app, Utc::now()))?;
        app.set_viewport(viewport);

        let start_refresh = tokio::select! {
            _ = ticker.tick() => app.start_refresh(),
            joined = async { pending.as_mut().expect("guarded by is_some").await },
                if pending.is_some() =>
            {
                pending = None;
                let statuses = joined.context("the status fetch task failed")?;
                app.finish_refresh(statuses, Utc::now());
                false
            }
            joined = async { reading.as_mut().expect("guarded by is_some").await },
                if reading.is_some() =>
            {
                reading = None;
                app.set_history(joined.unwrap_or_else(|error| read_failed(&error)));
                false
            }
            Some(opened) = opening.join_next(), if !opening.is_empty() => {
                if let Some(message) = open_failure(opened) {
                    notify(&mut app, &mut notice_until, message);
                }
                false
            }
            joined = async { saving.as_mut().expect("guarded by is_some").await },
                if saving.is_some() =>
            {
                saving = None;
                let (message, refetch) = match joined {
                    Ok((pick, saved)) => apply_saved(&mut app, &mut platforms, pick, saved),
                    Err(error) => (format!("Could not save: the save stopped: {error}"), false),
                };
                notify(&mut app, &mut notice_until, message);
                refetch
            }
            () = sleep_until(notice_until.unwrap_or_else(Instant::now)), if notice_until.is_some() => {
                app.clear_notice();
                notice_until = None;
                false
            }
            event = events.recv() => {
                match event.ok_or_else(|| anyhow!("terminal input stopped"))?? {
                    Event::Key(key) => match app.key(key) {
                        Some(Effect::Refresh) => true,
                        Some(Effect::Open(url)) => {
                            let launch = open(url.clone());
                            opening.spawn(async move { (url, launch.await) });
                            false
                        }
                        Some(Effect::Notice(message)) => {
                            notify(&mut app, &mut notice_until, message);
                            false
                        }
                        Some(Effect::Save(_)) if saving.is_some() => {
                            let message = "The last save is still running; try again in a moment".to_owned();
                            notify(&mut app, &mut notice_until, message);
                            false
                        }
                        Some(Effect::Save(pick)) => {
                            let saved = save(pick.clone());
                            saving = Some(tokio::spawn(async move { (pick, saved.await) }));
                            false
                        }
                        None => false,
                    },
                    // Anything else (e.g. a resize) just needs the redraw at the top of the loop.
                    _ => false,
                }
            }
        };

        if app.should_quit() {
            stop(pending, reading, saving).await;
            return Ok(());
        }
        if start_refresh {
            // The next automatic refresh comes a full interval after this one.
            ticker.reset();
            if let Some(task) = pending.take() {
                task.abort();
            }
            pending = Some(tokio::spawn(fetch(platforms.clone())));
            if reading.is_none() {
                reading = Some(tokio::spawn(load_history()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;
    use crate::model::Level;

    /// A history reader for tests that don't look at history: no file.
    async fn no_history() -> History {
        History::Missing
    }

    /// A config writer for tests that never save a filter.
    async fn never_save(pick: Pick) -> Result<String> {
        panic!("nothing should save {pick:?}");
    }

    /// An opener for tests that never press `o`.
    async fn never_open(url: String) -> Result<()> {
        panic!("nothing should open {url}");
    }

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn screen(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(usize::from(buffer.area.width))
            .map(|row| {
                row.iter()
                    .map(ratatui::buffer::Cell::symbol)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Drives the real loop with a fake fetcher: startup fetch, `j` `k`, `r`, the
    /// 60-second auto-refresh, then `q`.
    #[tokio::test(start_paused = true)]
    async fn loop_fetches_on_start_on_r_and_every_interval() {
        let platforms = vec![
            Platform::statuspage("GitHub", "https://www.githubstatus.com"),
            Platform::statuspage("Broken", "https://broken.example"),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let (done_tx, mut done) = unbounded_channel::<Instant>();
        let fetch = {
            let calls = Arc::clone(&calls);
            move |platforms: Vec<Platform>| {
                let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let done_tx = done_tx.clone();
                async move {
                    let statuses = platforms
                        .iter()
                        .map(|p| {
                            let mut status = PlatformStatus::unknown(
                                &p.name,
                                format!("fetch #{call} failed"),
                                Utc::now(),
                            );
                            if p.name == "GitHub" {
                                status.level = Level::Operational;
                                status.error = None;
                            }
                            status
                        })
                        .collect();
                    let _ = done_tx.send(Instant::now());
                    statuses
                }
            }
        };

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            let start = done.recv().await.unwrap();
            // With the clock paused, this sleep only ends once the loop is idle,
            // i.e. after it has stored the first results.
            tokio::time::sleep(Duration::from_millis(1)).await;
            // Broken (Unknown) sorts to the top and the first results select it.
            events_tx.send(Ok(key(KeyCode::Char('j')))).unwrap();
            events_tx.send(Ok(key(KeyCode::Char('k')))).unwrap();
            events_tx.send(Ok(key(KeyCode::Char('r')))).unwrap();
            let manual = done.recv().await.unwrap();
            assert!(
                manual - start < Duration::from_secs(1),
                "r refreshes at once"
            );
            let auto = done.recv().await.unwrap();
            assert_eq!(
                auto - manual,
                REFRESH_INTERVAL,
                "auto-refresh after a full interval"
            );
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                platforms,
                &mut events,
                fetch,
                never_open,
                no_history,
                never_save
            ),
            driver
        );
        result.unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let screen = screen(&terminal);
        assert!(
            screen.contains("> ● Broken"),
            "j then k moved the selection back to Broken:\n{screen}"
        );
        assert!(screen.contains("Error: fetch #3 failed"), "{screen}");
    }

    /// A provider panic on the first fetch leaves that platform Unknown, the
    /// loop running, and `r` still refreshes it.
    #[tokio::test]
    async fn a_fetch_panic_shows_unknown_and_the_next_refresh_works() {
        let platforms = vec![
            Platform::statuspage("GitHub", "https://www.githubstatus.com"),
            Platform::statuspage("Flaky", "https://flaky.example"),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let (done_tx, mut done) = unbounded_channel::<Vec<PlatformStatus>>();
        let fetch = {
            let calls = Arc::clone(&calls);
            move |platforms: Vec<Platform>| {
                let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
                let done_tx = done_tx.clone();
                async move {
                    let statuses =
                        fetch::fetch_all(&platforms, fetch::DEFAULT_TIMEOUT, move |p| async move {
                            assert!(!(p.name == "Flaky" && call == 1), "bad feed");
                            let mut status = PlatformStatus::unknown(&p.name, "", Utc::now());
                            status.level = Level::Operational;
                            status.error = None;
                            Ok(status)
                        })
                        .await;
                    let _ = done_tx.send(statuses.clone());
                    statuses
                }
            }
        };

        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            let first = done.recv().await.unwrap();
            assert_eq!(first[0].level, Level::Operational);
            assert_eq!(first[1].level, Level::Unknown);
            assert_eq!(first[1].error.as_deref(), Some("fetch panicked: bad feed"));
            tokio::task::yield_now().await;
            events_tx.send(Ok(key(KeyCode::Char('r')))).unwrap();
            let second = done.recv().await.unwrap();
            assert_eq!(second[1].level, Level::Operational);
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                platforms,
                &mut events,
                fetch,
                never_open,
                no_history,
                never_save
            ),
            driver
        );
        result.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// GitHub Degraded with two linked incidents, Calm Operational.
    async fn fetch_with_incidents(platforms: Vec<Platform>) -> Vec<PlatformStatus> {
        platforms
            .iter()
            .map(|p| {
                let mut status = PlatformStatus::unknown(&p.name, "", Utc::now());
                status.error = None;
                status.level = Level::Operational;
                if p.name == "GitHub" {
                    status.level = Level::Degraded;
                    status.incidents = ["a", "b"]
                        .iter()
                        .map(|id| crate::model::Incident {
                            kind: crate::model::IncidentKind::Incident,
                            title: format!("Incident {id}"),
                            components: Vec::new(),
                            started_at: None,
                            url: Some(format!("https://www.githubstatus.com/incidents/{id}")),
                            ..Default::default()
                        })
                        .collect();
                }
                status
            })
            .collect()
    }

    fn github_and_calm() -> Vec<Platform> {
        vec![
            Platform::statuspage("GitHub", "https://www.githubstatus.com"),
            Platform::statuspage("Calm", "https://calm.example"),
        ]
    }

    /// `o` hands the highlighted incident's link (after Tab) to the opener, and
    /// an Operational platform's status page; nothing shows in the footer.
    #[tokio::test(start_paused = true)]
    async fn o_opens_the_highlighted_link_or_the_status_page() {
        let (opened_tx, mut opened) = unbounded_channel::<String>();
        let open = move |url: String| {
            let opened_tx = opened_tx.clone();
            async move {
                opened_tx.send(url).unwrap();
                Ok(())
            }
        };
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            // Let the startup fetch land; GitHub (Degraded) is selected.
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Tab))).unwrap();
            events_tx.send(Ok(key(KeyCode::Char('o')))).unwrap();
            assert_eq!(
                opened.recv().await.unwrap(),
                "https://www.githubstatus.com/incidents/b"
            );
            events_tx.send(Ok(key(KeyCode::Char('j')))).unwrap();
            events_tx.send(Ok(key(KeyCode::Char('o')))).unwrap();
            assert_eq!(opened.recv().await.unwrap(), "https://calm.example");
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                github_and_calm(),
                &mut events,
                fetch_with_incidents,
                open,
                no_history,
                never_save
            ),
            driver
        );
        result.unwrap();
        assert!(
            screen(&terminal).contains("o open"),
            "no notice for a success"
        );
    }

    /// Run the loop with an opener that always fails: press `o`, wait `wait`,
    /// press `j`, then quit. Returns the screen as last drawn.
    async fn press_o_with_a_failing_opener(wait: Duration) -> String {
        let open = |_: String| async { Err(anyhow!("no browser found")) };
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            // Paused clock: each sleep returns once the loop is idle again.
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('o')))).unwrap();
            tokio::time::sleep(wait).await;
            events_tx.send(Ok(key(KeyCode::Char('j')))).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                github_and_calm(),
                &mut events,
                fetch_with_incidents,
                open,
                no_history,
                never_save
            ),
            driver
        );
        result.unwrap();
        screen(&terminal)
    }

    /// A failing opener puts the reason in the footer; the loop keeps running
    /// and takes keys meanwhile.
    #[tokio::test(start_paused = true)]
    async fn a_failing_opener_shows_a_footer_notice_and_the_loop_keeps_running() {
        let text = press_o_with_a_failing_opener(Duration::from_secs(1)).await;
        assert!(
            text.contains(
                " Could not open https://www.githubstatus.com/incidents/a: no browser found"
            ),
            "{text}"
        );
        assert!(!text.contains("o open"), "{text}");
        assert!(
            text.contains("> ● Calm"),
            "j still moved the selection: {text}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_footer_notice_clears_after_a_few_seconds() {
        let text = press_o_with_a_failing_opener(NOTICE_DURATION + Duration::from_millis(1)).await;
        assert!(!text.contains("Could not open"), "{text}");
        assert!(text.contains("o open"), "{text}");
    }

    /// The history file is read on start and again on each refresh (`r`
    /// here), and its records show in the selected platform's detail pane.
    #[tokio::test(start_paused = true)]
    async fn history_is_read_on_start_and_on_each_refresh() {
        let reads = Arc::new(AtomicUsize::new(0));
        let load_history = {
            let reads = Arc::clone(&reads);
            move || {
                reads.fetch_add(1, Ordering::SeqCst);
                async {
                    let mut status = PlatformStatus::unknown("GitHub", "", Utc::now());
                    status.level = Level::Outage;
                    History::Records(vec![history::Record {
                        recorded_at: Utc::now() - chrono::TimeDelta::hours(1),
                        status,
                        backfilled: false,
                    }])
                }
            }
        };
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('r')))).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                github_and_calm(),
                &mut events,
                fetch_with_incidents,
                never_open,
                load_history,
                never_save
            ),
            driver
        );
        result.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let screen = screen(&terminal);
        assert!(screen.contains("History (last 7 days)"), "{screen}");
        assert!(screen.contains("Outage, ongoing"), "{screen}");
    }

    /// `t` opens the timeline, and a refresh while it is open rereads the
    /// file into it.
    #[tokio::test(start_paused = true)]
    async fn t_shows_the_timeline_and_a_refresh_rereads_it() {
        let reads = Arc::new(AtomicUsize::new(0));
        let load_history = {
            let reads = Arc::clone(&reads);
            move || {
                // The second read finds a second change.
                let count = reads.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    let records = ["GitHub", "Calm"]
                        .into_iter()
                        .take(count)
                        .map(|name| {
                            let mut status = PlatformStatus::unknown(name, "", Utc::now());
                            status.level = Level::Outage;
                            history::Record {
                                recorded_at: Utc::now() - chrono::TimeDelta::hours(1),
                                status,
                                backfilled: false,
                            }
                        })
                        .collect();
                    History::Records(records)
                }
            }
        };
        let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('t')))).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('r')))).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                github_and_calm(),
                &mut events,
                fetch_with_incidents,
                never_open,
                load_history,
                never_save
            ),
            driver
        );
        result.unwrap();
        assert_eq!(reads.load(Ordering::SeqCst), 2);
        let screen = screen(&terminal);
        assert!(screen.contains(" Timeline (last 7 days) "), "{screen}");
        assert!(!screen.contains(" Platforms "), "{screen}");
        assert!(screen.contains(" 2 changes "), "the reread file: {screen}");
        assert!(screen.contains("GitHub  Outage, ongoing"), "{screen}");
        assert!(screen.contains("Calm    Outage, ongoing"), "{screen}");
    }

    fn aws_only() -> Vec<Platform> {
        vec![Platform::new(
            "AWS",
            Source::Aws {
                url: "https://aws.example/currentevents".to_owned(),
                regions: Some(vec!["us-west-2".to_owned()]),
            },
        )]
    }

    /// A fetcher that reports the AWS regions each fetch was given.
    fn fetch_reporting_regions(
        tx: UnboundedSender<Option<Vec<String>>>,
    ) -> impl Fn(Vec<Platform>) -> std::pin::Pin<Box<dyn Future<Output = Vec<PlatformStatus>> + Send>>
    {
        move |platforms: Vec<Platform>| {
            let tx = tx.clone();
            Box::pin(async move {
                if let Source::Aws { regions, .. } = &platforms[0].source {
                    tx.send(regions.clone()).unwrap();
                }
                platforms
                    .iter()
                    .map(|p| {
                        let mut status = PlatformStatus::unknown(&p.name, "", Utc::now());
                        status.level = Level::Operational;
                        status.error = None;
                        status
                    })
                    .collect()
            })
        }
    }

    /// `f`, tick us-east-1, Enter: the save gets the picks, its notice shows,
    /// and a refetch starts at once with the new regions.
    #[tokio::test(start_paused = true)]
    async fn saving_the_region_picker_refetches_with_the_new_regions() {
        let (fetched_tx, mut fetched) = unbounded_channel();
        let saves = Arc::new(std::sync::Mutex::new(Vec::new()));
        let save = {
            let saves = Arc::clone(&saves);
            move |pick: Pick| {
                saves.lock().unwrap().push(pick.keys().to_vec());
                async { Ok("Saved 2 AWS regions to /tmp/config.toml".to_owned()) }
            }
        };
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            let first = fetched.recv().await.unwrap();
            assert_eq!(first, Some(vec!["us-west-2".to_owned()]));
            tokio::time::sleep(Duration::from_millis(1)).await;
            for code in [KeyCode::Char('f'), KeyCode::Char(' '), KeyCode::Enter] {
                events_tx.send(Ok(key(code))).unwrap();
            }
            let second = fetched.recv().await.unwrap();
            assert_eq!(
                second,
                Some(vec!["us-east-1".to_owned(), "us-west-2".to_owned()])
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                aws_only(),
                &mut events,
                fetch_reporting_regions(fetched_tx),
                never_open,
                no_history,
                save
            ),
            driver
        );
        result.unwrap();
        assert_eq!(
            *saves.lock().unwrap(),
            [vec!["us-east-1".to_owned(), "us-west-2".to_owned()]]
        );
        let screen = screen(&terminal);
        assert!(
            screen.contains(" Saved 2 AWS regions to /tmp/config.toml"),
            "{screen}"
        );
        assert!(
            screen.contains(" Platforms "),
            "the picker closed: {screen}"
        );
    }

    /// A slow save does not hold up the loop: `r` still refetches (with the
    /// old regions) while it runs, and the new regions follow once it ends.
    #[tokio::test(start_paused = true)]
    async fn the_loop_keeps_going_while_a_save_runs() {
        let (fetched_tx, mut fetched) = unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let save = {
            let release = Arc::clone(&release);
            move |_: Pick| {
                let release = Arc::clone(&release);
                async move {
                    release.notified().await;
                    Ok("Saved".to_owned())
                }
            }
        };
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            fetched.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            for code in [KeyCode::Char('f'), KeyCode::Char(' '), KeyCode::Enter] {
                events_tx.send(Ok(key(code))).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('r')))).unwrap();
            let during = fetched.recv().await.unwrap();
            assert_eq!(during, Some(vec!["us-west-2".to_owned()]));
            release.notify_one();
            let after = fetched.recv().await.unwrap();
            assert_eq!(
                after,
                Some(vec!["us-east-1".to_owned(), "us-west-2".to_owned()])
            );
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                aws_only(),
                &mut events,
                fetch_reporting_regions(fetched_tx),
                never_open,
                no_history,
                save
            ),
            driver
        );
        result.unwrap();
    }

    /// A failed save shows why, starts no refetch, and keeps the old regions
    /// ticked the next time the picker opens.
    #[tokio::test(start_paused = true)]
    async fn a_failed_save_keeps_the_old_regions_and_says_why() {
        let (fetched_tx, mut fetched) = unbounded_channel();
        let save = |_: Pick| async {
            Err(anyhow!(
                "could not write /tmp/config.toml: Permission denied"
            ))
        };
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            fetched.recv().await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            for code in [KeyCode::Char('f'), KeyCode::Char(' '), KeyCode::Enter] {
                events_tx.send(Ok(key(code))).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
            assert!(fetched.try_recv().is_err(), "no refetch after a failure");
            events_tx.send(Ok(key(KeyCode::Char('f')))).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                aws_only(),
                &mut events,
                fetch_reporting_regions(fetched_tx),
                never_open,
                no_history,
                save
            ),
            driver
        );
        result.unwrap();
        let screen = screen(&terminal);
        assert!(
            screen.contains(
                " Could not save [aws] regions: could not write /tmp/config.toml: Permission denied"
            ),
            "{screen}"
        );
        assert!(screen.contains("[ ] us-east-1"), "{screen}");
        assert!(screen.contains("[x] us-west-2"), "{screen}");
    }

    /// `f` on a platform with no filter shows a footer notice that clears.
    #[tokio::test(start_paused = true)]
    async fn f_on_a_platform_without_a_filter_shows_a_notice() {
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('f')))).unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                vec![Platform::check("API", "https://api.example", None)],
                &mut events,
                fetch_with_incidents,
                never_open,
                no_history,
                never_save
            ),
            driver
        );
        result.unwrap();
        let screen = screen(&terminal);
        assert!(
            screen.contains(
                " API can't be filtered (only AWS regions and Statuspage components can be picked)"
            ),
            "{screen}"
        );
    }

    /// A fetcher that reports GitHub's watched components each fetch, and
    /// parses the GitHub fixture with them.
    fn fetch_reporting_components(
        tx: UnboundedSender<Option<Vec<String>>>,
    ) -> impl Fn(Vec<Platform>) -> std::future::Ready<Vec<PlatformStatus>> {
        move |platforms: Vec<Platform>| {
            let statuses = platforms
                .iter()
                .map(|platform| {
                    let Source::Statuspage { components, .. } = &platform.source else {
                        panic!("only Statuspage platforms here");
                    };
                    tx.send(components.clone()).unwrap();
                    crate::providers::statuspage::parse_watching(
                        &platform.name,
                        include_bytes!("../../tests/fixtures/github_summary.json"),
                        Utc::now(),
                        components.as_deref(),
                    )
                    .unwrap()
                })
                .collect();
            std::future::ready(statuses)
        }
    }

    /// `f` on GitHub, tick Git Operations, Enter: the save gets the platform
    /// and its names, and the refetch watches them, so the row's description
    /// becomes the watched-components one.
    #[tokio::test(start_paused = true)]
    async fn saving_the_component_picker_refetches_with_the_new_components() {
        let (fetched_tx, mut fetched) = unbounded_channel();
        let saves = Arc::new(std::sync::Mutex::new(Vec::new()));
        let save = {
            let saves = Arc::clone(&saves);
            move |pick: Pick| {
                saves.lock().unwrap().push(pick);
                async { Ok("Saved 1 GitHub component to /tmp/config.toml".to_owned()) }
            }
        };
        let mut terminal = Terminal::new(TestBackend::new(140, 20)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        let driver = async {
            assert_eq!(fetched.recv().await.unwrap(), None);
            tokio::time::sleep(Duration::from_millis(1)).await;
            for code in [KeyCode::Char('f'), KeyCode::Char(' '), KeyCode::Enter] {
                events_tx.send(Ok(key(code))).unwrap();
            }
            assert_eq!(
                fetched.recv().await.unwrap(),
                Some(vec!["Git Operations".to_owned()])
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
            events_tx.send(Ok(key(KeyCode::Char('q')))).unwrap();
        };
        let (result, ()) = tokio::join!(
            event_loop(
                &mut terminal,
                vec![Platform::statuspage(
                    "GitHub",
                    "https://www.githubstatus.com"
                )],
                &mut events,
                fetch_reporting_components(fetched_tx),
                never_open,
                no_history,
                save
            ),
            driver
        );
        result.unwrap();
        assert_eq!(
            *saves.lock().unwrap(),
            [Pick::Components {
                platform: "GitHub".into(),
                names: vec!["Git Operations".into()],
            }]
        );
        let screen = screen(&terminal);
        assert!(
            screen.contains("1 watched component operational"),
            "{screen}"
        );
        assert!(
            screen.contains(" Saved 1 GitHub component to /tmp/config.toml"),
            "{screen}"
        );
    }

    #[test]
    fn saved_component_notices_name_the_platform_and_file() {
        let path = Path::new("/home/me/config.toml");
        let notice = |saved, count| saved_components_notice(path, saved, "GitHub", count);
        assert_eq!(
            notice(Saved::Created, 1),
            "Created /home/me/config.toml watching 1 GitHub component"
        );
        assert_eq!(
            notice(Saved::Updated, 2),
            "Saved 2 GitHub components to /home/me/config.toml"
        );
        assert_eq!(
            notice(Saved::Updated, 0),
            "Removed GitHub from [components] in /home/me/config.toml: every component counts"
        );
        assert_eq!(
            notice(Saved::Unchanged, 0),
            "Every GitHub component already counts: /home/me/config.toml has no [components] entry for it"
        );
        assert_eq!(
            notice(Saved::Unchanged, 3),
            "/home/me/config.toml already watches these 3 GitHub components"
        );
    }

    #[test]
    fn saved_notices_name_the_file() {
        let path = Path::new("/home/me/.config/aretheydown/config.toml");
        assert_eq!(
            saved_regions_notice(path, Saved::Created, 1),
            "Created /home/me/.config/aretheydown/config.toml with 1 AWS region"
        );
        assert_eq!(
            saved_regions_notice(path, Saved::Updated, 3),
            "Saved 3 AWS regions to /home/me/.config/aretheydown/config.toml"
        );
        assert_eq!(
            saved_regions_notice(path, Saved::Updated, 0),
            "Removed [aws] regions from /home/me/.config/aretheydown/config.toml: every region counts"
        );
        assert_eq!(
            saved_regions_notice(path, Saved::Unchanged, 0),
            "Every AWS region already counts: /home/me/.config/aretheydown/config.toml has no [aws] regions"
        );
        assert_eq!(
            saved_regions_notice(path, Saved::Unchanged, 2),
            "/home/me/.config/aretheydown/config.toml already has these 2 AWS regions"
        );
    }

    #[test]
    fn read_history_tells_a_missing_file_from_an_unreadable_one() {
        let dir = std::env::temp_dir().join(format!("aretheydown-tui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(read_history(&dir.join("none.jsonl")), History::Missing);
        let History::Unreadable(error) = read_history(&dir) else {
            panic!("a directory is not a readable file");
        };
        assert!(
            error.starts_with(&format!(
                "could not read the history file {}: ",
                dir.display()
            )),
            "{error}"
        );
        std::fs::write(dir.join("h.jsonl"), "").unwrap();
        assert_eq!(
            read_history(&dir.join("h.jsonl")),
            History::Records(Vec::new())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn closed_input_ends_the_loop_with_an_error() {
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let (events_tx, mut events) = unbounded_channel();
        drop(events_tx);
        let fetch = |_: Vec<Platform>| async { Vec::new() };
        let error = event_loop(
            &mut terminal,
            Vec::new(),
            &mut events,
            fetch,
            never_open,
            no_history,
            never_save,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("input stopped"), "{error}");
    }
}
