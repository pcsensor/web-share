//! Seal small secrets (TOTP keys) at rest with AES-256-GCM.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;
use anyhow::{anyhow, Context};
use rand::Rng;

const NONCE_LEN: usize = 12;

/// Seal plaintext; returns `v1:` + hex(nonce || ciphertext+tag).
pub fn seal(key: &[u8; 32], plaintext: &[u8]) -> anyhow::Result<String> {
    let cipher = Aes256Gcm::new_from_slice(key).context("invalid seal key")?;
    let mut nonce_bytes = [0u8; NONCE_LEN];
    rand::rng().fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt((&nonce_bytes).into(), plaintext)
        .map_err(|_| anyhow!("encrypt failed"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(format!("v1:{}", hex::encode(out)))
}

pub fn unseal(key: &[u8; 32], sealed: &str) -> anyhow::Result<Vec<u8>> {
    let hex_part = sealed
        .strip_prefix("v1:")
        .ok_or_else(|| anyhow!("unknown seal version"))?;
    let raw = hex::decode(hex_part).context("invalid seal hex")?;
    if raw.len() < NONCE_LEN + 16 {
        return Err(anyhow!("seal too short"));
    }
    let (nonce_bytes, ct) = raw.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new_from_slice(key).context("invalid seal key")?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(nonce_bytes);
    cipher
        .decrypt((&nonce).into(), ct)
        .map_err(|_| anyhow!("decrypt failed"))
}

/// Parse or derive a 32-byte key from env-style material.
pub fn parse_secret_key(raw: &str) -> anyhow::Result<[u8; 32]> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("CHAT_SECRET_KEY is empty"));
    }
    // Prefer 64-char hex
    if trimmed.len() == 64 {
        if let Ok(bytes) = hex::decode(trimmed) {
            if bytes.len() == 32 {
                let mut key = [0u8; 32];
                key.copy_from_slice(&bytes);
                return Ok(key);
            }
        }
    }
    // Otherwise SHA-256 the string (dev convenience)
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(trimmed.as_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&digest);
    Ok(key)
}

pub fn generate_secret_key_hex() -> String {
    let mut key = [0u8; 32];
    rand::rng().fill_bytes(&mut key);
    hex::encode(key)
}
