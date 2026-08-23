//! The invariants that caught design bugs during planning. Each test is a
//! behavioral claim about the metrics, not about zstd internals; if one
//! fails after a zstd upgrade, the metric semantics regressed.

use cx_core::{Scorer, rescale};

/// Deterministic code-like text: different seeds give different content of
/// equal intrinsic complexity (same generator, same length).
fn gen_code(seed: u64, lines: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E3779B97F4A7C15) | 1;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut out = String::new();
    for i in 0..lines {
        out.push_str(&format!(
            "fn f_{i}_{:x}(x: u32) -> u32 {{ x.wrapping_mul({}).wrapping_add({}) }}\n",
            next() & 0xffff,
            next() & 0xffffff,
            next() & 0xffffff,
        ));
    }
    out.into_bytes()
}

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

    let review = s.score_sequential(&old_tree, &[&moved]);
    assert!(review[0] < 64, "moving known content should be ≈ free, got {}", review[0]);

    let remainder = s.assemble(&[&other]);
    let new_side = s.score_sequential(&remainder, &[&moved]);
    let old_side = s.score_sequential(&remainder, &[&moved]);
    assert_eq!(new_side, old_side, "identical sides must score identically");
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
    let review = s.score_sequential(&old_tree, &[&new])[0];

    let remainder = s.assemble(&[&other]);
    let c_new = s.score_sequential(&remainder, &[&new])[0];
    let c_old = s.score_sequential(&remainder, &[&old])[0];
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
    let refund_dup = s.score_sequential(&remainder_with_copies, &[&dup])[0];
    assert!(refund_dup < 64, "deleting 1-of-3 copies should refund ≈ 0, got {refund_dup}");

    let remainder_plain = s.assemble(&[&other]);
    let refund_unique = s.score_sequential(&remainder_plain, &[&unique])[0];
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

    let first = s.score_sequential(&reference, &[&pattern])[0];
    let scores = s.score_sequential(&reference, &[&pattern, &pattern, &pattern, &pattern]);
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

/// ΣΔᵢ ≈ joint total (per-frame overhead and greedy matching make the sum
/// run slightly high); the rescale factor is the noise gauge and rescaled
/// scores sum exactly to the joint.
#[test]
fn sequential_sum_tracks_joint() {
    let s = scorer();
    let items: Vec<Vec<u8>> = (0..5).map(|i| gen_code(100 + i, 80)).collect();
    let refs: Vec<&[u8]> = items.iter().map(|v| v.as_slice()).collect();
    let reference = s.assemble(&[&gen_code(90, 200)]);

    let seq = s.score_sequential(&reference, &refs);
    let joint = s.score_joint(&reference, &refs);
    let rescaled = rescale(&seq, joint);

    assert!(
        (0.7..=1.1).contains(&rescaled.scale),
        "scale factor should be near 1, got {}",
        rescaled.scale
    );
    let sum: f64 = rescaled.scores.iter().map(|sc| sc.rescaled).sum();
    assert!(
        (sum - joint as f64).abs() < 1e-6 * joint as f64,
        "rescaled scores must sum to joint: {sum} vs {joint}"
    );
}
