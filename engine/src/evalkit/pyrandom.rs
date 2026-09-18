//! CPython's `random.Random`, reproduced exactly.
//!
//! The stress tool rewrites each romanization in eleven typing habits, and most of those are
//! random: drop interior vowels with probability 0.7, capitalise 30% of letters, substitute one
//! adjacent key. Porting them without porting the generator would produce a tool that measures
//! something *similar* to what the Python measured -- similar enough to look right and to make
//! every historical stress result incomparable, which is the worst of both.
//!
//! So this is MT19937 with CPython's seeding and CPython's derived methods: the 53-bit `random()`,
//! the rejection-sampling `_randbelow`, and the `shuffle` that walks backwards. Checked against
//! CPython in the tests below, value for value.
//!
//! Reference: CPython `Modules/_randommodule.c` and `Lib/random.py`.

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

pub struct PyRandom {
    state: [u32; N],
    index: usize,
}

impl PyRandom {
    /// `random.Random(seed)` for a non-negative integer seed.
    ///
    /// CPython takes the absolute value, splits it into 32-bit little-endian words and calls
    /// `init_by_array`. A seed of 0 still yields a one-word key of `[0]`, not an empty one.
    pub fn new(seed: u64) -> PyRandom {
        let mut key: Vec<u32> = Vec::new();
        let mut s = seed;
        loop {
            key.push((s & 0xffff_ffff) as u32);
            s >>= 32;
            if s == 0 {
                break;
            }
        }
        let mut r = PyRandom { state: [0; N], index: N };
        r.init_by_array(&key);
        r
    }

    fn init_genrand(&mut self, s: u32) {
        self.state[0] = s;
        for i in 1..N {
            let prev = self.state[i - 1];
            self.state[i] = 1812433253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        self.index = N;
    }

    fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19650218);
        let mut i: usize = 1;
        let mut j: usize = 0;
        let mut k = std::cmp::max(N, key.len());
        while k > 0 {
            let prev = self.state[i - 1];
            self.state[i] = (self.state[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1664525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                self.state[0] = self.state[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
            k -= 1;
        }
        let mut k = N - 1;
        while k > 0 {
            let prev = self.state[i - 1];
            self.state[i] = (self.state[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1566083941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                self.state[0] = self.state[N - 1];
                i = 1;
            }
            k -= 1;
        }
        self.state[0] = 0x8000_0000;
    }

    fn genrand_uint32(&mut self) -> u32 {
        if self.index >= N {
            for i in 0..N {
                let y = (self.state[i] & UPPER_MASK) | (self.state[(i + 1) % N] & LOWER_MASK);
                let mut next = self.state[(i + M) % N] ^ (y >> 1);
                if y & 1 != 0 {
                    next ^= MATRIX_A;
                }
                self.state[i] = next;
            }
            self.index = 0;
        }
        let mut y = self.state[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// `random()`: a double in [0, 1) built from two draws, exactly as CPython does.
    pub fn random(&mut self) -> f64 {
        let a = self.genrand_uint32() >> 5;
        let b = self.genrand_uint32() >> 6;
        (a as f64 * 67_108_864.0 + b as f64) * (1.0 / 9_007_199_254_740_992.0)
    }

    /// `getrandbits(k)` for k <= 32, which is all the derived methods here need.
    fn getrandbits(&mut self, k: u32) -> u32 {
        debug_assert!(k <= 32);
        if k == 0 {
            return 0;
        }
        self.genrand_uint32() >> (32 - k)
    }

    /// `Random._randbelow`: rejection sampling on the bit length of `n`, so the result is uniform
    /// and -- more to the point here -- consumes the same draws CPython consumes.
    fn randbelow(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        let k = usize::BITS - (n as u32).leading_zeros(); // n.bit_length()
        loop {
            let r = self.getrandbits(k) as usize;
            if r < n {
                return r;
            }
        }
    }

    /// `randrange(start, stop)`.
    pub fn randrange(&mut self, start: usize, stop: usize) -> usize {
        start + self.randbelow(stop - start)
    }

    /// `choice(seq)`.
    pub fn choice<'a, T>(&mut self, seq: &'a [T]) -> &'a T {
        &seq[self.randbelow(seq.len())]
    }

    /// `shuffle(x)`: backwards, swapping each element with one at or before it.
    pub fn shuffle<T>(&mut self, x: &mut [T]) {
        for i in (1..x.len()).rev() {
            let j = self.randbelow(i + 1);
            x.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every expected value in this module was printed by CPython 3.12 and transcribed. The point
    /// of the module is to agree with that interpreter, so deriving the values by reasoning about
    /// the algorithm would only test the reasoning twice -- which is exactly what a first draft of
    /// these tests did, and three of its five guesses were wrong.
    #[test]
    fn random_matches_cpython_for_seed_13() {
        let mut r = PyRandom::new(13);
        let want = [
            0.2590084917154736,
            0.6852579929645369,
            0.6840819180161107,
            0.8493361613899302,
            0.1857241738737354,
        ];
        for (i, w) in want.iter().enumerate() {
            let got = r.random();
            assert!((got - w).abs() < 1e-15, "draw {i}: {got} against {w}");
        }
    }

    #[test]
    fn random_matches_cpython_for_seed_0() {
        let mut r = PyRandom::new(0);
        let want = [0.8444218515250481, 0.7579544029403025, 0.420571580830845];
        for (i, w) in want.iter().enumerate() {
            let got = r.random();
            assert!((got - w).abs() < 1e-15, "draw {i}: {got} against {w}");
        }
    }

    #[test]
    fn shuffle_matches_cpython() {
        // >>> r = random.Random(13); x = list(range(10)); r.shuffle(x); x
        let mut x: Vec<i32> = (0..10).collect();
        PyRandom::new(13).shuffle(&mut x);
        assert_eq!(x, vec![3, 0, 7, 8, 6, 1, 5, 2, 9, 4]);
    }

    #[test]
    fn choice_and_randrange_match_cpython() {
        // >>> r = random.Random(7); "".join(r.choice("yz") for _ in range(6))
        let mut r = PyRandom::new(7);
        let yz: Vec<char> = "yz".chars().collect();
        let got: String = (0..6).map(|_| *r.choice(&yz)).collect();
        assert_eq!(got, "zyzyyy");

        // >>> r = random.Random(7); [r.randrange(1, 9) for _ in range(6)]
        let mut r = PyRandom::new(7);
        let got: Vec<usize> = (0..6).map(|_| r.randrange(1, 9)).collect();
        assert_eq!(got, vec![6, 3, 7, 1, 2, 2]);
    }
}
