use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use driftsync_core::{
    DriftStatus, LOCK_FILE_NAME, MANIFEST_FILE_NAME, Manifest, PlanAction, RepoReport,
    ResolutionChoice, adopt_workspace, apply_workspace, ensure_no_symlink_in_chain,
    evaluate_workspace, generate_markdown_report, generate_pr_body, has_actionable_drift,
    hash_content, resolve_target_file, validate_relative_path,
};
use driftsync_git::{
    GitCommitOutcome, checkout_or_create_branch, create_pull_request_via_gh,
    ensure_clean_working_tree, ensure_remote_cloned, head_short_sha, init_repo_with_commit,
    is_git_repo, push_branch, stage_and_commit, validate_branch_name,
};
use owo_colors::OwoColorize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ResolveMode {
    Interactive,
    Ours,
    Theirs,
}

#[derive(Debug, Parser)]
#[command(
    name = "driftsync",
    version,
    about = "Multi-repository 3-way structural configuration drift & sync engine"
)]
struct Cli {
    /// Path to the workspace root containing `drift.toml`
    #[arg(short, long, global = true, default_value = ".")]
    root: PathBuf,

    /// Emit machine-readable JSON output
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Initialize a starter `drift.toml` manifest and `templates/` directory
    Init {
        /// Scaffold a ready-to-run multi-repo demo workspace (3 target services)
        #[arg(long)]
        demo: bool,
    },
    /// Classify and report configuration drift across all target repositories
    Status {
        /// Exit with code 1 if any target repository has actionable drift or conflicts
        #[arg(long)]
        fail_on_drift: bool,

        /// Render output as a GitHub Flavored Markdown table
        #[arg(long)]
        markdown: bool,

        /// Append the Markdown report to `$GITHUB_STEP_SUMMARY`
        #[arg(long)]
        step_summary: bool,
    },
    /// CI/CD audit check: exits non-zero when actionable drift or conflicts exist
    Check {
        /// Render output as a GitHub Flavored Markdown table
        #[arg(long)]
        markdown: bool,
    },
    /// Show unified diffs between target files and proposed 3-way merge outputs
    Diff,
    /// Adopt existing target repositories without overwriting local files
    Adopt,
    /// Resolve conflicting (`BothChangedConflict`) or untracked (`NeedsAdopt`) files
    Resolve {
        /// Resolution policy (`interactive`, `ours`, or `theirs`)
        #[arg(long, value_enum, default_value_t = ResolveMode::Interactive)]
        strategy: ResolveMode,

        /// Filter by target repository name
        #[arg(long)]
        target: Option<String>,

        /// Filter by target relative file path
        #[arg(long)]
        file: Option<String>,

        /// Write resolved files and `.driftsync.lock` updates to disk
        #[arg(long)]
        write: bool,
    },
    /// Apply planned updates and 3-way structural merges (dry-run by default)
    Apply {
        /// Write changes and `.driftsync.lock` updates to disk
        #[arg(long)]
        write: bool,

        /// Create or switch to a Git branch and commit managed changes in each modified repo
        #[arg(long)]
        branch: Option<String>,

        /// Commit message used with `--branch`
        #[arg(long, default_value = "chore: sync shared configuration via driftsync")]
        message: String,

        /// Proceed even if a target repository has uncommitted changes
        #[arg(long)]
        allow_dirty: bool,

        /// Push the sync branch to `origin` (`git push -u origin <branch>`)
        #[arg(long)]
        push: bool,

        /// Open a Pull Request via `gh pr create`
        #[arg(long)]
        pr: bool,
    },
    /// Sync target repositories onto a branch and open a Pull Request
    Pr {
        /// Sync branch name
        #[arg(long, default_value = "chore/config-sync")]
        branch: String,

        /// Commit and Pull Request title
        #[arg(long, default_value = "chore: sync shared configuration via driftsync")]
        title: String,

        /// Write changes and create commits on disk
        #[arg(long)]
        write: bool,

        /// Push the sync branch to `origin`
        #[arg(long)]
        push: bool,

        /// Open a Pull Request on GitHub via `gh pr create`
        #[arg(long)]
        open_gh: bool,

        /// Proceed even if a target repository has uncommitted changes
        #[arg(long)]
        allow_dirty: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { demo } => cmd_init(&cli.root, demo),
        Commands::Status {
            fail_on_drift,
            markdown,
            step_summary,
        } => cmd_status(&cli.root, cli.json, fail_on_drift, markdown, step_summary),
        Commands::Check { markdown } => cmd_status(&cli.root, cli.json, true, markdown, true),
        Commands::Diff => cmd_diff(&cli.root, cli.json),
        Commands::Adopt => cmd_adopt(&cli.root),
        Commands::Resolve {
            strategy,
            target,
            file,
            write,
        } => cmd_resolve(
            &cli.root,
            strategy,
            target.as_deref(),
            file.as_deref(),
            write,
            cli.json,
        ),
        Commands::Apply {
            write,
            branch,
            message,
            allow_dirty,
            push,
            pr,
        } => cmd_apply(
            &cli.root,
            write,
            branch.as_deref(),
            &message,
            allow_dirty,
            push,
            pr,
            cli.json,
        ),
        Commands::Pr {
            branch,
            title,
            write,
            push,
            open_gh,
            allow_dirty,
        } => cmd_apply(
            &cli.root,
            write,
            Some(&branch),
            &title,
            allow_dirty,
            push || open_gh,
            open_gh,
            cli.json,
        ),
    }
}

fn load_manifest(root: &Path) -> Result<Manifest> {
    let manifest_path = root.join(MANIFEST_FILE_NAME);
    Manifest::load_from_path(&manifest_path)
        .with_context(|| format!("failed to read '{}'", manifest_path.display()))
}

fn prepare_remote_targets(root: &Path, manifest: &Manifest) -> Result<()> {
    for target in &manifest.targets {
        if let Some(ref repo_spec) = target.repo {
            let rel_path = validate_relative_path(&target.effective_path())?;
            let dest = ensure_no_symlink_in_chain(root, &rel_path)?;
            ensure_remote_cloned(repo_spec, &dest).with_context(|| {
                format!(
                    "failed to clone remote '{}' for target '{}'",
                    repo_spec, target.name
                )
            })?;
        }
    }
    Ok(())
}

fn maybe_write_github_step_summary(markdown_report: &str) -> Result<()> {
    if let Ok(summary_path) = std::env::var("GITHUB_STEP_SUMMARY")
        && !summary_path.trim().is_empty()
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&summary_path)
            .with_context(|| format!("failed to open GITHUB_STEP_SUMMARY ('{summary_path}')"))?;
        writeln!(file, "{markdown_report}")?;
    }
    Ok(())
}

fn format_status_badge(status: DriftStatus) -> String {
    match status {
        DriftStatus::InSync => format!("{}", "IN-SYNC".green().bold()),
        DriftStatus::MissingInTarget => format!("{}", "MISSING".cyan().bold()),
        DriftStatus::UntrackedIdentical => format!("{}", "UNTRACKED-SAME".blue().bold()),
        DriftStatus::UntrackedDifferent => format!("{}", "NEEDS-ADOPT".yellow().bold()),
        DriftStatus::UpstreamChanged => format!("{}", "UPSTREAM-CHANGED".magenta().bold()),
        DriftStatus::LocallyModified => format!("{}", "LOCALLY-MODIFIED (KEPT)".green()),
        DriftStatus::BothChangedClean => format!("{}", "3-WAY-MERGE (CLEAN)".bright_cyan().bold()),
        DriftStatus::BothChangedConflict => format!("{}", "CONFLICT".red().bold()),
    }
}

fn format_action_label(plan: &PlanAction) -> &'static str {
    match plan {
        PlanAction::NoOp => "no changes",
        PlanAction::Create { .. } => "create file",
        PlanAction::RecordLock { .. } => "record lock",
        PlanAction::NeedsAdopt => "skipped (run `driftsync adopt`)",
        PlanAction::UpdateFastForward { .. } => "fast-forward update",
        PlanAction::KeepLocal => "keep local customization",
        PlanAction::MergeClean { .. } => "3-way structural merge",
        PlanAction::OverwriteWithUpstream { .. } => "overwrite with upstream",
        PlanAction::Conflict { .. } => "conflict (run `driftsync resolve`)",
    }
}

fn print_reports_table(reports: &[RepoReport]) {
    for repo in reports {
        println!(
            "\n{} {} ({})",
            "●".bold(),
            repo.target_name.bold().underline(),
            repo.target_path.dimmed()
        );
        for eval in &repo.evaluations {
            println!(
                "  ├─ {:<28} {:<30} → {}",
                eval.target_path,
                format_status_badge(eval.status),
                format_action_label(&eval.plan)
            );
        }
    }
}

fn cmd_status(
    root: &Path,
    as_json: bool,
    fail_on_drift: bool,
    as_markdown: bool,
    write_step_summary: bool,
) -> Result<()> {
    let manifest = load_manifest(root)?;
    prepare_remote_targets(root, &manifest)?;
    let reports = evaluate_workspace(root, &manifest)?;

    let md_report = generate_markdown_report(&reports);
    if write_step_summary {
        maybe_write_github_step_summary(&md_report)?;
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
    } else if as_markdown {
        print!("{md_report}");
    } else {
        print_reports_table(&reports);
    }

    if fail_on_drift && has_actionable_drift(&reports) {
        anyhow::bail!("actionable configuration drift or conflicts detected (`--fail-on-drift`)");
    }
    Ok(())
}

fn cmd_diff(root: &Path, as_json: bool) -> Result<()> {
    let manifest = load_manifest(root)?;
    prepare_remote_targets(root, &manifest)?;
    let reports = evaluate_workspace(root, &manifest)?;
    if as_json {
        println!("{}", serde_json::to_string_pretty(&reports)?);
        return Ok(());
    }

    let mut any_diff = false;
    for repo in &reports {
        for eval in &repo.evaluations {
            if !eval.unified_diff.is_empty() {
                any_diff = true;
                println!(
                    "\n{} {} :: {} [{}]",
                    "===".bold(),
                    repo.target_name.bold(),
                    eval.target_path.bold(),
                    format_status_badge(eval.status)
                );
                for line in eval.unified_diff.lines() {
                    if line.starts_with('+') && !line.starts_with("+++") {
                        println!("{}", line.green());
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        println!("{}", line.red());
                    } else if line.starts_with("@@") {
                        println!("{}", line.cyan());
                    } else {
                        println!("{line}");
                    }
                }
            }
        }
    }
    if !any_diff {
        println!(
            "{}",
            "All managed files are in sync or locally preserved; no diffs to display.".green()
        );
    }
    Ok(())
}

fn cmd_adopt(root: &Path) -> Result<()> {
    let manifest = load_manifest(root)?;
    prepare_remote_targets(root, &manifest)?;
    let rev = head_short_sha(root);
    let count = adopt_workspace(root, &manifest, rev)?;
    println!(
        "{} Updated `{}` across {} target repositories.",
        "✓".green().bold(),
        LOCK_FILE_NAME,
        count
    );
    Ok(())
}

fn cmd_resolve(
    root: &Path,
    strategy: ResolveMode,
    target_filter: Option<&str>,
    file_filter: Option<&str>,
    write: bool,
    as_json: bool,
) -> Result<()> {
    let manifest = load_manifest(root)?;
    prepare_remote_targets(root, &manifest)?;
    let reports = evaluate_workspace(root, &manifest)?;
    let rev = head_short_sha(root);

    let mut resolved_records = Vec::new();
    for (target, report) in manifest.targets.iter().zip(reports.iter()) {
        if let Some(tf) = target_filter
            && target.name != tf
        {
            continue;
        }
        for (managed_file, eval) in manifest.files.iter().zip(report.evaluations.iter()) {
            if let Some(ff) = file_filter
                && eval.target_path != ff
            {
                continue;
            }
            if !matches!(
                eval.status,
                DriftStatus::BothChangedConflict | DriftStatus::UntrackedDifferent
            ) {
                continue;
            }

            let choice = match strategy {
                ResolveMode::Ours => Some(ResolutionChoice::Ours),
                ResolveMode::Theirs => Some(ResolutionChoice::Theirs),
                ResolveMode::Interactive => {
                    println!(
                        "\n{} {} :: {} [{}]",
                        "⚡".yellow().bold(),
                        target.name.bold(),
                        eval.target_path.bold(),
                        format_status_badge(eval.status)
                    );
                    if !eval.unified_diff.is_empty() {
                        println!("{}", eval.unified_diff);
                    }
                    print!("Choose resolution: [o]urs / [t]heirs / [s]kip: ");
                    io::stdout().flush()?;
                    let mut input = String::new();
                    io::stdin().read_line(&mut input)?;
                    match input.trim().to_ascii_lowercase().as_str() {
                        "o" | "ours" => Some(ResolutionChoice::Ours),
                        "t" | "theirs" => Some(ResolutionChoice::Theirs),
                        _ => None,
                    }
                }
            };

            if let Some(c) = choice {
                let record = resolve_target_file(
                    root,
                    target,
                    managed_file,
                    &manifest.vars,
                    rev.as_deref(),
                    c,
                    write,
                )?;
                resolved_records.push(record);
            }
        }
    }

    if as_json {
        println!("{}", serde_json::to_string_pretty(&resolved_records)?);
        return Ok(());
    }

    if resolved_records.is_empty() {
        println!(
            "{}",
            "No conflicts or untracked files require resolution.".green()
        );
    } else if write {
        println!(
            "{} Resolved {} file(s) and updated `{}`.",
            "✓".green().bold(),
            resolved_records.len(),
            LOCK_FILE_NAME
        );
    } else {
        println!(
            "{} [DRY-RUN] {} file(s) would be resolved. Pass `--write` to persist changes.",
            "ℹ".cyan().bold(),
            resolved_records.len()
        );
    }
    Ok(())
}

fn planned_staged_paths(report: &RepoReport) -> Result<Vec<String>> {
    let mut staged = Vec::new();
    let mut any_lock_update = false;

    for eval in &report.evaluations {
        match &eval.plan {
            PlanAction::Create { .. }
            | PlanAction::UpdateFastForward { .. }
            | PlanAction::MergeClean { .. }
            | PlanAction::OverwriteWithUpstream { .. } => {
                validate_relative_path(&eval.target_path)?;
                staged.push(eval.target_path.clone());
                any_lock_update = true;
            }
            PlanAction::RecordLock { .. } => {
                any_lock_update = true;
            }
            PlanAction::NoOp
            | PlanAction::KeepLocal
            | PlanAction::NeedsAdopt
            | PlanAction::Conflict { .. } => {}
        }
    }

    if any_lock_update {
        staged.push(LOCK_FILE_NAME.to_string());
    }
    Ok(staged)
}

#[allow(clippy::too_many_arguments)]
fn cmd_apply(
    root: &Path,
    write: bool,
    branch: Option<&str>,
    message: &str,
    allow_dirty: bool,
    push: bool,
    open_pr: bool,
    as_json: bool,
) -> Result<()> {
    let validated_branch = match branch {
        Some(b) => Some(validate_branch_name(b)?),
        None => None,
    };

    let manifest = load_manifest(root)?;
    prepare_remote_targets(root, &manifest)?;
    let pre_reports = evaluate_workspace(root, &manifest)?;

    let mut branch_created_map = std::collections::BTreeMap::new();
    if write && let Some(ref branch_name) = validated_branch {
        for (target, report) in manifest.targets.iter().zip(pre_reports.iter()) {
            let staged_paths = planned_staged_paths(report)?;
            if staged_paths.is_empty() {
                continue;
            }
            let rel_path = validate_relative_path(&target.effective_path())?;
            let repo_root = ensure_no_symlink_in_chain(root, &rel_path)?;
            if allow_dirty {
                if !is_git_repo(&repo_root) {
                    anyhow::bail!("'{}' is not a valid Git repository", repo_root.display());
                }
            } else {
                ensure_clean_working_tree(&repo_root)?;
            }
        }

        for (target, report) in manifest.targets.iter().zip(pre_reports.iter()) {
            let staged_paths = planned_staged_paths(report)?;
            if staged_paths.is_empty() {
                continue;
            }
            let rel_path = validate_relative_path(&target.effective_path())?;
            let repo_root = ensure_no_symlink_in_chain(root, &rel_path)?;
            let created = checkout_or_create_branch(&repo_root, branch_name)?;
            branch_created_map.insert(target.name.clone(), created);
        }
    }

    let rev = head_short_sha(root);
    let (reports, summary) = apply_workspace(root, &manifest, rev.clone(), write)?;

    let mut git_outcomes: Vec<GitCommitOutcome> = Vec::new();
    if let Some(ref branch_name) = validated_branch {
        for (target, report) in manifest.targets.iter().zip(reports.iter()) {
            let staged_paths = planned_staged_paths(report)?;
            if staged_paths.is_empty() {
                continue;
            }
            let effective_path = target.effective_path();
            if write {
                let rel_path = validate_relative_path(&effective_path)?;
                let repo_root = ensure_no_symlink_in_chain(root, &rel_path)?;
                let commit_sha = stage_and_commit(&repo_root, &staged_paths, message)?;
                let branch_created = branch_created_map
                    .get(&target.name)
                    .copied()
                    .unwrap_or(false);

                let mut pushed = false;
                let mut pr_url = None;
                if commit_sha.is_some() && (push || open_pr) {
                    push_branch(&repo_root, "origin", branch_name)?;
                    pushed = true;
                }
                if commit_sha.is_some() && open_pr {
                    let pr_body = generate_pr_body(report, rev.as_deref());
                    let url =
                        create_pull_request_via_gh(&repo_root, branch_name, message, &pr_body)?;
                    pr_url = Some(url);
                }

                git_outcomes.push(GitCommitOutcome {
                    target_name: target.name.clone(),
                    target_path: effective_path,
                    branch: branch_name.clone(),
                    branch_created,
                    commit_sha,
                    pushed,
                    pr_url,
                });
            } else {
                git_outcomes.push(GitCommitOutcome {
                    target_name: target.name.clone(),
                    target_path: effective_path,
                    branch: branch_name.clone(),
                    branch_created: true,
                    commit_sha: None,
                    pushed: false,
                    pr_url: None,
                });
            }
        }
    }

    if as_json {
        let payload = serde_json::json!({
            "summary": summary,
            "reports": reports,
            "git": git_outcomes,
        });
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    print_reports_table(&reports);
    println!();
    if summary.dry_run {
        println!(
            "{} [DRY-RUN] {} file(s) to write, {} lockfile(s) to update, {} conflict(s) skipped, {} untracked file(s) skipped.",
            "ℹ".cyan().bold(),
            summary.files_written,
            summary.locks_updated,
            summary.conflicts_skipped,
            summary.needs_adopt_skipped
        );
        if let Some(ref branch_name) = validated_branch {
            println!(
                "  {} [DRY-RUN] Branch `{}` would be checked out and committed across {} target repo(s).",
                "⎇".magenta().bold(),
                branch_name.bold(),
                git_outcomes.len()
            );
        }
        println!(
            "  Run `{}` to write changes to disk.",
            "driftsync apply --write".bold()
        );
    } else {
        println!(
            "{} Updated {} file(s) and recorded `{}` across {} target repo(s).",
            "✓".green().bold(),
            summary.files_written,
            LOCK_FILE_NAME,
            summary.locks_updated
        );
        for outcome in &git_outcomes {
            let sha_label = outcome.commit_sha.as_deref().unwrap_or("no-op");
            let push_badge = if outcome.pushed { " [pushed]" } else { "" };
            let pr_badge = outcome
                .pr_url
                .as_deref()
                .map(|u| format!(" [PR: {u}]"))
                .unwrap_or_default();
            println!(
                "  {} {} → branch `{}` ({}) [commit: {}]{}{}",
                "⎇".magenta().bold(),
                outcome.target_name.bold(),
                outcome.branch.cyan(),
                if outcome.branch_created {
                    "created"
                } else {
                    "existing"
                },
                sha_label.yellow(),
                push_badge.green(),
                pr_badge.bright_blue()
            );
        }
    }
    Ok(())
}

fn cmd_init(root: &Path, demo: bool) -> Result<()> {
    fs::create_dir_all(root)?;
    let manifest_path = root.join(MANIFEST_FILE_NAME);
    if manifest_path.exists() {
        anyhow::bail!("'{}' already exists", manifest_path.display());
    }

    let templates_dir = root.join("templates");
    fs::create_dir_all(&templates_dir)?;

    let base_editorconfig = "root = true\n\n[*]\nindent_style = space\nindent_size = 4\nend_of_line = lf\ncharset = utf-8\ntrim_trailing_whitespace = true\n";
    let updated_editorconfig = "root = true\n\n[*]\nindent_style = space\nindent_size = 4\nend_of_line = lf\ncharset = utf-8\ntrim_trailing_whitespace = true\ninsert_final_newline = true\n";

    let base_ci = "name: CI\non: [push, pull_request]\njobs:\n  test:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test --locked\n";
    let updated_ci = "name: CI\non: [push, pull_request]\njobs:\n  test:\n    runs-on: ubuntu-24.04\n    timeout-minutes: 20\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test --locked\n";

    fs::write(templates_dir.join(".editorconfig"), updated_editorconfig)?;
    fs::write(templates_dir.join("ci.yml"), updated_ci)?;

    if !demo {
        let starter_manifest = r#"version = 1

# [[targets]]
# name = "example-service"
# path = "../example-service"

[[files]]
source = "templates/.editorconfig"
target = ".editorconfig"
strategy = "three-way"

[[files]]
source = "templates/ci.yml"
target = ".github/workflows/ci.yml"
strategy = "three-way"
"#;
        fs::write(&manifest_path, starter_manifest)?;
        println!(
            "{} Created '{}' and 'templates/' directory.",
            "✓".green().bold(),
            manifest_path.display()
        );
        return Ok(());
    }

    let demo_manifest = r#"version = 1

[[targets]]
name = "service-auth"
path = "repos/service-auth"

[[targets]]
name = "service-billing"
path = "repos/service-billing"

[[targets]]
name = "service-search"
path = "repos/service-search"

[[files]]
source = "templates/.editorconfig"
target = ".editorconfig"
strategy = "three-way"

[[files]]
source = "templates/ci.yml"
target = ".github/workflows/ci.yml"
strategy = "three-way"
"#;
    fs::write(&manifest_path, demo_manifest)?;

    let auth_dir = root.join("repos/service-auth");
    fs::create_dir_all(auth_dir.join(".github/workflows"))?;
    fs::write(auth_dir.join(".editorconfig"), base_editorconfig)?;
    fs::write(auth_dir.join(".github/workflows/ci.yml"), base_ci)?;
    let mut auth_lock = driftsync_core::LockFile::default();
    auth_lock.files.insert(
        ".editorconfig".to_string(),
        driftsync_core::LockedFileEntry {
            base_hash: hash_content(base_editorconfig),
            base_content: base_editorconfig.to_string(),
        },
    );
    auth_lock.files.insert(
        ".github/workflows/ci.yml".to_string(),
        driftsync_core::LockedFileEntry {
            base_hash: hash_content(base_ci),
            base_content: base_ci.to_string(),
        },
    );
    auth_lock.save(&auth_dir)?;

    let billing_dir = root.join("repos/service-billing");
    fs::create_dir_all(billing_dir.join(".github/workflows"))?;
    let billing_custom_ci = "name: CI\non: [push, pull_request]\njobs:\n  test:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test --locked\n      - run: ./scripts/verify-pci-compliance.sh\n";
    fs::write(billing_dir.join(".editorconfig"), updated_editorconfig)?;
    fs::write(
        billing_dir.join(".github/workflows/ci.yml"),
        billing_custom_ci,
    )?;
    let mut billing_lock = driftsync_core::LockFile::default();
    billing_lock.files.insert(
        ".editorconfig".to_string(),
        driftsync_core::LockedFileEntry {
            base_hash: hash_content(updated_editorconfig),
            base_content: updated_editorconfig.to_string(),
        },
    );
    billing_lock.files.insert(
        ".github/workflows/ci.yml".to_string(),
        driftsync_core::LockedFileEntry {
            base_hash: hash_content(base_ci),
            base_content: base_ci.to_string(),
        },
    );
    billing_lock.save(&billing_dir)?;

    let search_dir = root.join("repos/service-search");
    fs::create_dir_all(&search_dir)?;
    let search_custom_editorconfig =
        format!("{updated_editorconfig}\n[*.proto]\nindent_size = 2\n");
    fs::write(search_dir.join(".editorconfig"), search_custom_editorconfig)?;
    let mut search_lock = driftsync_core::LockFile::default();
    search_lock.files.insert(
        ".editorconfig".to_string(),
        driftsync_core::LockedFileEntry {
            base_hash: hash_content(updated_editorconfig),
            base_content: updated_editorconfig.to_string(),
        },
    );
    search_lock.save(&search_dir)?;

    let _ = init_repo_with_commit(&auth_dir, "chore: initial service-auth");
    let _ = init_repo_with_commit(&billing_dir, "chore: initial service-billing");
    let _ = init_repo_with_commit(&search_dir, "chore: initial service-search");

    println!(
        "{} Initialized demo workspace in '{}' (3 target repositories).",
        "✓".green().bold(),
        root.display()
    );
    println!("  Try: `driftsync status` or `driftsync diff`");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use driftsync_git::current_branch;

    #[test]
    fn apply_with_branch_dry_run_does_not_create_branch_or_write_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        cmd_init(root, true).expect("init demo");
        let auth_dir = root.join("repos/service-auth");
        let initial_branch = current_branch(&auth_dir).expect("initial branch");
        let initial_sha = head_short_sha(&auth_dir).expect("initial sha");

        cmd_apply(
            root,
            false,
            Some("chore/config-sync"),
            "chore: sync config",
            false,
            false,
            false,
            false,
        )
        .expect("dry run apply");

        assert_eq!(
            current_branch(&auth_dir).as_deref(),
            Some(initial_branch.as_str())
        );
        assert_eq!(
            head_short_sha(&auth_dir).as_deref(),
            Some(initial_sha.as_str())
        );
    }

    #[test]
    fn apply_with_write_and_branch_creates_branch_and_commits_across_repos() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        cmd_init(root, true).expect("init demo");
        let auth_dir = root.join("repos/service-auth");
        let billing_dir = root.join("repos/service-billing");
        let search_dir = root.join("repos/service-search");

        let auth_before_sha = head_short_sha(&auth_dir).expect("auth sha");
        let billing_before_sha = head_short_sha(&billing_dir).expect("billing sha");
        let search_before_sha = head_short_sha(&search_dir).expect("search sha");

        assert!(cmd_status(root, false, true, false, false).is_err());

        cmd_apply(
            root,
            true,
            Some("chore/config-sync"),
            "chore: sync shared configuration",
            false,
            false,
            false,
            false,
        )
        .expect("write apply with branch");

        for (dir, before_sha) in [
            (&auth_dir, &auth_before_sha),
            (&billing_dir, &billing_before_sha),
            (&search_dir, &search_before_sha),
        ] {
            assert_eq!(current_branch(dir).as_deref(), Some("chore/config-sync"));
            let after_sha = head_short_sha(dir).expect("after sha");
            assert_ne!(&after_sha, before_sha);
            ensure_clean_working_tree(dir).expect("working tree must be clean after commit");
        }

        cmd_status(root, false, true, true, false).expect("clean check after apply");
    }

    #[test]
    fn resolve_command_handles_conflicting_files_with_policy() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        cmd_init(root, true).expect("init demo");
        let auth_dir = root.join("repos/service-auth");
        fs::write(
            auth_dir.join(".editorconfig"),
            "root = true\n\n[*]\nindent_style = tab\nindent_size = 8\nend_of_line = lf\ncharset = utf-8\ntrim_trailing_whitespace = false\n",
        )
        .expect("write local conflict");
        fs::write(
            root.join("templates/.editorconfig"),
            "root = true\n\n[*]\nindent_style = space\nindent_size = 2\nend_of_line = lf\ncharset = utf-8\ntrim_trailing_whitespace = true\n",
        )
        .expect("write upstream conflict");

        cmd_resolve(
            root,
            ResolveMode::Ours,
            Some("service-auth"),
            Some(".editorconfig"),
            true,
            false,
        )
        .expect("resolve ours");

        let content_after = fs::read_to_string(auth_dir.join(".editorconfig")).expect("read");
        assert!(content_after.contains("indent_size = 8"));
    }

    #[test]
    fn status_and_apply_block_accidental_hardcoded_secret_in_templates() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        cmd_init(root, true).expect("init demo");
        let ghp = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
        fs::write(
            root.join("templates/.editorconfig"),
            format!("root = true\n# leaked token: {ghp}\n"),
        )
        .expect("inject secret");

        let err = cmd_status(root, false, false, false, false)
            .expect_err("must block hardcoded secret in template");
        let msg = format!("{err:#}");
        assert!(msg.contains("hardcoded secret detected"));
    }
}
