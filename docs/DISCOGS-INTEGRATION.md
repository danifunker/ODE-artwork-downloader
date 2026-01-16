# Discogs API Integration & Secrets Management

This document explains the Discogs API integration, secrets management system, and CI/CD build configuration for embedding API credentials securely.

## Overview

The application uses the Discogs API to search for album artwork. To avoid rate limiting and get better API access, we use authenticated API requests with consumer key/secret credentials.

**Key features:**
- Discogs API credentials are encrypted at build time
- Release builds have credentials embedded securely
- Development builds fall back to `secrets.json` or anonymous access
- All packaging formats (Windows, macOS, AppImage, Snap, Flatpak) use the same encrypted credentials

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                        Build Time                                │
├─────────────────────────────────────────────────────────────────┤
│  GitHub Secrets (PRODUCTION environment)                        │
│  ├── ENCRYPTION_KEY (32 bytes, base64 encoded)                  │
│  ├── DISCOGS_CONSUMER_KEY                                       │
│  └── DISCOGS_CONSUMER_SECRET                                    │
│                           │                                      │
│                           ▼                                      │
│  build.rs: Encrypts secrets with AES-256-GCM                    │
│                           │                                      │
│                           ▼                                      │
│  OUT_DIR/secrets.enc (embedded in binary via include_str!)      │
└─────────────────────────────────────────────────────────────────┘

┌─────────────────────────────────────────────────────────────────┐
│                        Runtime                                   │
├─────────────────────────────────────────────────────────────────┤
│  crypto.rs: Decrypts embedded secrets using compile-time key   │
│                           │                                      │
│                           ▼                                      │
│  discogs.rs: Uses credentials for API authentication            │
│                                                                  │
│  Fallback chain:                                                 │
│  1. Embedded encrypted secrets (release builds)                  │
│  2. secrets.json file (local development)                        │
│  3. Anonymous access (no credentials)                            │
└─────────────────────────────────────────────────────────────────┘
```

## Files Involved

| File | Purpose |
|------|---------|
| `src/api/discogs.rs` | Discogs API client with search and image retrieval |
| `src/crypto.rs` | Runtime decryption of embedded secrets |
| `src/config.rs` | Configuration loading including `secrets.json` |
| `build.rs` | Build-time encryption of secrets |
| `secrets.json` | Local development credentials (git-ignored) |
| `.github/workflows/release.yml` | CI/CD with secrets injection |

## Setup Instructions

### 1. Generate Encryption Key

Generate a 32-byte encryption key encoded as base64:

```bash
openssl rand -base64 32
```

This outputs a 44-character string like: `K7gNU3sdo+OL0wNhqoVWhr3g6s1xYv72ol/pe/Unols=`

### 2. Get Discogs API Credentials

1. Go to https://www.discogs.com/settings/developers
2. Create a new application
3. Note the Consumer Key and Consumer Secret

### 3. Configure GitHub Secrets

1. Go to your GitHub repository → Settings → Environments
2. Create an environment named `PRODUCTION`
3. Add these secrets:

| Secret Name | Value |
|-------------|-------|
| `ENCRYPTION_KEY` | Output from `openssl rand -base64 32` (44 chars) |
| `DISCOGS_CONSUMER_KEY` | Your Discogs consumer key |
| `DISCOGS_CONSUMER_SECRET` | Your Discogs consumer secret |

### 4. Local Development Setup

For local development, create a `secrets.json` file in the project root:

```json
{
  "discogs": {
    "consumer_key": "your_consumer_key",
    "consumer_secret": "your_consumer_secret"
  }
}
```

This file is git-ignored and only used for local development.

## How It Works

### Build Time (build.rs)

1. Checks for environment variables: `ENCRYPTION_KEY`, `DISCOGS_CONSUMER_KEY`, `DISCOGS_CONSUMER_SECRET`
2. If all present (CI release build):
   - Decodes the base64 encryption key (must be exactly 32 bytes)
   - Creates JSON with the Discogs credentials
   - Encrypts using AES-256-GCM with a deterministic nonce
   - Writes base64-encoded ciphertext to `OUT_DIR/secrets.enc`
3. If missing (local development):
   - Writes empty file to `OUT_DIR/secrets.enc`

### Runtime (crypto.rs)

1. `ENCRYPTED_SECRETS` constant contains the embedded `secrets.enc` content
2. `get_embedded_secrets()` attempts decryption:
   - If secrets are empty → returns `None` (dev build)
   - Decodes encryption key from compile-time `ENCRYPTION_KEY` env var
   - Decrypts using AES-256-GCM
   - Parses JSON and returns `EmbeddedSecrets`

### API Client (discogs.rs)

1. `build_client()` creates HTTP client with authentication:
   - First tries embedded secrets (`get_embedded_secrets()`)
   - Falls back to `secrets.json` (`get_secrets()`)
   - Falls back to anonymous access (rate limited)
2. Adds `Authorization: Discogs key=..., secret=...` header if credentials available

## CI/CD Build Configuration

### Pre-built Binary Strategy

Snap and Flatpak builds run cargo inside sandboxed environments (LXD containers, flatpak-builder) where GitHub secrets don't propagate. To solve this:

1. **Pre-build** the binary in the GitHub Actions environment (with secrets)
2. **Package** the pre-built binary into Snap/Flatpak

### Workflow Structure

```yaml
build-linux-snap-amd64:
  environment: PRODUCTION  # Access to secrets
  steps:
    # 1. Build binary with secrets
    - name: Build binary with secrets
      env:
        ENCRYPTION_KEY: ${{ secrets.ENCRYPTION_KEY }}
        DISCOGS_CONSUMER_KEY: ${{ secrets.DISCOGS_CONSUMER_KEY }}
        DISCOGS_CONSUMER_SECRET: ${{ secrets.DISCOGS_CONSUMER_SECRET }}
      run: cargo build --release

    # 2. Copy to location for snapcraft
    - run: |
        mkdir -p snap/local/bin
        cp target/release/ode-artwork-downloader snap/local/bin/

    # 3. Package with snapcraft (uses pre-built binary)
    - uses: snapcore/action-build@v1
```

### snapcraft.yaml (Snap)

```yaml
parts:
  ode-artwork-downloader:
    plugin: dump
    source: snap/local
    stage-packages:
      - libgtk-3-0
      - libssl3
    organize:
      bin/ode-artwork-downloader: bin/ode-artwork-downloader
```

### Flatpak Manifest

```yaml
modules:
  - name: ode-artwork-downloader
    buildsystem: simple
    build-commands:
      - install -Dm755 flatpak/bin/ode-artwork-downloader -t /app/bin/
    sources:
      - type: dir
        path: ..
```

## Troubleshooting

### Build fails with "ENCRYPTION_KEY must be 32 bytes"

The encryption key must decode to exactly 32 bytes. Regenerate with:

```bash
openssl rand -base64 32
```

Update the `ENCRYPTION_KEY` secret in GitHub with the new 44-character value.

### Some builds succeed, others fail

Check if failing builds have `environment: PRODUCTION` in the workflow. Without it, they won't have access to the secrets.

### Discogs API returns 401 Unauthorized

- Verify credentials are correct in GitHub secrets
- Check that the build log shows "Encrypting Discogs secrets for release build"
- For local dev, verify `secrets.json` format is correct

### Anonymous API access (rate limited)

If no credentials are available, the app falls back to anonymous Discogs API access which has lower rate limits. This is normal for development builds without `secrets.json`.

## Security Considerations

1. **Encryption key is embedded at compile time** - The `ENCRYPTION_KEY` is baked into the binary via `option_env!()`. This means anyone with the binary could potentially extract it.

2. **This is "security through obscurity"** - The goal is to prevent casual extraction of API keys, not to protect against determined reverse engineering.

3. **Suitable for:** API keys with rate limits where abuse would only affect the app's functionality, not cause financial or security damage.

4. **Not suitable for:** Payment credentials, user authentication tokens, or any secrets where extraction would cause significant harm.

5. **Alternative approaches:**
   - User-provided API keys in settings
   - Backend proxy service for API calls
   - OAuth flow where users authenticate with their own Discogs account
