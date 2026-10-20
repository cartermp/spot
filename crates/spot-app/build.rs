use std::process::Command;

fn git_stdout(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=SPOT_BUILD_COMMIT");
    println!("cargo:rerun-if-env-changed=SPOT_BUILD_NUMBER");
    println!("cargo:rerun-if-env-changed=GITHUB_RUN_NUMBER");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");

    let commit_sha = std::env::var("SPOT_BUILD_COMMIT")
        .ok()
        .filter(|s| s.trim().len() >= 7)
        .or_else(|| git_stdout(&["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "0000000000000000000000000000000000000000".to_string());

    let short_sha: String = commit_sha.trim().chars().take(7).collect();

    let commit_count = git_stdout(&["rev-list", "--count", "HEAD"])
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1);

    let build_number = std::env::var("SPOT_BUILD_NUMBER")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .or_else(|| {
            std::env::var("GITHUB_RUN_NUMBER")
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|run| run.max(commit_count))
        })
        .unwrap_or(commit_count)
        .max(1);

    let commit_date = git_stdout(&["log", "-1", "--format=%cI", "HEAD"])
        .unwrap_or_else(|| "2026-10-19T00:00:00Z".to_string());

    println!("cargo:rustc-env=SPOT_BUILD_COMMIT={}", commit_sha.trim());
    println!("cargo:rustc-env=SPOT_BUILD_SHORT_COMMIT={short_sha}");
    println!("cargo:rustc-env=SPOT_BUILD_NUMBER={build_number}");
    println!("cargo:rustc-env=SPOT_BUILD_TIMESTAMP={commit_date}");
}
