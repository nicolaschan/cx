//! Deterministic code-like text for tests and benchmarks: different seeds
//! give different content of equal intrinsic complexity (same generator,
//! same length). Not part of the scoring API.

/// Generate `lines` lines of code-like text from `seed`.
pub fn code(seed: u64, lines: usize) -> Vec<u8> {
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
