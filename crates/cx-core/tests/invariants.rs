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
/// Review cost ≈ 0, and complexity ΔCX is exactly 0 by symmetry.
#[test]
fn pure_move_is_free() {
    let s = scorer();
    let moved = gen_code(1, 100);
    let other = gen_code(2, 100);

    let review = s.score(&[&moved, &other], &moved);
    assert!(
        review < 64,
        "moving known content should be ≈ free, got {review}"
    );

    // Same computation twice: this can only fail on nondeterminism, which
    // is exactly the property ΔCX = 0 for pure moves rests on.
    assert_eq!(
        s.score(&[&other], &moved),
        s.score(&[&other], &moved),
        "scoring must be deterministic"
    );
}

/// A score is a function of the bytes alone: the same parts score the
/// same whether they come from one allocation or many.
#[test]
fn scores_do_not_depend_on_memory_layout() {
    let s = scorer();
    let (reference, input) = (gen_code(3, 200), gen_code(4, 50));
    let adjacent = [reference.as_slice(), input.as_slice()].concat();
    let (prefix, rest) = adjacent.split_at(reference.len());
    assert_eq!(s.score(&[prefix], rest), s.score(&[&reference], &input));
}

/// A full rewrite of equal intrinsic complexity: review cost stays high
/// (the reviewer must absorb all-new content) while complexity ΔCX ≈ 0
/// (the codebase is no more complex than before). The two metrics are
/// independent axes — this is why metric 1 is NOT a size subtraction.
#[test]
fn equal_complexity_rewrite() {
    let s = scorer();
    let old = gen_code(10, 200);
    let new = gen_code(20, 200);
    let other = gen_code(30, 200);

    let review = s.score(&[&old, &other], &new);
    let c_new = s.score(&[&other], &new);
    let c_old = s.score(&[&other], &old);
    let delta = c_new as i64 - c_old as i64;

    assert!(
        review as f64 > 0.5 * c_new as f64,
        "rewrite review cost should be comparable to writing it fresh: {review} vs {c_new}"
    );
    assert!(
        (delta.abs() as f64) < 0.10 * c_new as f64,
        "equal-complexity rewrite should have ΔCX ≈ 0: ΔCX={delta}, C(new)={c_new}"
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
    let refund_dup = s.score(&[&dup, &dup, &other], &dup);
    assert!(
        refund_dup < 64,
        "deleting 1-of-3 copies should refund ≈ 0, got {refund_dup}"
    );

    let refund_unique = s.score(&[&other], &unique);
    assert!(
        refund_unique > 500,
        "deleting unique content should refund its full cost, got {refund_unique}"
    );
}

/// Chain rule: N repetitions of a novel pattern cost ≈ one pattern in
/// total — the first occurrence carries it, siblings ride free.
#[test]
fn repeated_new_patterns_charged_once() {
    let s = scorer();
    let pattern = gen_code(70, 100);
    let reference = gen_code(80, 100);

    let first = s.score(&[&reference], &pattern);
    let scores = s
        .attribution(&[&reference], &[pattern.as_slice(); 4])
        .run(|_| {});
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

/// Reference and items are one stream; which parts are reported is a
/// label. Every item's score is what it adds after everything before it,
/// wherever the reference/items line is drawn.
#[test]
fn attributed_scores_are_per_item_conditionals() {
    let s = scorer();
    let reference = gen_code(200, 150);
    let (a, b, c) = (gen_code(301, 60), gen_code(302, 90), gen_code(303, 40));
    let items: [&[u8]; 5] = [&a, &b, &a, &c, &b];

    let scores = s.attribution(&[&reference], &items).run(|_| {});
    for i in 0..items.len() {
        let mut prefix: Vec<&[u8]> = vec![&reference];
        prefix.extend_from_slice(&items[..i]);
        let relabeled = s.attribution(&prefix, &items[i..]).run(|_| {});
        assert_eq!(scores[i], relabeled[0], "item {i}");
    }
    assert!(
        scores[2] < 64 && scores[4] < 64,
        "repeats ride free: {scores:?}"
    );
    assert!(
        scores[0] > 500 && scores[1] > 500 && scores[3] > 300,
        "{scores:?}"
    );
}

/// No lookahead: an item's score is a function of the bytes before it.
/// Appending anything after it — here enough to grow the window and span
/// several zstd blocks — leaves it untouched.
#[test]
fn later_items_do_not_change_earlier_scores() {
    let s = scorer();
    let reference = gen_code(400, 100);
    let (x, y, big) = (gen_code(501, 50), gen_code(502, 50), gen_code(503, 3000));
    let before = s.attribution(&[&reference], &[&x, &y]).run(|_| {});
    let after = s.attribution(&[&reference], &[&x, &y, &big]).run(|_| {});
    assert_eq!(before[..], after[..2]);
}

/// An empty part costs nothing and changes nothing around it.
#[test]
fn empty_items_are_free() {
    let s = scorer();
    let reference = gen_code(800, 100);
    let (a, b) = (gen_code(801, 40), gen_code(802, 40));
    let plain = s.attribution(&[&reference], &[&a, &b]).run(|_| {});
    let padded: [&[u8]; 5] = [&[], &a, &[], &b, &[]];
    assert_eq!(
        s.attribution(&[&reference], &padded).run(|_| {}),
        [0, plain[0], 0, plain[1], 0]
    );
}

/// A pass whose items are all empty scores 0 for every one of them, and
/// the reference it was conditioned on cannot change that. This is what
/// lets a caller skip such a pass without running it: both forms are
/// computed here, so the claim is checked against the compressor rather
/// than asserted.
#[test]
fn an_all_empty_pass_is_zero_whatever_its_reference() {
    let s = scorer();
    let reference = gen_code(900, 100);
    let items: [&[u8]; 3] = [&[], &[], &[]];
    assert_eq!(s.attribution(&[&reference], &items).run(|_| {}), [0, 0, 0]);
    assert_eq!(s.attribution(&[], &items).run(|_| {}), [0, 0, 0]);
}

/// Bytes zstd cannot compress come out whole, block headers included,
/// however many blocks they span: the output grows to fit.
#[test]
fn incompressible_parts_score_their_size() {
    let s = scorer();
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let noise: Vec<u8> = (0..300_000)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 56) as u8
        })
        .collect();
    let score = s.score(&[], &noise);
    assert!(
        score > noise.len() as u64 && score < noise.len() as u64 + 64,
        "{score}"
    );
}

/// What a progress bar is sized to is what the run reports.
#[test]
fn progress_advances_by_exactly_the_planned_cost() {
    let s = scorer();
    let reference = gen_code(600, 100);
    let reference_parts: [&[u8]; 1] = [&reference];
    let items: Vec<Vec<u8>> = (0..40).map(|i| gen_code(700 + i, 5)).collect();
    let refs: Vec<&[u8]> = items.iter().map(|v| v.as_slice()).collect();
    let attribution = s.attribution(&reference_parts, &refs);

    let advanced = AtomicU64::new(0);
    attribution.run(|bytes| {
        advanced.fetch_add(bytes, Ordering::Relaxed);
    });
    assert_eq!(advanced.into_inner(), attribution.bytes());
}
