use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("invalid or unsafe Git branch name: '{0}'")]
    InvalidBranchName(String),
    #[error("invalid or unsafe remote repository specification: '{0}'")]
    InvalidRemoteSpec(String),
    #[error("'{0}' is not a valid Git repository")]
    NotAGitRepo(PathBuf),
    #[error(
        "target repository has uncommitted changes ({repo}):\n{status_summary}\n(pass `--allow-dirty` to proceed anyway)"
    )]
    DirtyWorkingTree {
        repo: PathBuf,
        status_summary: String,
    },
    #[error("Git command failed ({repo}, `{command}`): {stderr}")]
    CommandFailed {
        repo: PathBuf,
        command: String,
        stderr: String,
    },
    #[error("Git execution error ({repo}): {source}")]
    Io {
        repo: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitOutcome {
    pub target_name: String,
    pub target_path: String,
    pub branch: String,
    pub branch_created: bool,
    pub commit_sha: Option<String>,
    #[serde(default)]
    pub pushed: bool,
    #[serde(default)]
    pub pr_url: Option<String>,
}

pub fn validate_remote_repo_spec(raw: &str) -> Result<String, GitError> {
    let trimmed = raw.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.is_empty()
        || trimmed.starts_with('-')
        || lower.starts_with("ext::")
        || lower.starts_with("http://")
        || lower.starts_with("file://")
        || trimmed
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace())
    {
        return Err(GitError::InvalidRemoteSpec(redact_git_text(raw)));
    }

    for scheme in ["https://", "ssh://"] {
        if let Some(after_scheme) = lower.strip_prefix(scheme) {
            let authority_end = after_scheme.find('/').unwrap_or(after_scheme.len());
            let authority = &after_scheme[..authority_end];
            if let Some(at_idx) = authority.find('@') {
                let userinfo = &authority[..at_idx];
                if scheme == "https://" || userinfo.contains(':') || userinfo != "git" {
                    return Err(GitError::InvalidRemoteSpec(redact_git_text(raw)));
                }
            }
        }
    }

    if Path::new(trimmed).exists() {
        return Ok(trimmed.to_string());
    }

    let parts: Vec<&str> = trimmed.split('/').collect();
    if parts.len() == 2
        && !trimmed.contains(':')
        && !trimmed.starts_with('.')
        && parts.iter().all(|p| {
            !p.is_empty()
                && p != &"."
                && p != &".."
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    {
        let repo_part = parts[1].strip_suffix(".git").unwrap_or(parts[1]);
        return Ok(format!("https://github.com/{}/{}.git", parts[0], repo_part));
    }

    Ok(trimmed.to_string())
}

pub fn validate_branch_name(raw: &str) -> Result<String, GitError> {
    let name = raw.trim();
    if name.is_empty()
        || name == "@"
        || name.starts_with('-')
        || name.starts_with('/')
        || name.starts_with('.')
        || name.ends_with('/')
        || name.ends_with('.')
        || name.ends_with(".lock")
        || name.contains("..")
        || name.contains("//")
        || name.contains("@{")
        || name.contains('\\')
    {
        return Err(GitError::InvalidBranchName(raw.to_string()));
    }

    for ch in name.chars() {
        if ch.is_ascii_control()
            || ch.is_whitespace()
            || matches!(ch, '~' | '^' | ':' | '?' | '*' | '[')
        {
            return Err(GitError::InvalidBranchName(raw.to_string()));
        }
    }

    for part in name.split('/') {
        if part.is_empty() || part.starts_with('.') || part.ends_with(".lock") {
            return Err(GitError::InvalidBranchName(raw.to_string()));
        }
    }

    Ok(name.to_string())
}

pub fn head_short_sha(repo_dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}

pub fn current_branch(repo_dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(repo_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if branch.is_empty() {
        None
    } else {
        Some(branch)
    }
}

pub fn is_git_repo(repo_dir: &Path) -> bool {
    if !repo_dir.exists() {
        return false;
    }
    Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(repo_dir)
        .output()
        .is_ok_and(|out| out.status.success())
}

pub fn ensure_clean_working_tree(repo_dir: &Path) -> Result<(), GitError> {
    if !is_git_repo(repo_dir) {
        return Err(GitError::NotAGitRepo(repo_dir.to_path_buf()));
    }
    let output = run_git(repo_dir, &["status", "--porcelain"])?;
    let status_text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !status_text.is_empty() {
        return Err(GitError::DirtyWorkingTree {
            repo: repo_dir.to_path_buf(),
            status_summary: status_text,
        });
    }
    Ok(())
}

pub fn checkout_or_create_branch(repo_dir: &Path, branch: &str) -> Result<bool, GitError> {
    let valid_branch = validate_branch_name(branch)?;
    if !is_git_repo(repo_dir) {
        return Err(GitError::NotAGitRepo(repo_dir.to_path_buf()));
    }

    let ref_name = format!("refs/heads/{valid_branch}");
    let branch_exists = Command::new("git")
        .args(["show-ref", "--verify", "--quiet", &ref_name])
        .current_dir(repo_dir)
        .output()
        .is_ok_and(|out| out.status.success());

    if branch_exists {
        run_git(repo_dir, &["checkout", &valid_branch])?;
        Ok(false)
    } else {
        run_git(repo_dir, &["checkout", "-b", &valid_branch])?;
        Ok(true)
    }
}

pub fn stage_and_commit(
    repo_dir: &Path,
    relative_paths: &[String],
    message: &str,
) -> Result<Option<String>, GitError> {
    if relative_paths.is_empty() {
        return Ok(None);
    }
    if !is_git_repo(repo_dir) {
        return Err(GitError::NotAGitRepo(repo_dir.to_path_buf()));
    }

    let mut add_args: Vec<&str> = vec!["add", "--"];
    for p in relative_paths {
        add_args.push(p.as_str());
    }
    run_git(repo_dir, &add_args)?;

    let diff_out = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(repo_dir)
        .output()
        .map_err(|source| GitError::Io {
            repo: repo_dir.to_path_buf(),
            source,
        })?;

    if diff_out.status.success() {
        return Ok(None);
    }

    let has_user_name = Command::new("git")
        .args(["config", "user.name"])
        .current_dir(repo_dir)
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());

    if has_user_name {
        run_git(repo_dir, &["commit", "--no-verify", "-m", message])?;
    } else {
        run_git(
            repo_dir,
            &[
                "-c",
                "user.name=driftsync",
                "-c",
                "user.email=driftsync@localhost",
                "commit",
                "--no-verify",
                "-m",
                message,
            ],
        )?;
    }

    Ok(head_short_sha(repo_dir))
}

pub fn init_repo_with_commit(repo_dir: &Path, message: &str) -> Result<String, GitError> {
    run_git(repo_dir, &["init"])?;
    run_git(repo_dir, &["add", "-A"])?;
    run_git(
        repo_dir,
        &[
            "-c",
            "user.name=driftsync",
            "-c",
            "user.email=driftsync@localhost",
            "commit",
            "--no-verify",
            "--allow-empty",
            "-m",
            message,
        ],
    )?;
    Ok(head_short_sha(repo_dir).unwrap_or_default())
}

pub fn ensure_remote_cloned(repo_spec: &str, dest_dir: &Path) -> Result<bool, GitError> {
    let url = validate_remote_repo_spec(repo_spec)?;
    if dest_dir.exists() && is_git_repo(dest_dir) {
        return Ok(false);
    }
    if let Some(parent) = dest_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|source| GitError::Io {
            repo: parent.to_path_buf(),
            source,
        })?;
    }
    let dest_str = dest_dir.to_string_lossy().to_string();
    let work_dir = dest_dir.parent().unwrap_or_else(|| Path::new("."));
    run_git(work_dir, &["clone", "--depth", "1", "--", &url, &dest_str])?;
    Ok(true)
}

pub fn push_branch(repo_dir: &Path, remote: &str, branch: &str) -> Result<(), GitError> {
    let valid_remote = validate_branch_name(remote)?;
    let valid_branch = validate_branch_name(branch)?;
    if !is_git_repo(repo_dir) {
        return Err(GitError::NotAGitRepo(repo_dir.to_path_buf()));
    }
    run_git(repo_dir, &["push", "-u", &valid_remote, &valid_branch])?;
    Ok(())
}

pub fn create_pull_request_via_gh(
    repo_dir: &Path,
    branch: &str,
    title: &str,
    body: &str,
) -> Result<String, GitError> {
    let valid_branch = validate_branch_name(branch)?;
    let output = Command::new("gh")
        .env("GH_PROMPT_DISABLED", "1")
        .args([
            "pr",
            "create",
            "--head",
            &valid_branch,
            "--title",
            title,
            "--body",
            body,
        ])
        .current_dir(repo_dir)
        .output()
        .map_err(|source| GitError::Io {
            repo: repo_dir.to_path_buf(),
            source,
        })?;

    if !output.status.success() {
        return Err(GitError::CommandFailed {
            repo: repo_dir.to_path_buf(),
            command: "gh pr create".to_string(),
            stderr: redact_git_text(String::from_utf8_lossy(&output.stderr).trim()),
        });
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_git(repo_dir: &Path, args: &[&str]) -> Result<std::process::Output, GitError> {
    let mut full_args = Vec::with_capacity(args.len() + 4);
    full_args.extend_from_slice(&[
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "core.fsmonitor=false",
    ]);
    full_args.extend_from_slice(args);

    let output = Command::new("git")
        .env("GIT_TERMINAL_PROMPT", "0")
        .args(&full_args)
        .current_dir(repo_dir)
        .output()
        .map_err(|source| GitError::Io {
            repo: repo_dir.to_path_buf(),
            source,
        })?;

    if !output.status.success() {
        return Err(GitError::CommandFailed {
            repo: repo_dir.to_path_buf(),
            command: redact_git_text(&format!("git {}", args.join(" "))),
            stderr: redact_git_text(String::from_utf8_lossy(&output.stderr).trim()),
        });
    }

    Ok(output)
}

pub fn redact_git_text(input: &str) -> String {
    let mut masked = input.to_string();
    for scheme in ["https://", "http://", "ssh://"] {
        if let Some(scheme_idx) = masked.find(scheme) {
            let after_scheme_start = scheme_idx + scheme.len();
            let after_scheme = &masked[after_scheme_start..];
            let host_end = after_scheme
                .find(['/', ' ', '"', '\'', '\n', '\r'])
                .unwrap_or(after_scheme.len());
            let authority = &after_scheme[..host_end];
            if let Some(at_idx) = authority.find('@') {
                let userinfo = &authority[..at_idx];
                if scheme != "ssh://" || userinfo.contains(':') || userinfo != "git" {
                    let prefix = &masked[..after_scheme_start];
                    let suffix = &after_scheme[at_idx..];
                    masked = format!("{prefix}[REDACTED]{suffix}");
                }
            }
        }
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn validates_branch_names_strictly() {
        assert!(validate_branch_name("chore/config-sync").is_ok());
        assert!(validate_branch_name("driftsync/2026-11-15").is_ok());

        assert!(validate_branch_name("-f").is_err());
        assert!(validate_branch_name("--upload-pack=evil").is_err());
        assert!(validate_branch_name("chore/..sneaky").is_err());
        assert!(validate_branch_name("chore/branch.lock").is_err());
        assert!(validate_branch_name("chore/with space").is_err());
        assert!(validate_branch_name("").is_err());
        assert!(validate_branch_name("@").is_err());
    }

    #[test]
    fn branch_creation_clean_check_and_commit_flow() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path();

        fs::write(repo.join(".editorconfig"), "root = true\n").expect("write initial");
        let initial_sha = init_repo_with_commit(repo, "initial commit").expect("init repo");
        assert!(!initial_sha.is_empty());

        ensure_clean_working_tree(repo).expect("clean tree");

        let created = checkout_or_create_branch(repo, "chore/config-sync").expect("create branch");
        assert!(created);
        assert_eq!(current_branch(repo).as_deref(), Some("chore/config-sync"));

        fs::write(
            repo.join(".editorconfig"),
            "root = true\ninsert_final_newline = true\n",
        )
        .expect("update file");
        fs::write(repo.join(".driftsync.lock"), "version = 1\n").expect("write lock");

        let new_sha = stage_and_commit(
            repo,
            &[".editorconfig".to_string(), ".driftsync.lock".to_string()],
            "chore: sync config",
        )
        .expect("commit")
        .expect("sha present");

        assert_ne!(initial_sha, new_sha);
        ensure_clean_working_tree(repo).expect("clean after commit");
    }

    #[test]
    fn dirty_working_tree_is_detected() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path();

        fs::write(repo.join("README.md"), "# test\n").expect("write");
        init_repo_with_commit(repo, "init").expect("init");

        fs::write(repo.join("untracked.txt"), "dirty\n").expect("dirty");
        let err = ensure_clean_working_tree(repo).expect_err("should fail on dirty tree");
        assert!(matches!(err, GitError::DirtyWorkingTree { .. }));
    }

    #[test]
    fn validates_remote_repo_specs_and_prevents_injection_and_cleartext_protocols() {
        assert_eq!(
            validate_remote_repo_spec("org/service-auth").expect("valid slug"),
            "https://github.com/org/service-auth.git"
        );
        assert!(validate_remote_repo_spec("https://github.com/org/service-auth.git").is_ok());
        assert!(validate_remote_repo_spec("git@github.com:org/service-auth.git").is_ok());

        assert!(validate_remote_repo_spec("-u").is_err());
        assert!(validate_remote_repo_spec("--upload-pack=evil").is_err());
        assert!(validate_remote_repo_spec("ext::sh -c evil").is_err());
        assert!(validate_remote_repo_spec("http://github.com/org/repo.git").is_err());
        assert!(validate_remote_repo_spec("file:///etc/passwd").is_err());
        let err = validate_remote_repo_spec("https://admin:superSecret@github.com/org/repo.git")
            .expect_err("embedded credentials must be rejected");
        let msg = err.to_string();
        assert!(!msg.contains("superSecret"));
        assert!(msg.contains("[REDACTED]"));
        assert!(validate_remote_repo_spec("").is_err());
    }

    #[test]
    fn stage_and_commit_bypasses_untrusted_pre_commit_hooks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo = tmp.path();

        fs::write(repo.join(".editorconfig"), "root = true\n").expect("write");
        init_repo_with_commit(repo, "init").expect("init");

        let hooks_dir = repo.join(".git/hooks");
        fs::create_dir_all(&hooks_dir).expect("hooks dir");
        let hook_path = hooks_dir.join("pre-commit");
        fs::write(&hook_path, "#!/bin/sh\nexit 1\n").expect("write hook");

        fs::write(repo.join(".editorconfig"), "root = true\nindent_size = 2\n").expect("modify");
        let sha = stage_and_commit(repo, &[".editorconfig".to_string()], "chore: sync")
            .expect("commit should succeed with --no-verify");
        assert!(sha.is_some());
    }

    #[test]
    fn shallow_clone_and_push_branch_to_bare_remote() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bare_remote = tmp.path().join("origin.git");
        let seed_repo = tmp.path().join("seed");
        let cloned_repo = tmp.path().join("cloned/service-auth");

        fs::create_dir_all(&bare_remote).expect("bare dir");
        run_git(&bare_remote, &["init", "--bare"]).expect("init bare");

        fs::create_dir_all(&seed_repo).expect("seed dir");
        fs::write(seed_repo.join(".editorconfig"), "root = true\n").expect("write seed");
        init_repo_with_commit(&seed_repo, "initial").expect("commit seed");
        let bare_str = bare_remote.to_string_lossy().to_string();
        run_git(&seed_repo, &["remote", "add", "origin", &bare_str]).expect("add remote");
        let seed_branch = current_branch(&seed_repo).expect("seed branch");
        run_git(&seed_repo, &["push", "-u", "origin", &seed_branch]).expect("push seed");

        let freshly_cloned = ensure_remote_cloned(&bare_str, &cloned_repo).expect("shallow clone");
        assert!(freshly_cloned);
        assert!(cloned_repo.join(".editorconfig").exists());

        let second_clone =
            ensure_remote_cloned(&bare_str, &cloned_repo).expect("second clone check");
        assert!(!second_clone);

        checkout_or_create_branch(&cloned_repo, "chore/config-sync").expect("branch");
        fs::write(
            cloned_repo.join(".editorconfig"),
            "root = true\nindent_size = 4\n",
        )
        .expect("update");
        stage_and_commit(
            &cloned_repo,
            &[".editorconfig".to_string()],
            "chore: sync config",
        )
        .expect("commit");
        push_branch(&cloned_repo, "origin", "chore/config-sync").expect("push branch");
    }
}
