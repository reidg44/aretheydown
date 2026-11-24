//! `--serve` alerts: POST a JSON alert to each `[alerts] webhook` when a
//! platform's level changes and the new level holds for two polls in a row,
//! limited to the changes [`wanted`] by `[alerts] levels`.
//!
//! [`Alerter`], [`wanted`], and [`payload`] are pure; [`post`] is the one
//! network call. Webhook URLs are secrets, so nothing here prints them.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::time::Duration;

use chrono::SecondsFormat;
use serde_json::{Value, json};

use crate::config::Webhook;
use crate::model::{Level, PlatformStatus};
use crate::render::{UPDATE_TEXT_LIMIT, shorten};

/// What sending one alert gives: `Err` is why it failed, without the URL.
pub type SendResult = Result<(), String>;

/// How long one POST may take before it is dropped with a warning.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// How many polls in a row a new level must be seen before it alerts.
const CONFIRM_POLLS: u8 = 2;

/// A confirmed level change for one platform.
#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    pub from: Level,
    pub to: Level,
    /// The platform's status at the poll that confirmed the change.
    pub status: PlatformStatus,
}

/// What the alerter knows about one platform.
struct Track {
    /// The level last alerted (or seeded at the first poll).
    confirmed: Level,
    /// A different level seen on the latest polls, and on how many in a row.
    pending: Option<(Level, u8)>,
}

/// Turns each poll into the alerts it confirms.
///
/// Per platform it keeps a confirmed level and a pending one with a count. A
/// poll at the confirmed level clears pending; a different level starts or
/// extends pending, and once it has been seen on [`CONFIRM_POLLS`] polls in a
/// row it alerts and becomes the confirmed level. A platform's first poll
/// only seeds its level.
#[derive(Default)]
pub struct Alerter {
    tracks: HashMap<String, Track>,
}

impl Alerter {
    /// An alerter that has seen no poll yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The alerts this poll confirms, in `statuses` order.
    pub fn observe(&mut self, statuses: &[PlatformStatus]) -> Vec<Alert> {
        let mut alerts = Vec::new();
        for status in statuses {
            let Some(track) = self.tracks.get_mut(&status.name) else {
                self.tracks.insert(
                    status.name.clone(),
                    Track {
                        confirmed: status.level,
                        pending: None,
                    },
                );
                continue;
            };
            if status.level == track.confirmed {
                track.pending = None;
                continue;
            }
            let count = match track.pending {
                Some((level, count)) if level == status.level => count + 1,
                _ => 1,
            };
            if count >= CONFIRM_POLLS {
                alerts.push(Alert {
                    from: track.confirmed,
                    to: status.level,
                    status: status.clone(),
                });
                track.confirmed = status.level;
                track.pending = None;
            } else {
                track.pending = Some((status.level, count));
            }
        }
        alerts
    }
}

/// Whether `alert` passes `[alerts] levels`: every alert without a list,
/// else one whose `from` or `to` is listed. Apply it after
/// [`Alerter::observe`], which moves the confirmed level either way, so a
/// later change is measured from the new level.
#[must_use]
pub fn wanted(alert: &Alert, levels: Option<&[Level]>) -> bool {
    levels.is_none_or(|levels| levels.contains(&alert.from) || levels.contains(&alert.to))
}

/// A level as `--json` spells it (`operational`, `outage`, …).
fn level_key(level: Level) -> Value {
    serde_json::to_value(level).unwrap_or(Value::Null)
}

/// The JSON body for `alert`: `text` (for Slack and the like), `platform`,
/// `from`, `to`, `at` (the confirming poll's `fetched_at`), and `incidents`
/// (the active ones, `title` and `url`, in feed order).
#[must_use]
pub fn payload(alert: &Alert) -> Value {
    let status = &alert.status;
    let incidents: Vec<Value> = status
        .incidents
        .iter()
        .map(|incident| json!({"title": incident.title, "url": incident.url}))
        .collect();
    json!({
        "text": text(alert),
        "platform": status.name,
        "from": level_key(alert.from),
        "to": level_key(alert.to),
        "at": status.fetched_at.to_rfc3339_opts(SecondsFormat::Secs, true),
        "incidents": incidents,
    })
}

/// `GitHub: Operational → Outage. <first incident> (+N more)`, then
/// `. <error>` when the new level is Unknown and there is one.
fn text(alert: &Alert) -> String {
    let status = &alert.status;
    let mut text = format!(
        "{}: {} → {}",
        status.name,
        alert.from.label(),
        alert.to.label()
    );
    if let Some((first, rest)) = status.incidents.split_first() {
        text.push_str(". ");
        text.push_str(&shorten(&first.title, UPDATE_TEXT_LIMIT));
        if !rest.is_empty() {
            let _ = write!(text, " (+{} more)", rest.len());
        }
    }
    if alert.to == Level::Unknown
        && let Some(error) = &status.error
    {
        text.push_str(". ");
        text.push_str(&shorten(error, UPDATE_TEXT_LIMIT));
    }
    text
}

/// The HTTP client for alerts: it does not follow redirects, since a
/// redirected POST may arrive as a GET and look delivered when it was not;
/// a redirect is reported like any other non-2xx answer.
///
/// # Errors
///
/// Fails when the client cannot be set up.
pub fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("aretheydown/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// POST `payload` to `webhook` within [`TIMEOUT`].
///
/// # Errors
///
/// A non-2xx answer (`HTTP 500 Internal Server Error`) or why the request
/// failed (`timed out after 10s`, `could not connect`, `the request failed`).
/// Never the URL.
pub async fn post(client: &reqwest::Client, webhook: &Webhook, payload: &Value) -> SendResult {
    let response = client
        .post(webhook.url().clone())
        .timeout(TIMEOUT)
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(payload.to_string())
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                format!("timed out after {}s", TIMEOUT.as_secs())
            } else if error.is_connect() {
                "could not connect".to_owned()
            } else {
                "the request failed".to_owned()
            }
        })?;
    let code = response.status();
    if code.is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {code}"))
    }
}

/// The stderr warning for an alert that could not be sent to the webhook at
/// `position` (from 1) in the config's list.
#[must_use]
pub fn warning(platform: &str, position: usize, error: &str) -> String {
    format!(
        "warning: could not send the alert for {platform} to webhook {position}: {error} (dropped)"
    )
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::post as route_post;
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::model::Incident;

    use Level::{Degraded, Operational, Outage, Unknown};

    fn status(name: &str, level: Level) -> PlatformStatus {
        PlatformStatus {
            name: name.to_owned(),
            level,
            description: String::new(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: Utc.with_ymd_and_hms(2026, 11, 15, 13, 40, 0).unwrap(),
            error: None,
            probe: None,
            regions: Vec::new(),
        }
    }

    /// Feed one platform these levels, one per poll; return each alert as
    /// (poll number from 1, from, to).
    fn run(levels: &[Level]) -> Vec<(usize, Level, Level)> {
        let mut alerter = Alerter::new();
        let mut fired = Vec::new();
        for (index, level) in levels.iter().enumerate() {
            for alert in alerter.observe(&[status("GitHub", *level)]) {
                fired.push((index + 1, alert.from, alert.to));
            }
        }
        fired
    }

    #[test]
    fn a_change_alerts_once_it_holds_for_two_polls() {
        assert_eq!(
            run(&[Operational, Outage, Outage]),
            [(3, Operational, Outage)]
        );
        // And only once while it holds.
        assert_eq!(
            run(&[Operational, Outage, Outage, Outage, Outage]),
            [(3, Operational, Outage)]
        );
    }

    #[test]
    fn a_failed_probe_never_alerts() {
        let mut alerter = Alerter::new();
        for ok in [true, false, false, false] {
            let mut github = status("GitHub", Operational);
            github.probe = Some(crate::model::Probe {
                url: "https://github.com/".to_owned(),
                ok,
                code: None,
                elapsed_ms: None,
                description: String::new(),
            });
            assert_eq!(alerter.observe(&[github]), []);
        }
    }

    #[test]
    fn a_one_poll_blip_sends_nothing() {
        assert_eq!(run(&[Operational, Outage, Operational]), []);
        assert_eq!(run(&[Operational, Unknown, Operational, Operational]), []);
    }

    #[test]
    fn a_new_pending_level_restarts_the_count() {
        assert_eq!(
            run(&[Operational, Degraded, Outage, Outage]),
            [(4, Operational, Outage)]
        );
    }

    #[test]
    fn changes_to_and_from_unknown_alert() {
        assert_eq!(
            run(&[Operational, Unknown, Unknown, Operational, Operational]),
            [(3, Operational, Unknown), (5, Unknown, Operational)]
        );
    }

    #[test]
    fn the_first_poll_only_seeds() {
        assert_eq!(run(&[Outage, Outage]), []);
        for level in [Operational, Degraded, Outage, Unknown] {
            let mut alerter = Alerter::new();
            let poll = [status("A", level), status("B", Outage)];
            assert_eq!(alerter.observe(&poll).len(), 0);
        }
    }

    #[test]
    fn a_platform_first_seen_later_seeds_silently() {
        let mut alerter = Alerter::new();
        alerter.observe(&[status("A", Operational)]);
        let poll = [status("A", Operational), status("B", Outage)];
        assert_eq!(alerter.observe(&poll).len(), 0);
        assert_eq!(alerter.observe(&poll).len(), 0);
    }

    #[test]
    fn two_platforms_changing_in_one_poll_alert_in_config_order() {
        let mut alerter = Alerter::new();
        let start = [status("Zeta", Operational), status("Alpha", Operational)];
        let changed = [status("Zeta", Outage), status("Alpha", Degraded)];
        assert_eq!(alerter.observe(&start).len(), 0);
        assert_eq!(alerter.observe(&changed).len(), 0);
        let alerts = alerter.observe(&changed);
        let names: Vec<(&str, Level)> = alerts
            .iter()
            .map(|a| (a.status.name.as_str(), a.to))
            .collect();
        assert_eq!(names, [("Zeta", Outage), ("Alpha", Degraded)]);
    }

    fn incident(title: &str, url: Option<&str>) -> Incident {
        Incident {
            title: title.to_owned(),
            url: url.map(str::to_owned),
            ..Default::default()
        }
    }

    fn alert(to: Level, incidents: Vec<Incident>) -> Alert {
        let mut status = status("GitHub", to);
        status.incidents = incidents;
        Alert {
            from: Operational,
            to,
            status,
        }
    }

    #[test]
    fn payload_matches_the_documented_shape() {
        let mut alert = alert(
            Outage,
            vec![
                incident("Incident with Git operations", Some("https://stspg.io/abc")),
                incident("Slow Actions", None),
            ],
        );
        alert.status.resolved = vec![incident("Old one", None)];
        assert_eq!(
            payload(&alert),
            json!({
                "text": "GitHub: Operational → Outage. Incident with Git operations (+1 more)",
                "platform": "GitHub",
                "from": "operational",
                "to": "outage",
                "at": "2026-11-15T13:40:00Z",
                "incidents": [
                    {"title": "Incident with Git operations", "url": "https://stspg.io/abc"},
                    {"title": "Slow Actions", "url": null}
                ]
            })
        );
    }

    #[test]
    fn text_with_no_one_and_three_incidents() {
        let text = |n: usize| {
            let incidents = (1..=n)
                .map(|i| incident(&format!("Issue {i}"), None))
                .collect();
            payload(&alert(Degraded, incidents))["text"].clone()
        };
        assert_eq!(text(0), "GitHub: Operational → Degraded");
        assert_eq!(text(1), "GitHub: Operational → Degraded. Issue 1");
        assert_eq!(text(3), "GitHub: Operational → Degraded. Issue 1 (+2 more)");
        let recovered = Alert {
            from: Outage,
            ..alert(Operational, Vec::new())
        };
        assert_eq!(payload(&recovered)["text"], "GitHub: Outage → Operational");
        assert_eq!(payload(&recovered)["incidents"], json!([]));
    }

    #[test]
    fn text_adds_the_error_when_the_new_level_is_unknown() {
        let mut unknown = alert(Unknown, Vec::new());
        unknown.status.error = Some("timed out after 10s".to_owned());
        assert_eq!(
            payload(&unknown)["text"],
            "GitHub: Operational → Unknown. timed out after 10s"
        );
        assert_eq!(payload(&unknown)["to"], "unknown");
        // Only for Unknown.
        let mut degraded = alert(Degraded, Vec::new());
        degraded.status.error = Some("ignored".to_owned());
        assert_eq!(payload(&degraded)["text"], "GitHub: Operational → Degraded");
    }

    #[test]
    fn long_titles_and_errors_are_cut_to_200_characters() {
        let title = "x".repeat(300);
        let text = payload(&alert(Outage, vec![incident(&title, None)]))["text"]
            .as_str()
            .unwrap()
            .to_owned();
        let expected = format!("GitHub: Operational → Outage. {}…", "x".repeat(200));
        assert_eq!(text, expected);
        // The incidents list keeps the title whole.
        assert_eq!(
            payload(&alert(Outage, vec![incident(&title, None)]))["incidents"][0]["title"],
            title
        );

        let mut unknown = alert(Unknown, Vec::new());
        unknown.status.error = Some("e".repeat(300));
        let text = payload(&unknown)["text"].as_str().unwrap().to_owned();
        assert!(text.ends_with(&format!(". {}…", "e".repeat(200))), "{text}");
    }

    #[test]
    fn the_warning_names_the_platform_and_the_error() {
        assert_eq!(
            warning("GitHub", 2, "HTTP 500 Internal Server Error"),
            "warning: could not send the alert for GitHub to webhook 2: HTTP 500 Internal Server Error (dropped)"
        );
    }

    /// Like [`run`], keeping only the alerts `levels` wants.
    fn run_filtered(levels: &[Level], wanted_levels: &[Level]) -> Vec<(usize, Level, Level)> {
        let mut alerter = Alerter::new();
        let mut fired = Vec::new();
        for (index, level) in levels.iter().enumerate() {
            for alert in alerter.observe(&[status("GitHub", *level)]) {
                if wanted(&alert, Some(wanted_levels)) {
                    fired.push((index + 1, alert.from, alert.to));
                }
            }
        }
        fired
    }

    #[test]
    fn without_levels_every_change_is_wanted() {
        for (from, to) in [(Operational, Degraded), (Unknown, Operational)] {
            let alert = Alert {
                from,
                ..alert(to, Vec::new())
            };
            assert!(wanted(&alert, None));
        }
    }

    #[test]
    fn levels_keep_changes_into_or_out_of_a_listed_level() {
        let alert_between = |from, to| Alert {
            from,
            ..alert(to, Vec::new())
        };
        let outage = [Outage];
        assert!(wanted(&alert_between(Degraded, Outage), Some(&outage)));
        assert!(wanted(&alert_between(Outage, Operational), Some(&outage)));
        assert!(wanted(&alert_between(Outage, Unknown), Some(&outage)));
        assert!(!wanted(
            &alert_between(Operational, Degraded),
            Some(&outage)
        ));
        assert!(!wanted(&alert_between(Operational, Unknown), Some(&outage)));
        let both = [Outage, Unknown];
        assert!(wanted(&alert_between(Operational, Unknown), Some(&both)));
    }

    #[test]
    fn a_dropped_change_still_moves_the_confirmed_level() {
        // Operational → Degraded is dropped, but the next change is measured
        // from Degraded, and the return to Operational is dropped too.
        assert_eq!(
            run_filtered(
                &[
                    Operational,
                    Degraded,
                    Degraded,
                    Outage,
                    Outage,
                    Degraded,
                    Degraded,
                    Operational,
                    Operational
                ],
                &[Outage],
            ),
            [(5, Degraded, Outage), (7, Outage, Degraded)]
        );
        // Without the move, Degraded → Operational would read Outage → Operational.
        assert_eq!(
            run_filtered(
                &[Operational, Degraded, Degraded, Operational, Operational],
                &[Outage]
            ),
            []
        );
    }

    type Received = Arc<Mutex<Vec<(Option<String>, Value)>>>;

    /// A local webhook answering `code`, recording each body it gets.
    async fn listener(code: StatusCode) -> (Webhook, Received) {
        let received: Received = Arc::default();
        let log = Arc::clone(&received);
        let app = Router::new().route(
            "/hook/s3cret",
            route_post(move |headers: HeaderMap, body: String| {
                let log = Arc::clone(&log);
                async move {
                    let content_type = headers
                        .get("content-type")
                        .map(|v| v.to_str().unwrap().to_owned());
                    log.lock()
                        .unwrap()
                        .push((content_type, serde_json::from_str(&body).unwrap()));
                    code
                }
            }),
        );
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(socket, app).await });
        (hook(&format!("http://{addr}/hook/s3cret")), received)
    }

    fn hook(url: &str) -> Webhook {
        let config = crate::config::parse(&format!("[alerts]\nwebhook = \"{url}\"\n")).unwrap();
        let alerts = crate::config::alerts(&config).unwrap().unwrap();
        alerts.webhooks[0].clone()
    }

    #[tokio::test]
    async fn post_sends_the_payload_as_json() {
        let (webhook, received) = listener(StatusCode::OK).await;
        let body = payload(&alert(Outage, Vec::new()));
        post(&reqwest::Client::new(), &webhook, &body)
            .await
            .unwrap();
        let received = received.lock().unwrap().clone();
        assert_eq!(received, [(Some("application/json".to_owned()), body)]);
    }

    #[tokio::test]
    async fn a_non_2xx_answer_is_an_error_without_the_url() {
        let (webhook, received) = listener(StatusCode::INTERNAL_SERVER_ERROR).await;
        let body = payload(&alert(Outage, Vec::new()));
        let error = post(&reqwest::Client::new(), &webhook, &body)
            .await
            .unwrap_err();
        assert_eq!(error, "HTTP 500 Internal Server Error");
        assert_eq!(received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_redirect_is_not_followed() {
        let (webhook, received) = listener(StatusCode::TEMPORARY_REDIRECT).await;
        let error = post(&client().unwrap(), &webhook, &json!({}))
            .await
            .unwrap_err();
        assert_eq!(error, "HTTP 307 Temporary Redirect");
        assert_eq!(received.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_refused_connection_is_an_error_without_the_url() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = socket.local_addr().unwrap();
        drop(socket);
        let webhook = hook(&format!("http://{addr}/hook/s3cret"));
        let error = post(&reqwest::Client::new(), &webhook, &json!({}))
            .await
            .unwrap_err();
        assert_eq!(error, "could not connect");
    }
}
