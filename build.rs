// Build script to embed resources into the executable

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::Serialize;
use std::fs;
use std::path::Path;

/// Secrets structure to encrypt
#[derive(Serialize)]
struct SecretsData {
    discogs_consumer_key: String,
    discogs_consumer_secret: String,
}

/// Encrypt secrets and write to file for embedding
fn encrypt_secrets() {
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let secrets_path = Path::new(&out_dir).join("secrets.enc");

    // Check if we have all required environment variables
    let encryption_key = std::env::var("ENCRYPTION_KEY").ok();
    let consumer_key = std::env::var("DISCOGS_CONSUMER_KEY").ok();
    let consumer_secret = std::env::var("DISCOGS_CONSUMER_SECRET").ok();

    match (encryption_key, consumer_key, consumer_secret) {
        (Some(key), Some(ck), Some(cs)) => {
            println!("cargo:warning=Encrypting Discogs secrets for release build");

            // Decode the base64 encryption key
            let key_bytes = BASE64.decode(&key).expect("Invalid ENCRYPTION_KEY base64");
            assert_eq!(key_bytes.len(), 32, "ENCRYPTION_KEY must be 32 bytes (256 bits)");

            // Create the secrets JSON
            let secrets = SecretsData {
                discogs_consumer_key: ck,
                discogs_consumer_secret: cs,
            };
            let plaintext = serde_json::to_string(&secrets).unwrap();

            // Generate a random nonce (12 bytes for AES-GCM)
            // For reproducible builds, we derive it from the key
            let nonce_bytes: [u8; 12] = {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                key_bytes.hash(&mut hasher);
                let hash = hasher.finish();
                let mut nonce = [0u8; 12];
                nonce[..8].copy_from_slice(&hash.to_le_bytes());
                nonce
            };
            let nonce = Nonce::from_slice(&nonce_bytes);

            // Encrypt
            let cipher = Aes256Gcm::new_from_slice(&key_bytes).unwrap();
            let ciphertext = cipher
                .encrypt(nonce, plaintext.as_bytes())
                .expect("Encryption failed");

            // Write: nonce (12 bytes) + ciphertext
            let mut output = Vec::new();
            output.extend_from_slice(&nonce_bytes);
            output.extend_from_slice(&ciphertext);

            // Write as base64 for easier embedding
            let encoded = BASE64.encode(&output);
            fs::write(&secrets_path, &encoded).expect("Failed to write secrets.enc");

            println!("cargo:warning=Secrets encrypted successfully ({} bytes)", encoded.len());
        }
        _ => {
            // No secrets available - write empty marker
            // This allows local development without secrets
            fs::write(&secrets_path, "").expect("Failed to write empty secrets.enc");
            println!("cargo:warning=No secrets environment variables found, using empty secrets");
        }
    }

    // Tell cargo to rerun if these env vars change
    println!("cargo:rerun-if-env-changed=ENCRYPTION_KEY");
    println!("cargo:rerun-if-env-changed=DISCOGS_CONSUMER_KEY");
    println!("cargo:rerun-if-env-changed=DISCOGS_CONSUMER_SECRET");
}

fn main() {
    // Encrypt secrets for embedding
    encrypt_secrets();
    // Set version at compile time
    // Reads from RELEASE_VERSION env var (set by CI) or falls back to Cargo.toml version
    let version = std::env::var("RELEASE_VERSION")
        .unwrap_or_else(|_| std::env::var("CARGO_PKG_VERSION").unwrap());
    
    // Add -dev suffix for debug builds
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let full_version = if profile == "debug" && std::env::var("RELEASE_VERSION").is_err() {
        format!("{}-dev", version)
    } else {
        version
    };
    
    println!("cargo:rustc-env=APP_VERSION={}", full_version);
    
    // Windows-specific icon and resource embedding
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/icons/icon.ico");
        
        // Set application info
        res.set("ProductName", "ODE Artwork Downloader");
        res.set("FileDescription", "Download artwork for ODE disc images");
        res.set("CompanyName", "dani");
        
        // Use the APP_VERSION we just set
        res.set("FileVersion", &full_version);
        res.set("ProductVersion", &full_version);
        
        if let Err(e) = res.compile() {
            eprintln!("Warning: Failed to compile Windows resources: {}", e);
            eprintln!("The .exe will still work but won't have an embedded icon.");
        }
    }
}
