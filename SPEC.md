# SPEC.md — `driftsync` Technical Specification

## 1. Purpose & Problem Statement
Organizations maintaining multiple repositories (microservices, SDKs, polyrepos) share common configuration files (`.github/workflows/*.yml`, `Cargo.toml`, `package.json`, `.editorconfig`). Existing sync bots copy upstream templates blindly over target repositories, wiping out legitimate repo-specific customizations.

`driftsync` solves this using **stateful 3-way structural merging**:
- **`base`**: The last-synced canonical template snapshot recorded in the target repository's `.driftsync.lock`.
- **`ours`**: The current file on disk in the target repository.
- **`theirs`**: The current canonical template in the source repository (after per-target variable interpolation).

## 2. Drift Classification Matrix

| `base` | `ours` | Relationship | `DriftStatus` | Default `ThreeWay` Action |
|---|---|---|---|---|
| `None` | `None` | Target file does not exist | `MissingInTarget` | `Create` |
| `None` | `Some` | `ours == theirs` | `UntrackedIdentical` | `RecordLock` |
| `None` | `Some` | `ours != theirs` | `UntrackedDifferent` | `NeedsAdopt` (never overwrite blindly) |
| `Some` | `Some` | `ours == theirs` | `InSync` | `NoOp` / `RecordLock` |
| `Some` | `Some` | `ours == base && theirs != base` | `UpstreamChanged` | `UpdateFastForward` |
| `Some` | `Some` | `ours != base && theirs == base` | `LocallyModified` | `KeepLocal` (local edit preserved) |
| `Some` | `Some` | `ours != base && theirs != base` (clean merge) | `BothChangedClean` | `MergeClean` |
| `Some` | `Some` | `ours != base && theirs != base` (conflict) | `BothChangedConflict` | `Conflict` (skipped during apply) |

## 3. Format-Aware 3-Way Merge Engine (`structural_merge.rs`)
- **`.toml` (`merge_toml_structural`)**: Uses `toml_edit::DocumentMut` to merge tables and inline tables key-by-key while preserving target comments and whitespace trivia.
- **`.json` (`merge_json_structural`)**: Uses `serde_json::Value` to recursively merge nested JSON objects key-by-key.
- **`.yml` / `.yaml` (`merge_yaml_structural`)**: Block-aware YAML merger that preserves leading comments (`# ...`), merges nested mappings by key, and merges sequence blocks (such as GitHub Actions `steps:`) while respecting local additions and deletions.
- **Fallback**: Any non-structured file or syntactically invalid document falls back gracefully to `diffy::merge` line-based 3-way merge.

## 4. Controlled Template Interpolation
- Single-pass (non-recursive) substitution for `{{ target.name }}`, `{{ target.path }}`, and `{{ vars.<key> }}`.
- Target-level `vars` override workspace-level `[vars]`.
- Expressions preceded by `$` (such as GitHub Actions `${{ secrets.TOKEN }}`) and unrecognized namespaces (such as Helm `{{ .Values.image }}`) are preserved verbatim.
- Unknown `target.<field>` or undefined `vars.<key>` references fail immediately with `DriftError::UndefinedTemplateVar`.

## 5. Git Branch, Remote Clone & PR Automation (`driftsync-git`)
- Validates branch names against `git-check-ref-format` rules and rejects leading `-` flags.
- Verifies clean working trees (`git status --porcelain`) before switching branches unless `--allow-dirty` is explicitly passed.
- Stages and commits only managed files plus `.driftsync.lock`.
- Supports `repo = "owner/repo"` or HTTPS/SSH URLs via shallow cloning (`git clone --depth 1`), branch pushing (`git push -u origin <branch>`), and Pull Request creation (`gh pr create`).

## 6. Security & Privacy Invariants (`security.rs`)
1. **Memory Safety**: `#![forbid(unsafe_code)]` enforced across all workspace crates.
2. **Dry-Run Default**: `apply` and `resolve` never mutate disk state unless `--write` is explicitly provided.
3. **Path & Symlink Containment**:
   - `validate_relative_path` rejects `..`, absolute paths, drive prefixes, `.git` (case-insensitive), `.driftsync.lock`, trailing dots/spaces, and Windows reserved device names (`CON`, `PRN`, `AUX`, `NUL`, `COM1..9`, `LPT1..9`).
   - `ensure_no_symlink_in_chain` inspects every path component via `fs::symlink_metadata` to prevent symlink escape attacks.
   - `read_bounded_file` enforces a 2 MiB ceiling (`MAX_MANAGED_FILE_BYTES`).
4. **Hardcoded Secret Prevention & Redaction**:
   - `scan_for_hardcoded_secrets` blocks PEM private keys, GitHub PATs (`ghp_*`, `github_pat_*`), GitLab PATs (`glpat-*`), AWS Access Key IDs (`AKIA*`), Slack tokens (`xoxb-*`), and `sk-*` API keys in templates and `drift.toml` variables.
   - `redact_sensitive_text` masks URL credentials (`https://user:pass@host`) and token prefixes in diffs, Markdown summaries, and Git error messages.
5. **Untrusted Git Hook Isolation**:
   - All Git invocations set `GIT_TERMINAL_PROMPT=0` and pass `-c protocol.ext.allow=never` and `-c core.fsmonitor=false`.
   - Commits pass `--no-verify` so untrusted `.git/hooks/pre-commit` scripts in target repositories cannot execute arbitrary code.
   - Remote specifications reject unencrypted `http://`, `file://`, `ext::`, and embedded URL credentials.
