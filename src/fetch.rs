//! Fetch many platforms at once, each in its own task with its own timeout.

use std::any::Any;
use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use chrono::Utc;
use tokio::task::{JoinError, JoinSet};

use crate::model::PlatformStatus;
use crate::providers::{self, Platform};

/// How often the long-running UIs (the TUI and `--serve`) fetch every platform.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// How long one platform gets before it is reported as `Unknown`.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

tokio::task_local! {
    /// Set while a platform's fetch is being polled; see [`in_fetch_task`].
    static IN_FETCH: ();
}

/// Whether the caller is running inside one platform's fetch task.
///
/// A panic hook can call this to tell a fetch panic (which [`fetch_all`]
/// catches and reports as `Unknown`) from one that ends the program.
#[must_use]
pub fn in_fetch_task() -> bool {
    IN_FETCH.try_with(|()| ()).is_ok()
}

/// Fetch every platform concurrently with `fetch`, returning statuses in input order.
///
/// Each platform runs in its own task. A platform whose fetch fails, panics,
/// or takes longer than `timeout` comes back as `Unknown` with the reason in
/// `error`; the others are unaffected. Dropping the returned future aborts
/// the fetches still running.
///
/// `fetch` itself is called on the caller's task; only the future it returns
/// runs in the platform's task. Keep its work inside that future (an
/// `async move` block or `async fn`) so a panic is caught.
pub async fn fetch_all<F, Fut>(
    platforms: &[Platform],
    timeout: Duration,
    fetch: F,
) -> Vec<PlatformStatus>
where
    F: Fn(Platform) -> Fut,
    Fut: Future<Output = Result<PlatformStatus>> + Send + 'static,
{
    let mut tasks = JoinSet::new();
    let mut slot_of = HashMap::new();
    for (slot, platform) in platforms.iter().cloned().enumerate() {
        let name = platform.name.clone();
        let check = fetch(platform);
        let task = tasks.spawn(IN_FETCH.scope((), async move {
            match tokio::time::timeout(timeout, check).await {
                Ok(Ok(status)) => status,
                Ok(Err(error)) => PlatformStatus::unknown(name, format!("{error:#}"), Utc::now()),
                Err(_) => PlatformStatus::unknown(
                    name,
                    format!("timed out after {}s", timeout.as_secs()),
                    Utc::now(),
                ),
            }
        }));
        slot_of.insert(task.id(), slot);
    }

    // Every task reports exactly once and replaces its placeholder; starting
    // from placeholders (not `Option`s) keeps this free of `unwrap`.
    let mut statuses: Vec<PlatformStatus> = platforms
        .iter()
        .map(|p| PlatformStatus::unknown(&p.name, "fetch did not finish", Utc::now()))
        .collect();
    while let Some(joined) = tasks.join_next_with_id().await {
        match joined {
            Ok((id, status)) => statuses[slot_of[&id]] = status,
            Err(error) => {
                let slot = slot_of[&error.id()];
                statuses[slot] = PlatformStatus::unknown(
                    platforms[slot].name.clone(),
                    describe_join_error(error),
                    Utc::now(),
                );
            }
        }
    }
    statuses
}

/// Describe a fetch task that ended without a status.
fn describe_join_error(error: JoinError) -> String {
    match error.try_into_panic() {
        Ok(payload) => match panic_message(payload.as_ref()) {
            Some(message) => format!("fetch panicked: {message}"),
            None => "fetch panicked".to_owned(),
        },
        Err(_) => "fetch was cancelled".to_owned(),
    }
}

/// The text of a panic, when it was raised with a string message.
fn panic_message(payload: &(dyn Any + Send)) -> Option<&str> {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
}

/// Fetch every platform from its live status feed, each within [`DEFAULT_TIMEOUT`].
pub async fn fetch_all_live(
    client: &reqwest::Client,
    platforms: &[Platform],
) -> Vec<PlatformStatus> {
    fetch_all(platforms, DEFAULT_TIMEOUT, |platform| {
        let client = client.clone();
        async move { providers::fetch(&client, &platform).await }
    })
    .await
}
