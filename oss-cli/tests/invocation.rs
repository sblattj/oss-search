//! Invocation behavior for the `oss-cli` binary (clap-built): --help and
//! --version print and exit 0; bare invocation is a usage error, not a hang.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_oss-cli");

fn run(args: &[&str]) -> (Option<i32>, String) {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .spawn()
        .expect("spawn oss-cli");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        match child.try_wait().expect("poll oss-cli") {
            Some(_) => {
                let out = child.wait_with_output().expect("collect oss-cli output");
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
                return (out.status.code(), text);
            }
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("oss-cli {:?} did not terminate within 20s", args);
            }
            None => std::thread::sleep(Duration::from_millis(25)),
        }
    }
}

#[test]
fn help_exits_zero_and_documents_all_seven_tools() {
    let (code, text) = run(&["--help"]);
    assert_eq!(code, Some(0));
    for sub in [
        "search-repos",
        "search-code",
        "repo-profile",
        "repo-tree",
        "fetch-file",
        "fetch-docs",
        "guide",
    ] {
        assert!(text.contains(sub), "--help missing subcommand {sub}:\n{text}");
    }
}

#[test]
fn version_exits_zero() {
    let (code, text) = run(&["--version"]);
    assert_eq!(code, Some(0));
    assert!(text.contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn bare_argv_is_a_usage_error_not_a_hang() {
    let (code, _) = run(&[]);
    assert_eq!(code, Some(2));
}
