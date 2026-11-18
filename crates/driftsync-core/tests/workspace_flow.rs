use driftsync_core::{
    DriftStatus, LockFile, LockedFileEntry, ManagedFile, Manifest, PlanAction, ResolutionChoice,
    SyncStrategy, TargetRepo, apply_workspace, evaluate_workspace, hash_content,
    resolve_target_file,
};
use std::fs;

#[test]
fn end_to_end_workspace_sync_preserves_local_and_merges_three_way() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let base_ci = "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test\n";
    let upstream_ci = "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-24.04\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test\n";
    let target_custom_ci = "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      - run: cargo test\n      - run: cargo clippy\n";

    fs::create_dir_all(root.join("templates")).expect("templates dir");
    fs::write(root.join("templates/ci.yml"), upstream_ci).expect("write upstream");

    let svc_dir = root.join("repos/svc-a");
    fs::create_dir_all(svc_dir.join(".github/workflows")).expect("svc workflows");
    fs::write(svc_dir.join(".github/workflows/ci.yml"), target_custom_ci).expect("write target");

    let mut lock = LockFile::default();
    lock.files.insert(
        ".github/workflows/ci.yml".to_string(),
        LockedFileEntry {
            base_hash: hash_content(base_ci),
            base_content: base_ci.to_string(),
        },
    );
    lock.save(&svc_dir).expect("save lock");

    let manifest = Manifest {
        version: 1,
        vars: Default::default(),
        targets: vec![TargetRepo {
            name: "svc-a".to_string(),
            path: "repos/svc-a".to_string(),
            repo: None,
            vars: Default::default(),
        }],
        files: vec![ManagedFile {
            source: "templates/ci.yml".to_string(),
            target: ".github/workflows/ci.yml".to_string(),
            strategy: SyncStrategy::ThreeWay,
        }],
    };

    let (reports, dry_summary) =
        apply_workspace(root, &manifest, Some("deadbeef".to_string()), false).expect("dry apply");
    assert!(dry_summary.dry_run);
    assert_eq!(dry_summary.files_written, 1);
    assert_eq!(
        reports[0].evaluations[0].status,
        DriftStatus::BothChangedClean
    );
    let disk_before =
        fs::read_to_string(svc_dir.join(".github/workflows/ci.yml")).expect("read before");
    assert_eq!(disk_before, target_custom_ci);

    let (_, write_summary) =
        apply_workspace(root, &manifest, Some("deadbeef".to_string()), true).expect("write apply");
    assert!(!write_summary.dry_run);
    assert_eq!(write_summary.files_written, 1);

    let merged_on_disk =
        fs::read_to_string(svc_dir.join(".github/workflows/ci.yml")).expect("read merged");
    assert!(merged_on_disk.contains("ubuntu-24.04"));
    assert!(merged_on_disk.contains("cargo clippy"));

    let post_reports = evaluate_workspace(root, &manifest).expect("post eval");
    assert_eq!(
        post_reports[0].evaluations[0].status,
        DriftStatus::LocallyModified
    );
}

#[test]
fn end_to_end_structural_toml_and_json_sync() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let base_toml =
        "[package]\nname = \"svc\"\nedition = \"2021\"\n\n[dependencies]\nserde = \"1.0\"\n";
    let upstream_toml = "[package]\nname = \"svc\"\nedition = \"2024\"\nrust-version = \"1.85\"\n\n[dependencies]\nserde = \"1.0\"\ntracing = \"0.1\"\n";
    let target_toml = "# Custom service configuration\n[package]\nname = \"svc\"\nedition = \"2021\"\npublish = false\n\n[dependencies]\n# Local dependency\nredis = \"0.27\"\nserde = \"1.0\"\n";

    let base_json = "{\n  \"name\": \"svc\",\n  \"scripts\": {\n    \"test\": \"vitest\"\n  }\n}\n";
    let upstream_json = "{\n  \"name\": \"svc\",\n  \"scripts\": {\n    \"lint\": \"biome check\",\n    \"test\": \"vitest run\"\n  }\n}\n";
    let target_json = "{\n  \"name\": \"svc\",\n  \"scripts\": {\n    \"dev\": \"vite\",\n    \"test\": \"vitest\"\n  }\n}\n";

    fs::create_dir_all(root.join("templates")).expect("templates dir");
    fs::write(root.join("templates/Cargo.toml"), upstream_toml).expect("write upstream toml");
    fs::write(root.join("templates/package.json"), upstream_json).expect("write upstream json");

    let svc_dir = root.join("repos/svc-structural");
    fs::create_dir_all(&svc_dir).expect("svc dir");
    fs::write(svc_dir.join("Cargo.toml"), target_toml).expect("write target toml");
    fs::write(svc_dir.join("package.json"), target_json).expect("write target json");

    let mut lock = LockFile::default();
    lock.files.insert(
        "Cargo.toml".to_string(),
        LockedFileEntry {
            base_hash: hash_content(base_toml),
            base_content: base_toml.to_string(),
        },
    );
    lock.files.insert(
        "package.json".to_string(),
        LockedFileEntry {
            base_hash: hash_content(base_json),
            base_content: base_json.to_string(),
        },
    );
    lock.save(&svc_dir).expect("save lock");

    let manifest = Manifest {
        version: 1,
        vars: Default::default(),
        targets: vec![TargetRepo {
            name: "svc-structural".to_string(),
            path: "repos/svc-structural".to_string(),
            repo: None,
            vars: Default::default(),
        }],
        files: vec![
            ManagedFile {
                source: "templates/Cargo.toml".to_string(),
                target: "Cargo.toml".to_string(),
                strategy: SyncStrategy::ThreeWay,
            },
            ManagedFile {
                source: "templates/package.json".to_string(),
                target: "package.json".to_string(),
                strategy: SyncStrategy::ThreeWay,
            },
        ],
    };

    let (reports, summary) =
        apply_workspace(root, &manifest, Some("cafebabe".to_string()), true).expect("apply");
    assert_eq!(summary.files_written, 2);
    assert_eq!(summary.conflicts_skipped, 0);
    assert_eq!(
        reports[0].evaluations[0].status,
        DriftStatus::BothChangedClean
    );
    assert_eq!(
        reports[0].evaluations[1].status,
        DriftStatus::BothChangedClean
    );

    let merged_toml = fs::read_to_string(svc_dir.join("Cargo.toml")).expect("read merged toml");
    assert!(merged_toml.contains("# Custom service configuration"));
    assert!(merged_toml.contains("# Local dependency"));
    assert!(merged_toml.contains("edition = \"2024\""));
    assert!(merged_toml.contains("rust-version = \"1.85\""));
    assert!(merged_toml.contains("publish = false"));
    assert!(merged_toml.contains("redis = \"0.27\""));
    assert!(merged_toml.contains("tracing = \"0.1\""));

    let merged_json = fs::read_to_string(svc_dir.join("package.json")).expect("read merged json");
    let parsed_json: serde_json::Value = serde_json::from_str(&merged_json).expect("valid json");
    assert_eq!(parsed_json["scripts"]["dev"], "vite");
    assert_eq!(parsed_json["scripts"]["lint"], "biome check");
    assert_eq!(parsed_json["scripts"]["test"], "vitest run");
}

#[test]
fn end_to_end_template_variable_interpolation_per_target() {
    use std::collections::BTreeMap;

    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let template_toml = r#"[package]
name = "{{ target.name }}"
edition = "{{ vars.edition }}"

[server]
port = "{{ vars.port }}"
"#;

    fs::create_dir_all(root.join("templates")).expect("templates dir");
    fs::write(root.join("templates/Cargo.toml"), template_toml).expect("write template");

    let auth_dir = root.join("repos/service-auth");
    let billing_dir = root.join("repos/service-billing");
    fs::create_dir_all(&auth_dir).expect("auth dir");
    fs::create_dir_all(&billing_dir).expect("billing dir");

    let mut global_vars = BTreeMap::new();
    global_vars.insert("edition".to_string(), "2024".to_string());
    global_vars.insert("port".to_string(), "8080".to_string());

    let mut billing_vars = BTreeMap::new();
    billing_vars.insert("port".to_string(), "9090".to_string());

    let manifest = Manifest {
        version: 1,
        vars: global_vars,
        targets: vec![
            TargetRepo {
                name: "service-auth".to_string(),
                path: "repos/service-auth".to_string(),
                repo: None,
                vars: BTreeMap::new(),
            },
            TargetRepo {
                name: "service-billing".to_string(),
                path: "repos/service-billing".to_string(),
                repo: None,
                vars: billing_vars,
            },
        ],
        files: vec![ManagedFile {
            source: "templates/Cargo.toml".to_string(),
            target: "Cargo.toml".to_string(),
            strategy: SyncStrategy::ThreeWay,
        }],
    };

    let (_, summary) =
        apply_workspace(root, &manifest, Some("11223344".to_string()), true).expect("apply");
    assert_eq!(summary.files_written, 2);

    let auth_toml = fs::read_to_string(auth_dir.join("Cargo.toml")).expect("read auth");
    assert!(auth_toml.contains("name = \"service-auth\""));
    assert!(auth_toml.contains("edition = \"2024\""));
    assert!(auth_toml.contains("port = \"8080\""));

    let billing_toml = fs::read_to_string(billing_dir.join("Cargo.toml")).expect("read billing");
    assert!(billing_toml.contains("name = \"service-billing\""));
    assert!(billing_toml.contains("edition = \"2024\""));
    assert!(billing_toml.contains("port = \"9090\""));

    let post_reports = evaluate_workspace(root, &manifest).expect("post eval");
    assert_eq!(post_reports[0].evaluations[0].status, DriftStatus::InSync);
    assert_eq!(post_reports[1].evaluations[0].status, DriftStatus::InSync);
}

#[test]
fn end_to_end_conflict_resolution_ours_and_theirs() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let base_cfg = "timeout = 30\n";
    let upstream_cfg = "timeout = 15\n";
    let target_cfg = "timeout = 60\n";

    fs::create_dir_all(root.join("templates")).expect("templates dir");
    fs::write(root.join("templates/cfg.toml"), upstream_cfg).expect("write upstream");

    let svc_dir = root.join("repos/svc-conflict");
    fs::create_dir_all(&svc_dir).expect("svc dir");
    fs::write(svc_dir.join("cfg.toml"), target_cfg).expect("write target");

    let mut lock = LockFile::default();
    lock.files.insert(
        "cfg.toml".to_string(),
        LockedFileEntry {
            base_hash: hash_content(base_cfg),
            base_content: base_cfg.to_string(),
        },
    );
    lock.save(&svc_dir).expect("save lock");

    let target = TargetRepo {
        name: "svc-conflict".to_string(),
        path: "repos/svc-conflict".to_string(),
        repo: None,
        vars: Default::default(),
    };
    let file = ManagedFile {
        source: "templates/cfg.toml".to_string(),
        target: "cfg.toml".to_string(),
        strategy: SyncStrategy::ThreeWay,
    };
    let manifest = Manifest {
        version: 1,
        vars: Default::default(),
        targets: vec![target.clone()],
        files: vec![file.clone()],
    };

    let initial = evaluate_workspace(root, &manifest).expect("initial eval");
    assert_eq!(
        initial[0].evaluations[0].status,
        DriftStatus::BothChangedConflict
    );

    resolve_target_file(
        root,
        &target,
        &file,
        &manifest.vars,
        Some("rev1"),
        ResolutionChoice::Ours,
        true,
    )
    .expect("resolve ours");

    let after_ours = evaluate_workspace(root, &manifest).expect("after ours");
    assert_eq!(
        after_ours[0].evaluations[0].status,
        DriftStatus::LocallyModified
    );
    assert_eq!(
        fs::read_to_string(svc_dir.join("cfg.toml")).expect("read"),
        target_cfg
    );

    resolve_target_file(
        root,
        &target,
        &file,
        &manifest.vars,
        Some("rev2"),
        ResolutionChoice::Theirs,
        true,
    )
    .expect("resolve theirs");

    let after_theirs = evaluate_workspace(root, &manifest).expect("after theirs");
    assert_eq!(after_theirs[0].evaluations[0].status, DriftStatus::InSync);
    assert_eq!(
        fs::read_to_string(svc_dir.join("cfg.toml")).expect("read"),
        upstream_cfg
    );
}

#[test]
fn end_to_end_tracked_local_deletion_and_delete_modify_conflict() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();

    let initial_conf = "enabled = true\n";
    let updated_upstream_conf = "enabled = true\nport = 8080\n";

    fs::create_dir_all(root.join("templates")).expect("templates dir");
    fs::write(root.join("templates/service.conf"), initial_conf).expect("write upstream");

    let svc_dir = root.join("repos/svc-del");
    fs::create_dir_all(&svc_dir).expect("svc dir");
    fs::write(svc_dir.join("service.conf"), initial_conf).expect("write svc conf");

    let mut lock = LockFile::default();
    lock.files.insert(
        "service.conf".to_string(),
        LockedFileEntry {
            base_hash: hash_content(initial_conf),
            base_content: initial_conf.to_string(),
        },
    );
    lock.save(&svc_dir).expect("save lock");

    let target = TargetRepo {
        name: "svc-del".to_string(),
        path: "repos/svc-del".to_string(),
        repo: None,
        vars: Default::default(),
    };
    let file = ManagedFile {
        source: "templates/service.conf".to_string(),
        target: "service.conf".to_string(),
        strategy: SyncStrategy::ThreeWay,
    };
    let manifest = Manifest {
        version: 1,
        vars: Default::default(),
        targets: vec![target.clone()],
        files: vec![file.clone()],
    };

    // 1. Target intentionally deletes the tracked file locally
    fs::remove_file(svc_dir.join("service.conf")).expect("delete local file");

    let reports = evaluate_workspace(root, &manifest).expect("eval");
    assert_eq!(
        reports[0].evaluations[0].status,
        DriftStatus::LocallyDeleted
    );
    assert_eq!(reports[0].evaluations[0].plan, PlanAction::KeepLocal);
    assert!(reports[0].evaluations[0].unified_diff.is_empty());

    // 2. apply --write must NOT resurrect the deleted file when upstream is unchanged
    let (_, summary) =
        apply_workspace(root, &manifest, Some("rev1".to_string()), true).expect("apply");
    assert_eq!(summary.files_written, 0);
    assert!(
        !svc_dir.join("service.conf").exists(),
        "deleted file must not be recreated"
    );

    // 3. Upstream modifies the template while target deleted it -> delete/modify conflict!
    fs::write(root.join("templates/service.conf"), updated_upstream_conf).expect("update upstream");

    let conflict_reports = evaluate_workspace(root, &manifest).expect("eval conflict");
    assert_eq!(
        conflict_reports[0].evaluations[0].status,
        DriftStatus::BothChangedConflict
    );
    assert!(matches!(
        conflict_reports[0].evaluations[0].plan,
        PlanAction::Conflict { .. }
    ));

    // apply --write skips conflicts
    let (_, conflict_summary) =
        apply_workspace(root, &manifest, Some("rev2".to_string()), true).expect("apply conflict");
    assert_eq!(conflict_summary.conflicts_skipped, 1);
    assert_eq!(conflict_summary.files_written, 0);
    assert!(!svc_dir.join("service.conf").exists());

    // 4. Resolve with Ours (keep deletion)
    resolve_target_file(
        root,
        &target,
        &file,
        &manifest.vars,
        Some("rev2"),
        ResolutionChoice::Ours,
        true,
    )
    .expect("resolve ours");

    assert!(!svc_dir.join("service.conf").exists());
    let after_ours = evaluate_workspace(root, &manifest).expect("after ours");
    assert_eq!(
        after_ours[0].evaluations[0].status,
        DriftStatus::LocallyDeleted
    );
    assert_eq!(after_ours[0].evaluations[0].plan, PlanAction::KeepLocal);

    // 5. Resolve with Theirs (recreate from upstream)
    resolve_target_file(
        root,
        &target,
        &file,
        &manifest.vars,
        Some("rev2"),
        ResolutionChoice::Theirs,
        true,
    )
    .expect("resolve theirs");

    assert!(svc_dir.join("service.conf").exists());
    assert_eq!(
        fs::read_to_string(svc_dir.join("service.conf")).expect("read"),
        updated_upstream_conf
    );
    let after_theirs = evaluate_workspace(root, &manifest).expect("after theirs");
    assert_eq!(after_theirs[0].evaluations[0].status, DriftStatus::InSync);
}
