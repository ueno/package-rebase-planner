//! Git operations, done by shelling out to the `git` binary.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

pub fn capture(home: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(home)
        .output()
        .context("failed to run git")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn parse_line(line: &str) -> Result<(String, String)> {
    let v: Vec<_> = line.splitn(2, " ").collect();
    if v.len() == 2 {
        Ok((v[0].to_string(), v[1].to_string()))
    } else {
        bail!("failed to parse git output {line}")
    }
}

pub fn list_commits(home: &Path, range: &str) -> Result<Vec<(String, String)>> {
    let commits = capture(
        home,
        &["log", "--format=format:%H %s", "--no-merges", range],
    )?;
    commits.lines().map(parse_line).collect()
}

pub fn list_adjacent_commits(
    home: &Path,
    commit: &str,
    count: usize,
) -> Result<Vec<(String, String)>> {
    let after_commits = capture(
        home,
        &[
            "log",
            "--format=format:%H %s",
            "--no-merges",
            &format!("--max-count-oldest={count}"),
            &format!("{commit}..HEAD"),
        ],
    )?;
    let before_commits = capture(
        home,
        &[
            "log",
            "--format=format:%H %s",
            "--no-merges",
            &format!("--max-count={count}"),
            commit,
        ],
    )?;

    let mut commits = vec![];

    let after_commits: Result<Vec<_>> = after_commits.lines().map(parse_line).collect();
    commits.extend(after_commits?);

    let before_commits: Result<Vec<_>> = before_commits.lines().map(parse_line).collect();
    commits.extend(before_commits?);

    Ok(commits)
}

pub fn read_commit(home: &Path, commit: &str) -> Result<String> {
    let commits = capture(home, &["show", commit])?;
    Ok(commits.trim().to_string())
}

pub fn read_file(home: &Path, commit: &str, path: &Path) -> Result<String> {
    let commits = capture(home, &["show", &format!("{}:{}", commit, path.display())])?;
    Ok(commits.trim().to_string())
}
