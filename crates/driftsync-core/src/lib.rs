use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub mod security;
pub mod structural_merge;
pub use security::{
    MAX_MANAGED_FILE_BYTES, ensure_no_symlink_in_chain, is_windows_reserved_device_name,
    read_bounded_file, redact_sensitive_text, scan_for_hardcoded_secrets, validate_target_name,
};
pub use structural_merge::{
    FileFormat, StructuralConflict, merge_json_structural, merge_toml_structural,
    merge_yaml_structural, three_way_merge_by_path,
};

pub const MANIFEST_FILE_NAME: &str = "drift.toml";
pub const LOCK_FILE_NAME: &str = ".driftsync.lock";

#[derive(Debug, Error)]
pub enum DriftError {
    #[error("invalid or unsafe relative path: '{0}'")]
    UnsafePath(String),
    #[error("invalid or unsafe target repository name: '{0}'")]
    InvalidTargetName(String),
    #[error("symlink traversal blocked at '{0}'")]
    SymlinkDetected(PathBuf),
    #[error("file exceeds size limit ({path}: {size_bytes} bytes > max {max_bytes} bytes)")]
    FileTooLarge {
        path: PathBuf,
        size_bytes: u64,
        max_bytes: u64,
    },
    #[error(
        "hardcoded secret detected in '{location}' ({secret_kind}); use environment variables or CI secrets instead"
    )]
    SecretDetected {
        location: String,
        secret_kind: &'static str,
    },
    #[error("I/O error ({path}): {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("TOML parse error ({path}): {source}")]
    TomlDe {
        path: PathBuf,
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error("TOML serialization error: {0}")]
    TomlSer(#[from] Box<toml::ser::Error>),
    #[error("unsupported manifest version: {0}")]
    UnsupportedVersion(u32),
    #[error("undefined template variable '{variable}' (template: {path}, target: '{target_name}')")]
    UndefinedTemplateVar {
        path: PathBuf,
        variable: String,
        target_name: String,
    },
}

pub fn validate_relative_path(raw: &str) -> Result<PathBuf, DriftError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed.starts_with('/')
        || trimmed.chars().any(|c| c.is_ascii_control())
    {
        return Err(DriftError::UnsafePath(raw.to_string()));
    }
    let path = Path::new(trimmed);
    let mut normalized = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(part) => {
                let s = part.to_string_lossy();
                if s.is_empty()
                    || s.contains(':')
                    || s.ends_with('.')
                    || s.ends_with(' ')
                    || s.eq_ignore_ascii_case(".git")
                    || s.eq_ignore_ascii_case(LOCK_FILE_NAME)
                    || is_windows_reserved_device_name(&s)
                {
                    return Err(DriftError::UnsafePath(raw.to_string()));
                }
                normalized.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(DriftError::UnsafePath(raw.to_string()));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(DriftError::UnsafePath(raw.to_string()));
    }
    Ok(normalized)
}

pub fn hash_content(content: &str) -> String {
    let normalized = normalize_newlines(content);
    let hash = blake3::hash(normalized.as_bytes());
    format!("blake3:{}", hash.to_hex())
}

pub fn normalize_newlines(input: &str) -> String {
    input.replace("\r\n", "\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SyncStrategy {
    #[default]
    ThreeWay,
    Overwrite,
    KeepLocal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub targets: Vec<TargetRepo>,
    #[serde(default)]
    pub files: Vec<ManagedFile>,
}

fn default_version() -> u32 {
    1
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TargetRepo {
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

impl TargetRepo {
    pub fn effective_path(&self) -> String {
        if !self.path.trim().is_empty() {
            self.path.clone()
        } else {
            format!(".driftsync-remotes/{}", self.name)
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedFile {
    pub source: String,
    pub target: String,
    #[serde(default)]
    pub strategy: SyncStrategy,
}

impl Manifest {
    pub fn load_from_path(path: &Path) -> Result<Self, DriftError> {
        let raw = read_bounded_file(path)?;
        let manifest: Self = toml::from_str(&raw).map_err(|source| DriftError::TomlDe {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), DriftError> {
        if self.version != 1 {
            return Err(DriftError::UnsupportedVersion(self.version));
        }
        for (k, v) in &self.vars {
            if let Some(secret_kind) = scan_for_hardcoded_secrets(v) {
                return Err(DriftError::SecretDetected {
                    location: format!("drift.toml [vars.{k}]"),
                    secret_kind,
                });
            }
        }
        for target in &self.targets {
            validate_target_name(&target.name)?;
            if !target.path.trim().is_empty() {
                validate_relative_path(&target.path)?;
            }
            for (k, v) in &target.vars {
                if let Some(secret_kind) = scan_for_hardcoded_secrets(v) {
                    return Err(DriftError::SecretDetected {
                        location: format!("drift.toml [targets({}).vars.{k}]", target.name),
                        secret_kind,
                    });
                }
            }
        }
        for file in &self.files {
            validate_relative_path(&file.source)?;
            validate_relative_path(&file.target)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub upstream_revision: Option<String>,
    #[serde(default)]
    pub files: BTreeMap<String, LockedFileEntry>,
}

impl Default for LockFile {
    fn default() -> Self {
        Self {
            version: 1,
            upstream_revision: None,
            files: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockedFileEntry {
    pub base_hash: String,
    pub base_content: String,
}

impl LockFile {
    pub fn load_optional(repo_root: &Path) -> Result<Self, DriftError> {
        let lock_path = ensure_no_symlink_in_chain(repo_root, Path::new(LOCK_FILE_NAME))?;
        if !lock_path.exists() {
            return Ok(Self::default());
        }
        let raw = read_bounded_file(&lock_path)?;
        let lock: Self = toml::from_str(&raw).map_err(|source| DriftError::TomlDe {
            path: lock_path,
            source: Box::new(source),
        })?;
        Ok(lock)
    }

    pub fn save(&self, repo_root: &Path) -> Result<(), DriftError> {
        let lock_path = ensure_no_symlink_in_chain(repo_root, Path::new(LOCK_FILE_NAME))?;
        let serialized = toml::to_string_pretty(self).map_err(Box::new)?;
        fs::write(&lock_path, serialized).map_err(|source| DriftError::Io {
            path: lock_path,
            source,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DriftStatus {
    InSync,
    MissingInTarget,
    UntrackedIdentical,
    UntrackedDifferent,
    UpstreamChanged,
    LocallyModified,
    LocallyDeleted,
    BothChangedClean,
    BothChangedConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum PlanAction {
    NoOp,
    Create {
        new_content: String,
    },
    RecordLock {
        base_content: String,
    },
    NeedsAdopt,
    UpdateFastForward {
        new_content: String,
    },
    KeepLocal,
    MergeClean {
        merged_content: String,
        new_base_content: String,
    },
    OverwriteWithUpstream {
        new_content: String,
    },
    Conflict {
        conflict_preview: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEvaluation {
    pub source_path: String,
    pub target_path: String,
    pub strategy: SyncStrategy,
    pub status: DriftStatus,
    pub plan: PlanAction,
    pub unified_diff: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoReport {
    pub target_name: String,
    pub target_path: String,
    pub evaluations: Vec<FileEvaluation>,
}

pub fn evaluate_file(
    source_path: &str,
    target_path: &str,
    strategy: SyncStrategy,
    base: Option<&str>,
    ours: Option<&str>,
    theirs: &str,
) -> FileEvaluation {
    let base_norm = base.map(normalize_newlines);
    let ours_norm = ours.map(normalize_newlines);
    let theirs_norm = normalize_newlines(theirs);

    let (status, plan, preview_after) = match (base_norm.as_deref(), ours_norm.as_deref()) {
        (None, None) => (
            DriftStatus::MissingInTarget,
            PlanAction::Create {
                new_content: theirs_norm.clone(),
            },
            theirs_norm.clone(),
        ),
        (Some(base_str), None) => {
            if base_str == theirs_norm {
                match strategy {
                    SyncStrategy::Overwrite => (
                        DriftStatus::LocallyDeleted,
                        PlanAction::OverwriteWithUpstream {
                            new_content: theirs_norm.clone(),
                        },
                        theirs_norm.clone(),
                    ),
                    SyncStrategy::ThreeWay | SyncStrategy::KeepLocal => (
                        DriftStatus::LocallyDeleted,
                        PlanAction::KeepLocal,
                        String::new(),
                    ),
                }
            } else {
                match strategy {
                    SyncStrategy::Overwrite => (
                        DriftStatus::UpstreamChanged,
                        PlanAction::OverwriteWithUpstream {
                            new_content: theirs_norm.clone(),
                        },
                        theirs_norm.clone(),
                    ),
                    SyncStrategy::KeepLocal => (
                        DriftStatus::LocallyDeleted,
                        PlanAction::KeepLocal,
                        String::new(),
                    ),
                    SyncStrategy::ThreeWay => {
                        let conflict_msg = format!(
                            "<<<<<<< ours (deleted locally)\n=======\n{}\n>>>>>>> theirs (upstream updated)\n",
                            theirs_norm
                        );
                        (
                            DriftStatus::BothChangedConflict,
                            PlanAction::Conflict {
                                conflict_preview: conflict_msg.clone(),
                            },
                            conflict_msg,
                        )
                    }
                }
            }
        }
        (None, Some(ours_str)) => {
            if ours_str == theirs_norm {
                (
                    DriftStatus::UntrackedIdentical,
                    PlanAction::RecordLock {
                        base_content: theirs_norm.clone(),
                    },
                    ours_str.to_string(),
                )
            } else {
                match strategy {
                    SyncStrategy::Overwrite => (
                        DriftStatus::UntrackedDifferent,
                        PlanAction::OverwriteWithUpstream {
                            new_content: theirs_norm.clone(),
                        },
                        theirs_norm.clone(),
                    ),
                    SyncStrategy::KeepLocal => (
                        DriftStatus::UntrackedDifferent,
                        PlanAction::KeepLocal,
                        ours_str.to_string(),
                    ),
                    SyncStrategy::ThreeWay => (
                        DriftStatus::UntrackedDifferent,
                        PlanAction::NeedsAdopt,
                        ours_str.to_string(),
                    ),
                }
            }
        }
        (Some(base_str), Some(ours_str)) => {
            if ours_str == theirs_norm && base_str == theirs_norm {
                (DriftStatus::InSync, PlanAction::NoOp, ours_str.to_string())
            } else if ours_str == theirs_norm && base_str != theirs_norm {
                (
                    DriftStatus::InSync,
                    PlanAction::RecordLock {
                        base_content: theirs_norm.clone(),
                    },
                    ours_str.to_string(),
                )
            } else if ours_str == base_str && theirs_norm != base_str {
                match strategy {
                    SyncStrategy::KeepLocal => (
                        DriftStatus::UpstreamChanged,
                        PlanAction::KeepLocal,
                        ours_str.to_string(),
                    ),
                    SyncStrategy::ThreeWay | SyncStrategy::Overwrite => (
                        DriftStatus::UpstreamChanged,
                        PlanAction::UpdateFastForward {
                            new_content: theirs_norm.clone(),
                        },
                        theirs_norm.clone(),
                    ),
                }
            } else if ours_str != base_str && theirs_norm == base_str {
                match strategy {
                    SyncStrategy::Overwrite => (
                        DriftStatus::LocallyModified,
                        PlanAction::OverwriteWithUpstream {
                            new_content: theirs_norm.clone(),
                        },
                        theirs_norm.clone(),
                    ),
                    SyncStrategy::ThreeWay | SyncStrategy::KeepLocal => (
                        DriftStatus::LocallyModified,
                        PlanAction::KeepLocal,
                        ours_str.to_string(),
                    ),
                }
            } else {
                match strategy {
                    SyncStrategy::Overwrite => (
                        DriftStatus::BothChangedClean,
                        PlanAction::OverwriteWithUpstream {
                            new_content: theirs_norm.clone(),
                        },
                        theirs_norm.clone(),
                    ),
                    SyncStrategy::KeepLocal => (
                        DriftStatus::BothChangedClean,
                        PlanAction::KeepLocal,
                        ours_str.to_string(),
                    ),
                    SyncStrategy::ThreeWay => {
                        match three_way_merge_by_path(target_path, base_str, ours_str, &theirs_norm)
                        {
                            Ok(merged) => (
                                DriftStatus::BothChangedClean,
                                PlanAction::MergeClean {
                                    merged_content: merged.clone(),
                                    new_base_content: theirs_norm.clone(),
                                },
                                merged,
                            ),
                            Err(conflict_text) => (
                                DriftStatus::BothChangedConflict,
                                PlanAction::Conflict {
                                    conflict_preview: conflict_text.clone(),
                                },
                                conflict_text,
                            ),
                        }
                    }
                }
            }
        }
    };

    let current_for_diff = ours_norm.as_deref().unwrap_or("");
    let unified_diff = if current_for_diff == preview_after {
        String::new()
    } else {
        let raw_diff = similar::TextDiff::from_lines(current_for_diff, &preview_after)
            .unified_diff()
            .context_radius(3)
            .header(
                &format!("a/{target_path} (ours)"),
                &format!("b/{target_path} (proposed)"),
            )
            .to_string();
        redact_sensitive_text(&raw_diff)
    };

    FileEvaluation {
        source_path: source_path.to_string(),
        target_path: target_path.to_string(),
        strategy,
        status,
        plan,
        unified_diff,
    }
}

pub fn render_template(
    input: &str,
    target: &TargetRepo,
    global_vars: &BTreeMap<String, String>,
    source_path: &Path,
) -> Result<String, DriftError> {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    let mut prev_ended_with_dollar = false;

    while let Some(open_idx) = rest.find("{{") {
        let before = &rest[..open_idx];
        output.push_str(before);

        let is_escaped_by_dollar = if open_idx > 0 {
            before.ends_with('$')
        } else {
            prev_ended_with_dollar
        };

        let after_open = &rest[open_idx + 2..];
        if is_escaped_by_dollar {
            output.push_str("{{");
            rest = after_open;
            prev_ended_with_dollar = false;
            continue;
        }

        if let Some(close_idx) = after_open.find("}}") {
            let expr = after_open[..close_idx].trim();
            if let Some(field) = expr.strip_prefix("target.") {
                let field = field.trim();
                match field {
                    "name" => output.push_str(&target.name),
                    "path" => output.push_str(&target.path),
                    _ => {
                        return Err(DriftError::UndefinedTemplateVar {
                            path: source_path.to_path_buf(),
                            variable: expr.to_string(),
                            target_name: target.name.clone(),
                        });
                    }
                }
            } else if let Some(var_key) = expr.strip_prefix("vars.") {
                let var_key = var_key.trim();
                if let Some(val) = target
                    .vars
                    .get(var_key)
                    .or_else(|| global_vars.get(var_key))
                {
                    output.push_str(val);
                } else {
                    return Err(DriftError::UndefinedTemplateVar {
                        path: source_path.to_path_buf(),
                        variable: expr.to_string(),
                        target_name: target.name.clone(),
                    });
                }
            } else {
                output.push_str("{{");
                output.push_str(&after_open[..close_idx]);
                output.push_str("}}");
            }

            let next_start = close_idx + 2;
            prev_ended_with_dollar = after_open[..next_start].ends_with('$');
            rest = &after_open[next_start..];
        } else {
            output.push_str(&rest[open_idx..]);
            rest = "";
            break;
        }
    }

    output.push_str(rest);
    Ok(output)
}

pub fn evaluate_workspace(
    source_root: &Path,
    manifest: &Manifest,
) -> Result<Vec<RepoReport>, DriftError> {
    manifest.validate()?;
    let mut reports = Vec::with_capacity(manifest.targets.len());

    for target in &manifest.targets {
        let effective_path = target.effective_path();
        let repo_rel = validate_relative_path(&effective_path)?;
        let repo_root = ensure_no_symlink_in_chain(source_root, &repo_rel)?;
        let lock = LockFile::load_optional(&repo_root)?;
        let mut evaluations = Vec::with_capacity(manifest.files.len());

        for file in &manifest.files {
            let src_rel = validate_relative_path(&file.source)?;
            let tgt_rel = validate_relative_path(&file.target)?;

            let src_full = ensure_no_symlink_in_chain(source_root, &src_rel)?;
            let raw_theirs = read_bounded_file(&src_full)?;
            if let Some(secret_kind) = scan_for_hardcoded_secrets(&raw_theirs) {
                return Err(DriftError::SecretDetected {
                    location: file.source.clone(),
                    secret_kind,
                });
            }
            let theirs = render_template(&raw_theirs, target, &manifest.vars, &src_full)?;
            if let Some(secret_kind) = scan_for_hardcoded_secrets(&theirs) {
                return Err(DriftError::SecretDetected {
                    location: format!("{} (target: {})", file.source, target.name),
                    secret_kind,
                });
            }

            let tgt_full = ensure_no_symlink_in_chain(&repo_root, &tgt_rel)?;
            let ours = if tgt_full.exists() {
                Some(read_bounded_file(&tgt_full)?)
            } else {
                None
            };

            let base = lock
                .files
                .get(&file.target)
                .map(|entry| entry.base_content.as_str());

            let eval = evaluate_file(
                &file.source,
                &file.target,
                file.strategy,
                base,
                ours.as_deref(),
                &theirs,
            );
            evaluations.push(eval);
        }

        reports.push(RepoReport {
            target_name: target.name.clone(),
            target_path: effective_path,
            evaluations,
        });
    }

    Ok(reports)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ApplySummary {
    pub dry_run: bool,
    pub files_written: usize,
    pub locks_updated: usize,
    pub conflicts_skipped: usize,
    pub needs_adopt_skipped: usize,
}

pub fn apply_workspace(
    source_root: &Path,
    manifest: &Manifest,
    upstream_revision: Option<String>,
    write: bool,
) -> Result<(Vec<RepoReport>, ApplySummary), DriftError> {
    let reports = evaluate_workspace(source_root, manifest)?;
    let mut summary = ApplySummary {
        dry_run: !write,
        ..Default::default()
    };

    for (target, report) in manifest.targets.iter().zip(reports.iter()) {
        let repo_rel = validate_relative_path(&target.effective_path())?;
        let repo_root = ensure_no_symlink_in_chain(source_root, &repo_rel)?;
        let mut lock = LockFile::load_optional(&repo_root)?;
        let mut lock_dirty = false;

        for eval in &report.evaluations {
            let tgt_rel = validate_relative_path(&eval.target_path)?;
            let tgt_full = ensure_no_symlink_in_chain(&repo_root, &tgt_rel)?;

            match &eval.plan {
                PlanAction::NoOp | PlanAction::KeepLocal => {}
                PlanAction::NeedsAdopt => {
                    summary.needs_adopt_skipped += 1;
                }
                PlanAction::Conflict { .. } => {
                    summary.conflicts_skipped += 1;
                }
                PlanAction::RecordLock { base_content } => {
                    lock.files.insert(
                        eval.target_path.clone(),
                        LockedFileEntry {
                            base_hash: hash_content(base_content),
                            base_content: base_content.clone(),
                        },
                    );
                    lock_dirty = true;
                }
                PlanAction::Create { new_content }
                | PlanAction::UpdateFastForward { new_content }
                | PlanAction::OverwriteWithUpstream { new_content } => {
                    if write {
                        if let Some(parent) = tgt_full.parent() {
                            fs::create_dir_all(parent).map_err(|source| DriftError::Io {
                                path: parent.to_path_buf(),
                                source,
                            })?;
                        }
                        fs::write(&tgt_full, new_content).map_err(|source| DriftError::Io {
                            path: tgt_full.clone(),
                            source,
                        })?;
                    }
                    summary.files_written += 1;
                    lock.files.insert(
                        eval.target_path.clone(),
                        LockedFileEntry {
                            base_hash: hash_content(new_content),
                            base_content: new_content.clone(),
                        },
                    );
                    lock_dirty = true;
                }
                PlanAction::MergeClean {
                    merged_content,
                    new_base_content,
                } => {
                    if write {
                        if let Some(parent) = tgt_full.parent() {
                            fs::create_dir_all(parent).map_err(|source| DriftError::Io {
                                path: parent.to_path_buf(),
                                source,
                            })?;
                        }
                        fs::write(&tgt_full, merged_content).map_err(|source| DriftError::Io {
                            path: tgt_full.clone(),
                            source,
                        })?;
                    }
                    summary.files_written += 1;
                    lock.files.insert(
                        eval.target_path.clone(),
                        LockedFileEntry {
                            base_hash: hash_content(new_base_content),
                            base_content: new_base_content.clone(),
                        },
                    );
                    lock_dirty = true;
                }
            }
        }

        if lock_dirty {
            if let Some(rev) = &upstream_revision {
                lock.upstream_revision = Some(rev.clone());
            }
            if write {
                fs::create_dir_all(&repo_root).map_err(|source| DriftError::Io {
                    path: repo_root.clone(),
                    source,
                })?;
                lock.save(&repo_root)?;
            }
            summary.locks_updated += 1;
        }
    }

    Ok((reports, summary))
}

pub fn adopt_workspace(
    source_root: &Path,
    manifest: &Manifest,
    upstream_revision: Option<String>,
) -> Result<usize, DriftError> {
    manifest.validate()?;
    let mut updated_repos = 0usize;

    for target in &manifest.targets {
        let repo_rel = validate_relative_path(&target.effective_path())?;
        let repo_root = ensure_no_symlink_in_chain(source_root, &repo_rel)?;
        let mut lock = LockFile::load_optional(&repo_root)?;
        let mut changed = false;

        for file in &manifest.files {
            if lock.files.contains_key(&file.target) {
                continue;
            }
            let src_rel = validate_relative_path(&file.source)?;
            let src_full = ensure_no_symlink_in_chain(source_root, &src_rel)?;
            let raw_theirs = read_bounded_file(&src_full)?;
            if let Some(secret_kind) = scan_for_hardcoded_secrets(&raw_theirs) {
                return Err(DriftError::SecretDetected {
                    location: file.source.clone(),
                    secret_kind,
                });
            }
            let rendered_theirs = render_template(&raw_theirs, target, &manifest.vars, &src_full)?;
            if let Some(secret_kind) = scan_for_hardcoded_secrets(&rendered_theirs) {
                return Err(DriftError::SecretDetected {
                    location: format!("{} (target: {})", file.source, target.name),
                    secret_kind,
                });
            }
            let theirs = normalize_newlines(&rendered_theirs);

            lock.files.insert(
                file.target.clone(),
                LockedFileEntry {
                    base_hash: hash_content(&theirs),
                    base_content: theirs,
                },
            );
            changed = true;
        }

        if changed {
            if let Some(rev) = &upstream_revision {
                lock.upstream_revision = Some(rev.clone());
            }
            fs::create_dir_all(&repo_root).map_err(|source| DriftError::Io {
                path: repo_root.clone(),
                source,
            })?;
            lock.save(&repo_root)?;
            updated_repos += 1;
        }
    }

    Ok(updated_repos)
}

pub fn has_actionable_drift(reports: &[RepoReport]) -> bool {
    reports.iter().any(|repo| {
        repo.evaluations.iter().any(|eval| {
            if matches!(
                eval.status,
                DriftStatus::InSync | DriftStatus::LocallyModified | DriftStatus::LocallyDeleted
            ) {
                !matches!(eval.plan, PlanAction::NoOp | PlanAction::KeepLocal)
            } else {
                true
            }
        })
    })
}

fn status_markdown_badge(status: DriftStatus) -> &'static str {
    match status {
        DriftStatus::InSync => "✅ `IN-SYNC`",
        DriftStatus::MissingInTarget => "➕ `MISSING`",
        DriftStatus::UntrackedIdentical => "🔵 `UNTRACKED-SAME`",
        DriftStatus::UntrackedDifferent => "⚠️ `NEEDS-ADOPT`",
        DriftStatus::UpstreamChanged => "⚡ `UPSTREAM-CHANGED`",
        DriftStatus::LocallyModified => "🛡️ `LOCALLY-MODIFIED`",
        DriftStatus::LocallyDeleted => "🛡️ `LOCALLY-DELETED`",
        DriftStatus::BothChangedClean => "🔀 `3-WAY-MERGE`",
        DriftStatus::BothChangedConflict => "❌ `CONFLICT`",
    }
}

fn plan_markdown_label(plan: &PlanAction) -> &'static str {
    match plan {
        PlanAction::NoOp => "No changes",
        PlanAction::Create { .. } => "Create",
        PlanAction::RecordLock { .. } => "Record lock",
        PlanAction::NeedsAdopt => "Adoption required (`driftsync adopt`)",
        PlanAction::UpdateFastForward { .. } => "Fast-forward update",
        PlanAction::KeepLocal => "Keep local customization",
        PlanAction::MergeClean { .. } => "3-way clean merge",
        PlanAction::OverwriteWithUpstream { .. } => "Overwrite with upstream",
        PlanAction::Conflict { .. } => "Conflict (`driftsync resolve` required)",
    }
}

pub fn generate_markdown_report(reports: &[RepoReport]) -> String {
    let mut out = String::from("## 🔄 driftsync — Multi-Repo Drift Report\n\n");
    out.push_str("| Target Repo | File | Strategy | Status | Planned Action |\n");
    out.push_str("|---|---|---|---|---|\n");

    let mut diff_sections = String::new();
    for repo in reports {
        for eval in &repo.evaluations {
            out.push_str(&format!(
                "| **{}** (`{}`) | `{}` | `{:?}` | {} | {} |\n",
                repo.target_name,
                repo.target_path,
                eval.target_path,
                eval.strategy,
                status_markdown_badge(eval.status),
                plan_markdown_label(&eval.plan)
            ));
            if !eval.unified_diff.is_empty() {
                let safe_diff = redact_sensitive_text(eval.unified_diff.trim_end());
                diff_sections.push_str(&format!(
                    "\n<details>\n<summary><b>{} :: {}</b> ({})</summary>\n\n```diff\n{}\n```\n</details>\n",
                    repo.target_name,
                    eval.target_path,
                    status_markdown_badge(eval.status),
                    safe_diff
                ));
            }
        }
    }

    if !diff_sections.is_empty() {
        out.push_str("\n### 📝 Proposed Changes (Diff Preview)\n");
        out.push_str(&diff_sections);
    }

    out
}

pub fn generate_pr_body(report: &RepoReport, upstream_revision: Option<&str>) -> String {
    let mut out = format!(
        "## 🔄 `driftsync` Configuration Sync (`{}`)\n\n",
        report.target_name
    );
    if let Some(rev) = upstream_revision {
        out.push_str(&format!("- **Upstream Revision:** `{rev}`\n"));
    }
    out.push_str(
        "- **Policy:** 3-way structural merge applied while preserving local customizations.\n\n",
    );

    out.push_str("| Managed File | Drift Status | Applied Action |\n");
    out.push_str("|---|---|---|\n");
    for eval in &report.evaluations {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            eval.target_path,
            status_markdown_badge(eval.status),
            plan_markdown_label(&eval.plan)
        ));
    }

    let mut any_diff = false;
    for eval in &report.evaluations {
        if !eval.unified_diff.is_empty() {
            if !any_diff {
                out.push_str("\n### 🔍 File Diffs\n");
                any_diff = true;
            }
            let safe_diff = redact_sensitive_text(eval.unified_diff.trim_end());
            out.push_str(&format!(
                "\n<details>\n<summary><code>{}</code></summary>\n\n```diff\n{}\n```\n</details>\n",
                eval.target_path, safe_diff
            ));
        }
    }

    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResolutionChoice {
    Ours,
    Theirs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedFileRecord {
    pub target_name: String,
    pub target_path: String,
    pub choice: ResolutionChoice,
    pub dry_run: bool,
}

pub fn resolve_target_file(
    source_root: &Path,
    target: &TargetRepo,
    file: &ManagedFile,
    global_vars: &BTreeMap<String, String>,
    upstream_revision: Option<&str>,
    choice: ResolutionChoice,
    write: bool,
) -> Result<ResolvedFileRecord, DriftError> {
    let src_rel = validate_relative_path(&file.source)?;
    let tgt_rel = validate_relative_path(&file.target)?;

    let src_full = ensure_no_symlink_in_chain(source_root, &src_rel)?;
    let raw_theirs = read_bounded_file(&src_full)?;
    if let Some(secret_kind) = scan_for_hardcoded_secrets(&raw_theirs) {
        return Err(DriftError::SecretDetected {
            location: file.source.clone(),
            secret_kind,
        });
    }
    let rendered_theirs = render_template(&raw_theirs, target, global_vars, &src_full)?;
    if let Some(secret_kind) = scan_for_hardcoded_secrets(&rendered_theirs) {
        return Err(DriftError::SecretDetected {
            location: format!("{} (target: {})", file.source, target.name),
            secret_kind,
        });
    }
    let theirs_norm = normalize_newlines(&rendered_theirs);

    let repo_rel = validate_relative_path(&target.effective_path())?;
    let repo_root = ensure_no_symlink_in_chain(source_root, &repo_rel)?;
    let tgt_full = ensure_no_symlink_in_chain(&repo_root, &tgt_rel)?;

    if write {
        if choice == ResolutionChoice::Theirs {
            if let Some(parent) = tgt_full.parent() {
                fs::create_dir_all(parent).map_err(|source| DriftError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            fs::write(&tgt_full, &theirs_norm).map_err(|source| DriftError::Io {
                path: tgt_full,
                source,
            })?;
        }

        let mut lock = LockFile::load_optional(&repo_root)?;
        lock.files.insert(
            file.target.clone(),
            LockedFileEntry {
                base_hash: hash_content(&theirs_norm),
                base_content: theirs_norm,
            },
        );
        if let Some(rev) = upstream_revision {
            lock.upstream_revision = Some(rev.to_string());
        }
        fs::create_dir_all(&repo_root).map_err(|source| DriftError::Io {
            path: repo_root.clone(),
            source,
        })?;
        lock.save(&repo_root)?;
    }

    Ok(ResolvedFileRecord {
        target_name: target.name.clone(),
        target_path: file.target.clone(),
        choice,
        dry_run: !write,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn rejects_unsafe_paths() {
        assert!(validate_relative_path("../secret").is_err());
        assert!(validate_relative_path("a/../../b").is_err());
        assert!(validate_relative_path("/etc/passwd").is_err());
        assert!(validate_relative_path("C:\\Windows\\win.ini").is_err());
        assert!(validate_relative_path("").is_err());

        assert!(validate_relative_path(".git/config").is_err());
        assert!(validate_relative_path(".GIT/hooks/pre-commit").is_err());
        assert!(validate_relative_path("nested/.git/hooks/post-checkout").is_err());
        assert!(validate_relative_path(".driftsync.lock").is_err());
        assert!(validate_relative_path("CON").is_err());
        assert!(validate_relative_path("dir/NUL.txt").is_err());
        assert!(validate_relative_path("COM1.json").is_err());
        assert!(validate_relative_path("LPT9").is_err());
        assert!(validate_relative_path("trailing./file.txt").is_err());

        assert!(validate_relative_path(".github/workflows/ci.yml").is_ok());
        assert!(validate_relative_path(".editorconfig").is_ok());
    }

    #[test]
    fn rejects_invalid_target_names() {
        assert!(validate_target_name("service-auth").is_ok());
        assert!(validate_target_name("web_dashboard_01").is_ok());

        assert!(validate_target_name("").is_err());
        assert!(validate_target_name("-flag-injection").is_err());
        assert!(validate_target_name(".hidden").is_err());
        assert!(validate_target_name("../escape").is_err());
        assert!(validate_target_name("org/repo").is_err());
        assert!(validate_target_name("CON").is_err());
        assert!(validate_target_name("with space").is_err());
    }

    #[test]
    fn secret_scanner_blocks_hardcoded_tokens_and_private_keys() {
        let ghp = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
        assert_eq!(
            scan_for_hardcoded_secrets(&format!("token = \"{ghp}\"")),
            Some("GitHub Personal Access Token")
        );

        let gh_pat = format!("github_pat_{}", "11AABBCCDD0123456789_abcdefghijklmnop");
        assert_eq!(
            scan_for_hardcoded_secrets(&gh_pat),
            Some("GitHub Fine-Grained PAT")
        );

        let glpat = format!("glpat-{}", "1234567890abcdefghij");
        assert_eq!(
            scan_for_hardcoded_secrets(&glpat),
            Some("GitLab Personal Access Token")
        );

        let aws = format!("AKIA{}", "IOSFODNN7EXAMPLE");
        assert_eq!(scan_for_hardcoded_secrets(&aws), Some("AWS Access Key ID"));

        let pem = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAAA...\n";
        assert_eq!(scan_for_hardcoded_secrets(pem), Some("PEM Private Key"));

        assert_eq!(
            scan_for_hardcoded_secrets("token: ${{ secrets.GITHUB_TOKEN }}"),
            None
        );

        let mut bad_vars = BTreeMap::new();
        bad_vars.insert("api_token".to_string(), ghp);
        let manifest = Manifest {
            version: 1,
            vars: bad_vars,
            targets: vec![],
            files: vec![],
        };
        let err = manifest.validate().expect_err("secret in vars must fail");
        assert!(matches!(err, DriftError::SecretDetected { .. }));
    }

    #[test]
    fn redacts_sensitive_urls_and_tokens_in_diffs_and_markdown() {
        let ghp = format!("ghp_{}", "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8");
        let ours = "registry = \"https://admin:superSecretPass@npm.example.com/private\"\n";
        let theirs = format!("registry = \"https://ci:{ghp}@npm.example.com/private\"\n");

        let eval = evaluate_file(
            "templates/.npmrc",
            ".npmrc",
            SyncStrategy::ThreeWay,
            Some(ours),
            Some(ours),
            &theirs,
        );

        assert!(!eval.unified_diff.contains("superSecretPass"));
        assert!(!eval.unified_diff.contains(&ghp));
        assert!(
            eval.unified_diff
                .contains("https://[REDACTED]@npm.example.com/private")
        );

        let report = RepoReport {
            target_name: "web-app".to_string(),
            target_path: "repos/web-app".to_string(),
            evaluations: vec![eval],
        };
        let md = generate_markdown_report(&[report]);
        assert!(!md.contains("superSecretPass"));
        assert!(!md.contains(&ghp));
    }

    #[test]
    fn bounded_file_reader_enforces_size_limit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let oversized = tmp.path().join("huge.toml");
        let f = fs::File::create(&oversized).expect("create");
        f.set_len(MAX_MANAGED_FILE_BYTES + 1024).expect("set_len");

        let err = read_bounded_file(&oversized).expect_err("must reject oversized file");
        assert!(matches!(err, DriftError::FileTooLarge { .. }));
    }

    #[test]
    fn template_rendering_interpolates_target_and_vars_while_preserving_github_actions() {
        let mut global_vars = BTreeMap::new();
        global_vars.insert("rust_version".to_string(), "1.85".to_string());
        global_vars.insert("port".to_string(), "8080".to_string());

        let mut target_vars = BTreeMap::new();
        target_vars.insert("port".to_string(), "9091".to_string());

        let target = TargetRepo {
            name: "service-auth".to_string(),
            path: "repos/service-auth".to_string(),
            repo: None,
            vars: target_vars,
        };

        let tpl = r#"name: CI ({{ target.name }})
env:
  SERVICE_NAME: "{{target.name}}"
  SERVICE_PATH: "{{ target.path }}"
  PORT: "{{ vars.port }}"
  RUST_VERSION: "{{ vars.rust_version }}"
  GH_SECRET: "${{ secrets.API_TOKEN }}"
  HELM_EXPR: "{{ .Values.image }}"
"#;

        let rendered =
            render_template(tpl, &target, &global_vars, Path::new("templates/ci.yml")).expect("ok");
        assert!(rendered.contains("name: CI (service-auth)"));
        assert!(rendered.contains("SERVICE_NAME: \"service-auth\""));
        assert!(rendered.contains("SERVICE_PATH: \"repos/service-auth\""));
        assert!(rendered.contains("PORT: \"9091\""));
        assert!(rendered.contains("RUST_VERSION: \"1.85\""));
        assert!(rendered.contains("GH_SECRET: \"${{ secrets.API_TOKEN }}\""));
        assert!(rendered.contains("HELM_EXPR: \"{{ .Values.image }}\""));
    }

    #[test]
    fn template_rendering_rejects_undefined_target_or_vars_keys() {
        let global_vars = BTreeMap::new();
        let target = TargetRepo {
            name: "service-auth".to_string(),
            path: "repos/service-auth".to_string(),
            repo: None,
            vars: BTreeMap::new(),
        };

        let err1 = render_template(
            "service: {{ target.unknown }}",
            &target,
            &global_vars,
            Path::new("templates/ci.yml"),
        )
        .expect_err("should fail on unknown target field");
        assert!(matches!(err1, DriftError::UndefinedTemplateVar { .. }));

        let err2 = render_template(
            "port: {{ vars.missing_port }}",
            &target,
            &global_vars,
            Path::new("templates/ci.yml"),
        )
        .expect_err("should fail on missing var");
        assert!(matches!(err2, DriftError::UndefinedTemplateVar { .. }));
    }

    #[test]
    fn three_way_merge_preserves_local_edits_when_non_overlapping() {
        let base = "line1\nline2\nline3\nline4\n";
        let ours = "line1\nline2-local-custom\nline3\nline4\n";
        let theirs = "line1\nline2\nline3\nline4-upstream-update\n";

        let eval = evaluate_file(
            "templates/cfg",
            "cfg",
            SyncStrategy::ThreeWay,
            Some(base),
            Some(ours),
            theirs,
        );

        assert_eq!(eval.status, DriftStatus::BothChangedClean);
        assert_eq!(
            eval.plan,
            PlanAction::MergeClean {
                merged_content: "line1\nline2-local-custom\nline3\nline4-upstream-update\n"
                    .to_string(),
                new_base_content: theirs.to_string(),
            }
        );
    }

    #[test]
    fn conflicting_edits_produce_conflict_without_overwriting() {
        let base = "timeout = 30\n";
        let ours = "timeout = 60\n";
        let theirs = "timeout = 15\n";

        let eval = evaluate_file(
            "templates/cfg.toml",
            "cfg.toml",
            SyncStrategy::ThreeWay,
            Some(base),
            Some(ours),
            theirs,
        );

        assert_eq!(eval.status, DriftStatus::BothChangedConflict);
        match &eval.plan {
            PlanAction::Conflict { conflict_preview } => {
                assert!(conflict_preview.contains("timeout"));
                assert!(conflict_preview.contains("60"));
                assert!(conflict_preview.contains("15"));
            }
            other => panic!("unexpected plan: {other:?}"),
        }
    }

    #[test]
    fn toml_structural_merge_resolves_adjacent_keys_and_preserves_comments() {
        let base = r#"# Shared Rust settings
[package]
name = "service-a"
edition = "2021"

[dependencies]
serde = "1.0"
"#;
        let ours = r#"# Shared Rust settings
[package]
name = "service-a"
edition = "2021" # local note
publish = false

[dependencies]
# Database driver
sqlx = "0.8"
serde = "1.0"
"#;
        let theirs = r#"# Shared Rust settings
[package]
name = "service-a"
edition = "2024"
rust-version = "1.85"

[dependencies]
serde = "1.0"
tracing = "0.1"
"#;

        let eval = evaluate_file(
            "templates/Cargo.toml",
            "Cargo.toml",
            SyncStrategy::ThreeWay,
            Some(base),
            Some(ours),
            theirs,
        );

        assert_eq!(eval.status, DriftStatus::BothChangedClean);
        match &eval.plan {
            PlanAction::MergeClean { merged_content, .. } => {
                assert!(merged_content.contains("# Shared Rust settings"));
                assert!(merged_content.contains("# Database driver"));
                assert!(merged_content.contains("edition = \"2024\" # local note"));
                assert!(merged_content.contains("publish = false"));
                assert!(merged_content.contains("rust-version = \"1.85\""));
                assert!(merged_content.contains("sqlx = \"0.8\""));
                assert!(merged_content.contains("tracing = \"0.1\""));
            }
            other => panic!("unexpected plan: {other:?}"),
        }
    }

    #[test]
    fn toml_structural_merge_handles_inline_tables_recursively() {
        let base = "[dependencies]\ntokio = { version = \"1.38\", features = [\"rt\"] }\n";
        let ours = "[dependencies]\ntokio = { version = \"1.38\", features = [\"rt\", \"net\"] }\n";
        let theirs = "[dependencies]\ntokio = { version = \"1.40\", features = [\"rt\"] }\n";

        let merged = merge_toml_structural(base, ours, theirs).expect("inline table merge");
        let doc: toml::Value = toml::from_str(&merged).expect("valid toml");
        assert_eq!(
            doc["dependencies"]["tokio"]["version"].as_str(),
            Some("1.40")
        );
        let features = doc["dependencies"]["tokio"]["features"]
            .as_array()
            .expect("features array");
        assert_eq!(features.len(), 2);
    }

    #[test]
    fn json_structural_merge_combines_nested_objects_cleanly() {
        let base = r#"{
  "name": "web-app",
  "scripts": {
    "build": "tsc",
    "test": "vitest"
  }
}
"#;
        let ours = r#"{
  "name": "web-app",
  "scripts": {
    "build": "tsc",
    "dev": "vite",
    "test": "vitest"
  }
}
"#;
        let theirs = r#"{
  "name": "web-app",
  "scripts": {
    "build": "tsc",
    "lint": "eslint .",
    "test": "vitest run"
  }
}
"#;

        let eval = evaluate_file(
            "templates/package.json",
            "package.json",
            SyncStrategy::ThreeWay,
            Some(base),
            Some(ours),
            theirs,
        );

        assert_eq!(eval.status, DriftStatus::BothChangedClean);
        match &eval.plan {
            PlanAction::MergeClean { merged_content, .. } => {
                let parsed: serde_json::Value =
                    serde_json::from_str(merged_content).expect("valid json");
                assert_eq!(parsed["scripts"]["dev"], "vite");
                assert_eq!(parsed["scripts"]["lint"], "eslint .");
                assert_eq!(parsed["scripts"]["test"], "vitest run");
            }
            other => panic!("unexpected plan: {other:?}"),
        }
    }

    #[test]
    fn yaml_structural_merge_combines_adjacent_keys_and_steps_without_conflict() {
        let base = r#"# CI Workflow
name: CI
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --locked
"#;
        let ours = r#"# CI Workflow
name: CI
on: [push]
jobs:
  test:
    runs-on: ubuntu-latest
    permissions: read-all
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --locked
      - run: ./scripts/verify-pci.sh
"#;
        let theirs = r#"# CI Workflow
name: CI
on: [push]
jobs:
  test:
    runs-on: ubuntu-24.04
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@v4
      - run: cargo test --locked
      - run: cargo fmt --all -- --check
"#;

        let eval = evaluate_file(
            "templates/ci.yml",
            ".github/workflows/ci.yml",
            SyncStrategy::ThreeWay,
            Some(base),
            Some(ours),
            theirs,
        );

        assert_eq!(eval.status, DriftStatus::BothChangedClean);
        match &eval.plan {
            PlanAction::MergeClean { merged_content, .. } => {
                assert!(merged_content.contains("# CI Workflow"));
                assert!(merged_content.contains("runs-on: ubuntu-24.04"));
                assert!(merged_content.contains("permissions: read-all"));
                assert!(merged_content.contains("timeout-minutes: 20"));
                assert!(merged_content.contains("cargo fmt --all -- --check"));
                assert!(merged_content.contains("./scripts/verify-pci.sh"));
            }
            other => panic!("unexpected plan: {other:?}"),
        }
    }

    #[test]
    fn markdown_report_and_pr_body_and_audit_check_work() {
        let eval = evaluate_file(
            "templates/.editorconfig",
            ".editorconfig",
            SyncStrategy::ThreeWay,
            Some("root = true\n"),
            Some("root = true\n"),
            "root = true\nindent_size = 4\n",
        );
        let report = RepoReport {
            target_name: "service-auth".to_string(),
            target_path: "repos/service-auth".to_string(),
            evaluations: vec![eval],
        };

        assert!(has_actionable_drift(std::slice::from_ref(&report)));
        let md = generate_markdown_report(std::slice::from_ref(&report));
        assert!(md.contains("service-auth"));
        assert!(md.contains("UPSTREAM-CHANGED"));

        let pr_body = generate_pr_body(&report, Some("abc1234"));
        assert!(pr_body.contains("abc1234"));
        assert!(pr_body.contains(".editorconfig"));
    }

    #[test]
    fn tracked_file_deleted_locally_preserves_deletion_when_upstream_unchanged() {
        let base = "version = 1\n";
        let theirs = "version = 1\n";

        let eval_threeway = evaluate_file(
            "templates/cfg.toml",
            "cfg.toml",
            SyncStrategy::ThreeWay,
            Some(base),
            None,
            theirs,
        );
        assert_eq!(eval_threeway.status, DriftStatus::LocallyDeleted);
        assert_eq!(eval_threeway.plan, PlanAction::KeepLocal);
        assert!(eval_threeway.unified_diff.is_empty());

        let eval_keep = evaluate_file(
            "templates/cfg.toml",
            "cfg.toml",
            SyncStrategy::KeepLocal,
            Some(base),
            None,
            theirs,
        );
        assert_eq!(eval_keep.status, DriftStatus::LocallyDeleted);
        assert_eq!(eval_keep.plan, PlanAction::KeepLocal);

        let eval_overwrite = evaluate_file(
            "templates/cfg.toml",
            "cfg.toml",
            SyncStrategy::Overwrite,
            Some(base),
            None,
            theirs,
        );
        assert_eq!(eval_overwrite.status, DriftStatus::LocallyDeleted);
        assert_eq!(
            eval_overwrite.plan,
            PlanAction::OverwriteWithUpstream {
                new_content: theirs.to_string(),
            }
        );
    }

    #[test]
    fn tracked_file_deleted_locally_flags_delete_modify_conflict_when_upstream_modified() {
        let base = "version = 1\n";
        let theirs = "version = 2\n";

        let eval = evaluate_file(
            "templates/cfg.toml",
            "cfg.toml",
            SyncStrategy::ThreeWay,
            Some(base),
            None,
            theirs,
        );
        assert_eq!(eval.status, DriftStatus::BothChangedConflict);
        match &eval.plan {
            PlanAction::Conflict { conflict_preview } => {
                assert!(conflict_preview.contains("deleted locally"));
                assert!(conflict_preview.contains("version = 2"));
            }
            other => panic!("expected conflict, got: {other:?}"),
        }

        let eval_keep = evaluate_file(
            "templates/cfg.toml",
            "cfg.toml",
            SyncStrategy::KeepLocal,
            Some(base),
            None,
            theirs,
        );
        assert_eq!(eval_keep.status, DriftStatus::LocallyDeleted);
        assert_eq!(eval_keep.plan, PlanAction::KeepLocal);
    }

    #[test]
    fn untracked_missing_file_triggers_create() {
        let theirs = "new_file = true\n";
        let eval = evaluate_file(
            "templates/new.toml",
            "new.toml",
            SyncStrategy::ThreeWay,
            None,
            None,
            theirs,
        );
        assert_eq!(eval.status, DriftStatus::MissingInTarget);
        assert_eq!(
            eval.plan,
            PlanAction::Create {
                new_content: theirs.to_string(),
            }
        );
    }

    proptest! {
        #[test]
        fn prop_local_changes_never_overwritten_when_upstream_unchanged(
            base in "[a-zA-Z0-9\n]{0,80}",
            ours in "[a-zA-Z0-9\n]{0,80}",
        ) {
            let eval = evaluate_file(
                "src.txt",
                "dst.txt",
                SyncStrategy::ThreeWay,
                Some(&base),
                Some(&ours),
                &base,
            );
            let preserves_local = matches!(eval.plan, PlanAction::NoOp | PlanAction::KeepLocal);
            prop_assert!(preserves_local, "unexpected plan: {:?}", eval.plan);
        }

        #[test]
        fn prop_fast_forward_when_target_unmodified(
            base in "[a-zA-Z0-9\n]{0,80}",
            theirs in "[a-zA-Z0-9\n]{0,80}",
        ) {
            let eval = evaluate_file(
                "src.txt",
                "dst.txt",
                SyncStrategy::ThreeWay,
                Some(&base),
                Some(&base),
                &theirs,
            );
            let expected_theirs = normalize_newlines(&theirs);
            let expected_base = normalize_newlines(&base);
            if expected_base == expected_theirs {
                prop_assert_eq!(eval.status, DriftStatus::InSync);
                prop_assert_eq!(eval.plan, PlanAction::NoOp);
            } else {
                prop_assert_eq!(eval.status, DriftStatus::UpstreamChanged);
                prop_assert_eq!(
                    eval.plan,
                    PlanAction::UpdateFastForward {
                        new_content: expected_theirs
                    }
                );
            }
        }

        #[test]
        fn prop_idempotent_after_apply(
            base in "[a-z]{1,10}\n[a-z]{1,10}\n[a-z]{1,10}\n",
            ours in "[a-z]{1,10}\n[a-z]{1,10}\n[a-z]{1,10}\n",
            theirs in "[a-z]{1,10}\n[a-z]{1,10}\n[a-z]{1,10}\n",
        ) {
            let first = evaluate_file(
                "src.txt",
                "dst.txt",
                SyncStrategy::ThreeWay,
                Some(&base),
                Some(&ours),
                &theirs,
            );

            let (next_base, next_ours) = match &first.plan {
                PlanAction::Create { new_content }
                | PlanAction::UpdateFastForward { new_content }
                | PlanAction::OverwriteWithUpstream { new_content } => {
                    (new_content.clone(), new_content.clone())
                }
                PlanAction::MergeClean { merged_content, new_base_content } => {
                    (new_base_content.clone(), merged_content.clone())
                }
                PlanAction::RecordLock { base_content } => {
                    (base_content.clone(), normalize_newlines(&ours))
                }
                PlanAction::NoOp | PlanAction::KeepLocal | PlanAction::NeedsAdopt | PlanAction::Conflict { .. } => {
                    return Ok(());
                }
            };

            let second = evaluate_file(
                "src.txt",
                "dst.txt",
                SyncStrategy::ThreeWay,
                Some(&next_base),
                Some(&next_ours),
                &theirs,
            );

            let no_second_write = matches!(second.plan, PlanAction::NoOp | PlanAction::KeepLocal);
            prop_assert!(no_second_write, "second evaluation produced write: {:?}", second);
        }

        #[test]
        fn prop_toml_disjoint_keys_always_merge_cleanly(
            local_key in "local_[a-z]{1,8}",
            upstream_key in "upstream_[a-z]{1,8}",
            local_val in 1i64..10_000,
            upstream_val in 1i64..10_000,
        ) {
            let base = "[settings]\nshared = 1\n";
            let ours = format!("[settings]\nshared = 1\n{local_key} = {local_val}\n");
            let theirs = format!("[settings]\nshared = 1\n{upstream_key} = {upstream_val}\n");

            let eval = evaluate_file(
                "templates/config.toml",
                "config.toml",
                SyncStrategy::ThreeWay,
                Some(base),
                Some(&ours),
                &theirs,
            );

            prop_assert_eq!(eval.status, DriftStatus::BothChangedClean);
            if let PlanAction::MergeClean { merged_content, .. } = eval.plan {
                let parsed: toml::Value = toml::from_str(&merged_content).expect("valid merged toml");
                prop_assert_eq!(parsed["settings"][&local_key].as_integer(), Some(local_val));
                prop_assert_eq!(parsed["settings"][&upstream_key].as_integer(), Some(upstream_val));
                prop_assert_eq!(parsed["settings"]["shared"].as_integer(), Some(1));
            } else {
                prop_assert!(false, "expected MergeClean");
            }
        }

        #[test]
        fn prop_json_disjoint_keys_always_merge_cleanly(
            local_key in "local_[a-z]{1,8}",
            upstream_key in "upstream_[a-z]{1,8}",
            local_val in 1i64..10_000,
            upstream_val in 1i64..10_000,
        ) {
            let base = "{\"settings\":{\"shared\":1}}\n";
            let ours = format!("{{\"settings\":{{\"shared\":1,\"{local_key}\":{local_val}}}}}\n");
            let theirs = format!("{{\"settings\":{{\"shared\":1,\"{upstream_key}\":{upstream_val}}}}}\n");

            let eval = evaluate_file(
                "templates/config.json",
                "config.json",
                SyncStrategy::ThreeWay,
                Some(base),
                Some(&ours),
                &theirs,
            );

            prop_assert_eq!(eval.status, DriftStatus::BothChangedClean);
            if let PlanAction::MergeClean { merged_content, .. } = eval.plan {
                let parsed: serde_json::Value = serde_json::from_str(&merged_content).expect("valid merged json");
                prop_assert_eq!(parsed["settings"][&local_key].as_i64(), Some(local_val));
                prop_assert_eq!(parsed["settings"][&upstream_key].as_i64(), Some(upstream_val));
                prop_assert_eq!(parsed["settings"]["shared"].as_i64(), Some(1));
            } else {
                prop_assert!(false, "expected MergeClean");
            }
        }
    }
}
