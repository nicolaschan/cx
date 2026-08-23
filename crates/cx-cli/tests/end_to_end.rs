//! End-to-end: build a real git repo, make a branch with a mix of changes
//! (novel add, pure move, lockfile, binary), and check the report tells
//! the story the metrics promise.

use std::fs;
use std::path::Path;
use std::process::Command;

use cx_cli::git::{Git, Status};
use cx_cli::pipeline::{self, AbsOptions, DiffOptions};

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

use cx_core::testgen::code as gen_code;

fn setup() -> (tempfile::TempDir, Git) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);

    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/keep.rs"), gen_code(1, 120)).unwrap();
    fs::write(root.join("src/mover.rs"), gen_code(2, 120)).unwrap();
    fs::write(root.join("src/gone.rs"), gen_code(3, 120)).unwrap();
    fs::write(root.join("Cargo.lock"), "# lockfile\nversion = 3\n").unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);

    git(root, &["checkout", "-q", "-b", "feature"]);
    // Novel logic: new content unlike anything in the tree.
    fs::write(root.join("src/novel.rs"), gen_code(99, 120)).unwrap();
    // A substantial new test, for --ignore-tests.
    fs::create_dir(root.join("tests")).unwrap();
    fs::write(root.join("tests/novel_test.rs"), gen_code(77, 120)).unwrap();
    // Pure move: same bytes, new path.
    git(root, &["mv", "src/mover.rs", "src/moved.rs"]);
    // Deletion of unique content.
    fs::remove_file(root.join("src/gone.rs")).unwrap();
    // Lockfile churn and a binary blob: both must be skipped.
    fs::write(root.join("Cargo.lock"), "# lockfile\nversion = 4\nmore\n").unwrap();
    fs::write(
        root.join("logo.png"),
        b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0binary",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "feature"]);

    let git = Git::discover_at(root).unwrap();
    (dir, git)
}

#[test]
fn scores_a_realistic_branch() {
    let (_dir, git) = setup();
    let report = pipeline::diff(&git, &DiffOptions::default()).unwrap();

    let by_path = |p: &str| {
        report
            .files
            .iter()
            .find(|f| f.path == p)
            .unwrap_or_else(|| panic!("{p} missing from report"))
    };

    let novel = by_path("src/novel.rs");
    assert!(
        novel.review_bytes > 500.0,
        "novel logic must cost review attention"
    );
    assert!(novel.delta_bytes > 500.0, "novel logic must add complexity");

    let moved = by_path("src/moved.rs");
    assert_eq!(
        moved.status,
        Status::Renamed {
            from: "src/mover.rs".into()
        }
    );
    assert!(
        moved.review_bytes < 64.0,
        "pure move must be ≈ free to review"
    );
    assert!(
        moved.delta_bytes.abs() < 64.0,
        "pure move must not change complexity"
    );

    let gone = by_path("src/gone.rs");
    assert_eq!(gone.status, Status::Deleted);
    assert!(
        gone.delta_bytes < -500.0,
        "deleting unique content refunds complexity"
    );

    let skipped: Vec<&str> = report.skipped.iter().map(|s| s.path.as_str()).collect();
    assert!(
        skipped.contains(&"Cargo.lock"),
        "lockfile churn must be skipped"
    );
    assert!(skipped.contains(&"logo.png"), "binary must be skipped");

    for scale in [
        report.scales.review,
        report.scales.delta_new,
        report.scales.delta_old,
    ] {
        assert!(
            (0.5..=1.5).contains(&scale),
            "implausible scale factor {scale}"
        );
    }
}

/// What excluding tests must and must not do to the numbers. That the
/// flag reaches scoring at all is covered through the binary below.
#[test]
fn ignoring_tests_drops_their_cost_and_leaves_the_rest() {
    let (_dir, git) = setup();
    let scored = |ignore_tests| {
        pipeline::diff(
            &git,
            &DiffOptions {
                ignore_tests,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let review = |r: &cx_cli::pipeline::DiffReport, path| {
        r.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.review_bytes)
    };
    let (with, without) = (scored(false), scored(true));

    assert!(
        review(&with, "tests/novel_test.rs").is_some_and(|b| b > 500.0),
        "the fixture's test must be substantial enough to matter"
    );
    assert_eq!(review(&without, "tests/novel_test.rs"), None);
    assert!(
        without.totals.review_bytes < with.totals.review_bytes,
        "dropping a substantial test must lower the total"
    );
    // Production is conditioned on the same reference either way, since
    // an excluded test was never part of it.
    let (before, after) = (
        review(&with, "src/novel.rs").unwrap(),
        review(&without, "src/novel.rs").unwrap(),
    );
    assert!(
        (before - after).abs() < 0.25 * before,
        "production scores should barely move: {before} vs {after}"
    );
}

/// The environment default through the real binary: a pinned value is
/// only useful if it reaches scoring and a single run can still veto it.
#[test]
fn ignore_tests_can_be_pinned_through_the_environment() {
    let (dir, _git) = setup();
    for (pinned, flag, expected) in [
        (None, None, false),
        (Some("1"), None, true),
        (Some("true"), None, true),
        // A set variable must not mean "true" whatever its value.
        (Some("0"), None, false),
        (Some("1"), Some("--ignore-tests=false"), false),
        (None, Some("--ignore-tests"), true),
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cx"));
        cmd.current_dir(dir.path())
            .args(["diff", "--json"])
            .args(flag);
        match pinned {
            Some(value) => cmd.env("CX_IGNORE_TESTS", value),
            // An inherited setting must not decide this test's outcome.
            None => cmd.env_remove("CX_IGNORE_TESTS"),
        };
        let out = cmd.output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let ignored = report["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["path"] == "tests/novel_test.rs" && s["reason"] == "test");
        assert_eq!(ignored, expected, "CX_IGNORE_TESTS={pinned:?}, {flag:?}");
    }
}

#[test]
fn staged_mode_scores_the_index() {
    let (dir, git) = setup();
    let root = dir.path();
    // Stage a new novel file on top of the feature branch, don't commit.
    fs::write(root.join("src/staged.rs"), gen_code(123, 80)).unwrap();
    Command::new("git")
        .current_dir(root)
        .args(["add", "src/staged.rs"])
        .status()
        .unwrap();

    let report = pipeline::diff(
        &git,
        &DiffOptions {
            staged: true,
            ..Default::default()
        },
    )
    .unwrap();
    let staged = report.files.iter().find(|f| f.path == "src/staged.rs");
    assert!(
        staged.is_some_and(|f| f.review_bytes > 300.0),
        "staged file must be scored"
    );
}

#[test]
fn tree_reports_absolute_complexity_with_contributions() {
    let (_dir, git) = setup();
    let report = pipeline::abs(&git, &AbsOptions::default()).unwrap();
    // keep.rs + moved.rs + novel.rs + tests/novel_test.rs;
    // Cargo.lock and logo.png excluded.
    assert_eq!(report.file_count, 4, "kept files at HEAD");
    assert!(report.compressed_bytes > 0);
    assert!(report.compressed_bytes < report.raw_bytes);

    assert_eq!(report.files.len(), 4);
    let sum: f64 = report.files.iter().map(|f| f.bytes).sum();
    assert!(
        (sum - report.compressed_bytes as f64).abs() < 1e-6 * sum,
        "contributions must sum to C(tree): {sum} vs {}",
        report.compressed_bytes
    );
    assert!(
        report.files.windows(2).all(|w| w[0].bytes >= w[1].bytes),
        "contributions must be sorted descending"
    );
    assert!(report.files.iter().all(|f| f.lines > 0));
}

#[test]
fn tree_contributions_are_suppressable() {
    let (_dir, git) = setup();
    let report = pipeline::abs(
        &git,
        &AbsOptions {
            no_files: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(report.file_count, 4);
    assert!(report.files.is_empty());
    assert_eq!(report.scale, 1.0);
}
