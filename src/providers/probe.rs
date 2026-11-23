//! Vendor probes: one direct GET of a vendor's own service (its "front
//! door", not its status feed) per fetch, shown beside the level the vendor
//! reports and never part of it.
//!
//! Most front doors answer a request without credentials with a 3xx or 4xx,
//! so what counts as "responding" is a status per probe ([`Expect`]): an
//! exact code (`401`) or a class (`4xx`). [`result`] judges an
//! [`Outcome`] and is pure; [`run`] makes the request, without following
//! redirects, within [`TIMEOUT`].

use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use super::check::{self, Outcome};
use crate::model::Probe;

/// How long a probe may take: a check's [`check::TIMEOUT`], under the
/// platform's [`crate::fetch::DEFAULT_TIMEOUT`], so a hanging front door
/// never holds up the platform's feed result.
pub const TIMEOUT: Duration = check::TIMEOUT;

/// The status a probe expects when its service is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Exactly this code, e.g. `401`.
    Code(u16),
    /// Any code in this hundred, e.g. `4` for `4xx`.
    Class(u16),
}

impl Expect {
    /// Whether `code` is what this expects.
    #[must_use]
    pub fn matches(self, code: u16) -> bool {
        match self {
            Self::Code(want) => code == want,
            Self::Class(hundred) => code / 100 == hundred,
        }
    }

    /// `401`, or `1xx` to `5xx` in any case; `None` for anything else.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim().to_ascii_lowercase();
        if let Some(digit) = text.strip_suffix("xx") {
            return match digit {
                "1" | "2" | "3" | "4" | "5" => digit.parse().ok().map(Self::Class),
                _ => None,
            };
        }
        if text.len() != 3 || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        text.parse().ok().and_then(Self::code)
    }

    /// An exact code, when it is an HTTP status code (100 to 599).
    #[must_use]
    pub fn code(code: u16) -> Option<Self> {
        (100..=599).contains(&code).then_some(Self::Code(code))
    }
}

impl fmt::Display for Expect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Code(code) => write!(f, "{code}"),
            Self::Class(hundred) => write!(f, "{hundred}xx"),
        }
    }
}

/// What to probe and what answer means up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub url: String,
    pub expect: Expect,
}

impl Target {
    #[must_use]
    pub fn new(url: &str, expect: Expect) -> Self {
        Self {
            url: url.to_owned(),
            expect,
        }
    }
}

/// The built-in platforms' default probes, by platform name. Each answered
/// as listed when checked live on 2026-11-16.
pub const DEFAULTS: [(&str, &str, Expect); 8] = [
    ("GitHub", "https://github.com/", Expect::Class(2)),
    (
        "Cloudflare",
        "https://www.cloudflare.com/cdn-cgi/trace",
        Expect::Class(2),
    ),
    ("AWS", "https://s3.amazonaws.com/", Expect::Class(3)),
    ("Azure", "https://management.azure.com/", Expect::Class(4)),
    (
        "Google Cloud",
        "https://storage.googleapis.com/",
        Expect::Class(4),
    ),
    (
        "OpenAI",
        "https://api.openai.com/v1/models",
        Expect::Code(401),
    ),
    (
        "Anthropic",
        "https://api.anthropic.com/v1/models",
        Expect::Code(401),
    ),
    (
        "Microsoft 365",
        "https://outlook.office.com/",
        Expect::Class(3),
    ),
];

/// The default probe of built-in platform `name`, if it has one.
#[must_use]
pub fn default_for(name: &str) -> Option<Target> {
    DEFAULTS
        .iter()
        .find(|(platform, ..)| *platform == name)
        .map(|(_, url, expect)| Target::new(url, *expect))
}

/// The probe result for `url` after `outcome`, judged against `expect`:
///
/// - the expected status: ok, `responding (HTTP 401 in 220 ms)`
/// - any other status: `unexpected HTTP 503 (expected 4xx)`
/// - no answer: `no answer (timed out after 9s)`, `no answer (could not
///   connect)`, or `no answer (the request failed)`
#[must_use]
pub fn result(url: &str, outcome: Outcome, expect: Expect) -> Probe {
    let (ok, code, elapsed_ms, description) = match outcome {
        Outcome::Answered { code, elapsed } => {
            let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
            if expect.matches(code) {
                (
                    true,
                    Some(code),
                    Some(ms),
                    format!("responding (HTTP {code} in {ms} ms)"),
                )
            } else {
                (
                    false,
                    Some(code),
                    Some(ms),
                    format!("unexpected HTTP {code} (expected {expect})"),
                )
            }
        }
        Outcome::TimedOut => (
            false,
            None,
            None,
            format!("no answer (timed out after {}s)", TIMEOUT.as_secs()),
        ),
        Outcome::CouldNotConnect => (
            false,
            None,
            None,
            "no answer (could not connect)".to_owned(),
        ),
        Outcome::Failed => (
            false,
            None,
            None,
            "no answer (the request failed)".to_owned(),
        ),
    };
    Probe {
        url: url.to_owned(),
        ok,
        code,
        elapsed_ms,
        description,
    }
}

/// The client live probes use, built once: [`crate::alerts::client`], which
/// follows no redirects, so a front door that answers `302` is judged on
/// that `302`. `None` when it cannot be set up.
#[must_use]
pub fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT.get_or_init(|| crate::alerts::client().ok()).as_ref()
}

/// Probe `target` with `client` within [`TIMEOUT`]; without a client, the
/// request failed. `client` should follow no redirects ([`client`]).
pub async fn run(client: Option<&reqwest::Client>, target: &Target) -> Probe {
    let outcome = match client {
        Some(client) => check::get(client, &target.url, TIMEOUT).await,
        None => Outcome::Failed,
    };
    result(&target.url, outcome, target.expect)
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::routing::get;

    use super::*;

    const URL: &str = "https://api.example.com/v1/models";

    fn answered(code: u16) -> Outcome {
        Outcome::Answered {
            code,
            elapsed: Duration::from_millis(220),
        }
    }

    #[test]
    fn an_exact_code_is_responding_and_keeps_code_and_time() {
        assert_eq!(
            result(URL, answered(401), Expect::Code(401)),
            Probe {
                url: URL.to_owned(),
                ok: true,
                code: Some(401),
                elapsed_ms: Some(220),
                description: "responding (HTTP 401 in 220 ms)".to_owned(),
            }
        );
    }

    #[test]
    fn a_class_matches_any_code_in_its_hundred() {
        for code in [400, 403, 404, 499] {
            assert!(result(URL, answered(code), Expect::Class(4)).ok, "{code}");
        }
        for code in [399, 500] {
            assert!(!result(URL, answered(code), Expect::Class(4)).ok, "{code}");
        }
    }

    #[test]
    fn a_mismatch_names_what_was_expected() {
        let probe = result(URL, answered(503), Expect::Class(4));
        assert!(!probe.ok);
        assert_eq!(probe.code, Some(503));
        assert_eq!(probe.description, "unexpected HTTP 503 (expected 4xx)");
        assert_eq!(
            result(URL, answered(200), Expect::Code(401)).description,
            "unexpected HTTP 200 (expected 401)"
        );
    }

    #[test]
    fn no_answer_has_no_code_or_time() {
        for (outcome, description) in [
            (Outcome::TimedOut, "no answer (timed out after 9s)"),
            (Outcome::CouldNotConnect, "no answer (could not connect)"),
            (Outcome::Failed, "no answer (the request failed)"),
        ] {
            let probe = result(URL, outcome, Expect::Class(2));
            assert_eq!(
                (probe.ok, probe.code, probe.elapsed_ms),
                (false, None, None)
            );
            assert_eq!(probe.description, description);
        }
    }

    #[test]
    fn expect_parses_codes_and_classes_in_any_case() {
        assert_eq!(Expect::parse("401"), Some(Expect::Code(401)));
        assert_eq!(Expect::parse("4xx"), Some(Expect::Class(4)));
        assert_eq!(Expect::parse(" 2XX "), Some(Expect::Class(2)));
        for bad in [
            "", "6xx", "0xx", "xx", "4x", "99", "600", "4000", "abc", "1xxx",
        ] {
            assert_eq!(Expect::parse(bad), None, "{bad:?}");
        }
        assert_eq!(Expect::code(99), None);
        assert_eq!(Expect::code(599), Some(Expect::Code(599)));
        assert_eq!(Expect::Class(3).to_string(), "3xx");
        assert_eq!(Expect::Code(401).to_string(), "401");
    }

    #[test]
    fn a_probe_gives_up_before_the_platform_times_out() {
        assert!(TIMEOUT < crate::fetch::DEFAULT_TIMEOUT);
    }

    /// `/moved` redirects to `/up`, which answers 200.
    async fn server() -> String {
        let app = Router::new().route("/up", get(|| async { "ok" })).route(
            "/moved",
            get(|| async { axum::response::Redirect::temporary("/up") }),
        );
        let socket = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(socket, app).await });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn run_does_not_follow_redirects() {
        let base = server().await;
        let client = crate::alerts::client().unwrap();
        let moved = run(
            Some(&client),
            &Target::new(&format!("{base}/moved"), Expect::Class(3)),
        )
        .await;
        assert!(moved.ok, "{}", moved.description);
        assert_eq!(moved.code, Some(307));
        let up = run(
            Some(&client),
            &Target::new(&format!("{base}/up"), Expect::Code(401)),
        )
        .await;
        assert_eq!(up.description, "unexpected HTTP 200 (expected 401)");
        let closed = run(
            Some(&client),
            &Target::new("http://127.0.0.1:1/", Expect::Class(2)),
        )
        .await;
        assert_eq!(closed.description, "no answer (could not connect)");
    }
}
