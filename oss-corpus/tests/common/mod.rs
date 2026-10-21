use std::path::Path;
use std::process::Command;

pub fn git(dir: &Path, args: &[&str]) {
    // Ignore the developer's global and system git config: a global
    // `init.templatedir` or `core.hooksPath` would install hooks that can
    // write files into the fixture worktree and change what tests count.
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_TEMPLATE_DIR")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.com")
        .output()
        .expect("git spawns");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn fixture_repo(dir: &Path, files: &[(&str, &str)]) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main", "."]);
    git(dir, &["config", "uploadpack.allowFilter", "true"]);
    git(dir, &["config", "uploadpack.allowAnySHA1InWant", "true"]);
    write_files(dir, files);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", "fixture c1"]);
}

pub fn commit_files(dir: &Path, files: &[(&str, &str)], message: &str) {
    write_files(dir, files);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-qm", message]);
}

fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, contents) in files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, contents).unwrap();
    }
}

pub fn file_url(dir: &Path) -> String {
    format!("file://{}", dir.canonicalize().unwrap().display())
}

pub fn source_lines(count: usize) -> String {
    (0..count)
        .map(|i| format!("let value{i} = compute{i}(arg{i}) + process{i}(seed{i});\n"))
        .collect()
}

pub fn modified_lines(count: usize) -> String {
    (0..count)
        .map(|i| {
            if i % 12 == 0 {
                format!("let changed{i} = compute{i}(arg{i}) + process{i}(seed{i});\n")
            } else {
                format!("let value{i} = compute{i}(arg{i}) + process{i}(seed{i});\n")
            }
        })
        .collect()
}
