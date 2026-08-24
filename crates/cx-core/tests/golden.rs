//! Golden scores for fixed inputs under the pinned zstd version. These are
//! exact-byte assertions on the estimator itself: if they change, the
//! compressor (or our parameters) changed, and scores are no longer
//! comparable with previously recorded ones. Bump deliberately, alongside
//! the zstd version bump, never to "fix" a failure.

use cx_core::{Scorer, Silent, zstd_version};

const REFERENCE: &[u8] = b"fn add(a: u32, b: u32) -> u32 { a + b }\n\
fn sub(a: u32, b: u32) -> u32 { a - b }\n\
fn mul(a: u32, b: u32) -> u32 { a * b }\n";

const NOVEL: &[u8] = b"struct Interval { lo: f64, hi: f64 }\n\
impl Interval { fn width(&self) -> f64 { self.hi - self.lo } }\n";

const CONVENTIONAL: &[u8] = b"fn div(a: u32, b: u32) -> u32 { a / b }\n";

#[test]
fn golden_scores() {
    assert_eq!(
        zstd_version(),
        "1.5.7",
        "zstd changed: re-pin all golden values"
    );
    let s = Scorer::default();

    assert_eq!(s.score(&[], REFERENCE), 60);
    assert_eq!(s.score(&[], NOVEL), 83);
    let attributed = s
        .attribution(REFERENCE, &[NOVEL, CONVENTIONAL])
        .run(&Silent);
    let raw: Vec<u64> = attributed.scores.iter().map(|sc| sc.raw).collect();
    assert_eq!(raw, [77, 19]);
    assert_eq!(attributed.joint, 103);
}
