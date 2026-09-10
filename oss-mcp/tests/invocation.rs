//! Invocation safety for the `oss-mcp` binary.
//!
//! These tests run the compiled binary in a controlled environment
//! (`Command::env_clear()`, the std equivalent of `env -i`, optionally with a
//! planted fake secret) and pin three contracts:
//!
//! 1. `--help` / `--version` print and exit 0 without ever starting the
//!    stdio server loop (they must terminate even with stdin closed).
//! 2. Help/version/error output never echoes environment values (a planted
//!    `OSS_FAKE_SECRET` must not leak into any stream).
//! 3. Bare `oss-mcp` with stdin closed terminates promptly — it serves, then
//!    exits when stdin hits EOF; it must neither hang nor panic.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_oss-mcp");
const PLANTED_SECRET: &str = "hunter2";

struct Outcome {
    exit: Option<i32>,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// Run the binary with an empty environment (like `env -i`), optionally
/// planting a fake secret, stdin closed, and a hard watchdog timeout.
fn run(args: &[&str], plant_secret: bool, timeout: Duration) -> Outcome {
    let mut cmd = Command::new(BIN);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear();
    if plant_secret {
        cmd.env("OSS_FAKE_SECRET", PLANTED_SECRET);
    }
    let mut child = cmd.spawn().expect("spawn oss-mcp");
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().expect("poll oss-mcp") {
            Some(_) => {
                let out = child.wait_with_output().expect("collect oss-mcp output");
                return Outcome {
                    exit: out.status.code(),
                    stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
                    timed_out: false,
                };
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Outcome {
                    exit: None,
                    stdout: String::new(),
                    stderr: String::new(),
                    timed_out: true,
                };
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn assert_no_secret(o: &Outcome, ctx: &str) {
    assert!(
        !o.stdout.contains(PLANTED_SECRET) && !o.stderr.contains(PLANTED_SECRET),
        "{ctx} leaked planted env secret OSS_FAKE_SECRET\nstdout:\n{}\nstderr:\n{}",
        o.stdout,
        o.stderr
    );
}

#[test]
fn help_exits_zero_without_serving_under_empty_env() {
    let o = run(&["--help"], true, Duration::from_secs(20));
    assert!(
        !o.timed_out,
        "oss-mcp --help did not terminate (stdio server loop started?)"
    );
    assert_eq!(o.exit, Some(0), "exit code mismatch\nstderr:\n{}", o.stderr);
    assert!(o.stdout.contains("USAGE"), "no usage in --help stdout:\n{}", o.stdout);
    assert!(
        o.stdout.contains("--live"),
        "usage does not document --live:\n{}",
        o.stdout
    );
    assert_no_secret(&o, "--help");
}

#[test]
fn help_wins_over_live_before_any_env_read() {
    // --help must be resolved before the env/config load: even combined with
    // --live under an empty environment, no GITHUB_TOKEN warning may print
    // and the process must exit 0 without serving.
    let o = run(&["--live", "--help"], false, Duration::from_secs(20));
    assert!(!o.timed_out, "oss-mcp --live --help did not terminate");
    assert_eq!(o.exit, Some(0), "exit code mismatch\nstderr:\n{}", o.stderr);
    assert!(
        !o.stderr.contains("GITHUB_TOKEN"),
        "--help was resolved after the environment read (warning printed):\n{}",
        o.stderr
    );
    assert!(o.stdout.contains("USAGE"));
}

#[test]
fn version_prints_and_exits_zero_without_serving() {
    let o = run(&["--version"], true, Duration::from_secs(20));
    assert!(!o.timed_out, "oss-mcp --version did not terminate");
    assert_eq!(o.exit, Some(0));
    assert_eq!(
        o.stdout.trim(),
        format!("oss-mcp {}", env!("CARGO_PKG_VERSION")),
        "--version output mismatch"
    );
    assert_no_secret(&o, "--version");
}

#[test]
fn unknown_argument_is_rejected_without_serving() {
    let o = run(&["--bogus"], true, Duration::from_secs(20));
    assert!(!o.timed_out, "oss-mcp --bogus did not terminate");
    assert_eq!(
        o.exit,
        Some(2),
        "unknown argument must exit 2 (usage error)\nstderr:\n{}",
        o.stderr
    );
    assert!(
        o.stderr.contains("unrecognized argument '--bogus'"),
        "missing rejection message:\n{}",
        o.stderr
    );
    assert!(o.stderr.contains("USAGE"), "no usage hint on error:\n{}", o.stderr);
    assert_no_secret(&o, "--bogus");
}

#[test]
fn bare_argv_serves_then_exits_cleanly_when_stdin_closes() {
    // Documented behavior: with no arguments and stdin already closed, the
    // server starts, immediately observes EOF, and exits — no hang, no panic.
    let o = run(&[], true, Duration::from_secs(20));
    assert!(
        !o.timed_out,
        "bare oss-mcp hung with stdin closed (stdio loop must end on EOF)"
    );
    assert!(
        matches!(o.exit, Some(0) | Some(1)),
        "unexpected exit code {:?}\nstderr:\n{}",
        o.exit,
        o.stderr
    );
    assert!(
        !o.stderr.contains("panicked"),
        "bare run panicked on stdin EOF instead of exiting cleanly:\n{}",
        o.stderr
    );
    assert_no_secret(&o, "bare argv");
}
