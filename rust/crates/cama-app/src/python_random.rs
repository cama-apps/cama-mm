//! Minimal port of CPython's `random.Random` for parity-critical selections.
//!
//! Python seeds a Mersenne Twister from an integer via `init_by_array` over
//! the integer's 32-bit little-endian words, and `choice(seq)` draws through
//! `_randbelow` (rejection sampling over `getrandbits`). Reproducing that
//! exact stream is required wherever Python derives a deterministic pick from
//! a seed (e.g. the daily economy event), because any other RNG selects a
//! different element from the same shortlist.
//!
//! Only the pieces the runtime needs are ported: integer seeding,
//! `getrandbits(k)` for `k <= 32`, and `randbelow`.

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

/// CPython-compatible Mersenne Twister (`random.Random(seed)`).
pub struct PythonRandom {
    state: [u32; N],
    index: usize,
}

impl PythonRandom {
    /// Equivalent to `random.Random(seed)` for a non-negative integer seed
    /// that fits in 64 bits (CPython splits the integer into 32-bit
    /// little-endian words and calls `init_by_array`).
    #[must_use]
    pub fn from_u64_seed(seed: u64) -> Self {
        let mut key = Vec::with_capacity(2);
        let mut remaining = seed;
        loop {
            key.push((remaining & 0xffff_ffff) as u32);
            remaining >>= 32;
            if remaining == 0 {
                break;
            }
        }
        Self::init_by_array(&key)
    }

    fn init_genrand(seed: u32) -> Self {
        let mut state = [0_u32; N];
        state[0] = seed;
        for index in 1..N {
            state[index] = 1_812_433_253_u32
                .wrapping_mul(state[index - 1] ^ (state[index - 1] >> 30))
                .wrapping_add(index as u32);
        }
        Self { state, index: N }
    }

    fn init_by_array(key: &[u32]) -> Self {
        let mut twister = Self::init_genrand(19_650_218);
        let mut i = 1_usize;
        let mut j = 0_usize;
        for _ in 0..N.max(key.len()) {
            twister.state[i] = (twister.state[i]
                ^ (twister.state[i - 1] ^ (twister.state[i - 1] >> 30)).wrapping_mul(1_664_525))
            .wrapping_add(key[j])
            .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                twister.state[0] = twister.state[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
        }
        for _ in 0..N - 1 {
            twister.state[i] = (twister.state[i]
                ^ (twister.state[i - 1] ^ (twister.state[i - 1] >> 30))
                    .wrapping_mul(1_566_083_941))
            .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                twister.state[0] = twister.state[N - 1];
                i = 1;
            }
        }
        twister.state[0] = 0x8000_0000;
        twister.index = N;
        twister
    }

    fn genrand_u32(&mut self) -> u32 {
        if self.index >= N {
            for i in 0..N {
                let y = (self.state[i] & UPPER_MASK) | (self.state[(i + 1) % N] & LOWER_MASK);
                let mut next = self.state[(i + M) % N] ^ (y >> 1);
                if y & 1 == 1 {
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
        y ^ (y >> 18)
    }

    /// `random.getrandbits(k)` for `1 <= k <= 32`.
    fn getrandbits_u32(&mut self, bits: u32) -> u32 {
        debug_assert!((1..=32).contains(&bits));
        self.genrand_u32() >> (32 - bits)
    }

    /// CPython's `Random._randbelow` (rejection sampling), the primitive
    /// behind `choice`, `randrange`, and `shuffle`.
    pub fn randbelow(&mut self, upper: usize) -> usize {
        if upper == 0 {
            return 0;
        }
        let n = u32::try_from(upper).expect("randbelow upper bound fits in 32 bits");
        let bits = 32 - n.leading_zeros();
        loop {
            let candidate = self.getrandbits_u32(bits);
            if candidate < n {
                return candidate as usize;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getrandbits_matches_cpython_stream() {
        // random.Random(595510540519174598); [getrandbits(32) for _ in range(5)]
        let mut rng = PythonRandom::from_u64_seed(595_510_540_519_174_598);
        let observed: Vec<u32> = (0..5).map(|_| rng.getrandbits_u32(32)).collect();
        assert_eq!(
            observed,
            [
                2_529_460_510,
                1_464_947_751,
                1_816_440_056,
                2_293_144_193,
                800_333_305
            ]
        );
    }

    #[test]
    fn randbelow_handles_degenerate_bounds() {
        let mut rng = PythonRandom::from_u64_seed(1);
        assert_eq!(rng.randbelow(0), 0);
        // n == 1 still consumes bits exactly like CPython's rejection loop.
        assert_eq!(rng.randbelow(1), 0);
    }
}
