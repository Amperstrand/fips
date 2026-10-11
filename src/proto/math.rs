//! no_std-shaped numeric helpers for the protocol cores.
//!
//! `f64::powi` and the transcendental methods live in `std`, not `core`. The
//! helpers here keep the small amount of protocol math the codecs need in a
//! `core`-only shape so the cores stay portable, without changing any result.

/// Raise `base` to a non-negative integer power via square-and-multiply.
///
/// Built from `core`-only IEEE-754 `f64` multiplication, so the result is
/// **deterministic across every platform** — the property that matters here,
/// since the bloom false-positive-rate feeds a reject comparison and the FMP
/// backoff feeds a `u64` timer, and mesh nodes must agree regardless of OS.
/// This is a strict improvement over `f64::powi`, which is not portably
/// bit-stable: Linux and macOS lower it to compiler-rt's `__powidf2` (whose
/// multiply-then-square order this mirrors, so we match them exactly), but
/// Windows/MSVC rounds differently (and by more as the exponent grows). The
/// `powi_deterministic_and_close` test pins our output to golden bits and bounds
/// the relative drift from `std`.
pub fn powi(base: f64, exp: u32) -> f64 {
    let mut result = 1.0_f64;
    let mut b = base;
    let mut e = exp;
    loop {
        if e & 1 == 1 {
            result *= b;
        }
        e >>= 1;
        if e == 0 {
            break;
        }
        b *= b;
    }
    result
}

#[cfg(test)]
#[path = "tests/math.rs"]
mod tests;
