//! Your own HTTP endpoints, from `[[check]]` in the config: a plain GET that
//! is up when it answers with the expected status.
//!
//! [`status`] turns what happened ([`Outcome`]) into a [`PlatformStatus`] and
//! is pure; [`run`] makes the request. A check is never `Unknown` and never
//! sets `error`: a failure to answer is the check's own Outage.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};

use crate::model::{Level, PlatformStatus};

/// How long a check may take. It stays under
/// [`crate::fetch::DEFAULT_TIMEOUT`], so a slow endpoint is the check's own
/// Outage (`timed out after 9s`) instead of a platform timing out as Unknown.
pub const TIMEOUT: Duration = Duration::from_secs(9);

/// What a check's GET came back with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// An answer (after any redirects) with this status code, `elapsed` after
    /// the request started.
    Answered { code: u16, elapsed: Duration },
    /// No answer within [`TIMEOUT`].
    TimedOut,
    /// The connection could not be made (refused, DNS, TLS).
    CouldNotConnect,
    /// Any other failure, e.g. too many redirects.
    Failed,
}

/// The status of check `name` after `outcome`. `expect` is the status code
/// that means up; without it, any 2xx does.
///
/// Up → Operational, `Responding: HTTP 200 in 142 ms`. Any other answer →
/// Outage, `HTTP 503 (expected 2xx)`. No answer → Outage, `could not
/// connect`, `timed out after 9s`, or `the request failed`.
#[must_use]
pub fn status(
    name: &str,
    outcome: Outcome,
    expect: Option<u16>,
    fetched_at: DateTime<Utc>,
) -> PlatformStatus {
    let (level, description) = match outcome {
        Outcome::Answered { code, elapsed } => {
            let up = expect.map_or((200..300).contains(&code), |want| code == want);
            if up {
                (
                    Level::Operational,
                    format!("{RESPONDING}HTTP {code} in {} ms", elapsed.as_millis()),
                )
            } else {
                let wanted = expect.map_or_else(|| "2xx".to_owned(), |want| want.to_string());
                (Level::Outage, format!("HTTP {code} (expected {wanted})"))
            }
        }
        Outcome::TimedOut => (
            Level::Outage,
            format!("timed out after {}s", TIMEOUT.as_secs()),
        ),
        Outcome::CouldNotConnect => (Level::Outage, "could not connect".to_owned()),
        Outcome::Failed => (Level::Outage, "the request failed".to_owned()),
    };
    PlatformStatus {
        name: name.to_owned(),
        level,
        description,
        incidents: Vec::new(),
        resolved: Vec::new(),
        fetched_at,
        error: None,
        probe: None,
    }
}

/// `description` without a check's response time: `Responding: HTTP 200 in
/// 142 ms` becomes `Responding: HTTP 200`; anything else is returned as is.
/// History compares descriptions this way, so a check whose time changes
/// every poll is not re-recorded every poll.
#[must_use]
pub fn without_latency(description: &str) -> &str {
    let Some(rest) = description.strip_prefix(RESPONDING) else {
        return description;
    };
    let Some((head, tail)) = rest.rsplit_once(" in ") else {
        return description;
    };
    let is_time = tail
        .strip_suffix(" ms")
        .is_some_and(|ms| !ms.is_empty() && ms.bytes().all(|b| b.is_ascii_digit()));
    if is_time {
        &description[..RESPONDING.len() + head.len()]
    } else {
        description
    }
}

/// How an up check's description starts.
const RESPONDING: &str = "Responding: ";

/// GET `url` with `client` (which follows redirects) within [`TIMEOUT`] and
/// return the check's status.
pub async fn run(
    client: &reqwest::Client,
    name: &str,
    url: &str,
    expect: Option<u16>,
) -> PlatformStatus {
    status(name, get(client, url, TIMEOUT).await, expect, Utc::now())
}

/// GET `url` with `client` within `timeout` and say what came back. Only
/// the status line is read, not the body. Whether redirects are followed is
/// up to `client`. Checks and vendor probes ([`super::probe`]) share this.
pub async fn get(client: &reqwest::Client, url: &str, timeout: Duration) -> Outcome {
    let started = Instant::now();
    match client.get(url).timeout(timeout).send().await {
        Ok(response) => Outcome::Answered {
            code: response.status().as_u16(),
            elapsed: started.elapsed(),
        },
        Err(error) if error.is_timeout() => Outcome::TimedOut,
        Err(error) if error.is_connect() => Outcome::CouldNotConnect,
        Err(_) => Outcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::http::StatusCode;
    use axum::routing::get;
    use chrono::TimeZone;

    use super::*;

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 16, 9, 0, 0).unwrap()
    }

    fn answered(code: u16) -> Outcome {
        Outcome::Answered {
            code,
            elapsed: Duration::from_millis(142),
        }
    }

    fn level_and_description(outcome: Outcome, expect: Option<u16>) -> (Level, String) {
        let status = status("API", outcome, expect, at());
        assert_eq!(status.name, "API");
        assert_eq!(status.fetched_at, at());
        assert_eq!(status.error, None, "a check never sets error");
        assert_eq!(status.incidents, []);
        (status.level, status.description)
    }

    #[test]
    fn any_2xx_is_up_by_default() {
        for code in [200, 204, 299] {
            assert_eq!(
                level_and_description(answered(code), None),
                (
                    Level::Operational,
                    format!("Responding: HTTP {code} in 142 ms")
                )
            );
        }
    }

    #[test]
    fn other_codes_are_an_outage_naming_what_was_expected() {
        for (code, expect, wanted) in [
            (503, None, "2xx"),
            (301, None, "2xx"),
            (199, None, "2xx"),
            (200, Some(204), "204"),
        ] {
            assert_eq!(
                level_and_description(answered(code), expect),
                (Level::Outage, format!("HTTP {code} (expected {wanted})"))
            );
        }
    }

    #[test]
    fn expect_picks_the_one_code_that_means_up() {
        assert_eq!(
            level_and_description(answered(401), Some(401)).0,
            Level::Operational
        );
    }

    #[test]
    fn no_answer_is_an_outage_never_unknown() {
        assert_eq!(
            level_and_description(Outcome::TimedOut, None),
            (Level::Outage, "timed out after 9s".to_owned())
        );
        assert_eq!(
            level_and_description(Outcome::CouldNotConnect, Some(200)),
            (Level::Outage, "could not connect".to_owned())
        );
        assert_eq!(
            level_and_description(Outcome::Failed, None),
            (Level::Outage, "the request failed".to_owned())
        );
    }

    #[test]
    fn without_latency_drops_only_a_checks_response_time() {
        let up = |ms: u64| {
            status(
                "API",
                Outcome::Answered {
                    code: 200,
                    elapsed: Duration::from_millis(ms),
                },
                None,
                at(),
            )
            .description
        };
        assert_eq!(without_latency(&up(142)), "Responding: HTTP 200");
        assert_eq!(without_latency(&up(9)), without_latency(&up(3100)));
        for other in [
            "HTTP 503 (expected 2xx)",
            "could not connect",
            "All Systems Operational",
            "Responding: HTTP 200 in a while",
            "Responding: HTTP 200 in  ms",
            "Seen in 142 ms",
        ] {
            assert_eq!(without_latency(other), other);
        }
    }

    #[test]
    fn a_check_times_out_before_the_platform_does() {
        assert!(TIMEOUT < crate::fetch::DEFAULT_TIMEOUT);
    }

    /// A local server: `/up` answers 200, `/down` 503, `/moved` redirects to `/up`.
    async fn server() -> String {
        let app = Router::new()
            .route("/up", get(|| async { "ok" }))
            .route("/down", get(|| async { StatusCode::SERVICE_UNAVAILABLE }))
            .route(
                "/moved",
                get(|| async { axum::response::Redirect::temporary("/up") }),
            );
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(socket, app).await });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn run_against_a_local_listener() {
        let base = server().await;
        let client = reqwest::Client::new();
        let up = run(&client, "API", &format!("{base}/up"), None).await;
        assert_eq!(up.level, Level::Operational);
        assert!(
            up.description.starts_with("Responding: HTTP 200 in "),
            "{}",
            up.description
        );
        let moved = run(&client, "API", &format!("{base}/moved"), None).await;
        assert_eq!(moved.level, Level::Operational, "redirects are followed");
        let down = run(&client, "API", &format!("{base}/down"), None).await;
        assert_eq!(
            (down.level, down.description.as_str()),
            (Level::Outage, "HTTP 503 (expected 2xx)")
        );
        assert_eq!(
            run(&client, "API", "http://127.0.0.1:1/", None)
                .await
                .description,
            "could not connect"
        );
    }
}
