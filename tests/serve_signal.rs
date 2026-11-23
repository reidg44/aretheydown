//! Stopping `--serve` with a signal: SIGTERM (what launchd, systemd, and
//! `brew services stop` send) and SIGINT (Ctrl-C) both exit 0.
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Start `--serve` on a free port with every built-in hidden and one
/// Statuspage vendor on a closed local port (no network), and wait until it
/// prints its `serving` line.
fn start_serve(tag: &str) -> Child {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    let config = dir.join(format!("serve-signal-{tag}.toml"));
    std::fs::write(
        &config,
        "hide = [\"GitHub\", \"Cloudflare\", \"AWS\", \"Azure\", \"Google Cloud\", \
         \"OpenAI\", \"Anthropic\", \"Microsoft 365\"]\n\
         [[statuspage]]\nname = \"Local\"\nurl = \"http://127.0.0.1:1\"\n",
    )
    .expect("write temp config");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aretheydown"))
        .args(["--serve", "--port", "0", "--config"])
        .arg(&config)
        .arg("--history")
        .arg(dir.join(format!("serve-signal-{tag}.jsonl")))
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run aretheydown");
    let stderr = child.stderr.take().expect("piped stderr");
    let mut lines = BufReader::new(stderr).lines();
    loop {
        let line = lines
            .next()
            .expect("--serve exited before serving")
            .expect("read stderr");
        if line.starts_with("serving ") {
            break;
        }
    }
    // Keep reading, so later log lines never hit a closed pipe.
    std::thread::spawn(move || lines.for_each(drop));
    child
}

/// Send `signal` (a `kill` name such as `TERM`) and return the exit code,
/// failing if the process is still running after 5 seconds.
fn stop_with(mut child: Child, signal: &str) -> Option<i32> {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(child.id().to_string())
        .status()
        .expect("run kill");
    assert!(status.success(), "kill -{signal} failed");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().expect("wait for --serve") {
            return status.code();
        }
        if Instant::now() > deadline {
            child.kill().ok();
            panic!("--serve still running 5s after SIGTERM/SIGINT");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn sigterm_stops_serve_with_exit_0() {
    let child = start_serve("term");
    assert_eq!(stop_with(child, "TERM"), Some(0));
}

#[test]
fn sigint_stops_serve_with_exit_0() {
    let child = start_serve("int");
    assert_eq!(stop_with(child, "INT"), Some(0));
}
