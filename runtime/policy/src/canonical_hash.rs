use crate::PolicyError;

const BLOCK_BYTES: usize = 64;
const LENGTH_OFFSET: usize = 56;
const CANONICAL_MAGIC: &[u8] = b"PDF.rs canonical identity\0";
const CANONICAL_SCHEMA: u16 = 1;

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

/// Infallible-looking canonical field writer with deferred checked SHA-256 framing failure.
pub(crate) struct CanonicalHasher {
    sha: Sha256,
    failed: bool,
}

impl CanonicalHasher {
    /// Creates a typed, schema-versioned hash stream.
    pub(crate) fn new(domain: &'static [u8]) -> Self {
        let mut value = Self {
            sha: Sha256::new(),
            failed: false,
        };
        value.bytes(CANONICAL_MAGIC);
        value.u16(CANONICAL_SCHEMA);
        value.u16(u16::try_from(domain.len()).unwrap_or(u16::MAX));
        value.bytes(domain);
        value
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes(&[value]);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn i32(&mut self, value: i32) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn i64(&mut self, value: i64) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes(&value.to_be_bytes());
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) {
        if !self.failed && self.sha.update(bytes).is_err() {
            self.failed = true;
        }
    }

    pub(crate) fn finish(self) -> Result<[u8; 32], PolicyError> {
        if self.failed {
            return Err(PolicyError::numeric_overflow());
        }
        self.sha
            .finalize()
            .map_err(|()| PolicyError::numeric_overflow())
    }
}

#[cfg(test)]
pub(crate) fn hash_preimage(preimage: &[u8]) -> Result<[u8; 32], PolicyError> {
    hash_preimage_observed(preimage, || Ok(()))
}

pub(crate) fn hash_preimage_observed(
    preimage: &[u8],
    mut observe: impl FnMut() -> Result<(), PolicyError>,
) -> Result<[u8; 32], PolicyError> {
    const HASH_CHUNK_BYTES: usize = 4 * 1024;
    let mut sha = Sha256::new();
    for chunk in preimage.chunks(HASH_CHUNK_BYTES) {
        observe()?;
        sha.update(chunk)
            .map_err(|()| PolicyError::numeric_overflow())?;
    }
    observe()?;
    sha.finalize().map_err(|()| PolicyError::numeric_overflow())
}

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; BLOCK_BYTES],
    buffer_len: usize,
    message_len: u64,
}

impl Sha256 {
    const fn new() -> Self {
        Self {
            state: INITIAL_STATE,
            buffer: [0; BLOCK_BYTES],
            buffer_len: 0,
            message_len: 0,
        }
    }

    fn update(&mut self, mut input: &[u8]) -> Result<(), ()> {
        let input_len = u64::try_from(input.len()).map_err(|_| ())?;
        self.message_len = self.message_len.checked_add(input_len).ok_or(())?;

        if self.buffer_len != 0 {
            let copied = (BLOCK_BYTES - self.buffer_len).min(input.len());
            let end = self.buffer_len + copied;
            self.buffer[self.buffer_len..end].copy_from_slice(&input[..copied]);
            self.buffer_len = end;
            input = &input[copied..];
            if self.buffer_len == BLOCK_BYTES {
                compress(&mut self.state, &self.buffer);
                self.buffer_len = 0;
            } else {
                return Ok(());
            }
        }

        let mut chunks = input.chunks_exact(BLOCK_BYTES);
        for chunk in &mut chunks {
            let mut block = [0_u8; BLOCK_BYTES];
            block.copy_from_slice(chunk);
            compress(&mut self.state, &block);
        }
        let remainder = chunks.remainder();
        self.buffer[..remainder.len()].copy_from_slice(remainder);
        self.buffer_len = remainder.len();
        Ok(())
    }

    fn finalize(mut self) -> Result<[u8; 32], ()> {
        let bit_len = self.message_len.checked_mul(8).ok_or(())?;
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

        let mut digest = [0_u8; 32];
        for (target, word) in digest.chunks_exact_mut(4).zip(self.state) {
            target.copy_from_slice(&word.to_be_bytes());
        }
        Ok(digest)
    }
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
        let sigma1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let choose = (e & f) ^ ((!e) & g);
        let temporary1 = h
            .wrapping_add(sigma1)
            .wrapping_add(choose)
            .wrapping_add(ROUND_CONSTANTS[index])
            .wrapping_add(schedule[index]);
        let sigma0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let majority = (a & b) ^ (a & c) ^ (b & c);
        let temporary2 = sigma0.wrapping_add(majority);
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
    use super::{CanonicalHasher, Sha256};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn domain_and_field_boundaries_are_part_of_identity() {
        let mut first = CanonicalHasher::new(b"a");
        first.u16(0x1234);
        let first = first.finish().unwrap();
        let mut second = CanonicalHasher::new(b"b");
        second.u16(0x1234);
        let second = second.finish().unwrap();
        let mut third = CanonicalHasher::new(b"a");
        third.bytes(&[0x12, 0x34]);
        let third = third.finish().unwrap();
        assert_ne!(first, second);
        assert_eq!(first, third);
        assert_eq!(hex(&first).len(), 64);
    }

    #[test]
    fn local_sha256_matches_the_published_abc_vector() {
        let mut sha = Sha256::new();
        sha.update(b"abc").unwrap();
        assert_eq!(
            hex(&sha.finalize().unwrap()),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
