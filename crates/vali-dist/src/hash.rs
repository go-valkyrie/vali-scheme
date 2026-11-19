//! SHA-256, the hash artifacts' fingerprints and dependency records use
//! (ADR 0001 #9).
//!
//! The format needs a collision-resistant hash and nothing else from a
//! crypto library, so it is written out here (FIPS 180-4) rather than
//! pulled in as a dependency of every crate that reads artifacts.

use std::fmt;

/// A SHA-256 digest.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest(pub [u8; 32]);

impl Digest {
    /// The digest of `bytes`.
    pub fn of(bytes: &[u8]) -> Digest {
        let mut hasher = Hasher::new();
        hasher.update(bytes);
        hasher.finish()
    }

    /// The digest as lower-case hexadecimal.
    pub fn hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "sha256:{}", self.hex())
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

/// A fast 128-bit digest of `bytes`, for telling whether two builds of a
/// library are the same one: the same bytes, the same digest, in any build
/// of Vali. It is not collision resistant, and not a defence against
/// bytes made to collide; where that matters, use [`Digest`].
pub fn fingerprint(bytes: &[u8]) -> u128 {
    const A: u64 = 0x9e37_79b9_7f4a_7c15;
    const B: u64 = 0xc2b2_ae3d_27d4_eb4f;
    let word = |chunk: &[u8]| u64::from_le_bytes(chunk.try_into().expect("eight bytes"));
    let length = bytes.len() as u64;
    let (mut a, mut b) = (A ^ length, B.rotate_left(17) ^ length);
    let mut chunks = bytes.chunks_exact(16);
    for chunk in &mut chunks {
        // Two lanes, each fed alternate words, so they run side by side;
        // the first also takes the second's state, so a word moves both.
        let (x, y) = (word(&chunk[..8]), word(&chunk[8..]));
        a = (a ^ x).wrapping_mul(A);
        b = (b ^ y).wrapping_mul(B);
        a ^= a >> 29;
        b ^= b >> 31;
        a ^= b.rotate_left(23);
    }
    let mut rest = [0u8; 16];
    rest[..chunks.remainder().len()].copy_from_slice(chunks.remainder());
    a = (a ^ word(&rest[..8])).wrapping_mul(A);
    b = (b ^ word(&rest[8..])).wrapping_mul(B);
    // Both lanes reach every bit of both outputs.
    a ^= b.rotate_left(32);
    b ^= a.rotate_left(32);
    a = (a ^ (a >> 32)).wrapping_mul(B);
    b = (b ^ (b >> 32)).wrapping_mul(A);
    a ^= a >> 29;
    b ^= b >> 31;
    (u128::from(a) << 64) | u128::from(b)
}

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// An incremental SHA-256.
#[derive(Clone, Debug)]
pub struct Hasher {
    state: [u32; 8],
    block: [u8; 64],
    filled: usize,
    length: u64,
}

impl Default for Hasher {
    fn default() -> Hasher {
        Hasher::new()
    }
}

impl Hasher {
    pub fn new() -> Hasher {
        Hasher {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            block: [0; 64],
            filled: 0,
            length: 0,
        }
    }

    pub fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);
        if self.filled > 0 {
            let take = (64 - self.filled).min(bytes.len());
            self.block[self.filled..self.filled + take].copy_from_slice(&bytes[..take]);
            self.filled += take;
            bytes = &bytes[take..];
            if self.filled < 64 {
                return;
            }
            let block = self.block;
            self.compress(&block);
            self.filled = 0;
        }
        while bytes.len() >= 64 {
            let (block, rest) = bytes.split_at(64);
            self.compress(block.try_into().expect("64 bytes"));
            bytes = rest;
        }
        self.block[..bytes.len()].copy_from_slice(bytes);
        self.filled = bytes.len();
    }

    /// Feeds a length-prefixed field, so fields run together cannot be
    /// confused with others that split the same bytes differently.
    pub fn field(&mut self, bytes: &[u8]) {
        self.update(&(bytes.len() as u64).to_le_bytes());
        self.update(bytes);
    }

    pub fn finish(mut self) -> Digest {
        let bits = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.filled != 56 {
            self.update(&[0]);
        }
        self.update(&bits.to_be_bytes());
        debug_assert_eq!(self.filled, 0);
        let mut out = [0; 32];
        for (chunk, word) in out.chunks_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        Digest(out)
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for (i, chunk) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes(chunk.try_into().expect("4 bytes"));
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (s, v) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *s = s.wrapping_add(v);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_tell_bytes_apart() {
        let a: Vec<u8> = (0..1000u32).map(|i| (i * 7) as u8).collect();
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
        // Any one byte, and the length, change it.
        for i in 0..a.len() {
            let mut b = a.clone();
            b[i] ^= 1;
            assert_ne!(fingerprint(&a), fingerprint(&b), "byte {i}");
        }
        assert_ne!(fingerprint(&a), fingerprint(&a[..999]));
        assert_ne!(fingerprint(b""), fingerprint(b"\0"));
        assert_ne!(fingerprint(b"\0"), fingerprint(b"\0\0"));
    }

    #[test]
    fn matches_the_published_vectors() {
        assert_eq!(
            Digest::of(b"").hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            Digest::of(b"abc").hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            Digest::of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq").hex(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn pieces_hash_as_a_whole() {
        let text: Vec<u8> = (0..1000u32).map(|n| (n % 251) as u8).collect();
        let mut hasher = Hasher::new();
        for piece in text.chunks(37) {
            hasher.update(piece);
        }
        assert_eq!(hasher.finish(), Digest::of(&text));
        let million = vec![b'a'; 1_000_000];
        assert_eq!(
            Digest::of(&million).hex(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
}
