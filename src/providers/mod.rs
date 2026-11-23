//! Where each platform's status comes from, and how to fetch it.

pub mod aws;
pub mod azure;
pub mod check;
pub mod gcp;
pub mod m365;
pub mod probe;
pub mod statuspage;

use std::future::Future;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::model::{PastIncident, PlatformStatus, Probe};

/// A platform to check: its display name, where its status lives, and the
/// front door to probe beside it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Platform {
    pub name: String,
    pub source: Source,
    /// Probed at every fetch, shown beside the level ([`probe`]).
    pub probe: Option<probe::Target>,
}

/// The kind of status feed a platform publishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// Atlassian Statuspage: `url` is the page's `/api/v2/summary.json`. With
    /// `components`, only those components (by component or group name) and
    /// the incidents touching them count.
    Statuspage {
        url: String,
        components: Option<Vec<String>>,
    },
    /// AWS Health Dashboard: `url` is the public `currentevents` feed. With
    /// `regions`, only events in those region codes (plus global events) count.
    Aws {
        url: String,
        regions: Option<Vec<String>>,
    },
    /// Azure's status RSS feed: `url` is the feed itself.
    Azure { url: String },
    /// Google Cloud's `incidents.json`: recent incidents, the active ones without an `end`.
    GoogleCloud { url: String },
    /// Microsoft 365's public consumer services feed (`api/posts/m365Consumer`).
    Microsoft365 { url: String },
    /// One of your own endpoints (`[[check]]`): a GET that is up when it
    /// answers `expect`, or any 2xx without it.
    Check { url: String, expect: Option<u16> },
}

impl Source {
    /// The feed URL to fetch.
    #[must_use]
    pub fn url(&self) -> &str {
        match self {
            Self::Statuspage { url, .. }
            | Self::Aws { url, .. }
            | Self::Azure { url }
            | Self::GoogleCloud { url }
            | Self::Microsoft365 { url }
            | Self::Check { url, .. } => url,
        }
    }

    /// The page a person reads in a browser: for Statuspage, the feed URL
    /// without `/api/v2/summary.json`; for a check, the URL it checks; for the
    /// others, a fixed page.
    #[must_use]
    pub fn status_page(&self) -> String {
        match self {
            Self::Statuspage { url, .. } => url
                .strip_suffix(STATUSPAGE_SUMMARY_PATH)
                .unwrap_or(url)
                .to_owned(),
            Self::Aws { .. } => aws::DASHBOARD_URL.to_owned(),
            Self::Azure { .. } => azure::STATUS_PAGE_URL.to_owned(),
            Self::GoogleCloud { .. } => gcp::STATUS_ROOT.to_owned(),
            Self::Microsoft365 { .. } => m365::STATUS_PAGE_URL.to_owned(),
            Self::Check { url, .. } => url.clone(),
        }
    }
}

/// Where a Statuspage site serves its summary, relative to the page root.
const STATUSPAGE_SUMMARY_PATH: &str = "/api/v2/summary.json";
/// Where a Statuspage site lists recent incidents, resolved ones included.
const STATUSPAGE_INCIDENTS_PATH: &str = "/api/v2/incidents.json";

/// How long the `incidents.json` request may take. It runs alongside the
/// summary and stays under [`crate::fetch::DEFAULT_TIMEOUT`], so a slow
/// history feed is dropped (no resolved list) instead of timing the whole
/// platform out.
pub const RESOLVED_TIMEOUT: Duration = Duration::from_secs(8);

/// The `incidents.json` URL for a Statuspage summary URL; `None` when the URL
/// does not end in `/api/v2/summary.json`.
#[must_use]
pub fn statuspage_incidents_url(summary_url: &str) -> Option<String> {
    summary_url
        .strip_suffix(STATUSPAGE_SUMMARY_PATH)
        .map(|root| format!("{root}{STATUSPAGE_INCIDENTS_PATH}"))
}

impl Platform {
    /// Limit an AWS platform to events in these region codes; other sources are
    /// left as they are.
    pub fn limit_aws_regions(&mut self, codes: &[String]) {
        if let Source::Aws { regions, .. } = &mut self.source {
            *regions = Some(codes.to_vec());
        }
    }

    /// Watch only these components on a Statuspage platform. Returns `false`
    /// (and changes nothing) for any other source.
    pub fn watch_components(&mut self, names: &[String]) -> bool {
        if let Source::Statuspage { components, .. } = &mut self.source {
            *components = Some(names.to_vec());
            true
        } else {
            false
        }
    }

    /// A check of one of your own endpoints (see [`Source::Check`]).
    #[must_use]
    pub fn check(name: &str, url: &str, expect: Option<u16>) -> Self {
        Self {
            name: name.to_owned(),
            source: Source::Check {
                url: url.to_owned(),
                expect,
            },
            probe: None,
        }
    }

    /// A platform with no probe, `source` alone.
    #[must_use]
    pub fn new(name: &str, source: Source) -> Self {
        Self {
            name: name.to_owned(),
            source,
            probe: None,
        }
    }

    /// A platform hosted on Atlassian Statuspage, given its page root (e.g. `https://www.githubstatus.com`).
    #[must_use]
    pub fn statuspage(name: &str, page_root: &str) -> Self {
        Self {
            name: name.to_owned(),
            source: Source::Statuspage {
                url: format!(
                    "{}{STATUSPAGE_SUMMARY_PATH}",
                    page_root.trim_end_matches('/')
                ),
                components: None,
            },
            probe: None,
        }
    }
}

/// The platforms checked when nothing else is configured, each with its
/// default probe ([`probe::DEFAULTS`]).
#[must_use]
pub fn builtin() -> Vec<Platform> {
    let mut platforms = vec![
        Platform::statuspage("GitHub", "https://www.githubstatus.com"),
        Platform::statuspage("Cloudflare", "https://www.cloudflarestatus.com"),
        Platform::new(
            "AWS",
            Source::Aws {
                url: aws::CURRENT_EVENTS_URL.to_owned(),
                regions: None,
            },
        ),
        Platform::new(
            "Azure",
            Source::Azure {
                url: "https://azure.status.microsoft/en-us/status/feed/".to_owned(),
            },
        ),
        Platform::new(
            "Google Cloud",
            Source::GoogleCloud {
                url: "https://status.cloud.google.com/incidents.json".to_owned(),
            },
        ),
        Platform::statuspage("OpenAI", "https://status.openai.com"),
        // status.anthropic.com redirects here; fetch it directly.
        Platform::statuspage("Anthropic", "https://status.claude.com"),
        Platform::new(
            "Microsoft 365",
            Source::Microsoft365 {
                url: m365::CONSUMER_FEED_URL.to_owned(),
            },
        ),
    ];
    for platform in &mut platforms {
        platform.probe = probe::default_for(&platform.name);
    }
    platforms
}

/// Fetch and parse one platform's current status, and probe its front door
/// at the same time when it has a probe ([`with_probe`]).
///
/// # Errors
///
/// Only without a probe: as [`fetch_feed`]. With one, a failed feed is an
/// `Unknown` status that still carries the probe.
pub async fn fetch(client: &reqwest::Client, platform: &Platform) -> Result<PlatformStatus> {
    let feed = fetch_feed(client, platform);
    match &platform.probe {
        None => feed.await,
        Some(target) => {
            let probe = probe::run(probe::client(), target);
            Ok(with_probe(&platform.name, feed, probe).await)
        }
    }
}

/// Run a platform's `feed` fetch and its `probe` together and put the probe
/// on the status. A failed feed becomes `Unknown` with the error, as
/// [`crate::fetch::fetch_all`] would make it, so the probe is not lost. The
/// probe gives up within [`probe::TIMEOUT`], under the platform's timeout,
/// so it never holds the feed's result past it.
pub async fn with_probe(
    name: &str,
    feed: impl Future<Output = Result<PlatformStatus>>,
    probe: impl Future<Output = Probe>,
) -> PlatformStatus {
    let (feed, probe) = tokio::join!(feed, probe);
    let mut status = feed
        .unwrap_or_else(|error| PlatformStatus::unknown(name, format!("{error:#}"), Utc::now()));
    status.probe = Some(probe);
    status
}

/// Fetch and parse one platform's status feed. Statuspage platforms also
/// fetch `incidents.json`, at the same time, for the resolved list; when that
/// request fails the status is just the summary's, with no resolved list. A
/// check never fails: no answer is its own Outage ([`check::run`]).
///
/// # Errors
///
/// Fails when the request fails, the server returns an error status, or the body
/// cannot be parsed.
pub async fn fetch_feed(client: &reqwest::Client, platform: &Platform) -> Result<PlatformStatus> {
    let name = platform.name.as_str();
    let source = &platform.source;
    if let Source::Check { url, expect } = source {
        return Ok(check::run(client, name, url, *expect).await);
    }
    if let Source::Statuspage { url, components } = source {
        // Never fails: a failed history request is just no resolved list.
        let history = async {
            let Some(url) = statuspage_incidents_url(url) else {
                return Ok(None);
            };
            Ok(get(client, &url, Some(RESOLVED_TIMEOUT)).await.ok())
        };
        // A failed summary returns at once, dropping the history request.
        let (summary, incidents) = tokio::try_join!(get(client, url, None), history)?;
        return statuspage::parse_with_resolved(
            name,
            &summary,
            incidents.as_deref(),
            Utc::now(),
            components.as_deref(),
        );
    }
    let body = get(client, source.url(), None).await?;
    parse_single_feed(name, source, &body)
}

/// How long each request of a history backfill may take ([`fetch_past`]).
pub const BACKFILL_TIMEOUT: Duration = Duration::from_secs(10);

/// Fetch a platform's past incidents for history backfill
/// ([`crate::history::backfill`]): Statuspage `incidents.json` (plus the
/// summary, for group names, when `[components]` is set) or Google Cloud's
/// `incidents.json`. `None` for sources with no incident history (AWS,
/// Azure, Microsoft 365, checks).
///
/// # Errors
///
/// Inside the `Some`: when a request fails or a body cannot be parsed.
pub async fn fetch_past(
    client: &reqwest::Client,
    platform: &Platform,
) -> Option<Result<Vec<PastIncident>>> {
    let timeout = Some(BACKFILL_TIMEOUT);
    match &platform.source {
        Source::Statuspage { url, components } => Some(
            async {
                let incidents_url = statuspage_incidents_url(url)
                    .context("the feed URL does not end in /api/v2/summary.json")?;
                let summary = async {
                    match components {
                        Some(_) => get(client, url, timeout).await.map(Some),
                        None => Ok(None),
                    }
                };
                let (incidents, summary) =
                    tokio::try_join!(get(client, &incidents_url, timeout), summary)?;
                statuspage::parse_past(&incidents, summary.as_deref(), components.as_deref())
            }
            .await,
        ),
        Source::GoogleCloud { url } => Some(
            get(client, url, timeout)
                .await
                .and_then(|body| gcp::parse_past(&body)),
        ),
        Source::Aws { .. }
        | Source::Azure { .. }
        | Source::Microsoft365 { .. }
        | Source::Check { .. } => None,
    }
}

/// Parse the one feed a non-Statuspage source publishes.
fn parse_single_feed(name: &str, source: &Source, body: &[u8]) -> Result<PlatformStatus> {
    let now = Utc::now();
    match source {
        // `fetch` handles Statuspage's two feeds itself; this is the summary alone.
        Source::Statuspage { components, .. } => {
            statuspage::parse_watching(name, body, now, components.as_deref())
        }
        Source::Aws { regions, .. } => aws::parse_in_regions(name, body, now, regions.as_deref()),
        Source::Azure { .. } => azure::parse(name, body, now),
        Source::GoogleCloud { .. } => gcp::parse(name, body, now),
        Source::Microsoft365 { .. } => m365::parse(name, body, now),
        // `fetch` runs checks itself; they have no feed.
        Source::Check { .. } => anyhow::bail!("a check has no feed to parse"),
    }
}

/// GET `url` and read the whole body, failing on an error status. With
/// `timeout`, the request (body included) is cut off after that long.
async fn get(client: &reqwest::Client, url: &str, timeout: Option<Duration>) -> Result<Vec<u8>> {
    let mut request = client.get(url);
    if let Some(timeout) = timeout {
        request = request.timeout(timeout);
    }
    request
        .send()
        .await
        .context("request failed")?
        .error_for_status()?
        .bytes()
        .await
        .map(Vec::from)
        .context("reading the response body failed")
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::{RESOLVED_TIMEOUT, Source, builtin, probe, statuspage_incidents_url, with_probe};
    use crate::fetch::{DEFAULT_TIMEOUT, fetch_all};
    use crate::model::{Level, PlatformStatus, Probe};

    #[test]
    fn the_history_request_gives_up_before_the_platform_times_out() {
        assert!(RESOLVED_TIMEOUT < DEFAULT_TIMEOUT);
    }

    #[test]
    fn incidents_url_sits_next_to_the_summary() {
        assert_eq!(
            statuspage_incidents_url("https://status.claude.com/api/v2/summary.json").as_deref(),
            Some("https://status.claude.com/api/v2/incidents.json")
        );
        assert_eq!(
            statuspage_incidents_url("https://status.example.com/custom.json"),
            None
        );
    }

    #[test]
    fn every_builtin_has_its_status_page() {
        let pages: Vec<(String, String)> = builtin()
            .into_iter()
            .map(|p| (p.name, p.source.status_page()))
            .collect();
        let expected = [
            ("GitHub", "https://www.githubstatus.com"),
            ("Cloudflare", "https://www.cloudflarestatus.com"),
            ("AWS", "https://health.aws.amazon.com/health/status"),
            ("Azure", "https://azure.status.microsoft/en-us/status"),
            ("Google Cloud", "https://status.cloud.google.com/"),
            ("OpenAI", "https://status.openai.com"),
            ("Anthropic", "https://status.claude.com"),
            ("Microsoft 365", "https://status.cloud.microsoft/"),
        ];
        let expected: Vec<(String, String)> = expected
            .iter()
            .map(|(name, page)| ((*name).to_owned(), (*page).to_owned()))
            .collect();
        assert_eq!(pages, expected);
    }

    #[test]
    fn every_builtin_probes_its_front_door_by_default() {
        let probes: Vec<(String, String, String)> = builtin()
            .into_iter()
            .map(|p| {
                let target = p.probe.expect("every built-in has a probe");
                (p.name, target.url, target.expect.to_string())
            })
            .collect();
        let expected = [
            ("GitHub", "https://github.com/", "2xx"),
            (
                "Cloudflare",
                "https://www.cloudflare.com/cdn-cgi/trace",
                "2xx",
            ),
            ("AWS", "https://s3.amazonaws.com/", "3xx"),
            ("Azure", "https://management.azure.com/", "4xx"),
            ("Google Cloud", "https://storage.googleapis.com/", "4xx"),
            ("OpenAI", "https://api.openai.com/v1/models", "401"),
            ("Anthropic", "https://api.anthropic.com/v1/models", "401"),
            ("Microsoft 365", "https://outlook.office.com/", "3xx"),
        ];
        let expected: Vec<(String, String, String)> = expected
            .iter()
            .map(|(n, u, e)| ((*n).to_owned(), (*u).to_owned(), (*e).to_owned()))
            .collect();
        assert_eq!(probes, expected);
    }

    #[test]
    fn added_platforms_have_no_probe() {
        assert_eq!(
            super::Platform::statuspage("Vercel", "https://x.example").probe,
            None
        );
        assert_eq!(
            super::Platform::check("API", "https://x.example", None).probe,
            None
        );
    }

    fn probe_result(ok: bool) -> Probe {
        Probe {
            url: "https://api.example.com/".to_owned(),
            ok,
            code: None,
            elapsed_ms: None,
            description: "no answer (timed out after 9s)".to_owned(),
        }
    }

    /// The feed answers at once; the probe takes its whole timeout.
    #[tokio::test(start_paused = true)]
    async fn a_slow_probe_does_not_push_the_feed_past_the_platform_timeout() {
        let platforms = [super::Platform::statuspage("Vendor", "https://x.example")];
        let started = tokio::time::Instant::now();
        let statuses = fetch_all(&platforms, DEFAULT_TIMEOUT, |platform| async move {
            let feed = async {
                let mut status = PlatformStatus::unknown(&platform.name, "x", Utc::now());
                status.level = Level::Operational;
                status.error = None;
                Ok(status)
            };
            let probe = async {
                tokio::time::sleep(probe::TIMEOUT).await;
                probe_result(false)
            };
            Ok(with_probe(&platform.name, feed, probe).await)
        })
        .await;
        assert!(started.elapsed() < DEFAULT_TIMEOUT);
        assert_eq!(statuses[0].level, Level::Operational, "{:?}", statuses[0]);
        assert_eq!(statuses[0].error, None);
        assert_eq!(statuses[0].probe, Some(probe_result(false)));
    }

    #[tokio::test]
    async fn a_failed_feed_keeps_the_probe() {
        let feed = async { Err(anyhow::anyhow!("HTTP 502")) };
        let status = with_probe("Vendor", feed, async { probe_result(true) }).await;
        assert_eq!(status.level, Level::Unknown);
        assert_eq!(status.error.as_deref(), Some("HTTP 502"));
        assert_eq!(status.probe, Some(probe_result(true)));
    }

    #[test]
    fn a_check_opens_the_url_it_checks() {
        let check = super::Platform::check("API", "https://api.example.com/healthz", None);
        assert_eq!(
            check.source.status_page(),
            "https://api.example.com/healthz"
        );
        assert_eq!(check.source.url(), "https://api.example.com/healthz");
    }

    #[test]
    fn a_statuspage_url_without_the_summary_path_is_its_own_page() {
        let source = Source::Statuspage {
            url: "https://status.example.com/custom.json".to_owned(),
            components: None,
        };
        assert_eq!(
            source.status_page(),
            "https://status.example.com/custom.json"
        );
    }
}
