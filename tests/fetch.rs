//! Concurrent fetching with a per-platform timeout, using fake fetchers (no network).

use std::time::Duration;

use anyhow::anyhow;
use aretheydown::fetch::{fetch_all, in_fetch_task};
use aretheydown::model::{Level, PlatformStatus};
use aretheydown::providers::Platform;
use chrono::Utc;
use tokio::time::{Instant, sleep};

fn platforms(names: &[&str]) -> Vec<Platform> {
    names
        .iter()
        .map(|name| Platform::statuspage(name, "https://example.invalid"))
        .collect()
}

fn operational(name: &str) -> PlatformStatus {
    PlatformStatus {
        name: name.to_owned(),
        level: Level::Operational,
        description: "All Systems Operational".to_owned(),
        incidents: Vec::new(),
        resolved: Vec::new(),
        fetched_at: Utc::now(),
        error: None,
        probe: None,
        regions: Vec::new(),
        component_tree: Vec::new(),
    }
}

#[tokio::test(start_paused = true)]
async fn platforms_are_fetched_concurrently_in_order() {
    let start = Instant::now();
    let statuses = fetch_all(
        &platforms(&["A", "B", "C"]),
        Duration::from_secs(10),
        |p| async move {
            sleep(Duration::from_secs(3)).await;
            Ok(operational(&p.name))
        },
    )
    .await;

    // Run one after another, these would take 9s.
    assert!(start.elapsed() < Duration::from_secs(4));
    let names: Vec<_> = statuses.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["A", "B", "C"]);
}

#[tokio::test(start_paused = true)]
async fn a_slow_platform_times_out_as_unknown_and_others_still_report() {
    let start = Instant::now();
    let statuses = fetch_all(
        &platforms(&["Fast", "Slow"]),
        Duration::from_secs(10),
        |p| async move {
            if p.name == "Slow" {
                sleep(Duration::from_secs(60)).await;
            }
            Ok(operational(&p.name))
        },
    )
    .await;

    assert!(start.elapsed() < Duration::from_secs(11));
    assert_eq!(statuses[0].level, Level::Operational);
    assert_eq!(statuses[1].name, "Slow");
    assert_eq!(statuses[1].level, Level::Unknown);
    assert_eq!(statuses[1].error.as_deref(), Some("timed out after 10s"));
}

#[tokio::test]
async fn a_failing_platform_is_unknown_with_its_error() {
    let statuses = fetch_all(
        &platforms(&["Ok", "Broken"]),
        Duration::from_secs(10),
        |p| async move {
            if p.name == "Broken" {
                return Err(anyhow!("HTTP status server error (503)"));
            }
            Ok(operational(&p.name))
        },
    )
    .await;

    assert_eq!(statuses[0].level, Level::Operational);
    assert_eq!(statuses[1].level, Level::Unknown);
    assert_eq!(
        statuses[1].error.as_deref(),
        Some("HTTP status server error (503)")
    );
}

#[tokio::test]
async fn a_panicking_platform_is_unknown_and_others_still_report() {
    let statuses = fetch_all(
        &platforms(&["Before", "Panics", "After"]),
        Duration::from_secs(10),
        |p| async move {
            assert!(p.name != "Panics", "feed had no `status` field");
            Ok(operational(&p.name))
        },
    )
    .await;

    let names: Vec<_> = statuses.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Before", "Panics", "After"]);
    assert_eq!(statuses[0].level, Level::Operational);
    assert_eq!(statuses[1].level, Level::Unknown);
    assert_eq!(
        statuses[1].error.as_deref(),
        Some("fetch panicked: feed had no `status` field")
    );
    assert_eq!(statuses[2].level, Level::Operational);
}

#[tokio::test]
async fn fetches_run_inside_a_fetch_task() {
    assert!(!in_fetch_task());
    let statuses = fetch_all(
        &platforms(&["A"]),
        Duration::from_secs(10),
        |p| async move {
            assert!(in_fetch_task());
            Ok(operational(&p.name))
        },
    )
    .await;
    assert_eq!(statuses[0].level, Level::Operational);
}
