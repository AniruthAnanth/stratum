//! The ONLY transcendentals in the engine (ADR-004, ARCHITECTURE §8.11).
//!
//! `f64::ln`, `f64::exp`, `f64::powf` and friends forward to the *platform*
//! libm, and glibc's, musl's, Apple's and MSVC's differ in the last ulp. That
//! alone breaks the cross-platform determinism gate (ADR-013) regardless of
//! what the Gram kernel does, so every call in the workspace routes through
//! this module, which routes to the `libm` crate — one implementation, on every
//! target, wasm included.
//!
//! `scripts/check-topology.sh check_no_fma` greps `crates/` for
//! `.ln( .exp( .powf( … .mul_add(` and excludes exactly this file;
//! `tests/fmt_golden.rs` runs the same grep as a unit test so the failure is
//! local rather than a CI surprise.
//!
//! # `sqrt` is exempt, and only `sqrt`
//!
//! IEEE-754 *requires* `sqrt` to be correctly rounded, so every conforming
//! implementation returns the same bits and hardware executes it in one
//! instruction. Routing it through `libm` would be slower and no more
//! deterministic. Nothing else in IEEE-754 carries that guarantee.
//!
//! # `mul_add` is banned, not merely discouraged
//!
//! `a.mul_add(b, c)` fuses to a single rounding step where the target has FMA
//! and emulates it in software where it does not — a different answer on the
//! same source. Rust never contracts `a * b + c` on its own (there is no
//! default fp-contract and we never set fast-math), so writing the expression
//! out is both faster to read and the deterministic choice.

/// Natural logarithm.
#[inline(always)]
#[must_use]
pub fn ln(x: f64) -> f64 {
    libm::log(x)
}

/// Base-10 logarithm.
#[inline(always)]
#[must_use]
pub fn log10(x: f64) -> f64 {
    libm::log10(x)
}

/// Base-2 logarithm.
#[inline(always)]
#[must_use]
pub fn log2(x: f64) -> f64 {
    libm::log2(x)
}

/// `ln(1 + x)`, accurate for small `x`.
#[inline(always)]
#[must_use]
pub fn ln_1p(x: f64) -> f64 {
    libm::log1p(x)
}

/// `e^x`.
#[inline(always)]
#[must_use]
pub fn exp(x: f64) -> f64 {
    libm::exp(x)
}

/// `e^x - 1`, accurate for small `x`.
#[inline(always)]
#[must_use]
pub fn exp_m1(x: f64) -> f64 {
    libm::expm1(x)
}

/// `x^y` for real `y`.
#[inline(always)]
#[must_use]
pub fn powf(x: f64, y: f64) -> f64 {
    libm::pow(x, y)
}

/// `x^n` for integer `n`.
///
/// Deliberately `libm::pow` and not a repeated-squaring loop: `powi` in `std`
/// expands to a multiply chain whose association order LLVM is free to change,
/// and two association orders differ in the last ulp.
#[inline(always)]
#[must_use]
pub fn powi(x: f64, n: i32) -> f64 {
    libm::pow(x, f64::from(n))
}

/// Square root. Correctly rounded by IEEE-754 on every target; see the module
/// note for why this one is not routed through `libm`.
#[inline(always)]
#[must_use]
pub fn sqrt(x: f64) -> f64 {
    x.sqrt()
}

/// Log of the absolute value of the gamma function.
#[inline(always)]
#[must_use]
pub fn lgamma(x: f64) -> f64 {
    libm::lgamma(x)
}

/// The gamma function.
#[inline(always)]
#[must_use]
pub fn tgamma(x: f64) -> f64 {
    libm::tgamma(x)
}

/// The error function.
#[inline(always)]
#[must_use]
pub fn erf(x: f64) -> f64 {
    libm::erf(x)
}

/// The complementary error function, `1 - erf(x)`, without the cancellation.
#[inline(always)]
#[must_use]
pub fn erfc(x: f64) -> f64 {
    libm::erfc(x)
}

/// Sine.
#[inline(always)]
#[must_use]
pub fn sin(x: f64) -> f64 {
    libm::sin(x)
}

/// Cosine.
#[inline(always)]
#[must_use]
pub fn cos(x: f64) -> f64 {
    libm::cos(x)
}

/// Tangent.
#[inline(always)]
#[must_use]
pub fn tan(x: f64) -> f64 {
    libm::tan(x)
}

/// Arc sine.
#[inline(always)]
#[must_use]
pub fn asin(x: f64) -> f64 {
    libm::asin(x)
}

/// Arc cosine.
#[inline(always)]
#[must_use]
pub fn acos(x: f64) -> f64 {
    libm::acos(x)
}

/// Arc tangent.
#[inline(always)]
#[must_use]
pub fn atan(x: f64) -> f64 {
    libm::atan(x)
}

/// Two-argument arc tangent.
#[inline(always)]
#[must_use]
pub fn atan2(y: f64, x: f64) -> f64 {
    libm::atan2(y, x)
}

/// Hyperbolic sine.
#[inline(always)]
#[must_use]
pub fn sinh(x: f64) -> f64 {
    libm::sinh(x)
}

/// Hyperbolic cosine.
#[inline(always)]
#[must_use]
pub fn cosh(x: f64) -> f64 {
    libm::cosh(x)
}

/// Hyperbolic tangent.
#[inline(always)]
#[must_use]
pub fn tanh(x: f64) -> f64 {
    libm::tanh(x)
}

/// Cube root.
#[inline(always)]
#[must_use]
pub fn cbrt(x: f64) -> f64 {
    libm::cbrt(x)
}

/// `sqrt(x^2 + y^2)` without intermediate overflow.
#[inline(always)]
#[must_use]
pub fn hypot(x: f64, y: f64) -> f64 {
    libm::hypot(x, y)
}

/// Integral part, rounding toward zero — Stata's `int()`.
#[inline(always)]
#[must_use]
pub fn trunc(x: f64) -> f64 {
    libm::trunc(x)
}

/// Round half away from zero — Stata's `round()`, and the rule its `%g`
/// formatter uses on an exact decimal tie (measured; `std`'s `f64::round` is
/// the same rule but goes through the platform libm).
#[inline(always)]
#[must_use]
pub fn round(x: f64) -> f64 {
    libm::round(x)
}

/// Round toward +infinity.
#[inline(always)]
#[must_use]
pub fn ceil(x: f64) -> f64 {
    libm::ceil(x)
}

/// Round toward -infinity.
#[inline(always)]
#[must_use]
pub fn floor(x: f64) -> f64 {
    libm::floor(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Known values, not a comparison against `std`.
    ///
    /// The point of this module is that the host libm is NOT the reference, and
    /// `clippy.toml` disallows `f64::ln` and friends workspace-wide — including
    /// here, which is correct: a test that called them would be asserting that
    /// two implementations agree, and they do not have to.
    #[test]
    fn known_values() {
        assert!((ln(1.0)).abs() < 1e-300);
        assert!((ln(core::f64::consts::E) - 1.0).abs() < 1e-15);
        assert!((exp(0.0) - 1.0).abs() < 1e-300);
        assert!((exp(1.0) - core::f64::consts::E).abs() < 1e-15);
        assert_eq!(sqrt(4.0), 2.0);
        assert_eq!(sqrt(0.0), 0.0);
        assert!((powf(2.0, 10.0) - 1024.0).abs() < 1e-9);
        assert!((powi(2.0, 10) - 1024.0).abs() < 1e-9);
        assert!((erfc(0.0) - 1.0).abs() < 1e-15);
        assert!((erf(0.0)).abs() < 1e-300);
        assert!((lgamma(5.0) - ln(24.0)).abs() < 1e-13);
        assert_eq!(round(2.5), 3.0);
        assert_eq!(round(-2.5), -3.0);
        assert_eq!(trunc(-2.9), -2.0);
        assert_eq!(floor(-2.1), -3.0);
        assert_eq!(ceil(2.1), 3.0);
    }
}
