use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;

use aretheydown::model::{Level, PlatformStatus};
use aretheydown::{config, fetch, history, render, serve, tui};
use chrono::{DateTime, Local, Utc};
use clap::{ArgGroup, Parser};

/// Show the current public status of GitHub, Cloudflare, and other platforms.
///
/// With no flags, opens an interactive TUI that refreshes every 60s. With
/// --serve, polls every 60s, serves the latest result as JSON at
/// <http://127.0.0.1:7341/api/status>, records each change to a history
/// file, which --at reads back, and alerts the webhooks set in the config.
///
/// Exit codes with --once or --at: 0 = every platform Operational, 1 = any
/// Degraded, Outage, or Unknown. 2 = could not run (usage or setup error, or
/// no history for --at).
#[derive(Parser)]
#[command(version, about)]
#[command(group = ArgGroup::new("output").args(["once", "at"]))]
struct Cli {
    /// Fetch every platform once, print the result, and exit
    #[arg(long)]
    once: bool,

    /// Keep polling every 60s, serve the latest result as a web page and JSON over HTTP, record changes to the history file, and POST level changes to the config's [alerts] webhooks
    #[arg(long, conflicts_with_all = ["once", "json"])]
    serve: bool,

    /// Print what each platform looked like at TIME, from --serve's history: RFC 3339, "now", "2h ago"/"1d 4h ago", or local "HH:MM" (most recent), "today HH:MM", "yesterday HH:MM", "YYYY-MM-DD HH:MM" (end in " UTC" or "Z" for UTC)
    #[arg(long, value_name = "TIME", conflicts_with_all = ["serve", "once"])]
    at: Option<String>,

    /// History file that --serve writes, and --at and the TUI read [default: $XDG_STATE_HOME/aretheydown/history.jsonl, else ~/.local/state/aretheydown/history.jsonl]
    // clap prints this doc as `--help` text, where backticks would show.
    #[allow(clippy::doc_markdown)]
    #[arg(long, value_name = "PATH", conflicts_with = "once")]
    history: Option<PathBuf>,

    /// Port for --serve [default: 7341]
    #[arg(long, requires = "serve", conflicts_with = "once")]
    port: Option<u16>,

    /// Address for --serve to listen on [default: 127.0.0.1]; anything else opens it to the network, with no auth
    #[arg(long, value_name = "ADDR", requires = "serve", conflicts_with = "once")]
    bind: Option<IpAddr>,

    /// Print JSON instead of text (needs --once or --at)
    #[arg(long, requires = "output")]
    json: bool,

    /// Config file to use instead of ~/.config/aretheydown/config.toml
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,
}

impl Cli {
    /// The address `--serve` listens on: `--bind`/`--port`, else 127.0.0.1:7341.
    fn serve_addr(&self) -> SocketAddr {
        SocketAddr::new(
            self.bind.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST)),
            self.port.unwrap_or(serve::DEFAULT_PORT),
        )
    }

    /// The history file: `--history`, else [`history::default_path`].
    fn history_path(&self) -> Result<PathBuf, String> {
        self.history
            .clone()
            .or_else(|| history::default_path(|name| std::env::var_os(name)))
            .ok_or_else(|| {
                "could not find a history file location (set HOME or XDG_STATE_HOME, or pass --history)".to_owned()
            })
    }
}

/// Exit code for a run: 0 when every platform is Operational, 1 otherwise.
fn exit_code<'a>(statuses: impl IntoIterator<Item = &'a PlatformStatus>) -> u8 {
    u8::from(statuses.into_iter().any(|s| s.level != Level::Operational))
}

/// `at` in local time as `YYYY-MM-DD HH:MM`, with `:SS` when `seconds`
/// (for two times that may share a minute).
fn local_time(at: DateTime<Utc>, seconds: bool) -> String {
    let format = if seconds {
        "%Y-%m-%d %H:%M:%S"
    } else {
        "%Y-%m-%d %H:%M"
    };
    at.with_timezone(&Local).format(format).to_string()
}

/// The records in force at `at` in the history `text`, one per platform, or
/// why there are none. Warns on stderr about unreadable lines.
fn records_at(text: &str, at: DateTime<Utc>) -> Result<Vec<history::Record>, String> {
    let (records, skipped) = history::parse(text);
    if skipped > 0 {
        eprintln!("warning: skipped {skipped} unreadable line(s) in the history file");
    }
    let found: Vec<history::Record> = history::at_records(&records, at)
        .into_iter()
        .cloned()
        .collect();
    if !found.is_empty() {
        return Ok(found);
    }
    Err(match history::earliest(&records) {
        Some(earliest) => format!(
            "no history at or before {}; the earliest record is {}",
            local_time(at, true),
            local_time(earliest, true)
        ),
        None => "the history file has no records yet; run --serve to record some".to_owned(),
    })
}

/// `--at`: print the recorded statuses at `time` and return the exit code.
/// Any error is printed and exits 2.
fn run_at(cli: &Cli, time: &str) -> ExitCode {
    let found = (|| {
        let at = history::parse_time(time, &Local::now())?;
        let path = cli.history_path()?;
        let text = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "could not read the history file {}: {error}",
                path.display()
            )
        })?;
        Ok::<_, String>((at, records_at(&text, at)?))
    })();
    let (at, records) = match found {
        Ok(found) => found,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::from(2);
        }
    };
    if cli.json {
        println!("{}", render::history_json(&records));
    } else {
        println!(
            "At {} (local), from history recorded by --serve:",
            local_time(at, false)
        );
        print!("{}", render::history_text(&records));
    }
    ExitCode::from(exit_code(records.iter().map(|r| &r.status)))
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // `--at` only reads the history file: no config, no network.
    if let Some(time) = &cli.at {
        return run_at(&cli, time);
    }
    let loaded = match config::load(cli.config.as_deref()) {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("error: {error:#}");
            return ExitCode::from(2);
        }
    };

    // No client-level timeout: `fetch_all` enforces the per-platform one, so a
    // hang always reports "timed out after 10s".
    let client = match reqwest::Client::builder()
        .user_agent(concat!("aretheydown/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("error: could not set up the HTTP client: {error}");
            return ExitCode::from(2);
        }
    };

    if cli.serve {
        let addr = cli.serve_addr();
        let history_path = match cli.history_path() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("error: {error}");
                return ExitCode::from(2);
            }
        };
        return match serve::run(client, loaded, addr, history_path).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error:#}");
                ExitCode::from(2)
            }
        };
    }

    if !cli.once {
        let config_path = cli.config.clone().or_else(config::default_path);
        return match tui::run(client, loaded.platforms, cli.history_path(), config_path).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {error:#}");
                ExitCode::from(2)
            }
        };
    }

    let statuses = fetch::fetch_all_live(&client, &loaded.platforms).await;
    if cli.json {
        println!("{}", render::json(&statuses));
    } else {
        print!("{}", render::text(&statuses));
    }
    ExitCode::from(exit_code(&statuses))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn status(level: Level) -> PlatformStatus {
        PlatformStatus {
            name: "Example".to_owned(),
            level,
            description: String::new(),
            incidents: Vec::new(),
            resolved: Vec::new(),
            fetched_at: Utc::now(),
            error: None,
            probe: None,
            regions: Vec::new(),
        }
    }

    #[test]
    fn exit_code_is_zero_only_when_all_operational() {
        let all_ok = [status(Level::Operational), status(Level::Operational)];
        assert_eq!(exit_code(&all_ok), 0);
        for bad in [Level::Degraded, Level::Outage, Level::Unknown] {
            let mixed = [status(Level::Operational), status(bad)];
            assert_eq!(exit_code(&mixed), 1, "{bad:?} should fail the exit code");
        }
    }

    #[test]
    fn a_failed_probe_changes_no_level_verdict_order_or_exit_code() {
        let mut failed = status(Level::Operational);
        failed.name = "Probed".to_owned();
        failed.probe = Some(aretheydown::model::Probe {
            url: "http://127.0.0.1:1/".to_owned(),
            ok: false,
            code: None,
            elapsed_ms: None,
            description: "no answer (could not connect)".to_owned(),
        });
        let statuses = [status(Level::Operational), failed];
        assert_eq!(exit_code(&statuses), 0);
        assert_eq!(
            aretheydown::model::verdict(&statuses).as_deref(),
            Some("All 2 operational")
        );
        let order: Vec<&str> = aretheydown::model::worst_first(&statuses)
            .iter()
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(order, ["Example", "Probed"]);
        assert!(render::text(&statuses).starts_with("All 2 operational\n"));
    }

    #[test]
    fn serve_defaults_to_localhost_7341() {
        let cli = Cli::try_parse_from(["aretheydown", "--serve"]).unwrap();
        assert!(cli.serve);
        assert_eq!(cli.serve_addr(), "127.0.0.1:7341".parse().unwrap());
        let cli = Cli::try_parse_from([
            "aretheydown",
            "--serve",
            "--port",
            "8080",
            "--bind",
            "0.0.0.0",
        ])
        .unwrap();
        assert_eq!(cli.serve_addr(), "0.0.0.0:8080".parse().unwrap());
    }

    #[test]
    fn serve_flag_combinations_that_are_usage_errors() {
        for args in [
            &["--serve", "--once"][..],
            &["--serve", "--once", "--json"],
            &["--serve", "--json"],
            &["--port", "8080"],
            &["--bind", "0.0.0.0"],
            &["--once", "--port", "8080"],
            &["--at", "02:14", "--serve"],
            &["--at", "02:14", "--once"],
            &["--at", "02:14", "--once", "--json"],
            &["--once", "--history", "/tmp/h.jsonl"],
            &["--once", "--json", "--history", "/tmp/h.jsonl"],
            &["--json", "--history", "/tmp/h.jsonl"],
            &["--json"],
        ] {
            let argv = std::iter::once("aretheydown").chain(args.iter().copied());
            let error = Cli::try_parse_from(argv).err();
            assert!(error.is_some(), "{args:?} should be rejected");
            // clap exits with 2 for usage errors.
            assert_eq!(error.unwrap().exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn at_and_history_combinations_that_are_accepted() {
        for args in [
            &["--at", "02:14"][..],
            &["--at", "02:14", "--json"],
            &["--at", "02:14", "--history", "/tmp/h.jsonl"],
            &[
                "--at",
                "2026-11-14 02:14",
                "--json",
                "--history",
                "/tmp/h.jsonl",
            ],
            &["--serve", "--history", "/tmp/h.jsonl"],
            // The TUI reads the history file too.
            &["--history", "/tmp/h.jsonl"],
            &["--history", "/tmp/h.jsonl", "--config", "/tmp/c.toml"],
            &["--once", "--json"],
        ] {
            let argv = std::iter::once("aretheydown").chain(args.iter().copied());
            assert!(Cli::try_parse_from(argv).is_ok(), "{args:?} should parse");
        }
        let cli = Cli::try_parse_from(["aretheydown", "--at", "x", "--history", "/h"]).unwrap();
        assert_eq!(cli.history_path(), Ok(PathBuf::from("/h")));
    }

    #[test]
    fn records_at_explains_when_nothing_was_recorded_by_then() {
        use chrono::TimeZone;
        let at = Utc.with_ymd_and_hms(2026, 11, 14, 2, 0, 0).unwrap();
        let record = history::Record {
            recorded_at: at,
            status: status(Level::Outage),
            backfilled: false,
        };
        let text = history::line(&record);
        assert_eq!(
            records_at(&text, at).unwrap()[0].status.level,
            Level::Outage
        );
        let before = at - chrono::TimeDelta::minutes(1);
        let error = records_at(&text, before).unwrap_err();
        assert!(error.contains("the earliest record is"), "{error}");
        assert!(error.contains(&local_time(at, true)), "{error}");
        assert!(records_at("", at).unwrap_err().contains("no records"));
    }

    #[test]
    fn resolved_incidents_do_not_fail_the_exit_code() {
        let mut recovered = status(Level::Operational);
        recovered.resolved = vec![aretheydown::model::Incident {
            title: "Resolved an hour ago".to_owned(),
            ended_at: Some(Utc::now()),
            ..Default::default()
        }];
        assert_eq!(exit_code(&[recovered]), 0);
    }
}
