//! The invariants that caught design bugs during planning. Each test is a
//! behavioral claim about the metrics, not about zstd internals; if one
//! fails after a zstd upgrade, the metric semantics regressed.

use std::sync::atomic::{AtomicU64, Ordering};

use cx_core::Scorer;
use cx_core::testgen::code as gen_code;

fn scorer() -> Scorer {
    Scorer::default()
}

/// A pure move: the "new" content already exists verbatim in the reference.
/// Review cost ≈ 0, and complexity Δ is exactly 0 by symmetry.
#[test]
fn pure_move_is_free() {
    let s = scorer();
    let moved = gen_code(1, 100);
    let other = gen_code(2, 100);
    let old_tree = s.assemble(&[&moved, &other]);

    let review = s.score(&old_tree, &moved);
    assert!(
        review < 64,
        "moving known content should be ≈ free, got {review}"
    );

    // Same computation twice: this can only fail on nondeterminism, which
    // is exactly the property Δ = 0 for pure moves rests on.
    let remainder = s.assemble(&[&other]);
    assert_eq!(
        s.score(&remainder, &moved),
        s.score(&remainder, &moved),
        "scoring must be deterministic"
    );
}

/// zstd compresses differently when a prefix happens to sit right before
/// its input in memory; the scorer pins one behavior, so a score is a
/// function of bytes alone, wherever they live.
#[test]
fn scores_do_not_depend_on_memory_layout() {
    let s = scorer();
    let (reference, input) = (gen_code(3, 200), gen_code(4, 50));
    let adjacent = [reference.as_slice(), input.as_slice()].concat();
    let (prefix, rest) = adjacent.split_at(reference.len());
    assert_eq!(s.score(prefix, rest), s.score(&reference, &input));
}

/// A full rewrite of equal intrinsic complexity: review cost stays high
/// (the reviewer must absorb all-new content) while complexity Δ ≈ 0
/// (the codebase is no more complex than before). The two metrics are
/// independent axes — this is why metric 1 is NOT a size subtraction.
#[test]
fn equal_complexity_rewrite() {
    let s = scorer();
    let old = gen_code(10, 200);
    let new = gen_code(20, 200);
    let other = gen_code(30, 200);

    let old_tree = s.assemble(&[&old, &other]);
    let review = s.score(&old_tree, &new);

    let remainder = s.assemble(&[&other]);
    let c_new = s.score(&remainder, &new);
    let c_old = s.score(&remainder, &old);
    let delta = c_new as i64 - c_old as i64;

    assert!(
        review as f64 > 0.5 * c_new as f64,
        "rewrite review cost should be comparable to writing it fresh: {review} vs {c_new}"
    );
    assert!(
        (delta.abs() as f64) < 0.10 * c_new as f64,
        "equal-complexity rewrite should have Δ ≈ 0: Δ={delta}, C(new)={c_new}"
    );
}

/// Deleting one of N duplicates refunds ≈ nothing (the pattern is still in
/// the codebase); deleting unique content refunds its full cost. The
/// metric declines to celebrate deleting one of 30 copies.
#[test]
fn deletion_refunds() {
    let s = scorer();
    let dup = gen_code(40, 100);
    let unique = gen_code(50, 100);
    let other = gen_code(60, 100);

    // Remainder still contains two more copies of `dup`.
    let remainder_with_copies = s.assemble(&[&dup, &dup, &other]);
    let refund_dup = s.score(&remainder_with_copies, &dup);
    assert!(
        refund_dup < 64,
        "deleting 1-of-3 copies should refund ≈ 0, got {refund_dup}"
    );

    let remainder_plain = s.assemble(&[&other]);
    let refund_unique = s.score(&remainder_plain, &unique);
    assert!(
        refund_unique > 500,
        "deleting unique content should refund its full cost, got {refund_unique}"
    );
}

/// Sequential chain rule: N repetitions of a novel pattern cost ≈ one
/// pattern in total — the first occurrence carries it, siblings ride free.
#[test]
fn repeated_new_patterns_charged_once() {
    let s = scorer();
    let pattern = gen_code(70, 100);
    let reference = s.assemble(&[&gen_code(80, 100)]);

    let first = s.score(&reference, &pattern);
    let scored = s
        .attribution(&reference, &[pattern.as_slice(); 4])
        .run(&|_| {});
    let scores: Vec<u64> = scored.scores.iter().map(|sc| sc.raw).collect();
    let total: u64 = scores.iter().sum();

    assert_eq!(scores[0], first);
    for (i, &sib) in scores[1..].iter().enumerate() {
        assert!(sib < 64, "repeat #{} should ride ≈ free, got {sib}", i + 2);
    }
    assert!(
        (total as f64) < 1.2 * first as f64,
        "4 copies should cost ≈ 1 pattern: total={total}, one={first}"
    );
}

/// The chain rule, item by item: each attributed score is the score of
/// that item alone against the explicit prefix it was conditioned on —
/// however the run is scheduled. Repeats sit at different positions so
/// the near-free scores land only where the pattern already appeared.
#[test]
fn attributed_scores_are_per_item_conditionals() {
    let s = scorer();
    let reference = s.assemble(&[&gen_code(200, 150)]);
    let (a, b, c) = (gen_code(301, 60), gen_code(302, 90), gen_code(303, 40));
    let items: [&[u8]; 5] = [&a, &b, &a, &c, &b];

    let scored = s.attribution(&reference, &items).run(&|_| {});
    let seq: Vec<u64> = scored.scores.iter().map(|sc| sc.raw).collect();
    for (i, item) in items.iter().enumerate() {
        let mut prefix = reference.clone();
        prefix.extend_from_slice(&s.assemble(&items[..i]));
        assert_eq!(seq[i], s.score(&prefix, item), "item {i}");
    }
    assert_eq!(scored.joint, s.score(&reference, &s.assemble(&items)));
    assert!(seq[2] < 64 && seq[4] < 64, "repeats ride free: {seq:?}");
    assert!(seq[0] > 500 && seq[1] > 500 && seq[3] > 300, "{seq:?}");
}

/// ΣΔᵢ ≈ joint total (per-frame overhead and greedy matching make the sum
/// run slightly high); the rescale factor is the noise gauge and rescaled
/// scores sum exactly to the joint.
#[test]
fn sequential_sum_tracks_joint() {
    let s = scorer();
    let items: Vec<Vec<u8>> = (0..5).map(|i| gen_code(100 + i, 80)).collect();
    let refs: Vec<&[u8]> = items.iter().map(|v| v.as_slice()).collect();
    let reference = s.assemble(&[&gen_code(90, 200)]);

    let scored = s.attribution(&reference, &refs).run(&|_| {});
    assert!(
        (0.7..=1.1).contains(&scored.scale),
        "scale factor should be near 1, got {}",
        scored.scale
    );
    let sum: f64 = scored.scores.iter().map(|sc| sc.rescaled).sum();
    assert!(
        (sum - scored.joint as f64).abs() < 1e-6 * scored.joint as f64,
        "rescaled scores must sum to joint: {sum} vs {}",
        scored.joint
    );
}

/// What a progress bar is sized to is what the run reports: every input
/// runs exactly once, with more items than threads so workers take turns.
#[test]
fn progress_advances_by_exactly_the_planned_cost() {
    let s = scorer();
    let reference = s.assemble(&[&gen_code(400, 100)]);
    let items: Vec<Vec<u8>> = (0..40).map(|i| gen_code(500 + i, 5)).collect();
    let refs: Vec<&[u8]> = items.iter().map(|v| v.as_slice()).collect();
    let attribution = s.attribution(&reference, &refs);

    let advanced = AtomicU64::new(0);
    attribution.run(&|bytes| {
        advanced.fetch_add(bytes, Ordering::Relaxed);
    });
    assert_eq!(advanced.into_inner(), attribution.cost());
}
