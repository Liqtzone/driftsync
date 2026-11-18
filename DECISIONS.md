# DECISIONS.md — Architectural Decision Records (ADR)

## ADR-001: Per-Target `.driftsync.lock` for 3-Way Merge Ancestor Tracking
- **Context:** Without knowing the exact upstream template version a target repository last synced from (`base`), a sync tool cannot tell whether a difference in `ours` is a local customization or an outdated upstream block.
- **Decision:** Store the normalized `base_content` and `blake3` hash of each managed file in `.driftsync.lock` inside the target repository root.
- **Consequences:** Enables deterministic, offline 3-way merges (`base`, `ours`, `theirs`) without requiring network calls or deep Git history traversal.

## ADR-002: Format-Aware Structural 3-Way Merging (TOML, JSON, YAML)
- **Context:** Line-based 3-way merge (`diffy`) triggers false-positive conflicts whenever upstream and target repositories add or edit adjacent keys in `Cargo.toml`, `package.json`, or `.github/workflows/ci.yml`.
- **Decision:** Route `.toml` files through `toml_edit` (preserving comments and decor), `.json` files through `serde_json::Value`, and `.yml`/`.yaml` files through a block-aware structural merger, falling back to `diffy` only when a file is unstructured text or syntactically invalid.
- **Consequences:** Adjacent key additions and workflow step appends merge cleanly without human intervention.

## ADR-003: Single-Pass Template Interpolation Preserving `${{ ... }}`
- **Context:** Shared CI workflows frequently contain GitHub Actions expressions (`${{ secrets.GITHUB_TOKEN }}`) alongside repo-specific parameters (`{{ target.name }}`, `{{ vars.port }}`).
- **Decision:** Implement a zero-dependency single-pass scanner that only evaluates `{{ target.* }}` and `{{ vars.* }}` when not preceded by `$`.
- **Consequences:** Prevents template injection, infinite recursion, and accidental corruption of GitHub Actions or Helm expressions.

## ADR-004: Pre-Flight Secret Scanning & Diff Redaction
- **Context:** A multi-repo sync tool can inadvertently broadcast a hardcoded API token across dozens of repositories and leak credentials into CI logs or Pull Request descriptions.
- **Decision:** Scan all templates, rendered outputs, and `drift.toml` variables before sync (`scan_for_hardcoded_secrets`) and redact URL credentials and token patterns in all diffs and error outputs (`redact_sensitive_text`).
- **Consequences:** Blocks accidental secret distribution at the source and guarantees privacy in CI/CD logs.

## ADR-005: Symlink Rejection & Untrusted Git Hook Isolation
- **Context:** Target repositories may be untrusted; a malicious target repo could place a symlink pointing outside the workspace or a `.git/hooks/pre-commit` script to achieve code execution during sync.
- **Decision:** Verify every path segment with `fs::symlink_metadata`, forbid `.git` as a managed path component, and execute Git commands with `-c core.fsmonitor=false`, `-c protocol.ext.allow=never`, `GIT_TERMINAL_PROMPT=0`, and `commit --no-verify`.
- **Consequences:** Eliminates symlink arbitrary file overwrite and Git hook RCE vectors.

## ADR-006: Tracked Deletion & Delete/Modify Conflict Handling
- **Context:** When a previously synced file is recorded in `.driftsync.lock` (`base`) but missing from the target repository (`ours = None`), treating it unconditionally as `MissingInTarget` and planning `Create` resurrects intentionally deleted files and silences delete/modify conflicts.
- **Decision:** Distinguish `(None, None)` (untracked missing file -> `MissingInTarget`, `Create`) from `(Some(base), None)` (tracked deletion). If upstream is unchanged (`theirs == base`), preserve the deletion (`LocallyDeleted`, plan `KeepLocal`). If upstream modified the file (`theirs != base`), report a 3-way `BothChangedConflict` (delete/modify conflict) requiring explicit resolution (`driftsync resolve`). Under `strategy = "overwrite"`, allow upstream to recreate the file.
- **Consequences:** Eliminates file resurrection bugs, respects local repo deletions as valid overrides under 3-way merge, and flags upstream changes to locally deleted files as conflicts.
