//! `--serve`: poll every platform in the background and serve the latest
//! result over HTTP, as JSON and as a web page (`crate::html`).
//!
//! Requests only read the cached result; they never start a vendor fetch.
//! Each poll's changes also go to the history file (`crate::history`), and,
//! with `[alerts] webhook` set, confirmed level changes to the webhook
//! (`crate::alerts`). At start, the history file is seeded from the vendors'
//! incident feeds ([`backfill`]).
//! Like `render` and `tui`, it only reads [`PlatformStatus`] values; the
//! records it writes are built in `crate::history`.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior, interval_at};

use crate::alerts::{self, Alerter, SendResult};
use crate::config::{self, Loaded, Webhook};
use crate::fetch;
use crate::history::{self, Recorder};
use crate::html::{self, StatusPages};
use crate::model::{Level, PastIncident, PlatformStatus};
use crate::providers::{self, Platform};
use crate::render;

/// Default port for `--serve`: the one reserved for this project's web UI.
pub const DEFAULT_PORT: u16 = 7341;

/// How often the server polls every platform (the TUI's interval).
pub const POLL_INTERVAL: Duration = fetch::REFRESH_INTERVAL;

/// Body of `/api/status` before the first poll has finished.
const NOT_READY_BODY: &str = r#"{"error": "first fetch still running"}"#;

/// The latest poll's statuses, `None` until the first poll finishes.
pub type Latest = watch::Receiver<Option<Arc<Vec<PlatformStatus>>>>;

/// What every request reads: the latest poll and each platform's status page.
#[derive(Clone)]
struct AppState {
    latest: Latest,
    status_pages: Arc<StatusPages>,
}

/// Each platform's status page by name, for the web page's links.
#[must_use]
pub fn status_pages(platforms: &[Platform]) -> StatusPages {
    platforms
        .iter()
        .map(|p| (p.name.clone(), p.source.status_page()))
        .collect()
}

/// The HTTP routes: `GET /` (the web page), `GET /api/status`, and `GET /healthz`.
///
/// Unknown paths get axum's `404`; other methods on these paths get `405`.
pub fn router(latest: Latest, status_pages: StatusPages) -> Router {
    Router::new()
        .route("/", get(web_page))
        .route("/api/status", get(status))
        .route("/healthz", get(|| async { "ok" }))
        .with_state(AppState {
            latest,
            status_pages: Arc::new(status_pages),
        })
}

/// `GET /api/status`: `render::json` of the latest poll, or `503` before the first.
async fn status(State(state): State<AppState>) -> Response {
    let json = [(header::CONTENT_TYPE, "application/json")];
    // Clone the `Arc` out so the watch lock is not held while rendering.
    let statuses = state.latest.borrow().clone();
    match statuses {
        Some(statuses) => (StatusCode::OK, json, render::json(&statuses)).into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, json, NOT_READY_BODY).into_response(),
    }
}

/// `GET /`: `html::page` of the latest poll, or `503` with
/// `html::loading_page` before the first.
async fn web_page(State(state): State<AppState>) -> Response {
    let html = [(header::CONTENT_TYPE, "text/html; charset=utf-8")];
    let statuses = state.latest.borrow().clone();
    match statuses {
        Some(statuses) => (
            StatusCode::OK,
            html,
            html::page(&statuses, &state.status_pages),
        )
            .into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, html, html::loading_page()).into_response(),
    }
}

/// Alerting for `--serve`: an [`Alerter`], the `[alerts] levels` filter,
/// the webhooks' config positions, and the closure that sends one payload to
/// one webhook (`alerts::post` in `run`, a fake in tests).
pub struct Alerting<S> {
    alerter: Alerter,
    levels: Option<Vec<Level>>,
    /// Each webhook's position in the config list (for warnings); the
    /// sender is called with an index into this list.
    positions: Vec<usize>,
    send: S,
}

/// The sender type of [`no_alerts`].
pub type NoSender = fn(usize, Value) -> std::future::Ready<SendResult>;

/// `None` with a concrete sender type, for a [`poll`] without alerts.
#[must_use]
pub fn no_alerts() -> Option<Alerting<NoSender>> {
    None
}

impl<S, SFut> Alerting<S>
where
    S: Fn(usize, Value) -> SFut,
    SFut: Future<Output = SendResult> + Send + 'static,
{
    /// Alerting that has seen no poll yet, sending each wanted alert to
    /// every webhook in `positions` with `send(index, payload)`.
    pub fn new(positions: Vec<usize>, levels: Option<Vec<Level>>, send: S) -> Self {
        Self {
            alerter: Alerter::new(),
            levels,
            positions,
            send,
        }
    }

    /// Send every alert this poll confirms and `levels` wants to every
    /// webhook, all at once, and wait for them; each failure is one stderr
    /// warning, that send is dropped, and the others go on.
    async fn handle(&mut self, statuses: &[PlatformStatus]) {
        let mut sends = JoinSet::new();
        for alert in self.alerter.observe(statuses) {
            if !alerts::wanted(&alert, self.levels.as_deref()) {
                continue;
            }
            let payload = alerts::payload(&alert);
            for (index, &position) in self.positions.iter().enumerate() {
                let sent = (self.send)(index, payload.clone());
                let platform = alert.status.name.clone();
                sends.spawn(async move { (platform, position, sent.await) });
            }
        }
        while let Some(joined) = sends.join_next().await {
            match joined {
                Ok((_, _, Ok(()))) => {}
                Ok((platform, position, Err(error))) => {
                    eprintln!("{}", alerts::warning(&platform, position, &error));
                }
                // A panic message is not printed: it could hold anything.
                Err(_) => eprintln!("warning: an alert send stopped unexpectedly (dropped)"),
            }
        }
    }
}

/// A boxed alert send, so [`alerting`] can name its sender type.
pub type SendFuture = Pin<Box<dyn Future<Output = SendResult> + Send>>;

/// Alerting that POSTs to every `settings` webhook with [`alerts::client`],
/// or `None` without an `[alerts]` table: then `--serve` sends nothing
/// beyond its fetches.
///
/// # Errors
///
/// Fails when the alert HTTP client cannot be set up.
pub fn alerting(
    settings: Option<config::Alerts>,
) -> Result<Option<Alerting<impl Fn(usize, Value) -> SendFuture + use<>>>> {
    let Some(config::Alerts { webhooks, levels }) = settings else {
        return Ok(None);
    };
    let client = alerts::client().context("could not set up the alert HTTP client")?;
    let positions = webhooks.iter().map(Webhook::position).collect();
    let webhooks = Arc::new(webhooks);
    Ok(Some(Alerting::new(
        positions,
        levels,
        move |index: usize, payload: Value| -> SendFuture {
            let client = client.clone();
            let webhooks = Arc::clone(&webhooks);
            Box::pin(async move { alerts::post(&client, &webhooks[index], &payload).await })
        },
    )))
}

/// Poll `platforms` with `fetch` now and then every `interval`, publishing
/// each result, handing it to `recorder` for the history file, and then to
/// `alerting`, if any.
///
/// Runs until every receiver of `tx` is gone. A slow poll delays the next
/// tick rather than queueing extra polls. A failed history write or alert
/// prints a warning on stderr and polling goes on.
pub async fn poll<F, Fut, W, S, SFut>(
    platforms: Vec<Platform>,
    interval: Duration,
    tx: watch::Sender<Option<Arc<Vec<PlatformStatus>>>>,
    fetch: F,
    mut recorder: Recorder<W>,
    mut alerting: Option<Alerting<S>>,
) where
    F: Fn(Vec<Platform>) -> Fut,
    Fut: Future<Output = Vec<PlatformStatus>>,
    W: FnMut(&[history::Record]) -> io::Result<()>,
    S: Fn(usize, Value) -> SFut,
    SFut: Future<Output = SendResult> + Send + 'static,
{
    let mut ticker = interval_at(Instant::now(), interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let statuses = Arc::new(fetch(platforms.clone()).await);
        if tx.send(Some(Arc::clone(&statuses))).is_err() {
            return;
        }
        if let Err(error) = recorder.record(&statuses, Utc::now()) {
            eprintln!("warning: could not write the history file: {error}");
        }
        if let Some(alerting) = &mut alerting {
            alerting.handle(&statuses).await;
        }
    }
}

/// Seed the history file at `path` from each platform's incident history
/// ([`history::backfill`]) at `now`, in one [`history::append`]; returns how
/// many records were written. `fetch_past` gives a platform's past incidents
/// ([`providers::fetch_past`] outside tests), `None` for a platform with no
/// incident feed; every platform is fetched at once.
///
/// Never fails: a failed or unparseable feed is a stderr warning naming the
/// platform, and an unreadable or unwritable file a warning too (nothing is
/// written then, so nothing can overlap what is already there).
pub async fn backfill<F, Fut>(
    platforms: &[Platform],
    path: &Path,
    now: DateTime<Utc>,
    fetch_past: F,
) -> usize
where
    F: Fn(Platform) -> Fut,
    Fut: Future<Output = Option<Result<Vec<PastIncident>>>> + Send + 'static,
{
    // Spawned now, so they run at once; awaited in platform order.
    let tasks: Vec<_> = platforms
        .iter()
        .map(|platform| tokio::spawn(fetch_past(platform.clone())))
        .collect();
    let mut fetched: Vec<Option<Result<Vec<PastIncident>>>> = Vec::new();
    for task in tasks {
        // A panicked fetch is a failed feed: warned about below.
        fetched.push(
            task.await
                .unwrap_or_else(|_| Some(Err(anyhow::anyhow!("the feed fetch panicked")))),
        );
    }
    let existing = match history::read(path) {
        Ok(records) => records.unwrap_or_default(),
        Err(error) => {
            eprintln!(
                "warning: could not read {} to backfill it: {error}",
                path.display()
            );
            return 0;
        }
    };
    let mut records = Vec::new();
    let mut names = Vec::new();
    for (platform, result) in platforms.iter().zip(fetched) {
        let name = platform.name.as_str();
        let past = match result {
            None => continue,
            Some(Ok(past)) => past,
            Some(Err(error)) => {
                eprintln!("warning: could not backfill history for {name}: {error:#}");
                continue;
            }
        };
        let added = history::backfill(name, &past, history::earliest_for(&existing, name), now);
        if !added.is_empty() {
            names.push(name);
            records.extend(added);
        }
    }
    if records.is_empty() {
        return 0;
    }
    if let Err(error) = history::append(path, &records) {
        eprintln!(
            "warning: could not write vendor history to {}: {error}",
            path.display()
        );
        return 0;
    }
    eprintln!(
        "backfilled {} history records from vendor incident feeds: {}",
        records.len(),
        names.join(", ")
    );
    records.len()
}

/// The start-up line saying where alerts go and which, without the URLs:
/// `sending level changes to 2 [alerts] webhooks (into or out of: outage)`.
#[must_use]
pub fn alerting_line(alerts: &config::Alerts) -> String {
    let count = alerts.webhooks.len();
    let line = if count == 1 {
        "sending level changes to the [alerts] webhook".to_owned()
    } else {
        format!("sending level changes to {count} [alerts] webhooks")
    };
    if let Some(levels) = &alerts.levels {
        let names: Vec<String> = levels
            .iter()
            .map(|level| level.label().to_ascii_lowercase())
            .collect();
        return format!("{line} (into or out of: {})", names.join(", "));
    }
    line
}

/// Whether `addr` is reachable only from this machine.
#[must_use]
pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().to_canonical().is_loopback()
}

/// Serve the status of `loaded.platforms` on `addr` until Ctrl-C, recording
/// changes to the history file at `history_path` and, with
/// `loaded.alerts`, alerting its webhooks on confirmed level changes.
///
/// Prints a warning to stderr when `addr` is not a loopback address, since
/// the server has no authentication. At start, records older than
/// [`history::RETENTION`] are dropped from the history file (a failure there
/// is a warning too), then it is seeded from the vendors' incident feeds
/// ([`backfill`]) before the first poll is recorded.
///
/// # Errors
///
/// Fails when the address cannot be bound (for example, the port is in use)
/// or the server stops with an I/O error, or the alert HTTP client cannot be
/// set up.
pub async fn run(
    client: reqwest::Client,
    loaded: Loaded,
    addr: SocketAddr,
    history_path: PathBuf,
) -> Result<()> {
    let Loaded {
        platforms,
        alerts: settings,
    } = loaded;
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("could not listen on {addr}"))?;
    if !is_loopback(&addr) {
        eprintln!(
            "warning: listening on {addr} with no authentication; anyone who can reach it can read the status"
        );
    }
    // Before the `serving` line: a signal sent once it shows (as
    // `tests/serve_signal.rs` does) must find the handlers in place, or it
    // kills the process instead of stopping it cleanly.
    let shutdown = shutdown_signal();
    // The bound address, so `--port 0` shows the port it got.
    let bound = listener.local_addr().unwrap_or(addr);
    eprintln!("serving http://{bound}/ and http://{bound}/api/status (Ctrl-C to stop)");
    eprintln!("recording history to {}", history_path.display());
    let line = settings.as_ref().map(alerting_line);
    let alerting = alerting(settings)?;
    if let Some(line) = line {
        eprintln!("{line}");
    }
    if let Err(error) = history::prune(&history_path, Utc::now()) {
        eprintln!(
            "warning: could not drop old records from {}: {error}",
            history_path.display()
        );
    }

    let pages = status_pages(&platforms);
    let (tx, latest) = watch::channel(None);
    let backfill_client = client.clone();
    let backfill_path = history_path.clone();
    let recorder =
        Recorder::new(move |records: &[history::Record]| history::append(&history_path, records));
    // The backfill goes first, in the poller's task: the server answers
    // (503 until the first poll) while the vendor feeds load.
    let poller = tokio::spawn(async move {
        backfill(&platforms, &backfill_path, Utc::now(), move |platform| {
            let client = backfill_client.clone();
            async move { providers::fetch_past(&client, &platform).await }
        })
        .await;
        poll(
            platforms,
            POLL_INTERVAL,
            tx,
            move |platforms| {
                let client = client.clone();
                async move { fetch::fetch_all_live(&client, &platforms).await }
            },
            recorder,
            alerting,
        )
        .await;
    });

    let served = axum::serve(listener, router(latest, pages))
        .with_graceful_shutdown(shutdown)
        .await;
    poller.abort();
    served.context("the server stopped")
}

/// A future that resolves on the first of Ctrl-C (SIGINT) and, on Unix,
/// SIGTERM (what launchd, systemd, and `brew services stop` send). On Unix
/// the handlers are installed by this call, not when the future is first
/// polled, so no signal slips past between the two. A handler that can't be
/// installed never fires, so the server runs until killed instead.
fn shutdown_signal() -> impl std::future::Future<Output = ()> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{Signal, SignalKind, signal};
        async fn next(stream: Option<Signal>) {
            match stream {
                Some(mut stream) => {
                    stream.recv().await;
                }
                None => std::future::pending::<()>().await,
            }
        }
        let interrupt = signal(SignalKind::interrupt()).ok();
        let terminate = signal(SignalKind::terminate()).ok();
        async move {
            tokio::select! {
                () = next(interrupt) => {}
                () = next(terminate) => {}
            }
        }
    }
    #[cfg(not(unix))]
    async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Request;
    use chrono::Utc;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::model::Level;
    use crate::providers::builtin;

    fn status(name: &str, level: Level) -> PlatformStatus {
        PlatformStatus {
            name: name.to_owned(),
            level,
            description: String::new(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: Utc::now(),
            error: None,
            probe: None,
            regions: Vec::new(),
            component_tree: Vec::new(),
        }
    }

    async fn call(app: Router, method: &str, path: &str) -> (StatusCode, Option<String>, String) {
        let request = Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let code = response.status();
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_owned());
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (
            code,
            content_type,
            String::from_utf8(body.to_vec()).unwrap(),
        )
    }

    #[tokio::test]
    async fn status_is_503_before_the_first_poll() {
        let (_tx, latest) = watch::channel(None);
        let (code, content_type, body) =
            call(router(latest, StatusPages::new()), "GET", "/api/status").await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(content_type.as_deref(), Some("application/json"));
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["error"], "first fetch still running");
    }

    #[tokio::test]
    async fn status_serves_render_json_of_the_latest_poll() {
        // An Outage is still a 200: callers read `level`.
        let statuses = vec![status("B", Level::Outage), status("A", Level::Operational)];
        let (_tx, latest) = watch::channel(Some(Arc::new(statuses.clone())));
        let (code, content_type, body) =
            call(router(latest, StatusPages::new()), "GET", "/api/status").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("application/json"));
        assert_eq!(body, render::json(&statuses));
    }

    #[tokio::test]
    async fn page_is_503_html_that_retries_before_the_first_poll() {
        let (_tx, latest) = watch::channel(None);
        let (code, content_type, body) = call(router(latest, StatusPages::new()), "GET", "/").await;
        assert_eq!(code, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(content_type.as_deref(), Some("text/html; charset=utf-8"));
        assert_eq!(body, html::loading_page());
    }

    #[tokio::test]
    async fn page_serves_html_page_of_the_latest_poll() {
        // An Outage is still a 200.
        let statuses = vec![status("B", Level::Outage), status("A", Level::Operational)];
        let pages = status_pages(&builtin());
        let (_tx, latest) = watch::channel(Some(Arc::new(statuses.clone())));
        let (code, content_type, body) = call(router(latest, pages.clone()), "GET", "/").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(content_type.as_deref(), Some("text/html; charset=utf-8"));
        assert_eq!(body, html::page(&statuses, &pages));
    }

    #[test]
    fn status_pages_map_each_platform_to_its_page() {
        let pages = status_pages(&builtin());
        assert_eq!(pages.len(), builtin().len());
        assert_eq!(pages["GitHub"], "https://www.githubstatus.com");
    }

    #[tokio::test]
    async fn healthz_answers_ok_before_any_poll() {
        let (_tx, latest) = watch::channel(None);
        let (code, _, body) = call(router(latest, StatusPages::new()), "GET", "/healthz").await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(body, "ok");
    }

    #[tokio::test]
    async fn unknown_paths_are_404_and_other_methods_405() {
        let (_tx, latest) = watch::channel(None);
        let app = router(latest, StatusPages::new());
        assert_eq!(
            call(app.clone(), "GET", "/nope").await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(app.clone(), "GET", "/index.html").await.0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            call(app.clone(), "POST", "/").await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            call(app.clone(), "POST", "/api/status").await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(
            call(app, "DELETE", "/healthz").await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test(start_paused = true)]
    async fn polls_at_start_and_every_interval_and_requests_never_fetch() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let (tx, latest) = watch::channel(None);
        let platforms = builtin();
        let poller = tokio::spawn(poll(
            platforms,
            POLL_INTERVAL,
            tx,
            move |platforms| {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    platforms
                        .iter()
                        .map(|p| {
                            let mut s = status(&p.name, Level::Operational);
                            s.description = format!("poll {n}");
                            s
                        })
                        .collect()
                }
            },
            Recorder::new(|_: &[history::Record]| Ok(())),
            no_alerts(),
        ));
        let app = router(latest.clone(), StatusPages::new());

        // Let the first (immediate) poll run.
        tokio::time::sleep(Duration::from_millis(1)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for _ in 0..5 {
            let (code, _, body) = call(app.clone(), "GET", "/api/status").await;
            assert_eq!(code, StatusCode::OK);
            assert!(body.contains("poll 1"));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1, "requests must not fetch");

        tokio::time::sleep(POLL_INTERVAL.saturating_sub(Duration::from_millis(10))).await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let (_, _, body) = call(app, "GET", "/api/status").await;
        assert!(body.contains("poll 2"));

        poller.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn poller_stops_when_every_receiver_is_gone() {
        let (tx, latest) = watch::channel(None);
        drop(latest);
        let poller = tokio::spawn(poll(
            builtin(),
            POLL_INTERVAL,
            tx,
            |_| async { Vec::new() },
            Recorder::new(|_: &[history::Record]| Ok(())),
            no_alerts(),
        ));
        tokio::time::timeout(Duration::from_secs(1), poller)
            .await
            .expect("the poller should end")
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn polls_write_only_changes_and_a_failed_write_does_not_stop_polling() {
        // Poll n returns level Outage for polls 3 and up, Operational before.
        let polls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&polls);
        // Each write call: the names written. The second write call fails.
        let writes: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let log = Arc::clone(&writes);
        let recorder = Recorder::new(move |records: &[history::Record]| {
            let mut log = log.lock().unwrap();
            log.push(records.iter().map(|r| r.status.name.clone()).collect());
            if log.len() == 2 {
                return Err(io::Error::other("disk full"));
            }
            Ok(())
        });
        let (tx, latest) = watch::channel(None);
        let poller = tokio::spawn(poll(
            builtin(),
            POLL_INTERVAL,
            tx,
            move |platforms| {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                async move {
                    platforms
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let level = if i == 0 && n >= 3 {
                                Level::Outage
                            } else {
                                Level::Operational
                            };
                            status(&p.name, level)
                        })
                        .collect()
                }
            },
            recorder,
            no_alerts(),
        ));

        tokio::time::sleep(Duration::from_millis(1)).await;
        for _ in 0..4 {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        assert_eq!(polls.load(Ordering::SeqCst), 5, "polling went on");
        let first = builtin()[0].name.clone();
        let writes = writes.lock().unwrap().clone();
        // Poll 1: every platform. Polls 2 and 5: no change, no write. Poll 3:
        // the change, but the write fails. Poll 4: the same change again.
        assert_eq!(writes.len(), 3, "{writes:?}");
        assert_eq!(writes[0].len(), builtin().len());
        assert_eq!(writes[1], [first.as_str()]);
        assert_eq!(writes[2], [first.as_str()]);
        assert_eq!(latest.borrow().as_ref().unwrap().len(), builtin().len());
        poller.abort();
    }

    /// A fetcher whose first platform is at `levels[n - 1]` on poll n (the
    /// last level after that) and every other platform Operational.
    fn scripted(
        levels: Vec<Level>,
        polls: Arc<AtomicUsize>,
    ) -> impl Fn(Vec<Platform>) -> std::future::Ready<Vec<PlatformStatus>> {
        move |platforms| {
            let n = polls.fetch_add(1, Ordering::SeqCst);
            let first = levels[n.min(levels.len() - 1)];
            std::future::ready(
                platforms
                    .iter()
                    .enumerate()
                    .map(|(i, p)| status(&p.name, if i == 0 { first } else { Level::Operational }))
                    .collect(),
            )
        }
    }

    type Sent = Arc<Mutex<Vec<(usize, Value)>>>;

    /// A fake sender that records each (webhook index, payload) and answers
    /// `results[index]`.
    fn fake_senders(
        sent: &Sent,
        results: Vec<SendResult>,
    ) -> impl Fn(usize, Value) -> std::future::Ready<SendResult> + use<> {
        let sent = Arc::clone(sent);
        move |index, payload| {
            sent.lock().unwrap().push((index, payload));
            std::future::ready(results[index].clone())
        }
    }

    /// [`fake_senders`] for one webhook.
    fn fake_sender(
        sent: &Sent,
        result: SendResult,
    ) -> impl Fn(usize, Value) -> std::future::Ready<SendResult> + use<> {
        fake_senders(sent, vec![result])
    }

    /// The payloads `sent` got, ignoring which webhook.
    fn payloads(sent: &Sent) -> Vec<Value> {
        sent.lock()
            .unwrap()
            .iter()
            .map(|(_, p)| p.clone())
            .collect()
    }

    async fn run_polls<S, SFut>(
        levels: Vec<Level>,
        alerting: Option<Alerting<S>>,
        writes: &Arc<AtomicUsize>,
    ) where
        S: Fn(usize, Value) -> SFut + Send + 'static,
        SFut: Future<Output = SendResult> + Send + 'static,
    {
        let polls = Arc::new(AtomicUsize::new(0));
        let count = levels.len();
        let written = Arc::clone(writes);
        let (tx, _latest) = watch::channel(None);
        let poller = tokio::spawn(poll(
            builtin(),
            POLL_INTERVAL,
            tx,
            scripted(levels, Arc::clone(&polls)),
            Recorder::new(move |_: &[history::Record]| {
                written.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            alerting,
        ));
        tokio::time::sleep(Duration::from_millis(1)).await;
        for _ in 1..count {
            tokio::time::sleep(POLL_INTERVAL).await;
        }
        assert_eq!(polls.load(Ordering::SeqCst), count, "polling went on");
        poller.abort();
    }

    #[test]
    fn without_a_webhook_there_is_no_alerting() {
        // `run` hands this straight to `poll`, so no alert is ever sent.
        assert!(alerting(None).unwrap().is_none());
    }

    #[tokio::test]
    async fn a_non_2xx_webhook_does_not_stop_polling_or_history() {
        use axum::routing::post as route_post;
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let hook_app = Router::new().route(
            "/hook",
            route_post(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::INTERNAL_SERVER_ERROR }
            }),
        );
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(socket, hook_app).await });
        let config =
            crate::config::parse(&format!("[alerts]\nwebhook = \"http://{addr}/hook\"\n")).unwrap();
        let settings = crate::config::alerts(&config).unwrap();

        let polls = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let written = Arc::clone(&writes);
        let (tx, _latest) = watch::channel(None);
        let poller = tokio::spawn(poll(
            builtin(),
            Duration::from_millis(20),
            tx,
            // Operational, then Outage (alerts on poll 3), then Degraded
            // from poll 5 (alerts on poll 6).
            scripted(
                vec![
                    Level::Operational,
                    Level::Outage,
                    Level::Outage,
                    Level::Outage,
                    Level::Degraded,
                ],
                Arc::clone(&polls),
            ),
            Recorder::new(move |_: &[history::Record]| {
                written.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }),
            alerting(settings).unwrap(),
        ));
        let deadline = Instant::now() + Duration::from_secs(5);
        while polls.load(Ordering::SeqCst) < 8 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        poller.abort();
        assert!(polls.load(Ordering::SeqCst) >= 8, "polling went on");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "each change POSTed once, not retried"
        );
        // History: start-up, Outage, Degraded.
        assert_eq!(writes.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn a_held_change_sends_one_alert_per_change() {
        let sent = Sent::default();
        let alerting = Some(Alerting::new(vec![1], None, fake_sender(&sent, Ok(()))));
        let writes = Arc::new(AtomicUsize::new(0));
        run_polls(
            vec![
                Level::Outage, // start-up: seeds only
                Level::Outage,
                Level::Operational,
                Level::Operational,
                Level::Operational,
                Level::Unknown, // a one-poll blip
                Level::Operational,
            ],
            alerting,
            &writes,
        )
        .await;
        let sent = payloads(&sent);
        assert_eq!(sent.len(), 1, "{sent:?}");
        assert_eq!(sent[0]["platform"], builtin()[0].name.as_str());
        assert_eq!(sent[0]["from"], "outage");
        assert_eq!(sent[0]["to"], "operational");
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_send_does_not_stop_polling_or_history() {
        let sent = Sent::default();
        let alerting = Some(Alerting::new(
            vec![1],
            None,
            fake_sender(&sent, Err("HTTP 500 Internal Server Error".to_owned())),
        ));
        let writes = Arc::new(AtomicUsize::new(0));
        run_polls(
            vec![
                Level::Operational,
                Level::Outage,
                Level::Outage, // alert fails here
                Level::Operational,
                Level::Operational, // and here; not retried
                Level::Degraded,
            ],
            alerting,
            &writes,
        )
        .await;
        assert_eq!(sent.lock().unwrap().len(), 2, "each change tried once");
        // History: start-up, Outage, Operational, Degraded.
        assert_eq!(writes.load(Ordering::SeqCst), 4);
    }

    /// Operational, Outage held (alerts), Degraded held (alerts),
    /// Operational held (alerts).
    fn three_changes() -> Vec<Level> {
        vec![
            Level::Operational,
            Level::Outage,
            Level::Outage,
            Level::Degraded,
            Level::Degraded,
            Level::Operational,
            Level::Operational,
        ]
    }

    #[tokio::test(start_paused = true)]
    async fn each_alert_goes_to_every_webhook_once() {
        let sent = Sent::default();
        let alerting = Some(Alerting::new(
            vec![1, 3],
            None,
            fake_senders(&sent, vec![Ok(()), Ok(())]),
        ));
        run_polls(three_changes(), alerting, &Arc::new(AtomicUsize::new(0))).await;
        let mut sent: Vec<(usize, String)> = sent
            .lock()
            .unwrap()
            .iter()
            .map(|(index, p)| (*index, format!("{} → {}", p["from"], p["to"])))
            .collect();
        sent.sort();
        assert_eq!(
            sent,
            [
                (0, "\"degraded\" → \"operational\"".to_owned()),
                (0, "\"operational\" → \"outage\"".to_owned()),
                (0, "\"outage\" → \"degraded\"".to_owned()),
                (1, "\"degraded\" → \"operational\"".to_owned()),
                (1, "\"operational\" → \"outage\"".to_owned()),
                (1, "\"outage\" → \"degraded\"".to_owned()),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn levels_drop_changes_that_do_not_touch_them() {
        let sent = Sent::default();
        let alerting = Some(Alerting::new(
            vec![1],
            Some(vec![Level::Outage]),
            fake_sender(&sent, Ok(())),
        ));
        run_polls(three_changes(), alerting, &Arc::new(AtomicUsize::new(0))).await;
        let changes: Vec<(Value, Value)> = payloads(&sent)
            .iter()
            .map(|p| (p["from"].clone(), p["to"].clone()))
            .collect();
        // Degraded → Operational touches no listed level.
        assert_eq!(
            changes,
            [
                (Value::from("operational"), Value::from("outage")),
                (Value::from("outage"), Value::from("degraded")),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_failing_webhook_does_not_stop_the_others() {
        let sent = Sent::default();
        let alerting = Some(Alerting::new(
            vec![1, 2],
            None,
            fake_senders(&sent, vec![Err("could not connect".to_owned()), Ok(())]),
        ));
        let writes = Arc::new(AtomicUsize::new(0));
        run_polls(three_changes(), alerting, &writes).await;
        let per_webhook = |wanted: usize| {
            sent.lock()
                .unwrap()
                .iter()
                .filter(|(index, _)| *index == wanted)
                .count()
        };
        assert_eq!((per_webhook(0), per_webhook(1)), (3, 3));
        assert_eq!(writes.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn the_alerting_line_counts_webhooks_and_names_levels_without_urls() {
        let settings = |text: &str| {
            crate::config::alerts(&crate::config::parse(text).unwrap())
                .unwrap()
                .unwrap()
        };
        let one = settings("[alerts]\nwebhook = \"https://hooks.example.com/s3cret\"\n");
        assert_eq!(
            alerting_line(&one),
            "sending level changes to the [alerts] webhook"
        );
        let two = settings(
            "[alerts]\nwebhook = [\"https://hooks.example.com/s3cret\", \"http://127.0.0.1:9/h\"]\nlevels = [\"outage\", \"unknown\"]\n",
        );
        assert_eq!(
            alerting_line(&two),
            "sending level changes to 2 [alerts] webhooks (into or out of: outage, unknown)"
        );
    }

    #[test]
    fn loopback_addresses_are_recognised() {
        assert!(is_loopback(&"127.0.0.1:7341".parse().unwrap()));
        assert!(is_loopback(&"[::1]:7341".parse().unwrap()));
        assert!(is_loopback(&"[::ffff:127.0.0.1]:7341".parse().unwrap()));
        assert!(!is_loopback(&"[::]:7341".parse().unwrap()));
        assert!(!is_loopback(&"0.0.0.0:7341".parse().unwrap()));
        assert!(!is_loopback(&"192.168.1.5:7341".parse().unwrap()));
    }

    fn past(start: DateTime<Utc>, end: DateTime<Utc>) -> PastIncident {
        PastIncident {
            level: Level::Outage,
            incident: crate::model::Incident {
                title: "Past".to_owned(),
                started_at: Some(start),
                ended_at: Some(end),
                ..Default::default()
            },
        }
    }

    #[tokio::test]
    async fn backfill_fills_before_each_platforms_earliest_record_once() {
        use chrono::{TimeDelta, TimeZone};
        let dir = std::env::temp_dir().join(format!("aretheydown-backfill-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("history.jsonl");
        let now = Utc.with_ymd_and_hms(2026, 11, 14, 12, 0, 0).unwrap();
        let hours_ago = move |h| now - TimeDelta::hours(h);
        // GitHub was recorded 5h ago; Cloudflare never.
        let seen = history::Record {
            recorded_at: hours_ago(5),
            status: status("GitHub", Level::Operational),
            backfilled: false,
        };
        history::append(&path, std::slice::from_ref(&seen)).unwrap();
        let platforms: Vec<Platform> = builtin()
            .into_iter()
            .filter(|p| {
                ["GitHub", "Cloudflare", "AWS", "Google Cloud", "OpenAI"].contains(&p.name.as_str())
            })
            .collect();
        let fetch_past = move |platform: Platform| async move {
            match platform.name.as_str() {
                // One incident before the record, one after: only the first.
                "GitHub" => Some(Ok(vec![
                    past(hours_ago(8), hours_ago(7)),
                    past(hours_ago(3), hours_ago(2)),
                ])),
                "Cloudflare" => Some(Ok(vec![past(hours_ago(3), hours_ago(2))])),
                "Google Cloud" => Some(Err(anyhow::anyhow!("HTTP 503"))),
                "OpenAI" => panic!("a fetch that panics is a warning, not a crash"),
                _ => None,
            }
        };

        assert_eq!(backfill(&platforms, &path, now, fetch_past).await, 4);
        let records = history::read(&path).unwrap().unwrap();
        let rows: Vec<(&str, DateTime<Utc>, Level, bool)> = records
            .iter()
            .map(|r| {
                (
                    r.status.name.as_str(),
                    r.recorded_at,
                    r.status.level,
                    r.backfilled,
                )
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("GitHub", hours_ago(5), Level::Operational, false),
                ("GitHub", hours_ago(8), Level::Outage, true),
                ("GitHub", hours_ago(7), Level::Operational, true),
                ("Cloudflare", hours_ago(3), Level::Outage, true),
                ("Cloudflare", hours_ago(2), Level::Operational, true),
            ]
        );
        // `--at` 6h ago sees GitHub's backfilled state; Cloudflare had none yet.
        let then = history::at_records(&records, hours_ago(6));
        assert_eq!(then.len(), 1);
        assert!(then[0].backfilled);
        assert_eq!(then[0].status.level, Level::Operational);

        // A second start adds nothing.
        let later = now + TimeDelta::minutes(5);
        assert_eq!(backfill(&platforms, &path, later, fetch_past).await, 0);
        assert_eq!(history::read(&path).unwrap().unwrap(), records);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
