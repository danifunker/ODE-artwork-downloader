//! Cryptographic utilities for secure secrets handling
//!
//! Decrypts embedded secrets at runtime using AES-256-GCM.

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Deserialize;
use std::sync::OnceLock;

/// Embedded encrypted secrets (generated at build time)
const ENCRYPTED_SECRETS: &str = include_str!(concat!(env!("OUT_DIR"), "/secrets.enc"));

/// Decrypted secrets cache
static DECRYPTED_SECRETS: OnceLock<Option<EmbeddedSecrets>> = OnceLock::new();

/// Decrypted secrets structure
#[derive(Debug, Deserialize, Clone)]
pub struct EmbeddedSecrets {
    pub discogs_consumer_key: String,
    pub discogs_consumer_secret: String,
}

impl EmbeddedSecrets {
    /// Check if credentials are available
    pub fn has_credentials(&self) -> bool {
        !self.discogs_consumer_key.is_empty() && !self.discogs_consumer_secret.is_empty()
    }
}

/// Get the embedded secrets (decrypted at first access)
///
/// Returns None if:
/// - No secrets were embedded at build time
/// - Decryption fails (wrong key, corrupted data)
/// - The ENCRYPTION_KEY environment variable is not set
pub fn get_embedded_secrets() -> Option<&'static EmbeddedSecrets> {
    DECRYPTED_SECRETS
        .get_or_init(|| decrypt_secrets())
        .as_ref()
}

/// Decrypt the embedded secrets
fn decrypt_secrets() -> Option<EmbeddedSecrets> {
    // Check if we have encrypted secrets
    if ENCRYPTED_SECRETS.is_empty() {
        log::debug!("No embedded secrets found (local development build)");
        return None;
    }

    // Get the encryption key from compile-time environment variable
    let key_base64 = option_env!("ENCRYPTION_KEY")?;

    let key_bytes = match BASE64.decode(key_base64) {
        Ok(bytes) if bytes.len() == 32 => bytes,
        Ok(_) => {
            log::error!("Invalid encryption key length");
            return None;
        }
        Err(e) => {
            log::error!("Failed to decode encryption key: {}", e);
            return None;
        }
    };

    // Decode the encrypted data
    let encrypted_data = match BASE64.decode(ENCRYPTED_SECRETS.trim()) {
        Ok(data) => data,
        Err(e) => {
            log::error!("Failed to decode encrypted secrets: {}", e);
            return None;
        }
    };

    // Extract nonce (first 12 bytes) and ciphertext
    if encrypted_data.len() < 12 {
        log::error!("Encrypted data too short");
        return None;
    }

    let (nonce_bytes, ciphertext) = encrypted_data.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    // Decrypt
    let cipher = Aes256Gcm::new_from_slice(&key_bytes).ok()?;
    let plaintext = match cipher.decrypt(nonce, ciphertext) {
        Ok(data) => data,
        Err(e) => {
            log::error!("Failed to decrypt secrets: {}", e);
            return None;
        }
    };

    // Parse JSON
    match serde_json::from_slice(&plaintext) {
        Ok(secrets) => {
            log::debug!("Successfully decrypted embedded secrets");
            Some(secrets)
        }
        Err(e) => {
            log::error!("Failed to parse decrypted secrets: {}", e);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_no_embedded_secrets_in_dev() {
        // In development builds without secrets, this should return None gracefully
        let secrets = get_embedded_secrets();
        // This test just ensures it doesn't panic
        println!("Embedded secrets available: {}", secrets.is_some());
    }
}
