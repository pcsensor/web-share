//! RFC 6238 TOTP (SHA-1, 30s, 6 digits) + recovery-code helpers.

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha1::Sha1;
use sha2::{Digest, Sha256};

type HmacSha1 = Hmac<Sha1>;

const STEP_SECS: u64 = 30;
const DIGITS: u32 = 6;

/// Generate a 20-byte TOTP secret (160-bit, Authenticator-compatible).
pub fn generate_secret() -> Vec<u8> {
    let mut secret = vec![0u8; 20];
    rand::rng().fill_bytes(&mut secret);
    secret
}

pub fn secret_to_base32(secret: &[u8]) -> String {
    BASE32_NOPAD.encode(secret)
}

#[allow(dead_code)]
pub fn secret_from_base32(encoded: &str) -> Option<Vec<u8>> {
    let cleaned: String = encoded
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_uppercase();
    BASE32_NOPAD.decode(cleaned.as_bytes()).ok()
}

pub fn otpauth_uri(issuer: &str, account: &str, secret: &[u8]) -> String {
    let label_raw = format!("{issuer}:{account}");
    let label = urlencoding::encode(&label_raw);
    let issuer_q = urlencoding::encode(issuer);
    let secret_b32 = secret_to_base32(secret);
    format!(
        "otpauth://totp/{label}?secret={secret_b32}&issuer={issuer_q}&algorithm=SHA1&digits=6&period=30"
    )
}

#[cfg_attr(not(test), allow(dead_code))]
pub fn generate_code(secret: &[u8], unix_secs: u64) -> u32 {
    let counter = unix_secs / STEP_SECS;
    generate_hotp(secret, counter)
}

fn generate_hotp(secret: &[u8], counter: u64) -> u32 {
    let mut mac =
        HmacSha1::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&counter.to_be_bytes());
    let result = mac.finalize().into_bytes();
    let offset = (result[19] & 0x0f) as usize;
    let bin = ((result[offset] as u32 & 0x7f) << 24)
        | ((result[offset + 1] as u32) << 16)
        | ((result[offset + 2] as u32) << 8)
        | (result[offset + 3] as u32);
    bin % 10u32.pow(DIGITS)
}

/// Verify a user-provided code. Allows ±1 time step.
/// Returns the matched step counter on success (for replay protection).
pub fn verify_code(secret: &[u8], code: &str, unix_secs: u64) -> Option<u64> {
    let code = code.trim();
    if code.len() != 6 || !code.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let expected: u32 = code.parse().ok()?;
    let counter = unix_secs / STEP_SECS;
    for delta in [-1i64, 0, 1] {
        let c = counter as i64 + delta;
        if c < 0 {
            continue;
        }
        let c = c as u64;
        if generate_hotp(secret, c) == expected {
            return Some(c);
        }
    }
    None
}

/// 10 recovery codes, each 10 chars (XXXX-XXXX style without dash in storage form).
pub fn generate_recovery_codes(count: usize) -> Vec<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut out = Vec::with_capacity(count);
    let mut rng = rand::rng();
    for _ in 0..count {
        let mut bytes = [0u8; 8];
        rng.fill_bytes(&mut bytes);
        let mut s = String::with_capacity(9);
        for (i, b) in bytes.iter().enumerate() {
            if i == 4 {
                s.push('-');
            }
            s.push(ALPHABET[(*b as usize) % ALPHABET.len()] as char);
        }
        out.push(s);
    }
    out
}

pub fn hash_recovery_code(code: &str) -> String {
    let normalized: String = code
        .chars()
        .filter(|c| *c != '-' && !c.is_whitespace())
        .flat_map(|c| c.to_uppercase())
        .collect();
    let digest = Sha256::digest(normalized.as_bytes());
    hex::encode(digest)
}

#[allow(dead_code)]
pub fn codes_equal_hash(code: &str, hash: &str) -> bool {
    let computed = hash_recovery_code(code);
    // constant-time compare on hex strings via subtle
    use subtle::ConstantTimeEq;
    if computed.len() != hash.len() {
        return false;
    }
    computed.as_bytes().ct_eq(hash.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6238_sha1_vectors_approx() {
        // secret = "12345678901234567890"
        let secret = b"12345678901234567890";
        // At T=59, counter=1 → known vector 287082 for 8 digits; we use 6 digits
        let code = generate_code(secret, 59);
        assert_eq!(code, 287082 % 1_000_000);
    }

    #[test]
    fn roundtrip_base32() {
        let s = generate_secret();
        let b = secret_to_base32(&s);
        assert_eq!(secret_from_base32(&b).unwrap(), s);
    }
}
