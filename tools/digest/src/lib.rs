#![forbid(unsafe_code)]
#![deny(missing_docs)]

//! Small, deterministic SHA-256 implementation for content-addressed tooling artifacts.
//!
//! The implementation follows the equations and constants published in FIPS
//! 180-4. It is test tooling, not a cryptographic API for PDF security handlers.

use std::fmt;

const BLOCK_BYTES: usize = 64;
const LENGTH_OFFSET: usize = 56;

const INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// Failure to represent an input length in SHA-256's 64-bit bit-length field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HashError {
    /// The cumulative byte or bit length exceeds the algorithm's framing limit.
    InputTooLong,
}

impl fmt::Display for HashError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InputTooLong => {
                formatter.write_str("SHA-256 input length exceeds 64-bit framing")
            }
        }
    }
}

impl std::error::Error for HashError {}

/// Incremental SHA-256 state for bounded development-tool inputs.
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; BLOCK_BYTES],
    buffer_len: usize,
    message_len: u64,
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    /// Creates an empty SHA-256 state.
    pub const fn new() -> Self {
        Self {
            state: INITIAL_STATE,
            buffer: [0; BLOCK_BYTES],
            buffer_len: 0,
            message_len: 0,
        }
    }

    /// Adds bytes without retaining the caller's buffer.
    pub fn update(&mut self, mut input: &[u8]) -> Result<(), HashError> {
        let input_len = u64::try_from(input.len()).map_err(|_| HashError::InputTooLong)?;
        self.message_len = self
            .message_len
            .checked_add(input_len)
            .ok_or(HashError::InputTooLong)?;

        if self.buffer_len != 0 {
            let missing = BLOCK_BYTES - self.buffer_len;
            let copied = missing.min(input.len());
            let end = self.buffer_len + copied;
            self.buffer[self.buffer_len..end].copy_from_slice(&input[..copied]);
            self.buffer_len = end;
            input = &input[copied..];

            if self.buffer_len == BLOCK_BYTES {
                compress(&mut self.state, &self.buffer);
                self.buffer_len = 0;
            } else {
                debug_assert!(input.is_empty());
                return Ok(());
            }
        }

        let mut chunks = input.chunks_exact(BLOCK_BYTES);
        for chunk in &mut chunks {
            let block: &[u8; BLOCK_BYTES] = chunk
                .try_into()
                .expect("chunks_exact yields one complete SHA-256 block");
            compress(&mut self.state, block);
        }

        let remainder = chunks.remainder();
        self.buffer[..remainder.len()].copy_from_slice(remainder);
        self.buffer_len = remainder.len();
        Ok(())
    }

    /// Consumes the state and returns the 32-byte digest.
    pub fn finalize(mut self) -> Result<[u8; 32], HashError> {
        let bit_len = self
            .message_len
            .checked_mul(8)
            .ok_or(HashError::InputTooLong)?;

        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;

        if self.buffer_len > LENGTH_OFFSET {
            self.buffer[self.buffer_len..].fill(0);
            compress(&mut self.state, &self.buffer);
            self.buffer = [0; BLOCK_BYTES];
            self.buffer_len = 0;
        }

        self.buffer[self.buffer_len..LENGTH_OFFSET].fill(0);
        self.buffer[LENGTH_OFFSET..].copy_from_slice(&bit_len.to_be_bytes());
        compress(&mut self.state, &self.buffer);

        let mut digest = [0; 32];
        for (chunk, word) in digest.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        Ok(digest)
    }
}

/// Hashes one in-memory byte slice.
pub fn sha256(input: &[u8]) -> Result<[u8; 32], HashError> {
    let mut hasher = Sha256::new();
    hasher.update(input)?;
    hasher.finalize()
}

/// Encodes a digest as 64 lowercase hexadecimal characters.
pub fn hex_digest(digest: &[u8; 32]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn compress(state: &mut [u32; 8], block: &[u8; BLOCK_BYTES]) {
    let mut schedule = [0_u32; 64];
    for (index, chunk) in block.chunks_exact(4).enumerate() {
        schedule[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    for index in 16..64 {
        let sigma0 = schedule[index - 15].rotate_right(7)
            ^ schedule[index - 15].rotate_right(18)
            ^ (schedule[index - 15] >> 3);
        let sigma1 = schedule[index - 2].rotate_right(17)
            ^ schedule[index - 2].rotate_right(19)
            ^ (schedule[index - 2] >> 10);
        schedule[index] = schedule[index - 16]
            .wrapping_add(sigma0)
            .wrapping_add(schedule[index - 7])
            .wrapping_add(sigma1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for index in 0..64 {
        let big_sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ ((!e) & g);
        let temporary1 = h
            .wrapping_add(big_sigma1)
            .wrapping_add(choose)
            .wrapping_add(ROUND_CONSTANTS[index])
            .wrapping_add(schedule[index]);
        let big_sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temporary2 = big_sigma0.wrapping_add(majority);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(temporary1);
        d = c;
        c = b;
        b = a;
        a = temporary1.wrapping_add(temporary2);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
    state[4] = state[4].wrapping_add(e);
    state[5] = state[5].wrapping_add(f);
    state[6] = state[6].wrapping_add(g);
    state[7] = state[7].wrapping_add(h);
}

#[cfg(test)]
mod tests {
    use super::{Sha256, hex_digest, sha256};

    #[test]
    fn matches_published_short_vectors() {
        assert_eq!(
            hex_digest(&sha256(b"").unwrap()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_digest(&sha256(b"abc").unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex_digest(
                &sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq").unwrap()
            ),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn chunk_boundaries_do_not_change_the_digest() {
        let input = vec![0x5a; 1025];
        let expected = sha256(&input).unwrap();
        let mut hasher = Sha256::new();
        for chunk in input.chunks(13) {
            hasher.update(chunk).unwrap();
        }
        assert_eq!(hasher.finalize().unwrap(), expected);
    }
}
