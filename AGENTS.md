# AGENTS.md — AI-Assisted Development Guidelines

This repository follows strict architectural and security invariants across every session:

1. **Specification First**: Check and update `SPEC.md` before changing core behavior.
2. **Architectural Boundaries**:
   - `crates/driftsync-core`: Pure domain logic, manifest/lockfile models, path & symlink validation, secret scanning, drift classification, and 3-way structural merging. Never invokes network or `git` subprocesses.
   - `crates/driftsync-git`: Hardened Git subprocess operations (`rev-parse`, `status`, clean working tree checks, branch management, shallow clones, and PR creation).
   - `crates/driftsync-cli`: `clap`-based CLI (`init`, `status`, `check`, `diff`, `adopt`, `resolve`, `apply`, `pr`), colorized terminal output, Markdown reports, and `--json` output.
3. **Security Invariants**:
   - `#![forbid(unsafe_code)]` must never be removed.
   - No relative path may touch the filesystem without passing `validate_relative_path` and `ensure_no_symlink_in_chain`.
   - `apply` and `resolve` must never write to disk without `--write`.
4. **Verification Commands** (must pass after every change):
   ```powershell
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all -- --check
   ```
