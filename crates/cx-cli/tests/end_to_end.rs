//! End-to-end: build a real git repo, make a branch with a mix of changes
//! (novel add, pure move, lockfile, binary), and check the report tells
//! the story the metrics promise.

use std::fs;
use std::path::Path;
use std::process::Command;

use cx_cli::git::{Git, Side, Status};
use cx_cli::pipeline::{self, AbsOptions, DiffOptions};
use cx_cli::progress::Progress;

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
    // A substantial new test, for --include-tests.
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
    let report = pipeline::diff(
        &git,
        &DiffOptions {
            side: Side::Head,
            ..Default::default()
        },
        Progress::default(),
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
        novel.review_bytes > 500,
        "novel logic must cost review attention"
    );
    assert!(novel.delta_bytes > 500, "novel logic must add complexity");

    let moved = by_path("src/moved.rs");
    assert_eq!(
        moved.status,
        Status::Renamed {
            from: "src/mover.rs".into()
        }
    );
    assert!(
        moved.review_bytes < 64,
        "pure move must be ≈ free to review"
    );
    assert!(
        moved.delta_bytes.abs() < 64,
        "pure move must not change complexity"
    );

    let gone = by_path("src/gone.rs");
    assert_eq!(gone.status, Status::Deleted);
    assert!(
        gone.delta_bytes < -500,
        "deleting unique content refunds complexity"
    );

    let skipped: Vec<&str> = report.skipped.iter().map(|s| s.path.as_str()).collect();
    assert!(
        skipped.contains(&"Cargo.lock"),
        "lockfile churn must be skipped"
    );
    assert!(skipped.contains(&"logo.png"), "binary must be skipped");
}

/// Line churn, counted as git counts it: the fixture adds novel.rs and
/// novel_test.rs (120 lines each) and deletes gone.rs (120), while the
/// rename of mover.rs and the skipped lockfile and binary count for
/// nothing. Tests are out by default, so novel_test.rs's 120 added
/// lines only appear once `--include-tests` asks for them.
#[test]
fn line_churn_counts_what_git_counts() {
    let (_dir, git) = setup();
    let report = pipeline::diff(&git, &DiffOptions::default(), Progress::default()).unwrap();
    assert_eq!(
        (report.totals.added_lines, report.totals.deleted_lines),
        (120, 120)
    );

    // Including the test file adds exactly its lines, no others.
    let with = pipeline::diff(
        &git,
        &DiffOptions {
            include_tests: true,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();
    assert_eq!(
        (with.totals.added_lines, with.totals.deleted_lines),
        (240, 120)
    );
}

/// What excluding tests must and must not do to the numbers. That the
/// flag reaches scoring at all is covered through the binary below.
#[test]
fn excluding_tests_drops_their_cost_and_leaves_the_rest() {
    let (_dir, git) = setup();
    let scored = |include_tests| {
        pipeline::diff(
            &git,
            &DiffOptions {
                include_tests,
                ..Default::default()
            },
            Progress::default(),
        )
        .unwrap()
    };
    let review = |r: &cx_cli::pipeline::DiffReport, path| {
        r.files
            .iter()
            .find(|f| f.path == path)
            .map(|f| f.review_bytes)
    };
    let (with, without) = (scored(true), scored(false));

    assert!(
        review(&with, "tests/novel_test.rs").is_some_and(|b| b > 500),
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
        before.abs_diff(after) < before / 4,
        "production scores should barely move: {before} vs {after}"
    );
}

/// The environment default through the real binary: a pinned value is
/// only useful if it reaches scoring and a single run can still veto it.
#[test]
fn include_tests_can_be_pinned_through_the_environment() {
    let (dir, _git) = setup();
    // `expected` is whether the test file ends up skipped, so it reads
    // as the negation of whatever asked for tests to be included.
    for (pinned, flag, expected) in [
        (None, None, true),
        (Some("1"), None, false),
        (Some("true"), None, false),
        // A set variable must not mean "true" whatever its value.
        (Some("0"), None, true),
        (Some("1"), Some("--include-tests=false"), true),
        (None, Some("--include-tests"), false),
    ] {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cx"));
        cmd.current_dir(dir.path())
            .args(["diff", "--json"])
            .args(flag);
        match pinned {
            Some(value) => cmd.env("CX_INCLUDE_TESTS", value),
            // An inherited setting must not decide this test's outcome.
            None => cmd.env_remove("CX_INCLUDE_TESTS"),
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
        assert_eq!(ignored, expected, "CX_INCLUDE_TESTS={pinned:?}, {flag:?}");
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
            side: Side::Index,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();
    let staged = report.files.iter().find(|f| f.path == "src/staged.rs");
    assert!(
        staged.is_some_and(|f| f.review_bytes > 300),
        "staged file must be scored"
    );
}

#[test]
fn tree_reports_absolute_complexity_with_contributions() {
    let (_dir, git) = setup();
    let report = pipeline::abs(
        &git,
        &AbsOptions {
            side: Side::Head,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();
    // keep.rs + moved.rs + novel.rs; Cargo.lock, logo.png and
    // tests/novel_test.rs excluded.
    assert_eq!(report.file_count, 3, "kept files at HEAD");
    assert!(report.compressed_bytes > 0);
    assert!(report.compressed_bytes < report.raw_bytes);

    assert_eq!(report.files.len(), 3);
    let sum: u64 = report.files.iter().map(|f| f.bytes).sum();
    assert_eq!(
        sum, report.compressed_bytes,
        "contributions must sum to C(tree)"
    );
    assert!(
        report.files.windows(2).all(|w| w[0].bytes >= w[1].bytes),
        "contributions must be sorted descending"
    );
    assert!(report.files.iter().all(|f| f.lines > 0));
}

#[test]
fn worktree_side_scores_the_whole_working_tree() {
    let (dir, repo) = setup();
    let root = dir.path();
    // One of each kind of dirt on top of the feature branch.
    fs::write(root.join("src/staged.rs"), gen_code(123, 80)).unwrap();
    git(root, &["add", "src/staged.rs"]);
    fs::write(root.join("src/keep.rs"), gen_code(200, 80)).unwrap();
    fs::write(root.join("src/untracked.rs"), gen_code(77, 80)).unwrap();
    // Excluded without touching a tracked .gitignore, which would itself
    // show up as a change.
    fs::write(root.join(".git/info/exclude"), "ignored.rs\n").unwrap();
    fs::write(root.join("ignored.rs"), gen_code(55, 80)).unwrap();

    let report = pipeline::diff(
        &repo,
        &DiffOptions {
            side: Side::Worktree,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();
    let scored = |p: &str| report.files.iter().find(|f| f.path == p);

    for path in ["src/staged.rs", "src/keep.rs", "src/untracked.rs"] {
        assert!(
            scored(path).is_some_and(|f| f.review_bytes > 300),
            "{path} must be scored: {:?}",
            report.files.iter().map(|f| &f.path).collect::<Vec<_>>()
        );
    }
    assert_eq!(scored("src/untracked.rs").unwrap().status, Status::Added);
    assert_eq!(scored("src/keep.rs").unwrap().status, Status::Modified);
    assert!(
        scored("ignored.rs").is_none(),
        "an ignored file is not part of the working tree's changes"
    );

    // The unstaged and untracked halves are exactly what --staged misses.
    let staged_only = pipeline::diff(
        &repo,
        &DiffOptions {
            side: Side::Index,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();
    let staged_paths: Vec<&str> = staged_only.files.iter().map(|f| f.path.as_str()).collect();
    assert!(staged_paths.contains(&"src/staged.rs"));
    assert!(!staged_paths.contains(&"src/keep.rs"));
    assert!(!staged_paths.contains(&"src/untracked.rs"));
}

#[test]
fn abs_measures_the_snapshot_it_is_asked_for() {
    let (dir, repo) = setup();
    let root = dir.path();
    fs::write(root.join("src/staged.rs"), gen_code(123, 80)).unwrap();
    git(root, &["add", "src/staged.rs"]);
    fs::write(root.join("src/untracked.rs"), gen_code(77, 80)).unwrap();
    // Tracked at HEAD and in the index, but no longer on disk.
    fs::remove_file(root.join("src/keep.rs")).unwrap();
    fs::write(root.join(".git/info/exclude"), "ignored.rs\n").unwrap();
    fs::write(root.join("ignored.rs"), gen_code(55, 80)).unwrap();

    let measure = |side| {
        let report = pipeline::abs(
            &repo,
            &AbsOptions {
                side,
                ..Default::default()
            },
            Progress::default(),
        )
        .unwrap();
        let mut paths: Vec<String> = report.files.iter().map(|f| f.path.clone()).collect();
        paths.sort();
        assert_eq!(report.file_count, paths.len(), "{side:?}");
        (paths, report.snapshot)
    };

    assert_eq!(
        measure(Side::Head),
        (
            vec![
                "src/keep.rs".to_owned(),
                "src/moved.rs".to_owned(),
                "src/novel.rs".to_owned()
            ],
            "HEAD"
        )
    );
    assert_eq!(
        measure(Side::Index),
        (
            vec![
                "src/keep.rs".to_owned(),
                "src/moved.rs".to_owned(),
                "src/novel.rs".to_owned(),
                "src/staged.rs".to_owned()
            ],
            "index"
        )
    );
    // keep.rs is gone from disk; untracked.rs is there without being
    // tracked; ignored.rs is on disk but excluded.
    assert_eq!(
        measure(Side::Worktree),
        (
            vec![
                "src/moved.rs".to_owned(),
                "src/novel.rs".to_owned(),
                "src/staged.rs".to_owned(),
                "src/untracked.rs".to_owned()
            ],
            "worktree"
        )
    );
}

/// `git ls-files --cached` reports an unmerged path once per conflict
/// stage, so scoring it verbatim would count the file three times.
#[test]
fn an_unmerged_path_is_scored_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    git(root, &["init", "-q", "-b", "main"]);
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/f.rs"), gen_code(1, 120)).unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "base"]);
    git(root, &["checkout", "-q", "-b", "other"]);
    fs::write(root.join("src/f.rs"), gen_code(2, 120)).unwrap();
    git(root, &["commit", "-q", "-am", "other"]);
    git(root, &["checkout", "-q", "main"]);
    fs::write(root.join("src/f.rs"), gen_code(3, 120)).unwrap();
    git(root, &["commit", "-q", "-am", "mine"]);
    // Expected to fail: it leaves src/f.rs unmerged, which is the point.
    let merge = Command::new("git")
        .current_dir(root)
        .args(["merge", "other"])
        .output()
        .unwrap();
    assert!(!merge.status.success(), "the merge must conflict");

    let repo = Git::discover_at(root).unwrap();
    let report = pipeline::abs(
        &repo,
        &AbsOptions {
            side: Side::Worktree,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();
    assert_eq!(
        report.file_count, 1,
        "src/f.rs is one file, not one per stage"
    );
}

/// `git diff --numstat` has no row for a file git isn't tracking yet, so
/// the churn totals have to account for untracked files separately.
#[test]
fn untracked_lines_reach_the_churn_totals() {
    let (dir, repo) = setup();
    let root = dir.path();
    let committed = pipeline::diff(
        &repo,
        &DiffOptions {
            side: Side::Head,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();

    fs::write(root.join("src/fresh.rs"), gen_code(77, 40)).unwrap();
    let worktree = pipeline::diff(
        &repo,
        &DiffOptions {
            side: Side::Worktree,
            ..Default::default()
        },
        Progress::default(),
    )
    .unwrap();

    assert_eq!(
        worktree.totals.added_lines,
        committed.totals.added_lines + 40,
        "the untracked file's 40 lines must be counted"
    );
    assert_eq!(
        worktree.totals.deleted_lines,
        committed.totals.deleted_lines
    );
}
