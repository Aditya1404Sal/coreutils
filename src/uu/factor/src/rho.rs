// This file is part of the uutils coreutils package.
//
// For the full copyright and license information, please view the LICENSE
// file that was distributed with this source code.

// spell-checker:ignore ninv Montgomery

//! Pollard's rho, with Brent's cycle detection, for composites of up to 192 bits.
//!
//! `num_prime` gives up on a composite after a fixed number of rho steps from random starts,
//! so a number with two large prime factors (`10^54 + 1` has a 40-bit and a 96-bit one) was
//! sometimes left unfactored, and always slowly: its arithmetic allocates a big integer for
//! every step. This keeps the numbers in three machine words in Montgomery form, starts from
//! fixed points, and tries another curve whenever one fails, so it always finishes, as GNU's
//! factor does.

use num_bigint::BigUint;

/// Arithmetic modulo an odd `n` below 2^192, in Montgomery form with R = 2^192.
struct Montgomery {
    n: [u64; 3],
    /// -n^-1 mod 2^64.
    ninv: u64,
    /// R^2 mod n, which turns a number into Montgomery form.
    r2: [u64; 3],
}

fn limbs(value: &BigUint) -> [u64; 3] {
    let digits = value.to_u64_digits();
    let mut limbs = [0; 3];
    for (limb, digit) in limbs.iter_mut().zip(digits) {
        *limb = digit;
    }
    limbs
}

fn from_limbs(limbs: &[u64; 3]) -> BigUint {
    let mut digits = Vec::with_capacity(6);
    for limb in limbs {
        digits.push(*limb as u32);
        digits.push((*limb >> 32) as u32);
    }
    BigUint::new(digits)
}

fn at_least(a: &[u64; 3], b: &[u64; 3]) -> bool {
    for i in (0..3).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

fn subtract(a: &[u64; 3], b: &[u64; 3]) -> [u64; 3] {
    let mut result = [0; 3];
    let mut borrow = false;
    for i in 0..3 {
        let (difference, under) = a[i].overflowing_sub(b[i]);
        let (difference, under_again) = difference.overflowing_sub(u64::from(borrow));
        result[i] = difference;
        borrow = under || under_again;
    }
    result
}

fn add(a: &[u64; 3], b: &[u64; 3]) -> ([u64; 3], bool) {
    let mut result = [0; 3];
    let mut carry = false;
    for i in 0..3 {
        let (sum, over) = a[i].overflowing_add(b[i]);
        let (sum, over_again) = sum.overflowing_add(u64::from(carry));
        result[i] = sum;
        carry = over || over_again;
    }
    (result, carry)
}

impl Montgomery {
    /// `None` unless `n` is odd and below 2^192.
    fn new(n: &BigUint) -> Option<Self> {
        if n.bits() > 192 || !n.bit(0) {
            return None;
        }
        let n_limbs = limbs(n);
        // Newton's iteration doubles the correct low bits of the inverse each time.
        let mut inverse: u64 = 1;
        for _ in 0..7 {
            inverse = inverse.wrapping_mul(2u64.wrapping_sub(n_limbs[0].wrapping_mul(inverse)));
        }
        let r2 = limbs(&((BigUint::from(1u8) << 384usize) % n));
        Some(Self {
            n: n_limbs,
            ninv: inverse.wrapping_neg(),
            r2,
        })
    }

    /// a * b / R mod n (coarsely interleaved product and reduction).
    fn multiply(&self, a: &[u64; 3], b: &[u64; 3]) -> [u64; 3] {
        let mut t = [0u64; 5];
        for &ai in a {
            let mut carry = 0u128;
            for j in 0..3 {
                let sum = u128::from(t[j]) + u128::from(ai) * u128::from(b[j]) + carry;
                t[j] = sum as u64;
                carry = sum >> 64;
            }
            let sum = u128::from(t[3]) + carry;
            t[3] = sum as u64;
            t[4] = (sum >> 64) as u64;

            let m = t[0].wrapping_mul(self.ninv);
            let mut carry = (u128::from(t[0]) + u128::from(m) * u128::from(self.n[0])) >> 64;
            for j in 1..3 {
                let sum = u128::from(t[j]) + u128::from(m) * u128::from(self.n[j]) + carry;
                t[j - 1] = sum as u64;
                carry = sum >> 64;
            }
            let sum = u128::from(t[3]) + carry;
            t[2] = sum as u64;
            t[3] = t[4] + (sum >> 64) as u64;
            t[4] = 0;
        }
        let result = [t[0], t[1], t[2]];
        if t[3] != 0 || at_least(&result, &self.n) {
            subtract(&result, &self.n)
        } else {
            result
        }
    }

    fn to_form(&self, a: &[u64; 3]) -> [u64; 3] {
        self.multiply(a, &self.r2)
    }

    /// (a - b) mod n, for a and b below n.
    fn difference(&self, a: &[u64; 3], b: &[u64; 3]) -> [u64; 3] {
        if at_least(a, b) {
            subtract(a, b)
        } else {
            let (sum, _) = add(&subtract(a, b), &self.n);
            sum
        }
    }

    /// x^2 + c mod n, all in Montgomery form.
    fn step(&self, x: &[u64; 3], c: &[u64; 3]) -> [u64; 3] {
        let square = self.multiply(x, x);
        let (sum, carry) = add(&square, c);
        if carry || at_least(&sum, &self.n) {
            subtract(&sum, &self.n)
        } else {
            sum
        }
    }
}

fn gcd(mut a: BigUint, mut b: BigUint) -> BigUint {
    while b != BigUint::ZERO {
        let remainder = &a % &b;
        a = b;
        b = remainder;
    }
    a
}

/// One run of Pollard-Brent from the curve x^2 + `c`: a proper divisor of `n`, or `None` when
/// this curve cycles without one.
fn brent(space: &Montgomery, n: &BigUint, c: u64) -> Option<BigUint> {
    const BATCH: usize = 128;
    let one = BigUint::from(1u8);
    let c = space.to_form(&[c, 0, 0]);
    let mut y = space.to_form(&[2, 0, 0]);
    let mut product = space.to_form(&[1, 0, 0]);
    let mut divisor = one.clone();
    let mut length = 1usize;
    let (mut x, mut saved) = (y, y);
    while divisor == one {
        x = y;
        for _ in 0..length {
            y = space.step(&y, &c);
        }
        let mut done = 0;
        while done < length && divisor == one {
            saved = y;
            for _ in 0..BATCH.min(length - done) {
                y = space.step(&y, &c);
                product = space.multiply(&product, &space.difference(&x, &y));
            }
            divisor = gcd(from_limbs(&product), n.clone());
            done += BATCH;
        }
        length *= 2;
    }
    if &divisor == n {
        // The batch overshot: step through it one at a time.
        loop {
            saved = space.step(&saved, &c);
            divisor = gcd(from_limbs(&space.difference(&x, &saved)), n.clone());
            if divisor != one {
                break;
            }
        }
    }
    (&divisor != n).then_some(divisor)
}

/// A proper divisor of the odd composite `n` below 2^192, trying one curve after another until
/// one finds it. `None` when `n` is out of range.
pub fn divisor(n: &BigUint) -> Option<BigUint> {
    let space = Montgomery::new(n)?;
    (1u64..).find_map(|c| brent(&space, n, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiplies_in_montgomery_form() {
        let n = BigUint::from(1_000_000_000_000_000_000_000_007u128);
        let space = Montgomery::new(&n).unwrap();
        let a = space.to_form(&limbs(&BigUint::from(123_456_789_012_345u64)));
        let b = space.to_form(&limbs(&BigUint::from(987_654_321_098_765u64)));
        let product = space.multiply(&space.multiply(&a, &b), &[1, 0, 0]);
        let expected = BigUint::from(123_456_789_012_345u64) * 987_654_321_098_765u64 % &n;
        assert_eq!(from_limbs(&product), expected);
    }

    #[test]
    fn splits_a_product_of_two_large_primes() {
        let p = BigUint::from(999_999_000_001u64);
        let q: BigUint = "59779577156334533866654838281".parse().unwrap();
        let n = &p * &q;
        let d = divisor(&n).unwrap();
        assert!(d == p || d == q);
    }
}
