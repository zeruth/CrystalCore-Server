//! The lobby connection's encryption, as ffxiv.exe 672261 does it
//! (`LobbyProtoChannel::ServiceConsumerConnectionManager::LobbyCryptEngine`).
//!
//! The client sends an encryption init (segment type 9) carrying a key phrase and a key (its
//! `time()` when it first connects). Both ends hash them into a Blowfish key:
//!
//! ```text
//! MD5( 0x12345678 u32 | key u32 | 1000 u32 | key phrase [u8; 32] )      all little-endian
//! ```
//!
//! (0x12345678 and 1000 are set by the engine's constructor @0xe377b0; the hash is taken by
//! `ReadEncryptionReply` @0xe38140.) The server answers with an encryption reply (segment type 10)
//! whose payload is encrypted with that key; from then on IPC payloads are encrypted too.
//!
//! The Blowfish is the standard one with two differences that matter on the wire:
//! - each 8-byte block is two little-endian `u32` halves (x86 memory order), not big-endian;
//! - the key schedule reads key bytes as signed `char` and ORs them in sign-extended, so a key byte
//!   of 0x80 or above sets every higher bit of the word being built (@0x409a70).

mod tables;

use md5::{Digest, Md5};

/// Encryption parameters the 672261 client puts in front of the key and key phrase.
const KEY_MAGIC: u32 = 0x1234_5678;
const KEY_VERSION: u32 = 1000;
/// The key phrase's field in the hashed block (and in the encryption init).
pub const KEY_PHRASE_LEN: usize = 32;

/// The Blowfish key for a connection: the MD5 digest of the base key block. `key_phrase` is used
/// up to [`KEY_PHRASE_LEN`] bytes and zero-padded, as the client keeps it.
pub fn lobby_key(key: u32, key_phrase: &[u8]) -> [u8; 16] {
    let mut block = [0u8; 12 + KEY_PHRASE_LEN];
    block[0..4].copy_from_slice(&KEY_MAGIC.to_le_bytes());
    block[4..8].copy_from_slice(&key.to_le_bytes());
    block[8..12].copy_from_slice(&KEY_VERSION.to_le_bytes());
    let n = key_phrase.len().min(KEY_PHRASE_LEN);
    block[12..12 + n].copy_from_slice(&key_phrase[..n]);
    Md5::digest(block).into()
}

/// Blowfish keyed the client's way. 0x1048 bytes, the size the client allocates for it.
#[derive(Clone)]
pub struct Blowfish {
    p: [u32; 18],
    s: [[u32; 256]; 4],
}

impl Blowfish {
    pub fn new(key: &[u8]) -> Self {
        assert!(!key.is_empty(), "Blowfish key is empty");
        let mut bf = Self { p: tables::P, s: tables::S };
        let mut j = 0;
        for i in 0..18 {
            let mut word = 0u32;
            for _ in 0..4 {
                // signed char, sign-extended, ORed in: the client's schedule, not the standard one
                word = (word << 8) | key[j] as i8 as i32 as u32;
                j = (j + 1) % key.len();
            }
            bf.p[i] ^= word;
        }
        let (mut l, mut r) = (0, 0);
        for i in (0..18).step_by(2) {
            (l, r) = bf.encrypt_block(l, r);
            bf.p[i] = l;
            bf.p[i + 1] = r;
        }
        for i in 0..4 {
            for k in (0..256).step_by(2) {
                (l, r) = bf.encrypt_block(l, r);
                bf.s[i][k] = l;
                bf.s[i][k + 1] = r;
            }
        }
        bf
    }

    fn f(&self, x: u32) -> u32 {
        let [a, b, c, d] = x.to_be_bytes();
        (self.s[0][a as usize].wrapping_add(self.s[1][b as usize]) ^ self.s[2][c as usize])
            .wrapping_add(self.s[3][d as usize])
    }

    fn encrypt_block(&self, mut l: u32, mut r: u32) -> (u32, u32) {
        for i in 0..16 {
            l ^= self.p[i];
            r ^= self.f(l);
            (l, r) = (r, l);
        }
        (r ^ self.p[17], l ^ self.p[16])
    }

    fn decrypt_block(&self, mut l: u32, mut r: u32) -> (u32, u32) {
        for i in (2..18).rev() {
            l ^= self.p[i];
            r ^= self.f(l);
            (l, r) = (r, l);
        }
        (r ^ self.p[0], l ^ self.p[1])
    }

    /// Encrypts the whole 8-byte blocks of `data` in place; a tail shorter than a block is left.
    pub fn encrypt(&self, data: &mut [u8]) {
        self.apply(data, Self::encrypt_block);
    }

    /// Decrypts the whole 8-byte blocks of `data` in place; a tail shorter than a block is left.
    pub fn decrypt(&self, data: &mut [u8]) {
        self.apply(data, Self::decrypt_block);
    }

    fn apply(&self, data: &mut [u8], block: fn(&Self, u32, u32) -> (u32, u32)) {
        for chunk in data.chunks_exact_mut(8) {
            let l = u32::from_le_bytes(chunk[0..4].try_into().unwrap());
            let r = u32::from_le_bytes(chunk[4..8].try_into().unwrap());
            let (l, r) = block(self, l, r);
            chunk[0..4].copy_from_slice(&l.to_le_bytes());
            chunk[4..8].copy_from_slice(&r.to_le_bytes());
        }
    }

    /// An IPC payload: only its leading multiple of 32 bytes is encrypted
    /// (`LobbyCryptEngine::Encrypt` @0xe369b0 passes `len & ~0x1f`).
    pub fn encrypt_ipc(&self, payload: &mut [u8]) {
        let n = payload.len() & !0x1f;
        self.encrypt(&mut payload[..n]);
    }

    /// The inverse of [`Self::encrypt_ipc`] (`LobbyCryptEngine::Decrypt` @0xe369e0).
    pub fn decrypt_ipc(&self, payload: &mut [u8]) {
        let n = payload.len() & !0x1f;
        self.decrypt(&mut payload[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Vectors from the client's own code (MD5 @0x408da0/0x40b1f0/0x40b2a0, Blowfish @0x409a70,
    // @0x407e80, @0x407f40) run under 32-bit emulation.

    const PLAIN: [u8; 32] = {
        let mut b = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            b[i] = i as u8;
            i += 1;
        }
        b
    };

    #[test]
    fn key_of_the_capture() {
        // key 0x6ade798e and "Test Ticket Data", from protocol/672261/captures/lobby-encryption-init.bin
        assert_eq!(hex::encode(lobby_key(0x6ade_798e, b"Test Ticket Data")), "528dcf3065a2a2ad40bb62e0f15cab56");
    }

    #[test]
    fn blowfish_matches_the_client() {
        let bf = Blowfish::new(&lobby_key(0x6ade_798e, b"Test Ticket Data"));
        let mut data = PLAIN;
        bf.encrypt(&mut data);
        assert_eq!(hex::encode(data), "ddd28fe795ec99087b2a37a67b52c8b456c90a4331284b2c50e567153c8215fd");
        bf.decrypt(&mut data);
        assert_eq!(data, PLAIN);
    }

    #[test]
    fn signed_key_schedule() {
        let key: Vec<u8> = (0x80..0x90).collect();
        let mut data = PLAIN;
        Blowfish::new(&key).encrypt(&mut data);
        assert_eq!(hex::encode(data), "cbbde671d7389d86b31b7d2eeee831e6f273bcb0ee383eb43680e2849efdaff4");
    }

    #[test]
    fn standard_vector_little_endian() {
        // Schneier's zero key, zero block: 4EF99745 6198DD78, here as little-endian words
        let mut data = [0u8; 8];
        Blowfish::new(&[0; 8]).encrypt(&mut data);
        assert_eq!(hex::encode(data), "4597f94e78dd9861");
    }

    #[test]
    fn ipc_leaves_the_tail() {
        let bf = Blowfish::new(b"key");
        let mut data = [7u8; 40];
        bf.encrypt_ipc(&mut data);
        assert_eq!(data[32..], [7u8; 8]);
        bf.decrypt_ipc(&mut data);
        assert_eq!(data, [7u8; 40]);
    }
}
