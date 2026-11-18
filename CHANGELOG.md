# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-11-19

### Fixed
- **Tracked Deletion vs. Missing File Distinction:** Fixed an edge-case bug in `evaluate_file` where missing target files were unconditionally matched as `(_, None) => MissingInTarget`, causing `apply --write` to resurrect intentionally deleted files even under `KeepLocal` sync strategy.
- **Delete/Modify Conflict Detection:** Upstream modifications to locally deleted files now correctly trigger a 3-way `BothChangedConflict` (delete/modify conflict) requiring explicit resolution (`driftsync resolve`) instead of silently recreating the file.

### Added
- **`DriftStatus::LocallyDeleted`:** New status badge (`🛡️ LOCALLY-DELETED (KEPT)`) that preserves intentional local file deletions when upstream has not changed (`theirs == base`).
- **ADR-006:** Documented *Tracked Deletion & Delete/Modify Conflict Handling* in `DECISIONS.md` and updated Section 2 Drift Classification Matrix in `SPEC.md`.
- End-to-end integration tests in `workspace_flow.rs` and unit tests in `driftsync-core` verifying local deletion preservation and delete/modify resolution.

### Acknowledgments & Community Credits
- Special thanks to Reddit community members **[/u/MongooseWonderful198](https://www.reddit.com/user/MongooseWonderful198)** and **[/u/Steel_half2](https://www.reddit.com/user/Steel_half2)** for in-depth source code review on `crates/driftsync-core` and identifying this 3-way merge edge case!

---

## [0.1.0] - 2026-11-18

### Added
- **Multi-Repo 3-Way Structural Merge Engine:**
  - Stateful ancestor tracking via per-target `.driftsync.lock`.
  - Format-aware structural merge for `.toml` (via `toml_edit` preserving comments), `.json` (via recursive `serde_json`), and `.yml`/`.yaml` (block-aware merge).
  - Line-based fallback via `diffy` for non-structured text.
- **Pre-Flight Secret Scanning & Redaction:**
  - Blocks hardcoded PEM private keys, GitHub PATs, GitLab PATs, AWS Access Keys, and Slack tokens in templates and variables.
  - Automatically redacts URL credentials and tokens in unified diffs and Markdown reports.
- **Controlled Template Interpolation:** Single-pass substitution for `{{ target.name }}`, `{{ target.path }}`, and `{{ vars.<key> }}` while preserving GitHub Actions `${{ ... }}` expressions.
- **Hardened Git Automation (`driftsync-git`):**
  - Path traversal and Windows reserved device name validation.
  - Deep symlink chain verification preventing symlink escapes.
  - Git hook execution isolation (`--no-verify`, `protocol.ext.allow=never`).
  - Branch creation, clean working tree enforcement, and Pull Request creation (`gh pr create`).
- **CLI Subcommands:** `init`, `status`, `check`, `diff`, `adopt`, `resolve`, `apply`, `pr`.
- **Licensing:** Released under Elastic License 2.0 (ELv2).
