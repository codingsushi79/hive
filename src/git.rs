//! The few git operations hive needs: isolate an agent in a worktree, see
//! what it changed, and fold its branch back in.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .context("running git")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        bail!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            if err.is_empty() { "failed" } else { err }
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn repo_root(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "--show-toplevel"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn head(dir: &Path) -> Result<String> {
    Ok(git(dir, &["rev-parse", "HEAD"])
        .context("the repository needs at least one commit")?
        .trim()
        .to_string())
}

pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok()
}

pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    git(
        repo,
        &["worktree", "add", "-b", branch, &path.to_string_lossy(), base],
    )?;
    Ok(())
}

pub fn remove_worktree(repo: &Path, path: &Path) -> Result<()> {
    if path.exists() {
        git(repo, &["worktree", "remove", "--force", &path.to_string_lossy()])?;
    }
    let _ = git(repo, &["worktree", "prune"]);
    Ok(())
}

pub fn delete_branch(repo: &Path, branch: &str) -> Result<()> {
    git(repo, &["branch", "-D", branch])?;
    Ok(())
}

/// Mark new files so `git diff` shows them. Only used inside hive's own
/// worktrees, never on a checkout the user is working in.
pub fn track_new_files(dir: &Path) {
    let _ = git(dir, &["add", "--intent-to-add", "--all"]);
}

/// Everything changed since `base`: commits and uncommitted work alike.
pub fn diff(dir: &Path, base: &str) -> Result<String> {
    git(dir, &["diff", "--no-color", "--no-ext-diff", base])
}

/// `3 files changed, 40 insertions(+), 2 deletions(-)` → `3 files +40 -2`.
pub fn shortstat(dir: &Path, base: &str) -> String {
    let Ok(raw) = git(dir, &["diff", "--shortstat", base]) else {
        return String::new();
    };
    compact_shortstat(&raw)
}

pub fn compact_shortstat(raw: &str) -> String {
    let mut files = 0;
    let mut plus = 0;
    let mut minus = 0;
    for part in raw.trim().split(',') {
        let part = part.trim();
        let n: u32 = part
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        if part.contains("file") {
            files = n;
        } else if part.contains("insertion") {
            plus = n;
        } else if part.contains("deletion") {
            minus = n;
        }
    }
    if files == 0 {
        String::new()
    } else {
        format!("{files} file{} +{plus} -{minus}", if files == 1 { "" } else { "s" })
    }
}

/// Commit whatever is uncommitted. Returns whether there was anything.
pub fn commit_all(dir: &Path, message: &str) -> Result<bool> {
    git(dir, &["add", "--all"])?;
    if git(dir, &["diff", "--cached", "--quiet"]).is_ok() {
        return Ok(false);
    }
    git(
        dir,
        &[
            "-c",
            "user.name=hive",
            "-c",
            "user.email=hive@localhost",
            "commit",
            "--no-verify",
            "-m",
            message,
        ],
    )?;
    Ok(true)
}

pub fn current_branch(repo: &Path) -> String {
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "HEAD".into())
}

/// Merge `branch` into whatever `repo` has checked out. A conflicted merge is
/// aborted so the checkout is left exactly as it was.
pub fn merge(repo: &Path, branch: &str, message: &str) -> Result<()> {
    if let Err(e) = git(repo, &["merge", "--no-ff", "--no-edit", "-m", message, branch]) {
        let _ = git(repo, &["merge", "--abort"]);
        bail!("{e} (merge aborted, nothing changed)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortstat_compacts() {
        assert_eq!(
            compact_shortstat(" 3 files changed, 40 insertions(+), 2 deletions(-)\n"),
            "3 files +40 -2"
        );
        assert_eq!(compact_shortstat(" 1 file changed, 1 insertion(+)"), "1 file +1 -0");
        assert_eq!(compact_shortstat(""), "");
    }

    #[test]
    fn worktree_round_trip() {
        let tmp = std::env::temp_dir().join(format!("hive-git-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]).unwrap();
        std::fs::write(repo.join("a.txt"), "one\n").unwrap();
        commit_all(&repo, "init").unwrap();
        let base = head(&repo).unwrap();

        let wt = tmp.join("wt");
        add_worktree(&repo, &wt, "hive/t", &base).unwrap();
        std::fs::write(wt.join("b.txt"), "two\n").unwrap();
        track_new_files(&wt);
        assert!(diff(&wt, &base).unwrap().contains("+two"));
        assert_eq!(shortstat(&wt, &base), "1 file +1 -0");

        assert!(commit_all(&wt, "work").unwrap());
        merge(&repo, "hive/t", "merge").unwrap();
        assert!(repo.join("b.txt").exists());
        remove_worktree(&repo, &wt).unwrap();
        delete_branch(&repo, "hive/t").unwrap();
        std::fs::remove_dir_all(&tmp).ok();
    }
}
