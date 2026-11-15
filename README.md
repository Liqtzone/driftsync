# driftsync

**Multi-repository 3-way structural configuration drift & sync engine.**

`driftsync` synchronizes shared configuration files (`.github/workflows/*.yml`, `Cargo.toml`, `package.json`, `.editorconfig`, linter rules) across dozens of repositories **without overwriting intentional local customizations**.

Unlike blind file-copying bots that clobber service-specific overrides on every upstream change, `driftsync` tracks the last-synced ancestor (`base`) in a lightweight `.driftsync.lock` file and performs **format-aware 3-way structural merges** across `base`, `ours` (target repo), and `theirs` (canonical template).

---

## Key Capabilities

- **True 3-Way Drift Classification:** Distinguishes between `InSync`, `UpstreamChanged` (fast-forward), `LocallyModified` (preserved), `BothChangedClean` (auto-merged), and `BothChangedConflict` (never clobbered).
- **Format-Aware Structural 3-Way Merge:**
  - **TOML (`toml_edit`):** Key/table/inline-table merge that preserves target comments and formatting.
  - **JSON (`serde_json`):** Recursive object key merge without false-positive adjacent-line conflicts.
  - **YAML (block-aware):** Indentation, comment (`#`), mapping, and sequence (`steps:`) block merge tailored for GitHub Actions and Kubernetes manifests.
  - **Text (`diffy`):** Standard 3-way line merge fallback.
- **Controlled Template Interpolation:** Single-pass substitution for `{{ target.name }}`, `{{ target.path }}`, and `{{ vars.<key> }}` while leaving GitHub Actions `${{ ... }}` and Helm `{{ .Values.* }}` untouched.
- **Git Branch, Push & Pull Request Automation:** Automatically clones remote repositories (`repo = "org/service-auth"`), creates sync branches, commits only managed files, pushes, and opens rich Markdown Pull Requests via `gh`.
- **CI/CD Drift Auditing:** `driftsync check` (`--fail-on-drift`) exits non-zero on actionable drift and writes collapsible unified diff tables to `$GITHUB_STEP_SUMMARY`.
- **Hardened Security & Privacy:**
  - `#![forbid(unsafe_code)]`, dry-run by default (`--write` required for disk mutation).
  - Pre-sync secret scanner blocks accidental leaks of PEM private keys, GitHub PATs (`ghp_*`, `github_pat_*`), GitLab PATs (`glpat-*`), AWS keys (`AKIA*`), Slack tokens (`xoxb-*`), and API keys (`sk-*`).
  - Automatic redaction of embedded URL credentials and tokens in diffs, Markdown reports, and Git error outputs.
  - Path traversal, symlink chain (`fs::symlink_metadata`), `.git` hook directory, Windows reserved device name, and 2 MiB file size guards.
  - Untrusted target repo Git hooks are bypassed (`--no-verify`, `-c core.fsmonitor=false`, `-c protocol.ext.allow=never`).

---

## Installation

### From Source (Cargo)

```bash
cargo install --path crates/driftsync-cli --locked
```

### Shell / PowerShell Installer

```bash
# Linux & macOS
curl -fsSL https://raw.githubusercontent.com/Liqtzone/driftsync/main/scripts/install.sh | sh
```

```powershell
# Windows PowerShell
irm https://raw.githubusercontent.com/Liqtzone/driftsync/main/scripts/install.ps1 | iex
```

### Homebrew

```bash
brew install Formula/driftsync.rb
```

---

## Quick Start (60-Second Demo)

```bash
# 1. Scaffold a 3-repo demo workspace with realistic drift scenarios
driftsync --root demo-workspace init --demo

# 2. Inspect fleet-wide drift status
driftsync --root demo-workspace status

# 3. Preview proposed 3-way structural merges
driftsync --root demo-workspace diff

# 4. Apply changes, checkout 'chore/config-sync', and commit per target repo
driftsync --root demo-workspace apply --write --branch chore/config-sync

# 5. Run CI/CD audit check (exits 0 when all repos are in sync or locally preserved)
driftsync --root demo-workspace check
```

---

## Configuration (`drift.toml`)

```toml
version = 1

[vars]
rust_version = "1.85"
port = "8080"

[[targets]]
name = "service-auth"
path = "repos/service-auth"
# Or sync directly against a remote GitHub repository:
# repo = "org/service-auth"

[[targets]]
name = "service-billing"
path = "repos/service-billing"
vars = { port = "9091" }

[[files]]
source = "templates/.editorconfig"
target = ".editorconfig"
strategy = "three-way" # "three-way" (default) | "overwrite" | "keep-local"

[[files]]
source = "templates/ci.yml"
target = ".github/workflows/ci.yml"
strategy = "three-way"
```

---

## CLI Reference

| Command | Description |
|---|---|
| `driftsync init [--demo]` | Create a starter `drift.toml` and `templates/` directory (or full 3-repo demo). |
| `driftsync status [--fail-on-drift] [--markdown] [--step-summary]` | Classify and report drift across all target repositories. |
| `driftsync check [--markdown]` | CI/CD audit mode (`--fail-on-drift` + `$GITHUB_STEP_SUMMARY`). |
| `driftsync diff` | Display colorized unified diffs of proposed 3-way merges. |
| `driftsync adopt` | Record `.driftsync.lock` baselines for existing repos without overwriting local edits. |
| `driftsync resolve [--strategy interactive\|ours\|theirs] [--write]` | Resolve conflicting or untracked files interactively or by policy. |
| `driftsync apply [--write] [--branch <name>] [--push] [--pr]` | Apply planned updates and 3-way merges (dry-run without `--write`). |
| `driftsync pr [--branch <name>] [--write] [--push] [--open-gh]` | Sync target repos onto a branch and open Pull Requests with Markdown diff tables. |

---

## GitHub Action Usage

```yaml
name: Config Drift Audit
on:
  pull_request:
  schedule:
    - cron: "0 6 * * 1"

jobs:
  drift:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: ./ # or example/driftsync@v0.1.0
        with:
          mode: check # 'check' | 'status' | 'pr'
          github-token: ${{ secrets.GITHUB_TOKEN }}
```

---

## Workspace Architecture

- `crates/driftsync-core`: Pure domain logic, manifest/lockfile models, path & symlink validation, secret scanner, output redaction, template interpolation, and TOML/JSON/YAML/text 3-way merge engines.
- `crates/driftsync-git`: Hardened Git operations (`rev-parse`, clean working tree check, branch creation, hook-isolated commits, shallow remote clones, branch push, and `gh pr create`).
- `crates/driftsync-cli`: `clap`-powered CLI, terminal tables, unified diff viewer, interactive conflict resolver, JSON output, and GitHub Step Summary integration.

---

## License

Licensed under the **Elastic License 2.0 (ELv2)**. See [LICENSE](LICENSE) for the full text.

- **You may:** Freely use, modify, copy, and distribute this software for personal or commercial projects.
- **You may not:** Provide this software to third parties as a hosted or managed service that provides substantially similar functionality, or remove attribution notices.
