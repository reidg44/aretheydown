//! The optional TOML config file: add Atlassian Statuspage vendors and checks
//! of your own endpoints, hide built-ins, limit AWS to chosen regions, and
//! watch only some components of a Statuspage platform. The `[alerts]` table
//! gives `--serve` webhooks to call when a platform's level changes.
//!
//! The file lives at `$XDG_CONFIG_HOME/aretheydown/config.toml`, falling back to
//! `~/.config/aretheydown/config.toml` (the same path on every OS), or wherever
//! `--config` points. See `config.example.toml` at the repo root.
//!
//! [`parse`], [`apply`], and [`alerts`] are pure so tests need no files;
//! [`load`] is the one call a UI makes to get the final platform list and the
//! alert settings.

use std::collections::{BTreeMap, HashSet};
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use crate::model::Level;
use crate::providers::probe::{Expect, Target};
use crate::providers::{self, Platform};

/// The config file's contents. Every key is optional; unknown keys are rejected.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Names of built-in platforms to skip (case-insensitive).
    #[serde(default)]
    pub hide: Vec<String>,
    /// Extra Atlassian Statuspage vendors to check, after the built-ins.
    #[serde(default)]
    pub statuspage: Vec<StatuspageVendor>,
    /// Your own endpoints to check, after the vendors.
    #[serde(default)]
    pub check: Vec<CheckEntry>,
    /// The `[aws]` table.
    #[serde(default)]
    pub aws: AwsConfig,
    /// The `[components]` table: Statuspage platform name (case-insensitive)
    /// to the component or group names that decide its level.
    #[serde(default)]
    pub components: BTreeMap<String, Vec<String>>,
    /// The `[probes]` table: platform name (case-insensitive) to `false`
    /// (no probe) or the probe's `url` and `expect`.
    #[serde(default)]
    pub probes: BTreeMap<String, ProbeEntry>,
    /// The `[alerts]` table; `--serve` only.
    pub alerts: Option<AlertsConfig>,
}

/// The `[alerts]` table.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AlertsConfig {
    /// The URL or URLs `--serve` POSTs each alert to. Secrets: never printed.
    pub webhook: WebhookUrls,
    /// Alert only on changes into or out of these levels (`--json`
    /// spelling); every change when unset.
    pub levels: Option<Vec<String>>,
}

/// `[alerts] webhook`: one URL or a list of them.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum WebhookUrls {
    One(String),
    Many(Vec<String>),
}

impl std::fmt::Debug for AlertsConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AlertsConfig")
            .field("webhook", &"<redacted>")
            .field("levels", &"<redacted>")
            .finish()
    }
}

/// A validated `[alerts] webhook` URL. Anyone holding it can post, so its
/// `Debug` hides it and nothing here prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Webhook {
    url: reqwest::Url,
    position: usize,
}

impl Webhook {
    /// The URL, for sending to.
    #[must_use]
    pub fn url(&self) -> &reqwest::Url {
        &self.url
    }

    /// Where it sits in the config's `webhook` list, from 1 (1 for a single
    /// URL): what warnings name it by (`webhook 2`).
    #[must_use]
    pub fn position(&self) -> usize {
        self.position
    }
}

impl std::fmt::Debug for Webhook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Webhook({}, <redacted>)", self.position)
    }
}

/// A validated `[alerts]` table: where to send alerts, and which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alerts {
    /// At least one, duplicates dropped, in config order.
    pub webhooks: Vec<Webhook>,
    /// The levels a change must go into or out of to alert; `None` alerts
    /// every change.
    pub levels: Option<Vec<Level>>,
}

/// What a config file sets up: the platforms to check and, for `--serve`,
/// where to send alerts and which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loaded {
    pub platforms: Vec<Platform>,
    pub alerts: Option<Alerts>,
}

/// The `[aws]` table.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwsConfig {
    /// Region codes (e.g. `us-east-1`) whose events count. Unset means every
    /// region; global events always count.
    pub regions: Option<Vec<String>>,
}

/// One `[probes]` value: `false` turns the platform's probe off; a table
/// sets its URL, its expected status, or both.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    untagged,
    expecting = "a probe must be false, or a table with only url and/or expect"
)]
pub enum ProbeEntry {
    /// `false` turns the probe off; `true` is an error.
    Toggle(bool),
    Set(ProbeSettings),
}

/// A `[probes]` table value.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeSettings {
    /// The URL to GET (http or https; redirects are not followed). Shown on
    /// screen and in `--json`, so it must not hold a secret.
    pub url: Option<String>,
    /// The status that means up: a code (`401`) or a class (`"4xx"`).
    pub expect: Option<ExpectValue>,
}

/// A probe's `expect` as written: a number or a string.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ExpectValue {
    Code(i64),
    Text(String),
}

/// One `[[statuspage]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatuspageVendor {
    /// Display name, e.g. `"OpenAI"`.
    pub name: String,
    /// The status page root, e.g. `"https://status.openai.com"`.
    pub url: String,
}

/// One `[[check]]` entry: a GET of one of your own endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckEntry {
    /// Display name, e.g. `"API"`.
    pub name: String,
    /// The URL to GET, e.g. `"https://api.example.com/healthz"`. Shown on
    /// screen and in `--json`, so it must not hold a secret.
    pub url: String,
    /// The status code that means up; any 2xx when unset. Read as a wide
    /// integer so an out-of-range value gets [`apply`]'s message.
    pub expect: Option<i64>,
}

/// The suffix Statuspage serves its summary at; accepted (and stripped) in `url`.
const SUMMARY_PATH: &str = "/api/v2/summary.json";

/// Parse config text. Checks syntax and keys only; [`apply`] checks the values.
///
/// # Errors
///
/// Fails on invalid TOML, a wrong type, or an unknown key. The message gives
/// the line and column but not the line itself, and hides any URL in it, since
/// the line may hold the webhook.
pub fn parse(text: &str) -> Result<Config> {
    toml::from_str(text).map_err(|error| {
        let message = redact_values(error.message().trim_end());
        match error.span() {
            Some(span) => {
                let before = &text[..span.start.min(text.len())];
                let line = before.matches('\n').count() + 1;
                let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
                anyhow!("line {line}, column {column}: {message}")
            }
            None => anyhow!("{message}"),
        }
    })
}

/// `message` with every quoted value and every word that holds `://`
/// hidden, so a config error never repeats a webhook URL, with or without
/// its scheme. Key names (in backticks) stay.
fn redact_values(message: &str) -> String {
    let mut out = String::new();
    for (index, part) in message.split('"').enumerate() {
        if index % 2 == 1 {
            out.push_str("\"…\"");
        } else {
            let words: Vec<&str> = part
                .split(' ')
                .map(|word| {
                    if word.contains("://") {
                        "<URL hidden>"
                    } else {
                        word
                    }
                })
                .collect();
            out.push_str(&words.join(" "));
        }
    }
    out
}

/// Validate the `[alerts]` table: `None` without one.
///
/// # Errors
///
/// Fails when `webhook` is an empty list or holds a URL that is not an
/// http(s) URL or has no host (the message says which, by position in a
/// list, without repeating the URL), or when `levels` is empty or names an
/// unknown level.
pub fn alerts(config: &Config) -> Result<Option<Alerts>> {
    let Some(alerts) = &config.alerts else {
        return Ok(None);
    };
    let urls: Vec<(String, &str)> = match &alerts.webhook {
        WebhookUrls::One(url) => vec![("alerts.webhook".to_owned(), url.as_str())],
        WebhookUrls::Many(urls) if urls.is_empty() => {
            bail!("alerts.webhook is an empty list; give at least one URL")
        }
        WebhookUrls::Many(urls) => urls
            .iter()
            .enumerate()
            .map(|(index, url)| (format!("alerts.webhook {}", index + 1), url.as_str()))
            .collect(),
    };
    let mut webhooks: Vec<Webhook> = Vec::new();
    for (position, (label, text)) in urls.into_iter().enumerate() {
        let url = http_url(text).map_err(|problem| anyhow!("{label} {problem}"))?;
        if webhooks.iter().all(|seen| seen.url != url) {
            webhooks.push(Webhook {
                url,
                position: position + 1,
            });
        }
    }
    let levels = alerts.levels.as_deref().map(alert_levels).transpose()?;
    Ok(Some(Alerts { webhooks, levels }))
}

/// `[alerts] levels` as [`Level`]s, duplicates dropped.
fn alert_levels(names: &[String]) -> Result<Vec<Level>> {
    if names.is_empty() {
        bail!("alerts.levels is an empty list; leave it out to alert on every change");
    }
    let mut levels = Vec::new();
    for name in names {
        let level = match name.trim().to_ascii_lowercase().as_str() {
            "operational" => Level::Operational,
            "degraded" => Level::Degraded,
            "outage" => Level::Outage,
            "unknown" => Level::Unknown,
            _ => {
                // A level name is not a secret, but a pasted URL would be,
                // with or without its scheme: show only word-like values.
                let word_like = name.len() <= 20
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'));
                let shown = if word_like {
                    format!("{name:?}")
                } else {
                    "<value hidden>".to_owned()
                };
                bail!(
                    "alerts.levels has an unknown level {shown}; use operational, degraded, outage, or unknown"
                );
            }
        };
        if !levels.contains(&level) {
            levels.push(level);
        }
    }
    Ok(levels)
}

/// Turn the built-in platforms plus a config into the final list: hidden
/// built-ins removed, added vendors then checks appended in file order.
///
/// # Errors
///
/// Fails when a hidden name is not a built-in, a vendor or check name is empty
/// or used twice (including by a built-in that is not hidden), a URL is not
/// http(s), a check's `expect` is not 100 to 599, the
/// AWS region list is empty or holds something that is not a region code, a
/// `[components]` key names no platform, a hidden one, or one that is not on
/// Statuspage, a component list is empty or holds an empty name, or nothing is
/// left to check.
pub fn apply(builtins: Vec<Platform>, config: &Config) -> Result<Vec<Platform>> {
    let aws_regions = config
        .aws
        .regions
        .as_deref()
        .map(validated_aws_regions)
        .transpose()?;
    for name in &config.hide {
        if !builtins.iter().any(|p| same_name(&p.name, name)) {
            let known: Vec<&str> = builtins.iter().map(|p| p.name.as_str()).collect();
            bail!(
                "hide: {name:?} is not a built-in platform (built-ins: {})",
                known.join(", ")
            );
        }
    }

    let mut platforms: Vec<Platform> = builtins
        .into_iter()
        .filter(|p| !config.hide.iter().any(|h| same_name(&p.name, h)))
        .collect();
    if let Some(codes) = aws_regions {
        for platform in &mut platforms {
            platform.limit_aws_regions(&codes);
        }
    }

    let mut taken: HashSet<String> = platforms.iter().map(|p| p.name.to_lowercase()).collect();
    for (index, vendor) in config.statuspage.iter().enumerate() {
        let entry = format!("statuspage entry {}", index + 1);
        let name = new_name(&mut taken, &entry, &vendor.name)?;
        let root = page_root(&vendor.url).with_context(|| format!("{entry} ({name:?})"))?;
        platforms.push(Platform::statuspage(name, &root));
    }
    for (index, check) in config.check.iter().enumerate() {
        let entry = format!("check entry {}", index + 1);
        let name = new_name(&mut taken, &entry, &check.name)?;
        let url = check.url.trim();
        let parsed = http_url(url)
            .map_err(|problem| anyhow!("{entry} ({name:?}): url {url:?} {problem}"))?;
        let expect = check
            .expect
            .map(|code| {
                u16::try_from(code)
                    .ok()
                    .filter(|code| (100..=599).contains(code))
                    .ok_or_else(|| anyhow!("{entry} ({name:?}): expect = {code} is not an HTTP status code (100 to 599)"))
            })
            .transpose()?;
        platforms.push(Platform::check(name, parsed.as_str(), expect));
    }

    let mut watched = HashSet::new();
    for (key, names) in &config.components {
        if !watched.insert(key.trim().to_lowercase()) {
            bail!("components: {key:?} is listed twice (names are not case-sensitive)");
        }
        watch_components(&mut platforms, &config.hide, key, names)?;
    }

    let mut probed = HashSet::new();
    for (key, entry) in &config.probes {
        if !probed.insert(key.trim().to_lowercase()) {
            bail!("probes: {key:?} is listed twice (names are not case-sensitive)");
        }
        set_probe(&mut platforms, &config.hide, key, entry)?;
    }

    if platforms.is_empty() {
        bail!("every platform is hidden and none are added, so there is nothing to check");
    }
    Ok(platforms)
}

/// An added platform's name, trimmed, after checking it is not empty and not
/// already in `taken` (case-insensitive), which it is then added to.
fn new_name<'a>(taken: &mut HashSet<String>, entry: &str, name: &'a str) -> Result<&'a str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("{entry}: name is empty");
    }
    if !taken.insert(name.to_lowercase()) {
        bail!(
            "{entry}: the name {name:?} is already used (to replace a built-in, also list it in `hide`)"
        );
    }
    Ok(name)
}

/// Validate one `[components]` entry and set it on the platform it names.
fn watch_components(
    platforms: &mut [Platform],
    hide: &[String],
    key: &str,
    names: &[String],
) -> Result<()> {
    if names.is_empty() {
        bail!(
            "components.{key} is empty; list component or group names like \"Actions\", or remove it to count every component"
        );
    }
    if names.iter().any(|n| n.trim().is_empty()) {
        bail!("components.{key} holds an empty name");
    }
    let platform = configured_platform(platforms, hide, "components", key)?;
    if !platform.watch_components(names) {
        let hint = if matches!(platform.source, providers::Source::Aws { .. }) {
            "; to limit AWS, use [aws] regions"
        } else {
            ""
        };
        bail!(
            "components: {:?} is not a Statuspage platform, so it has no components to pick{hint}",
            platform.name
        );
    }
    Ok(())
}

/// The platform a `[table]` key names, or an error saying the key names a
/// hidden platform or none.
fn configured_platform<'a>(
    platforms: &'a mut [Platform],
    hide: &[String],
    table: &str,
    key: &str,
) -> Result<&'a mut Platform> {
    let known: Vec<String> = platforms.iter().map(|p| p.name.clone()).collect();
    if let Some(platform) = platforms.iter_mut().find(|p| same_name(&p.name, key)) {
        return Ok(platform);
    }
    if hide.iter().any(|h| same_name(h, key)) {
        bail!("{table}: {key:?} is hidden; remove it from `hide` or from [{table}]");
    }
    bail!(
        "{table}: {key:?} is not a platform (platforms: {})",
        known.join(", ")
    );
}

/// Validate one `[probes]` entry and set it on the platform it names. A URL
/// without `expect` expects any 2xx; `expect` alone keeps the built-in URL.
/// No error repeats the URL or a string `expect`.
fn set_probe(
    platforms: &mut [Platform],
    hide: &[String],
    key: &str,
    entry: &ProbeEntry,
) -> Result<()> {
    let platform = configured_platform(platforms, hide, "probes", key)?;
    if matches!(platform.source, providers::Source::Check { .. }) {
        bail!(
            "probes: {:?} is a [[check]], which is already a probe of its own endpoint",
            platform.name
        );
    }
    let settings = match entry {
        ProbeEntry::Toggle(false) => {
            platform.probe = None;
            return Ok(());
        }
        ProbeEntry::Toggle(true) => {
            bail!(
                "probes.{key} = true: give a table like {{ url = \"https://…\" }}, or false to turn the probe off"
            )
        }
        ProbeEntry::Set(settings) => settings,
    };
    let expect = settings
        .expect
        .as_ref()
        .map(|value| {
            let parsed = match value {
                ExpectValue::Code(code) => u16::try_from(*code).ok().and_then(Expect::code),
                ExpectValue::Text(text) => Expect::parse(text),
            };
            parsed.ok_or_else(|| {
                anyhow!("probes.{key}: expect is not an HTTP status code (100 to 599) or class (\"1xx\" to \"5xx\")")
            })
        })
        .transpose()?;
    let url = settings
        .url
        .as_deref()
        .map(|url| {
            http_url(url)
                .map(|parsed| parsed.to_string())
                .map_err(|problem| anyhow!("probes.{key}: url {problem}"))
        })
        .transpose()?;
    platform.probe = Some(match (url, expect, platform.probe.take()) {
        (Some(url), expect, _) => Target::new(&url, expect.unwrap_or(Expect::Class(2))),
        (None, Some(expect), Some(old)) => Target { expect, ..old },
        (None, Some(_), None) => bail!(
            "probes.{key} needs a url: {:?} has no built-in probe",
            platform.name
        ),
        (None, None, _) => {
            bail!(
                "probes.{key} is empty; give a url, an expect, or both, or false to turn the probe off"
            )
        }
    });
    Ok(())
}

/// Validate `[aws] regions` and return the codes trimmed and lowercased.
fn validated_aws_regions(codes: &[String]) -> Result<Vec<String>> {
    if codes.is_empty() {
        bail!(
            "aws.regions is empty; list region codes like \"us-east-1\", or remove it to count every region"
        );
    }
    codes
        .iter()
        .map(|code| {
            let code = code.trim().to_lowercase();
            if is_region_code(&code) {
                Ok(code)
            } else {
                Err(anyhow!(
                    "aws.regions: {code:?} is not an AWS region code (like \"us-east-1\" or \"us-gov-west-1\")"
                ))
            }
        })
        .collect()
}

/// An AWS region code: a prefix of two or more letters, one or more words, and
/// a number, joined by hyphens (`us-east-1`, `us-gov-west-1`, `eusc-de-east-1`).
fn is_region_code(code: &str) -> bool {
    let parts: Vec<&str> = code.split('-').collect();
    let [prefix, words @ .., number] = parts.as_slice() else {
        return false;
    };
    let letters = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_lowercase());
    prefix.len() >= 2
        && letters(prefix)
        && !words.is_empty()
        && words.iter().all(|word| letters(word))
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

/// Built-in platform names match `hide` entries regardless of case.
fn same_name(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// `text` as an http(s) URL with a host, or what is wrong with it. The
/// problem never repeats the URL, so a webhook stays secret.
fn http_url(text: &str) -> std::result::Result<reqwest::Url, &'static str> {
    const NOT_HTTP: &str = "must be a full URL starting with http:// or https://";
    // The parse error is the problem only (`empty host`), never the input.
    let url = reqwest::Url::parse(text.trim()).map_err(|error| {
        if error.to_string() == "empty host" {
            "has no host"
        } else {
            NOT_HTTP
        }
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(NOT_HTTP);
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("has no host");
    }
    Ok(url)
}

/// Validate a vendor URL and return its page root (without `/api/v2/summary.json`).
fn page_root(url: &str) -> Result<String> {
    let url = url.trim();
    let parsed = http_url(url).map_err(|problem| anyhow!("url {url:?} {problem}"))?;
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("url {url:?} must not have a query (?…) or fragment (#…); use the page root");
    }
    let path = parsed.path().trim_end_matches('/');
    let path = path.strip_suffix(SUMMARY_PATH).unwrap_or(path);
    if path.contains("/api/") || path.ends_with("/api") {
        bail!(
            "url {url:?} points at a Statuspage API path other than {SUMMARY_PATH}; use the page root"
        );
    }
    let mut root = parsed.clone();
    root.set_path(path);
    Ok(root.as_str().trim_end_matches('/').to_owned())
}

/// Where the config file lives when `--config` is not given:
/// `$XDG_CONFIG_HOME/aretheydown/config.toml`, else
/// `$HOME/.config/aretheydown/config.toml` (`%USERPROFILE%` on Windows when
/// `HOME` is unset). `None` when no home directory is known.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    default_path_from(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
    )
}

/// [`default_path`] with the environment passed in, for tests. A relative or
/// empty `XDG_CONFIG_HOME` is ignored, as the XDG spec requires.
fn default_path_from(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    let base = xdg_config_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            home.filter(|h| !h.is_empty())
                .map(|h| PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("aretheydown").join("config.toml"))
}

/// The final platform list ([`providers::builtin`] plus the config file) and
/// the alert settings. Every mode checks `[alerts]`; only `--serve` uses it.
///
/// With `explicit` (from `--config`), that file must exist. Without it, the
/// default path is used, and a missing file means built-ins only.
///
/// # Errors
///
/// Fails when the file cannot be read or is invalid; the message names the file
/// and the problem.
pub fn load(explicit: Option<&Path>) -> Result<Loaded> {
    let builtins_only = || Loaded {
        platforms: providers::builtin(),
        alerts: None,
    };
    let path = match explicit {
        Some(path) => path.to_path_buf(),
        None => match default_path() {
            Some(path) => path,
            None => return Ok(builtins_only()),
        },
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if explicit.is_none() && error.kind() == ErrorKind::NotFound => {
            return Ok(builtins_only());
        }
        Err(error) => bail!("could not read config file {}: {error}", path.display()),
    };
    parse(&text)
        .and_then(|config| {
            Ok(Loaded {
                platforms: apply(providers::builtin(), &config)?,
                alerts: alerts(&config)?,
            })
        })
        .with_context(|| format!("invalid config file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Source;

    fn builtins() -> Vec<Platform> {
        vec![
            Platform::statuspage("GitHub", "https://www.githubstatus.com"),
            Platform::statuspage("Cloudflare", "https://www.cloudflarestatus.com"),
        ]
    }

    fn names(platforms: &[Platform]) -> Vec<&str> {
        platforms.iter().map(|p| p.name.as_str()).collect()
    }

    fn apply_text(text: &str) -> Result<Vec<Platform>> {
        apply(builtins(), &parse(text)?)
    }

    fn error(text: &str) -> String {
        format!(
            "{:#}",
            apply_text(text).expect_err("config should be rejected")
        )
    }

    #[test]
    fn empty_config_keeps_builtins() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(apply_text("").unwrap(), builtins());
        assert_eq!(apply_text("# just a comment\n").unwrap(), builtins());
    }

    #[test]
    fn hide_removes_builtins_case_insensitively() {
        assert_eq!(
            names(&apply_text(r#"hide = ["cloudflare"]"#).unwrap()),
            ["GitHub"]
        );
    }

    #[test]
    fn added_vendors_come_after_builtins_in_file_order() {
        let text = r#"
            hide = ["GitHub"]

            [[statuspage]]
            name = "OpenAI"
            url = "https://status.openai.com"

            [[statuspage]]
            name = "Vercel"
            url = "https://www.vercel-status.com/"
        "#;
        let platforms = apply_text(text).unwrap();
        assert_eq!(names(&platforms), ["Cloudflare", "OpenAI", "Vercel"]);
        assert_eq!(
            platforms[2].source,
            Source::Statuspage {
                url: "https://www.vercel-status.com/api/v2/summary.json".to_owned(),
                components: None,
            }
        );
    }

    #[test]
    fn full_summary_url_is_accepted() {
        let text = r#"
            [[statuspage]]
            name = "npm"
            url = "https://status.npmjs.org/api/v2/summary.json"
        "#;
        let platforms = apply_text(text).unwrap();
        assert_eq!(
            platforms[2].source,
            Source::Statuspage {
                url: "https://status.npmjs.org/api/v2/summary.json".to_owned(),
                components: None,
            }
        );
    }

    #[test]
    fn hidden_builtin_name_can_be_reused() {
        let text = r#"
            hide = ["GitHub"]
            [[statuspage]]
            name = "GitHub"
            url = "https://example.com"
        "#;
        assert_eq!(names(&apply_text(text).unwrap()), ["Cloudflare", "GitHub"]);
    }

    #[test]
    fn unknown_keys_are_rejected_with_line_and_column() {
        let message = error("hide = []\ncolour = \"blue\"\n");
        assert!(message.contains("line 2, column 1"), "{message}");
        assert!(message.contains("unknown field `colour`"), "{message}");

        let message =
            error("[[statuspage]]\nname = \"A\"\nurl = \"https://a.example\"\npage = 1\n");
        assert!(message.contains("unknown field `page`"), "{message}");
    }

    #[test]
    fn bad_toml_and_wrong_types_are_rejected() {
        assert!(error("hide = [").contains("line 1"));
        assert!(error(r#"hide = "GitHub""#).contains("invalid type"));
        assert!(error("[[statuspage]]\nname = \"A\"\n").contains("missing field `url`"));
    }

    #[test]
    fn unknown_hide_name_is_rejected() {
        let message = error(r#"hide = ["Gitlab"]"#);
        assert!(
            message.contains(r#""Gitlab" is not a built-in"#),
            "{message}"
        );
        assert!(message.contains("GitHub, Cloudflare"), "{message}");
    }

    #[test]
    fn hide_cannot_target_an_added_vendor() {
        let text = r#"
            hide = ["OpenAI"]
            [[statuspage]]
            name = "OpenAI"
            url = "https://status.openai.com"
        "#;
        assert!(error(text).contains("not a built-in"));
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let twice = r#"
            [[statuspage]]
            name = "OpenAI"
            url = "https://status.openai.com"
            [[statuspage]]
            name = "openai"
            url = "https://status.openai.com"
        "#;
        let message = error(twice);
        assert!(message.contains("statuspage entry 2"), "{message}");
        assert!(message.contains("already used"), "{message}");

        let clashes_with_builtin = r#"
            [[statuspage]]
            name = "GitHub"
            url = "https://example.com"
        "#;
        assert!(error(clashes_with_builtin).contains("already used"));
    }

    #[test]
    fn empty_name_is_rejected() {
        let text = "[[statuspage]]\nname = \" \"\nurl = \"https://a.example\"\n";
        assert!(error(text).contains("name is empty"));
    }

    #[test]
    fn non_http_urls_are_rejected() {
        for url in [
            "ftp://status.example.com",
            "file:///etc/passwd",
            "status.example.com",
            "",
        ] {
            let text = format!("[[statuspage]]\nname = \"X\"\nurl = \"{url}\"\n");
            let message = error(&text);
            assert!(message.contains("statuspage entry 1 (\"X\")"), "{message}");
            assert!(message.contains("url"), "{message}");
        }
        assert!(
            error("[[statuspage]]\nname = \"X\"\nurl = \"ftp://a.example\"\n")
                .contains("must be a full URL starting with http:// or https://")
        );
    }

    #[test]
    fn urls_with_query_fragment_or_other_api_path_are_rejected() {
        for (url, expected) in [
            ("https://status.openai.com/?utm=x", "must not have a query"),
            ("https://status.openai.com/#top", "must not have a query"),
            (
                "https://status.openai.com/api/v2/status.json",
                "Statuspage API path other than",
            ),
        ] {
            let text = format!("[[statuspage]]\nname = \"X\"\nurl = \"{url}\"\n");
            let message = error(&text);
            assert!(message.contains(expected), "{url}: {message}");
        }
    }

    #[test]
    fn page_root_trailing_slash_is_dropped() {
        let text = "[[statuspage]]\nname = \"npm\"\nurl = \"https://status.npmjs.org/\"\n";
        assert_eq!(
            apply_text(text).unwrap()[2].source,
            Source::Statuspage {
                url: "https://status.npmjs.org/api/v2/summary.json".to_owned(),
                components: None,
            }
        );
    }

    #[test]
    fn an_added_vendor_opens_its_page_root() {
        let text = "[[statuspage]]\nname = \"npm\"\nurl = \"https://status.npmjs.org/\"\n";
        assert_eq!(
            apply_text(text).unwrap()[2].source.status_page(),
            "https://status.npmjs.org"
        );
    }

    #[test]
    fn hiding_everything_is_rejected() {
        let message = error(r#"hide = ["GitHub", "Cloudflare"]"#);
        assert!(message.contains("nothing to check"), "{message}");
    }

    fn with_aws() -> Vec<Platform> {
        let mut platforms = builtins();
        platforms.push(Platform {
            name: "AWS".to_owned(),
            source: Source::Aws {
                url: "https://aws.example/currentevents".to_owned(),
                regions: None,
            },
            probe: None,
        });
        platforms
    }

    fn configured_aws_regions(text: &str) -> Result<Option<Vec<String>>> {
        let platforms = apply(with_aws(), &parse(text)?)?;
        let aws = platforms.into_iter().find(|p| p.name == "AWS").unwrap();
        match aws.source {
            Source::Aws { regions, .. } => Ok(regions),
            other => panic!("AWS has the wrong source: {other:?}"),
        }
    }

    fn aws_error(text: &str) -> String {
        format!(
            "{:#}",
            configured_aws_regions(text).expect_err("config should be rejected")
        )
    }

    #[test]
    fn aws_regions_default_to_every_region() {
        assert_eq!(configured_aws_regions("").unwrap(), None);
        assert_eq!(configured_aws_regions("[aws]\n").unwrap(), None);
    }

    #[test]
    fn aws_regions_are_passed_to_the_aws_platform() {
        let text = "[aws]\nregions = [\"us-east-1\", \" US-West-2 \", \"us-gov-west-1\", \"eusc-de-east-1\"]\n";
        assert_eq!(
            configured_aws_regions(text).unwrap(),
            Some(vec![
                "us-east-1".to_owned(),
                "us-west-2".to_owned(),
                "us-gov-west-1".to_owned(),
                "eusc-de-east-1".to_owned()
            ])
        );
    }

    #[test]
    fn empty_aws_region_list_is_rejected() {
        let message = aws_error("[aws]\nregions = []\n");
        assert!(message.contains("aws.regions"), "{message}");
        assert!(message.contains("empty"), "{message}");
    }

    #[test]
    fn malformed_aws_region_codes_are_rejected() {
        for code in [
            "us-east1",
            "useast-1",
            "us_east_1",
            "N. Virginia",
            "global",
            "",
            "u-east-1",
            "us--1",
        ] {
            let message = aws_error(&format!("[aws]\nregions = [\"{code}\"]\n"));
            assert!(message.contains("aws.regions"), "{code}: {message}");
            assert!(
                message.contains("is not an AWS region code"),
                "{code}: {message}"
            );
        }
    }

    #[test]
    fn unknown_aws_keys_are_rejected() {
        let message = aws_error("[aws]\nregion = [\"us-east-1\"]\n");
        assert!(message.contains("unknown field `region`"), "{message}");
    }

    #[test]
    fn aws_regions_are_ignored_when_aws_is_hidden() {
        let text = "hide = [\"AWS\"]\n[aws]\nregions = [\"us-east-1\"]\n";
        let platforms = apply(with_aws(), &parse(text).unwrap()).unwrap();
        assert_eq!(names(&platforms), ["GitHub", "Cloudflare"]);
    }

    fn watched(platforms: &[Platform], name: &str) -> Option<Vec<String>> {
        match &platforms.iter().find(|p| p.name == name).unwrap().source {
            Source::Statuspage { components, .. } => components.clone(),
            other => panic!("{name} has the wrong source: {other:?}"),
        }
    }

    #[test]
    fn components_apply_to_a_builtin_and_an_added_vendor() {
        let text = r#"
            [[statuspage]]
            name = "Vercel"
            url = "https://www.vercel-status.com"

            [components]
            cloudflare = ["Workers", "North America"]
            VERCEL = ["Builds"]
        "#;
        let platforms = apply_text(text).unwrap();
        assert_eq!(watched(&platforms, "GitHub"), None);
        assert_eq!(
            watched(&platforms, "Cloudflare"),
            Some(vec!["Workers".to_owned(), "North America".to_owned()])
        );
        assert_eq!(
            watched(&platforms, "Vercel"),
            Some(vec!["Builds".to_owned()])
        );
    }

    #[test]
    fn components_for_an_unknown_platform_are_rejected() {
        let message = error("[components]\nGitlab = [\"CI\"]\n");
        assert!(
            message.contains(r#"components: "Gitlab" is not a platform"#),
            "{message}"
        );
        assert!(message.contains("GitHub, Cloudflare"), "{message}");
    }

    #[test]
    fn components_for_a_non_statuspage_platform_are_rejected() {
        let text = "[components]\naws = [\"EC2\"]\n";
        let message = format!(
            "{:#}",
            apply(with_aws(), &parse(text).unwrap()).unwrap_err()
        );
        assert!(message.contains("not a Statuspage platform"), "{message}");
        assert!(message.contains("[aws] regions"), "{message}");

        let mut platforms = builtins();
        platforms.push(Platform {
            name: "Azure".to_owned(),
            source: Source::Azure {
                url: "https://azure.example/feed".to_owned(),
            },
            probe: None,
        });
        let text = "[components]\nAzure = [\"VMs\"]\n";
        let message = format!("{:#}", apply(platforms, &parse(text).unwrap()).unwrap_err());
        assert!(message.contains("not a Statuspage platform"), "{message}");
        assert!(!message.contains("[aws]"), "{message}");
    }

    #[test]
    fn checks_come_after_vendors_in_file_order() {
        let text = r#"
            [[check]]
            name = " API "
            url = " https://api.example.com/healthz "
            expect = 204

            [[statuspage]]
            name = "Vercel"
            url = "https://www.vercel-status.com"

            [[check]]
            name = "Web"
            url = "http://127.0.0.1:8080"
        "#;
        let platforms = apply_text(text).unwrap();
        assert_eq!(
            names(&platforms),
            ["GitHub", "Cloudflare", "Vercel", "API", "Web"]
        );
        assert_eq!(
            platforms[3].source,
            Source::Check {
                url: "https://api.example.com/healthz".to_owned(),
                expect: Some(204),
            }
        );
        assert_eq!(
            platforms[4].source,
            Source::Check {
                url: "http://127.0.0.1:8080/".to_owned(),
                expect: None,
            }
        );
    }

    #[test]
    fn bad_checks_are_rejected() {
        let check = |body: &str| error(&format!("[[check]]\n{body}\n"));
        let message = check("name = \"API\"\nurl = \"ftp://api.example.com\"");
        assert!(message.contains("check entry 1 (\"API\")"), "{message}");
        assert!(message.contains("must be a full URL"), "{message}");
        for code in [99, 600, -1, 70000] {
            let message = check(&format!(
                "name = \"API\"\nurl = \"https://api.example.com\"\nexpect = {code}"
            ));
            assert!(
                message.contains(&format!(
                    "expect = {code} is not an HTTP status code (100 to 599)"
                )),
                "{message}"
            );
        }
        assert!(
            check("name = \" \"\nurl = \"https://a.example\"")
                .contains("check entry 1: name is empty")
        );
        let clash = check("name = \"github\"\nurl = \"https://a.example\"");
        assert!(clash.contains("already used"), "{clash}");
        let text = "[[statuspage]]\nname = \"API\"\nurl = \"https://s.example\"\n[[check]]\nname = \" api\"\nurl = \"https://a.example\"\n";
        assert!(error(text).contains("check entry 1: the name \"api\" is already used"));
        assert!(check("name = \"API\"").contains("missing field `url`"));
        assert!(
            check("name = \"API\"\nurl = \"https://a.example\"\nmethod = \"POST\"")
                .contains("unknown field")
        );
    }

    fn probes(text: &str) -> Vec<(String, Option<(String, String)>)> {
        let mut platforms = builtins();
        platforms[0].probe = Some(Target::new("https://github.com/", Expect::Class(2)));
        apply(platforms, &parse(text).unwrap())
            .unwrap()
            .into_iter()
            .map(|p| {
                let probe = p.probe.map(|t| (t.url, t.expect.to_string()));
                (p.name, probe)
            })
            .collect()
    }

    fn probe_of(text: &str, name: &str) -> Option<(String, String)> {
        probes(text).into_iter().find(|(n, _)| n == name).unwrap().1
    }

    fn pair(url: &str, expect: &str) -> (String, String) {
        (url.to_owned(), expect.to_owned())
    }

    #[test]
    fn probes_change_a_builtins_url_or_expect() {
        assert_eq!(
            probe_of("", "GitHub"),
            Some(pair("https://github.com/", "2xx"))
        );
        assert_eq!(
            probe_of(
                "[probes]
github = { expect = \"3XX\" }
",
                "GitHub"
            ),
            Some(pair("https://github.com/", "3xx"))
        );
        assert_eq!(
            probe_of(
                "[probes]
GitHub = { expect = 401 }
",
                "GitHub"
            ),
            Some(pair("https://github.com/", "401"))
        );
        assert_eq!(
            probe_of(
                "[probes]
GitHub = { url = \"https://api.github.com/\" }
",
                "GitHub"
            ),
            Some(pair("https://api.github.com/", "2xx")),
            "a new url without expect expects any 2xx"
        );
        assert_eq!(
            probe_of(
                "[probes]
GitHub = { url = \"https://api.github.com/\", expect = \"401\" }
",
                "GitHub"
            ),
            Some(pair("https://api.github.com/", "401"))
        );
    }

    #[test]
    fn probes_false_turns_one_off() {
        assert_eq!(
            probe_of(
                "[probes]
GitHub = false
",
                "GitHub"
            ),
            None
        );
    }

    #[test]
    fn probes_add_one_to_a_statuspage_vendor() {
        let text = "[[statuspage]]\nname = \"Vercel\"\nurl = \"https://www.vercel-status.com\"\n[probes]\nVercel = { url = \"https://vercel.com/\", expect = \"3xx\" }\n";
        assert_eq!(
            probe_of(text, "Vercel"),
            Some(pair("https://vercel.com/", "3xx"))
        );
        assert_eq!(probe_of(text, "Cloudflare"), None);
        let no_url = "[[statuspage]]\nname = \"Vercel\"\nurl = \"https://www.vercel-status.com\"\n[probes]\nVercel = { expect = \"3xx\" }\n";
        let message = error(no_url);
        assert!(message.contains("probes.Vercel needs a url"), "{message}");
    }

    #[test]
    fn probes_keys_must_name_a_shown_platform_that_is_not_a_check() {
        let message = error("[probes]\nGitlab = false\n");
        assert!(
            message.contains(r#"probes: "Gitlab" is not a platform"#),
            "{message}"
        );
        let message = error("hide = [\"GitHub\"]\n[probes]\nGitHub = false\n");
        assert!(message.contains("is hidden"), "{message}");
        let check = "[[check]]\nname = \"API\"\nurl = \"https://api.example.com\"\n";
        let message = error(&format!("{check}[probes]\nAPI = false\n"));
        assert!(message.contains("is a [[check]]"), "{message}");
        let message = error("[probes]\nGitHub = false\ngithub = false\n");
        assert!(message.contains("listed twice"), "{message}");
    }

    #[test]
    fn probes_reject_bad_values_without_repeating_the_url() {
        let secret = "s3cret-token";
        for (value, wanted) in [
            (
                format!("{{ url = \"ftp://x.example/{secret}\" }}"),
                "url must be a full URL",
            ),
            (
                format!("{{ url = \"{secret}\" }}"),
                "url must be a full URL",
            ),
            (
                "{ expect = 600 }".to_owned(),
                "expect is not an HTTP status code",
            ),
            (
                "{ expect = 99 }".to_owned(),
                "expect is not an HTTP status code",
            ),
            (
                format!("{{ expect = \"{secret}\" }}"),
                "expect is not an HTTP status code",
            ),
            (
                "{ expect = \"6xx\" }".to_owned(),
                "expect is not an HTTP status code",
            ),
            ("{}".to_owned(), "probes.GitHub is empty"),
            ("true".to_owned(), "probes.GitHub = true"),
        ] {
            let message = error(&format!("[probes]\nGitHub = {value}\n"));
            assert!(message.contains(wanted), "{value}: {message}");
            assert!(!message.contains(secret), "{value}: {message}");
        }
        let message = format!(
            "{:#}",
            parse(&format!(
                "[probes]\nGitHub = {{ url = \"https://{secret}\", method = \"HEAD\" }}\n"
            ))
            .unwrap_err()
        );
        assert!(!message.contains(secret), "{message}");
    }

    #[test]
    fn hide_and_components_cannot_name_a_check() {
        let check = "[[check]]\nname = \"API\"\nurl = \"https://api.example.com\"\n";
        assert!(error(&format!("hide = [\"API\"]\n{check}")).contains("not a built-in"));
        let message = error(&format!("{check}[components]\nAPI = [\"x\"]\n"));
        assert!(message.contains("not a Statuspage platform"), "{message}");
    }

    #[test]
    fn components_for_microsoft_365_are_rejected() {
        let text = "[components]\n\"Microsoft 365\" = [\"OneDrive\"]\n";
        let message = format!(
            "{:#}",
            apply(providers::builtin(), &parse(text).unwrap()).unwrap_err()
        );
        assert!(message.contains("not a Statuspage platform"), "{message}");
    }

    #[test]
    fn components_for_a_hidden_platform_are_rejected() {
        let message = error("hide = [\"GitHub\"]\n[components]\nGitHub = [\"Actions\"]\n");
        assert!(message.contains(r#""GitHub" is hidden"#), "{message}");
    }

    #[test]
    fn empty_component_lists_and_names_are_rejected() {
        let message = error("[components]\nGitHub = []\n");
        assert!(message.contains("components.GitHub"), "{message}");
        assert!(message.contains("empty"), "{message}");

        let message = error("[components]\nGitHub = [\"Actions\", \" \"]\n");
        assert!(message.contains("components.GitHub"), "{message}");
        assert!(message.contains("empty name"), "{message}");
    }

    #[test]
    fn components_listed_twice_are_rejected() {
        let message = error("[components]\nGitHub = [\"Actions\"]\ngithub = [\"Pages\"]\n");
        assert!(message.contains("listed twice"), "{message}");
    }

    #[test]
    fn component_lists_must_be_lists_of_strings() {
        assert!(error("[components]\nGitHub = \"Actions\"\n").contains("invalid type"));
    }

    #[test]
    fn default_path_prefers_absolute_xdg_config_home() {
        let path = default_path_from(Some("/xdg".into()), Some("/home/me".into()));
        assert_eq!(path, Some(PathBuf::from("/xdg/aretheydown/config.toml")));

        for ignored in ["", "relative/dir"] {
            let path = default_path_from(Some(ignored.into()), Some("/home/me".into()));
            assert_eq!(
                path,
                Some(PathBuf::from("/home/me/.config/aretheydown/config.toml"))
            );
        }
        assert_eq!(default_path_from(None, None), None);
        assert_eq!(default_path_from(None, Some("".into())), None);
    }

    #[test]
    fn example_config_parses_as_is_and_uncommented() {
        let example = include_str!("../config.example.toml");
        assert_eq!(apply_text(example).unwrap(), builtins());

        let settings = [
            "expect = ",
            "hide = [",
            "name = \"",
            "url = \"",
            "regions = [",
            "GitHub = [",
            "webhook = [",
            "levels = [",
            "OpenAI = false",
            "GitHub = { url",
            "Vercel = { url",
        ];
        let uncommented: String = example
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                Some(rest)
                    if rest == "[[statuspage]]"
                        || rest == "[[check]]"
                        || rest == "[aws]"
                        || rest == "[components]"
                        || rest == "[alerts]"
                        || rest == "[probes]"
                        || settings.iter().any(|s| rest.starts_with(s)) =>
                {
                    rest
                }
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut base = builtins();
        base.push(Platform {
            probe: Some(Target::new("https://api.openai.com/", Expect::Code(401))),
            ..Platform::statuspage("OpenAI", "https://status.openai.com")
        });
        let platforms = apply(base, &parse(&uncommented).unwrap()).unwrap();
        assert_eq!(
            names(&platforms),
            ["GitHub", "OpenAI", "Vercel", "npm", "Atlassian", "API"]
        );
        let probe = |name: &str| {
            platforms
                .iter()
                .find(|p| p.name == name)
                .unwrap()
                .probe
                .clone()
        };
        assert_eq!(probe("OpenAI"), None);
        assert_eq!(
            probe("GitHub"),
            Some(Target::new("https://api.github.com/", Expect::Class(2)))
        );
        assert_eq!(
            probe("Vercel"),
            Some(Target::new("https://vercel.com/", Expect::Class(3)))
        );
        assert_eq!(
            platforms.last().unwrap().source,
            Source::Check {
                url: "https://api.example.com/healthz".to_owned(),
                expect: Some(200),
            }
        );
        assert_eq!(
            parse(&uncommented).unwrap().aws.regions,
            Some(vec!["us-east-1".to_owned(), "us-west-2".to_owned()])
        );
        assert_eq!(
            watched(&platforms, "GitHub"),
            Some(vec!["Actions".to_owned(), "Packages".to_owned()])
        );
        let uncommented_alerts = alerts(&parse(&uncommented).unwrap()).unwrap().unwrap();
        assert_eq!(uncommented_alerts.webhooks.len(), 2);
        assert_eq!(uncommented_alerts.levels, Some(vec![Level::Outage]));
        assert_eq!(alerts(&parse(example).unwrap()).unwrap(), None);
    }

    const HOOK: &str = "https://hooks.example.com/services/T0/B0/s3cret";

    fn alerts_text(text: &str) -> Result<Option<Alerts>> {
        alerts(&parse(text)?)
    }

    /// The one webhook `text` sets.
    fn webhook_text(text: &str) -> Result<Option<Webhook>> {
        Ok(alerts_text(text)?.map(|alerts| {
            assert_eq!(alerts.webhooks.len(), 1);
            alerts.webhooks[0].clone()
        }))
    }

    #[test]
    fn no_alerts_table_means_no_webhook() {
        assert_eq!(webhook_text("").unwrap(), None);
    }

    #[test]
    fn alerts_webhook_parses() {
        let text = format!("[alerts]\nwebhook = \" {HOOK} \"\n");
        let hook = webhook_text(&text).unwrap().unwrap();
        assert_eq!(hook.url().as_str(), HOOK);
        assert!(!format!("{hook:?}").contains("s3cret"));
        assert!(!format!("{:?}", parse(&text).unwrap()).contains("s3cret"));
        let plain = webhook_text("[alerts]\nwebhook = \"http://127.0.0.1:9000/hook\"\n");
        assert_eq!(plain.unwrap().unwrap().url().port(), Some(9000));
    }

    #[test]
    fn bad_alerts_tables_are_errors_that_hide_the_url() {
        for (text, expected) in [
            ("[alerts]\n".to_owned(), "missing field `webhook`"),
            (
                format!("[alerts]\nwebhook = \"{HOOK}\"\nchannel = \"{HOOK}\"\n"),
                "unknown field `channel`",
            ),
            (
                format!("[alerts]\nwebhok = \"{HOOK}\"\n"),
                "unknown field `webhok`",
            ),
            (format!("alerts = \"{HOOK}\"\n"), "invalid type"),
            (
                "alerts = \"hooks.example.com/s3cret\"\n".to_owned(),
                "invalid type",
            ),
            (
                "[alerts]\nwebhook = \"ftp://hooks.example.com/s3cret\"\n".to_owned(),
                "must be a full URL starting with http:// or https://",
            ),
            (
                "[alerts]\nwebhook = \"hooks.example.com/s3cret\"\n".to_owned(),
                "must be a full URL starting with http:// or https://",
            ),
            (
                "[alerts]\nwebhook = \"https://\"\n".to_owned(),
                "has no host",
            ),
            (
                "[alerts]\nwebhook = []\n".to_owned(),
                "alerts.webhook is an empty list",
            ),
            (
                format!("[alerts]\nwebhook = [\"{HOOK}\", \"hooks.example.com/s3cret\"]\n"),
                "alerts.webhook 2 must be a full URL starting with http:// or https://",
            ),
            (
                format!("[alerts]\nwebhook = [\"{HOOK}\", 5]\n"),
                "did not match any variant",
            ),
            (
                format!("[alerts]\nwebhook = 5\nlevels = [\"{HOOK}\"]\n"),
                "did not match any variant",
            ),
            (
                format!("[alerts]\nwebhook = \"{HOOK}\"\nlevels = [\"{HOOK}\"]\n"),
                "alerts.levels has an unknown level <value hidden>",
            ),
            (
                format!(
                    "[alerts]\nwebhook = \"{HOOK}\"\nlevels = [\"hooks.example.com/s3cret\"]\n"
                ),
                "alerts.levels has an unknown level <value hidden>",
            ),
        ] {
            let message = format!(
                "{:#}",
                webhook_text(&text).expect_err("config should be rejected")
            );
            assert!(message.contains(expected), "{text}: {message}");
            assert!(!message.contains("s3cret"), "{text}: {message}");
            assert!(!message.contains("hooks.example"), "{text}: {message}");
        }
    }

    #[test]
    fn alerts_webhook_takes_a_list_and_drops_duplicates() {
        let text = format!(
            "[alerts]\nwebhook = [\"{HOOK}\", \"http://127.0.0.1:9000/hook\", \" {HOOK}\"]\n"
        );
        let alerts = alerts_text(&text).unwrap().unwrap();
        let urls: Vec<(usize, &str)> = alerts
            .webhooks
            .iter()
            .map(|hook| (hook.position(), hook.url().as_str()))
            .collect();
        assert_eq!(urls, [(1, HOOK), (2, "http://127.0.0.1:9000/hook")]);
        assert_eq!(alerts.levels, None);
        assert!(!format!("{alerts:?}").contains("s3cret"));
        assert!(!format!("{alerts:?}").contains("9000"));
        let pasted = format!("[alerts]\nwebhook = \"{HOOK}\"\nlevels = [\"{HOOK}\"]\n");
        assert!(!format!("{:?}", parse(&pasted).unwrap()).contains("s3cret"));
        assert!(!format!("{:?}", parse(&text).unwrap()).contains("s3cret"));
        // A one-item list is the same as the string.
        let one = alerts_text(&format!("[alerts]\nwebhook = [\"{HOOK}\"]\n")).unwrap();
        assert_eq!(
            one,
            alerts_text(&format!("[alerts]\nwebhook = \"{HOOK}\"\n")).unwrap()
        );
    }

    #[test]
    fn alerts_levels_parse_in_json_spelling() {
        let levels = |list: &str| {
            alerts_text(&format!(
                "[alerts]\nwebhook = \"{HOOK}\"\nlevels = {list}\n"
            ))
            .map(|alerts| alerts.unwrap().levels)
        };
        assert_eq!(levels("[\"outage\"]").unwrap(), Some(vec![Level::Outage]));
        assert_eq!(
            levels("[\"Outage\", \" unknown \", \"degraded\", \"operational\", \"outage\"]")
                .unwrap(),
            Some(vec![
                Level::Outage,
                Level::Unknown,
                Level::Degraded,
                Level::Operational
            ])
        );
        for (list, expected) in [
            (
                "[]",
                "alerts.levels is an empty list; leave it out to alert on every change",
            ),
            (
                "[\"outage\", \"down\"]",
                "alerts.levels has an unknown level \"down\"; use operational, degraded, outage, or unknown",
            ),
            ("\"outage\"", "invalid type"),
        ] {
            let message = format!("{:#}", levels(list).expect_err("levels should be rejected"));
            assert!(message.contains(expected), "{list}: {message}");
            assert!(!message.contains("s3cret"), "{list}: {message}");
        }
    }

    #[test]
    fn load_returns_the_alert_settings() {
        let dir = std::env::temp_dir().join(format!("aretheydown-alerts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            format!("[alerts]\nwebhook = [\"{HOOK}\"]\nlevels = [\"outage\"]\n"),
        )
        .unwrap();
        let loaded = load(Some(&path));
        std::fs::remove_dir_all(&dir).unwrap();
        let alerts = loaded.unwrap().alerts.unwrap();
        assert_eq!(alerts.webhooks.len(), 1);
        assert_eq!(alerts.levels, Some(vec![Level::Outage]));
    }

    #[test]
    fn parse_errors_point_at_the_line_without_echoing_it() {
        let message = error("hide = \"https://x.example/s3cret\"\n");
        assert!(
            message.starts_with("line 1, column 8: invalid type"),
            "{message}"
        );
        assert!(!message.contains("s3cret"), "{message}");
    }

    #[test]
    fn explicit_missing_file_is_an_error_naming_it() {
        let path = Path::new("/definitely/not/here/config.toml");
        let message = format!("{:#}", load(Some(path)).unwrap_err());
        assert!(
            message.contains("/definitely/not/here/config.toml"),
            "{message}"
        );
    }
}
