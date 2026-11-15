use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::DriftError;

pub const MAX_MANAGED_FILE_BYTES: u64 = 2 * 1024 * 1024;

pub fn is_windows_reserved_device_name(segment: &str) -> bool {
    let stem = segment.split('.').next().unwrap_or(segment).trim();
    let upper = stem.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

pub fn validate_target_name(raw: &str) -> Result<String, DriftError> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.len() > 128
        || trimmed.starts_with('.')
        || trimmed.starts_with('-')
        || trimmed.ends_with('.')
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains(':')
        || trimmed.contains("..")
        || is_windows_reserved_device_name(trimmed)
        || trimmed
            .chars()
            .any(|c| c.is_ascii_control() || c.is_whitespace())
    {
        return Err(DriftError::InvalidTargetName(raw.to_string()));
    }
    Ok(trimmed.to_string())
}

pub fn ensure_no_symlink_in_chain(root: &Path, relative: &Path) -> Result<PathBuf, DriftError> {
    if root.exists() {
        let root_meta = fs::symlink_metadata(root).map_err(|source| DriftError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        if root_meta.file_type().is_symlink() {
            return Err(DriftError::SymlinkDetected(root.to_path_buf()));
        }
    }

    let mut current = root.to_path_buf();
    for comp in relative.components() {
        if let Component::Normal(part) = comp {
            current.push(part);
            if let Ok(meta) = fs::symlink_metadata(&current)
                && meta.file_type().is_symlink()
            {
                return Err(DriftError::SymlinkDetected(current));
            }
        }
    }
    Ok(current)
}

pub fn read_bounded_file(path: &Path) -> Result<String, DriftError> {
    let meta = fs::symlink_metadata(path).map_err(|source| DriftError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if meta.file_type().is_symlink() {
        return Err(DriftError::SymlinkDetected(path.to_path_buf()));
    }
    if meta.len() > MAX_MANAGED_FILE_BYTES {
        return Err(DriftError::FileTooLarge {
            path: path.to_path_buf(),
            size_bytes: meta.len(),
            max_bytes: MAX_MANAGED_FILE_BYTES,
        });
    }
    fs::read_to_string(path).map_err(|source| DriftError::Io {
        path: path.to_path_buf(),
        source,
    })
}

pub fn scan_for_hardcoded_secrets(content: &str) -> Option<&'static str> {
    for header in [
        "-----BEGIN RSA PRIVATE KEY-----",
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "-----BEGIN EC PRIVATE KEY-----",
        "-----BEGIN PRIVATE KEY-----",
        "-----BEGIN DSA PRIVATE KEY-----",
    ] {
        if content.contains(header) {
            return Some("PEM Private Key");
        }
    }

    for token in tokenize_candidates(content) {
        if let Some(rest) = token
            .strip_prefix("ghp_")
            .or_else(|| token.strip_prefix("gho_"))
            .or_else(|| token.strip_prefix("ghs_"))
            .or_else(|| token.strip_prefix("ghu_"))
            && rest.len() >= 20
            && rest.chars().all(|c| c.is_ascii_alphanumeric())
        {
            return Some("GitHub Personal Access Token");
        }
        if let Some(rest) = token.strip_prefix("github_pat_")
            && rest.len() >= 20
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Some("GitHub Fine-Grained PAT");
        }
        if let Some(rest) = token.strip_prefix("glpat-")
            && rest.len() >= 20
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Some("GitLab Personal Access Token");
        }
        if let Some(rest) = token.strip_prefix("AKIA")
            && rest.len() == 16
            && rest
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
        {
            return Some("AWS Access Key ID");
        }
        if let Some(rest) = token
            .strip_prefix("xoxb-")
            .or_else(|| token.strip_prefix("xoxp-"))
            .or_else(|| token.strip_prefix("xoxa-"))
            .or_else(|| token.strip_prefix("xoxr-"))
            && rest.len() >= 12
            && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Some("Slack Token");
        }
        if let Some(rest) = token.strip_prefix("sk-")
            && rest.len() >= 24
            && rest
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Some("API Secret Key (sk-*)");
        }
    }

    None
}

fn tokenize_candidates(content: &str) -> impl Iterator<Item = &str> {
    content.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '`' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '='
            )
    })
}

pub fn redact_sensitive_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for line in input.split_inclusive('\n') {
        out.push_str(&redact_line(line));
    }
    out
}

fn redact_line(line: &str) -> String {
    let mut masked = line.to_string();

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

    for prefix in [
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
        "github_pat_",
        "glpat-",
        "AKIA",
        "xoxb-",
        "xoxp-",
        "sk-",
    ] {
        let mut search_from = 0;
        while let Some(rel_idx) = masked[search_from..].find(prefix) {
            let start = search_from + rel_idx;
            let after_prefix = &masked[start + prefix.len()..];
            let token_len: usize = after_prefix
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                .map(char::len_utf8)
                .sum();
            if token_len >= 12 {
                let end = start + prefix.len() + token_len;
                let replacement = format!("{prefix}[REDACTED]");
                masked.replace_range(start..end, &replacement);
                search_from = start + replacement.len();
            } else {
                search_from = start + prefix.len();
            }
        }
    }

    masked
}
