//! The `--serve` web page: the same picture as `--once` text, as HTML.
//!
//! Pure, like `render`: it only reads [`PlatformStatus`] values and measures
//! ages against each platform's `fetched_at`. Every piece of feed text is
//! escaped, and only `http(s)` URLs become links.

use std::collections::HashMap;
use std::fmt::Write as _;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::fetch::REFRESH_INTERVAL;
use crate::model::{
    Incident, IncidentKind, PlatformStatus, RESOLVED_HOURS, age, verdict, worst_first,
};
use crate::render;

/// Platform name → the status page a person reads (`Source::status_page`).
pub type StatusPages = HashMap<String, String>;

/// Shared by both pages: light and dark colors follow the OS.
const STYLE: &str = r"
:root { color-scheme: light dark; --bg: #fafafa; --fg: #1d1d1f; --muted: #6e6e73;
  --card: #ffffff; --line: #e0e0e5; --ok: #1a7f37; --warn: #9a6700; --bad: #cf222e; --unk: #6e40c9; }
@media (prefers-color-scheme: dark) {
  :root { --bg: #111113; --fg: #ececf0; --muted: #a1a1aa; --card: #1c1c1f; --line: #2e2e33;
    --ok: #3fb950; --warn: #d29922; --bad: #f85149; --unk: #a371f7; }
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--bg); color: var(--fg);
  font: 15px/1.45 -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; }
main { max-width: 860px; margin: 0 auto; padding: 24px 16px 48px; }
h1 { font-size: 1.5rem; margin: 0 0 4px; }
h2 { font-size: 1.05rem; margin: 0; }
h3 { font-size: 0.85rem; margin: 12px 0 4px; color: var(--muted); font-weight: 600; }
a { color: inherit; }
.muted, .meta { color: var(--muted); font-size: 0.85rem; }
.platform { background: var(--card); border: 1px solid var(--line); border-left-width: 4px;
  border-radius: 8px; padding: 12px 14px; margin: 12px 0; }
.platform.operational { border-left-color: var(--ok); }
.platform.degraded { border-left-color: var(--warn); }
.platform.outage { border-left-color: var(--bad); }
.platform.unknown { border-left-color: var(--unk); }
.head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 10px; }
.level { font-weight: 600; font-size: 0.85rem; }
.level.operational { color: var(--ok); } .level.degraded { color: var(--warn); }
.level.outage { color: var(--bad); } .level.unknown { color: var(--unk); }
.head .link { margin-left: auto; font-size: 0.85rem; }
.description { margin: 4px 0 0; }
.probe { margin: 4px 0 0; font-size: 0.85rem; color: var(--muted); overflow-wrap: anywhere; }
.probe.failed { color: var(--warn); font-weight: 600; }
.error { margin: 6px 0 0; color: var(--bad); overflow-wrap: anywhere; }
ul { list-style: none; margin: 8px 0 0; padding: 0; }
.incident { border-top: 1px solid var(--line); padding: 8px 0 0; margin-top: 8px; }
.incident .title { font-weight: 600; overflow-wrap: anywhere; }
.tag { font-size: 0.75rem; border: 1px solid var(--line); border-radius: 4px; padding: 0 4px; margin-right: 4px; }
.update { margin: 6px 0 0; white-space: pre-wrap; overflow-wrap: anywhere; }
.resolved li { margin: 4px 0; overflow-wrap: anywhere; }
";

/// The dashboard for `statuses`: the verdict, then each platform worst first
/// with its probe line (`render::probe_line`), error, active incidents (full
/// latest update), resolved incidents, and status page link. Ages are measured against each platform's
/// `fetched_at`. A short inline script re-fetches the page every
/// [`REFRESH_INTERVAL`] and swaps in the new `<main>`; without JavaScript the
/// page just stays as it is.
#[must_use]
pub fn page(statuses: &[PlatformStatus], status_pages: &StatusPages) -> String {
    let heading = verdict(statuses).unwrap_or_else(|| "No platforms configured".to_owned());
    let heading = escape(&heading);
    let mut main = format!("<main>\n<h1>{heading}</h1>\n");
    if let Some(fetched_at) = statuses.iter().map(|s| s.fetched_at).max() {
        let _ = writeln!(
            main,
            r#"<p class="muted">Fetched <time datetime="{}">{}</time>, refreshes every {}s. <a href="/api/status">JSON</a></p>"#,
            fetched_at.to_rfc3339_opts(SecondsFormat::Secs, true),
            fetched_at.format("%Y-%m-%d %H:%M:%S UTC"),
            REFRESH_INTERVAL.as_secs(),
        );
    }
    for status in worst_first(statuses) {
        write_platform(&mut main, status, status_pages.get(&status.name));
    }
    main.push_str("</main>\n");
    let script = format!(
        r#"<script>
setInterval(async () => {{
  try {{
    const response = await fetch(location.pathname, {{ cache: "no-store" }});
    if (!response.ok) return;
    const next = new DOMParser().parseFromString(await response.text(), "text/html");
    const main = next.querySelector("main");
    if (main) {{ document.querySelector("main").replaceWith(main); document.title = next.title; }}
  }} catch (_) {{}}
}}, {});
</script>
"#,
        REFRESH_INTERVAL.as_millis()
    );
    document(&heading, "", &format!("{main}{script}"))
}

/// The page served before the first poll has finished; it reloads itself
/// every few seconds (no JavaScript needed).
#[must_use]
pub fn loading_page() -> String {
    document(
        "First fetch still running",
        "<meta http-equiv=\"refresh\" content=\"5\">\n",
        "<main>\n<h1>First fetch still running</h1>\n<p class=\"muted\">This page reloads in a few seconds.</p>\n</main>\n",
    )
}

/// A whole HTML document; `title` must already be escaped.
fn document(title: &str, extra_head: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         {extra_head}<title>{title}</title>\n<style>{STYLE}</style>\n</head>\n<body>\n{body}</body>\n</html>\n"
    )
}

fn write_platform(out: &mut String, status: &PlatformStatus, status_page: Option<&String>) {
    let class = status.level.label().to_ascii_lowercase();
    let _ = writeln!(out, r#"<section class="platform {class}">"#);
    let _ = write!(
        out,
        r#"<div class="head"><h2>{}</h2><span class="level {class}">{}</span>"#,
        escape(&status.name),
        status.level.label()
    );
    if let Some(link) = status_page.and_then(|url| link(url, "Status page")) {
        let _ = write!(out, r#"<span class="link">{link}</span>"#);
    }
    out.push_str("</div>\n");
    if !status.description.is_empty() {
        let _ = writeln!(
            out,
            r#"<p class="description">{}</p>"#,
            escape(&status.description)
        );
    }
    if let (Some(probe), Some(line)) = (&status.probe, render::probe_line(status)) {
        let class = if probe.ok { "ok" } else { "failed" };
        let _ = writeln!(
            out,
            r#"<p class="probe {class}">Probe: {}</p>"#,
            escape(&line)
        );
    }
    if let Some(error) = &status.error {
        let _ = writeln!(out, r#"<p class="error">Error: {}</p>"#, escape(error));
    }
    if !status.incidents.is_empty() {
        out.push_str("<ul>\n");
        for incident in &status.incidents {
            write_incident(out, incident, status.fetched_at);
        }
        out.push_str("</ul>\n");
    }
    if !status.resolved.is_empty() {
        let _ = writeln!(
            out,
            "<h3>Resolved in the last {RESOLVED_HOURS}h ({})</h3>\n<ul class=\"resolved\">",
            status.resolved.len()
        );
        for incident in &status.resolved {
            let title = titled_link(incident);
            let ago = incident
                .ended_at
                .map(|ended_at| {
                    format!(
                        r#" <span class="meta">resolved {}</span>"#,
                        age(ended_at, status.fetched_at)
                    )
                })
                .unwrap_or_default();
            let _ = writeln!(out, "<li>{title}{ago}</li>");
        }
        out.push_str("</ul>\n");
    }
    out.push_str("</section>\n");
}

fn write_incident(out: &mut String, incident: &Incident, fetched_at: DateTime<Utc>) {
    out.push_str(r#"<li class="incident"><div class="title">"#);
    if incident.kind == IncidentKind::Maintenance {
        out.push_str(r#"<span class="tag">Maintenance</span>"#);
    }
    out.push_str(&titled_link(incident));
    out.push_str("</div>\n");
    let mut meta: Vec<String> = incident.stage.iter().map(|stage| escape(stage)).collect();
    meta.extend(
        render::incident_details(incident, fetched_at)
            .iter()
            .map(|detail| escape(detail)),
    );
    if !meta.is_empty() {
        let _ = writeln!(out, r#"<div class="meta">{}</div>"#, meta.join(" · "));
    }
    if let Some(update) = &incident.latest_update {
        let when = update.posted_at.map_or_else(
            || "Latest update".to_owned(),
            |posted_at| format!("Latest update, {}", age(posted_at, fetched_at)),
        );
        let _ = writeln!(
            out,
            r#"<div class="meta">{when}:</div><p class="update">{}</p>"#,
            escape(&update.text)
        );
    }
    out.push_str("</li>\n");
}

/// The incident's escaped title, as a link when its URL is `http(s)`.
fn titled_link(incident: &Incident) -> String {
    incident
        .url
        .as_deref()
        .and_then(|url| link(url, &incident.title))
        .unwrap_or_else(|| escape(&incident.title))
}

/// `<a href="url">text</a>`, both escaped; `None` unless `url` is `http(s)`,
/// so a feed cannot plant a `javascript:` link.
fn link(url: &str, text: &str) -> Option<String> {
    let lower = url.trim_start().to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return None;
    }
    Some(format!(r#"<a href="{}">{}</a>"#, escape(url), escape(text)))
}

/// `text` safe inside HTML text and double- or single-quoted attributes.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};

    use super::*;
    use crate::model::{Incident, IncidentKind, Level, Probe, Update};

    fn at() -> DateTime<Utc> {
        "2026-11-15T12:00:00Z".parse().unwrap()
    }

    fn status(name: &str, level: Level) -> PlatformStatus {
        PlatformStatus {
            name: name.to_owned(),
            level,
            description: format!("{name} description"),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: at(),
            error: None,
            probe: None,
        }
    }

    fn position(html: &str, needle: &str) -> usize {
        html.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} not in page:\n{html}"))
    }

    #[test]
    fn is_a_complete_utf8_document_that_refreshes_itself() {
        let html = page(&[status("GitHub", Level::Operational)], &StatusPages::new());
        assert!(html.starts_with("<!doctype html>"), "{html}");
        assert!(html.contains(r#"<meta charset="utf-8">"#));
        assert!(html.contains(r#"name="viewport""#));
        assert!(html.contains("prefers-color-scheme: dark"));
        // The refresh script swaps <main> every 60s; no external resources.
        assert!(html.contains("<main>"));
        assert!(html.contains("setInterval"));
        assert!(html.contains("60000"));
        assert!(!html.contains("src=\"http"), "no external scripts");
        assert!(!html.contains("<link "), "no external stylesheets");
    }

    #[test]
    fn leads_with_the_verdict_and_lists_platforms_worst_first() {
        let statuses = [
            status("Alpha", Level::Operational),
            status("Bravo", Level::Degraded),
            status("Charlie", Level::Outage),
            status("Delta", Level::Degraded),
        ];
        let html = page(&statuses, &StatusPages::new());
        let verdict = "3 of 4 need attention: Charlie, Bravo, Delta";
        assert!(
            html.contains(&format!("<title>{verdict}</title>")),
            "{html}"
        );
        let heading = position(&html, &format!("<h1>{verdict}</h1>"));
        let order: Vec<usize> = ["Charlie", "Bravo", "Delta", "Alpha"]
            .iter()
            .map(|name| position(&html, &format!("<h2>{name}</h2>")))
            .collect();
        assert!(heading < order[0]);
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{order:?}");
    }

    #[test]
    fn levels_are_spelled_out_not_only_colored() {
        let statuses = [
            status("A", Level::Outage),
            status("B", Level::Unknown),
            status("C", Level::Degraded),
            status("D", Level::Operational),
        ];
        let html = page(&statuses, &StatusPages::new());
        for (class, word) in [
            ("outage", "Outage"),
            ("unknown", "Unknown"),
            ("degraded", "Degraded"),
            ("operational", "Operational"),
        ] {
            assert!(
                html.contains(&format!(r#"<span class="level {class}">{word}</span>"#)),
                "{word}: {html}"
            );
        }
        assert!(html.contains("A description"));
    }

    #[test]
    fn shows_each_incident_in_full() {
        let mut github = status("GitHub", Level::Degraded);
        github.incidents.push(Incident {
            kind: IncidentKind::Maintenance,
            title: "Actions delays".to_owned(),
            components: vec!["Actions".to_owned(), "Pages".to_owned()],
            started_at: Some(at() - TimeDelta::minutes(134)),
            url: Some("https://stspg.io/abc".to_owned()),
            stage: Some("monitoring".to_owned()),
            latest_update: Update::new(
                Some("First line.\nSecond line, which is long enough that --once would cut it. Second line, which is long enough that --once would cut it. Second line, which is long enough that --once would cut it."),
                Some(at() - TimeDelta::minutes(5)),
            ),
            ..Incident::default()
        });
        let html = page(&[github], &StatusPages::new());
        assert!(html.contains("Actions delays"), "{html}");
        assert!(html.contains("Maintenance"));
        assert!(html.contains("Actions, Pages"));
        assert!(html.contains("2026-11-15 09:46 UTC"));
        assert!(html.contains("2h 14m ago"));
        assert!(html.contains("monitoring"));
        assert!(html.contains("5m ago"));
        // Whole and with its line break; the CSS keeps it (pre-wrap).
        assert!(html.contains("First line.\nSecond line,"));
        assert!(html.contains("would cut it.</p>"));
        assert!(html.contains("white-space: pre-wrap"));
        assert!(html.contains(r#"<a href="https://stspg.io/abc">"#));
    }

    #[test]
    fn shows_errors_resolved_incidents_and_the_status_page() {
        let mut aws = status("AWS", Level::Unknown);
        aws.error = Some("timed out after 10s".to_owned());
        aws.resolved.push(Incident {
            title: "S3 errors".to_owned(),
            ended_at: Some(at() - TimeDelta::minutes(65)),
            url: Some("https://example.com/s3".to_owned()),
            ..Incident::default()
        });
        let pages = StatusPages::from([(
            "AWS".to_owned(),
            "https://health.aws.amazon.com/health/status".to_owned(),
        )]);
        let html = page(&[aws], &pages);
        assert!(html.contains("timed out after 10s"), "{html}");
        assert!(html.contains("Resolved in the last 24h"));
        assert!(html.contains("S3 errors"));
        assert!(html.contains("1h 5m ago"));
        assert!(html.contains(r#"<a href="https://example.com/s3">"#));
        assert!(html.contains(r#"<a href="https://health.aws.amazon.com/health/status">"#));
    }

    #[test]
    fn shows_when_the_data_was_fetched() {
        let mut later = status("B", Level::Operational);
        later.fetched_at = at() + TimeDelta::seconds(7);
        let html = page(
            &[status("A", Level::Operational), later],
            &StatusPages::new(),
        );
        assert!(
            html.contains(
                r#"<time datetime="2026-11-15T12:00:07Z">2026-11-15 12:00:07 UTC</time>"#
            ),
            "{html}"
        );
    }

    #[test]
    fn escapes_feed_text_and_links_only_http_urls() {
        let evil = r#"<script>alert(1)</script> "quoted" & <img src=x onerror=alert(2)>"#;
        let mut s = status(evil, Level::Outage);
        s.description = evil.to_owned();
        s.error = Some(evil.to_owned());
        s.incidents.push(Incident {
            title: evil.to_owned(),
            components: vec![evil.to_owned()],
            stage: Some(evil.to_owned()),
            latest_update: Update::new(Some(evil), None),
            url: Some("javascript:alert(3)".to_owned()),
            ..Incident::default()
        });
        s.resolved.push(Incident {
            title: evil.to_owned(),
            url: Some(r#"https://x.test/"><script>alert(4)</script>"#.to_owned()),
            ..Incident::default()
        });
        let pages = StatusPages::from([(evil.to_owned(), "JAVASCRIPT:alert(5)".to_owned())]);
        let html = page(&[s], &pages);
        let body = &html[position(&html, "<main>")..];
        assert!(!body.contains("<script>alert"), "{body}");
        assert!(!body.contains("<img"), "{body}");
        assert!(!body.to_lowercase().contains("javascript:"), "{body}");
        assert!(
            body.contains("&lt;script&gt;alert(1)&lt;/script&gt; &quot;quoted&quot; &amp; &lt;img")
        );
        assert!(body.contains(
            r#"<a href="https://x.test/&quot;&gt;&lt;script&gt;alert(4)&lt;/script&gt;">"#
        ));
    }

    fn probe(ok: bool, description: &str) -> Probe {
        Probe {
            url: "https://api.anthropic.com/v1/models".to_owned(),
            ok,
            code: ok.then_some(401),
            elapsed_ms: ok.then_some(220),
            description: description.to_owned(),
        }
    }

    #[test]
    fn shows_each_probe_under_the_description() {
        let mut up = status("Anthropic", Level::Operational);
        up.probe = Some(probe(true, "responding (HTTP 401 in 220 ms)"));
        let mut down = status("GitHub", Level::Operational);
        down.probe = Some(probe(false, "no answer (could not connect)"));
        let none = status("Plain", Level::Operational);
        let html = page(&[up, down, none], &StatusPages::new());
        let ok_line = r#"<p class="probe ok">Probe: responding (HTTP 401 in 220 ms), GET https://api.anthropic.com/v1/models</p>"#;
        let failed_line = r#"<p class="probe failed">Probe: no answer (could not connect), GET https://api.anthropic.com/v1/models</p>"#;
        assert!(html.contains(ok_line), "{html}");
        assert!(html.contains(failed_line), "{html}");
        assert!(position(&html, "Anthropic description") < position(&html, ok_line));
        assert_eq!(html.matches(r#"class="probe "#).count(), 2, "{html}");
        // Inside <main>, so the self-refresh swaps it in.
        assert!(position(&html, failed_line) < position(&html, "</main>"));
        assert!(html.contains(".probe.failed"));
        // Shown as text, not a link.
        assert!(!html.contains(r#"href="https://api.anthropic.com"#));
    }

    #[test]
    fn a_failed_probe_leaves_the_level_and_verdict_alone() {
        let mut github = status("GitHub", Level::Operational);
        github.probe = Some(probe(false, "no answer (timed out after 9s)"));
        let html = page(&[github], &StatusPages::new());
        assert!(html.contains("<h1>All 1 operational</h1>"), "{html}");
        assert!(html.contains(r#"<span class="level operational">Operational</span>"#));
        assert!(html.contains(r#"<section class="platform operational">"#));
    }

    #[test]
    fn escapes_probe_text() {
        let mut s = status("GitHub", Level::Operational);
        let mut p = probe(false, "<b>no</b> & \"answer\"");
        p.url = "https://x.test/<script>".to_owned();
        s.probe = Some(p);
        let html = page(&[s], &StatusPages::new());
        assert!(
            html.contains(
                "Probe: &lt;b&gt;no&lt;/b&gt; &amp; &quot;answer&quot;, GET https://x.test/&lt;script&gt;"
            ),
            "{html}"
        );
        assert!(!html.contains("<b>no"));
    }

    #[test]
    fn says_so_when_there_are_no_platforms() {
        let html = page(&[], &StatusPages::new());
        assert!(html.contains("<h1>No platforms configured</h1>"), "{html}");
    }

    #[test]
    fn loading_page_retries_without_javascript() {
        let html = loading_page();
        assert!(html.starts_with("<!doctype html>"), "{html}");
        assert!(html.contains("First fetch still running"));
        assert!(html.contains(r#"<meta http-equiv="refresh" content="5">"#));
    }
}
