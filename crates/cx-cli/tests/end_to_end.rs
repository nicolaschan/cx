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
    let report = pipeline::diff(
        &git,
        &DiffOptions {
            base: None,
            staged: false,
        },
    )
    .unwrap();

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
            base: None,
            staged: true,
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
    let report = pipeline::abs(&git, &AbsOptions { with_files: true }).unwrap();
    // keep.rs + moved.rs + novel.rs; Cargo.lock and logo.png excluded.
    assert_eq!(report.file_count, 3, "kept files at HEAD");
    assert!(report.compressed_bytes > 0);
    assert!(report.compressed_bytes < report.raw_bytes);

    assert_eq!(report.files.len(), 3);
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
    let report = pipeline::abs(&git, &AbsOptions { with_files: false }).unwrap();
    assert_eq!(report.file_count, 3);
    assert!(report.files.is_empty());
    assert_eq!(report.scale, 1.0);
}
