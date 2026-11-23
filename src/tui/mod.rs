//! The interactive terminal UI: a platform list, a detail pane for the
//! selected platform, and a refresh every [`REFRESH_INTERVAL`].
//!
//! Fetching, reading the history file, and opening links run in background
//! tasks; the UI only reads the [`PlatformStatus`] values and history records
//! they hand back.

pub mod app;
mod link;
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

use crate::model::PlatformStatus;
use crate::providers::Platform;
use crate::{fetch, history};
use app::{Action, App, DetailViewport, Effect, History};

/// How often the statuses refresh on their own.
pub use crate::fetch::REFRESH_INTERVAL;

/// How long a footer notice (e.g. a link that would not open) stays up.
const NOTICE_DURATION: Duration = Duration::from_secs(5);

/// How long the input thread waits for a key before checking whether the UI has quit.
const INPUT_POLL: Duration = Duration::from_millis(200);

/// Run the TUI over `platforms` until the user quits. `history` is the
/// history file `--serve` writes (or why there is none), read on each refresh
/// for the detail pane's history section.
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
    let result = event_loop(
        &mut terminal,
        platforms,
        &mut events,
        fetch,
        open,
        load_history,
    )
    .await;

    // Closing the channel stops the input thread; wait so it is not still
    // reading the terminal once it is handed back to the shell.
    drop(events);
    let _ = input.join();
    ratatui::restore();
    result
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
/// in its own task alongside each refresh's fetch.
async fn event_loop<B, F, Fut, O, OFut, H, HFut>(
    terminal: &mut Terminal<B>,
    platforms: Vec<Platform>,
    events: &mut UnboundedReceiver<io::Result<Event>>,
    fetch: F,
    open: O,
    load_history: H,
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
{
    let mut app = App::with_status_pages(
        platforms
            .iter()
            .map(|p| (p.name.clone(), p.source.status_page()))
            .collect(),
    );
    // The first tick fires at once, so data loads on startup.
    let mut ticker = interval_at(Instant::now(), REFRESH_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut pending: Option<tokio::task::JoinHandle<Vec<PlatformStatus>>> = None;
    let mut reading: Option<tokio::task::JoinHandle<History>> = None;
    let mut opening: JoinSet<(String, Result<()>)> = JoinSet::new();
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
                let failure = match opened {
                    Ok((_, Ok(()))) => None,
                    Ok((url, Err(error))) => Some(format!("Could not open {url}: {error:#}")),
                    Err(error) => Some(format!("Could not open the link: {error}")),
                };
                if let Some(message) = failure {
                    app.set_notice(message);
                    notice_until = Some(Instant::now() + NOTICE_DURATION);
                }
                false
            }
            () = sleep_until(notice_until.unwrap_or_else(Instant::now)), if notice_until.is_some() => {
                app.clear_notice();
                notice_until = None;
                false
            }
            event = events.recv() => {
                match event.ok_or_else(|| anyhow!("terminal input stopped"))?? {
                    Event::Key(key) => match Action::from_key(key).and_then(|action| app.apply(action)) {
                        Some(Effect::Refresh) => true,
                        Some(Effect::Open(url)) => {
                            let launch = open(url.clone());
                            opening.spawn(async move { (url, launch.await) });
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
            if let Some(task) = pending {
                task.abort();
            }
            if let Some(task) = reading {
                task.abort();
            }
            return Ok(());
        }
        if start_refresh {
            // The next automatic refresh comes a full interval after this one.
            ticker.reset();
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
                no_history
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
                no_history
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
                no_history
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
                no_history
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
                load_history
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
                load_history
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
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("input stopped"), "{error}");
    }
}
