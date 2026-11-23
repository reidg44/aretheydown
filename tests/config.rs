//! Running the binary with a bad `--config`: it must exit 2 before fetching
//! anything and name the file and the problem.

use std::path::PathBuf;
use std::process::{Command, Output};

/// Write `text` to a fresh file under Cargo's per-test temp dir.
fn write_config(file_name: &str, text: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(file_name);
    std::fs::write(&path, text).expect("write temp config");
    path
}

fn run_once(config: &PathBuf) -> Output {
    Command::new(env!("CARGO_BIN_EXE_aretheydown"))
        .arg("--once")
        .arg("--config")
        .arg(config)
        .output()
        .expect("run aretheydown")
}

#[test]
fn invalid_config_exits_2_and_names_the_file_and_problem() {
    let path = write_config("invalid-config.toml", "hide = []\ncolour = \"blue\"\n");
    let output = run_once(&path);
    std::fs::remove_file(&path).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(
        output.stdout.is_empty(),
        "nothing should be fetched or printed"
    );
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(stderr.contains("line 2, column 1"), "{stderr}");
    assert!(stderr.contains("unknown field `colour`"), "{stderr}");
}

#[test]
fn unknown_hide_name_exits_2() {
    let path = write_config("unknown-hide.toml", "hide = [\"NoSuchPlatform\"]\n");
    let output = run_once(&path);
    std::fs::remove_file(&path).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(
        stderr.contains("\"NoSuchPlatform\" is not a built-in"),
        "{stderr}"
    );
}

#[test]
fn missing_explicit_config_exits_2() {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("does-not-exist.toml");
    let output = run_once(&path);

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains("could not read config file"), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
}

#[test]
fn malformed_aws_region_exits_2() {
    let path = write_config("bad-aws-region.toml", "[aws]\nregions = [\"us-east1\"]\n");
    let output = run_once(&path);
    std::fs::remove_file(&path).ok();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "stderr: {stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(
        stderr.contains("aws.regions: \"us-east1\" is not an AWS region code"),
        "{stderr}"
    );
}

/// A `[[check]]` at a closed local port: an Outage that the verdict names,
/// and `--once` exits 1. No network beyond 127.0.0.1.
#[test]
fn a_failing_check_is_named_in_the_verdict_and_exits_1() {
    let path = write_config(
        "failing-check.toml",
        "hide = [\"GitHub\", \"Cloudflare\", \"AWS\", \"Azure\", \"Google Cloud\", \"OpenAI\", \"Anthropic\", \"Microsoft 365\"]\n\
         [[check]]\nname = \"API\"\nurl = \"http://127.0.0.1:1/healthz\"\n",
    );
    let output = run_once(&path);
    std::fs::remove_file(&path).ok();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(1), "stdout: {stdout}");
    assert!(
        stdout.starts_with("1 of 1 needs attention: API\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("API  Outage  could not connect"),
        "{stdout}"
    );
}
