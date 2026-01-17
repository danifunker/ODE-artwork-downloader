# Plan: Capture User Agent via Local Server

## How It Works

1. App starts a temporary local HTTP server (e.g., `http://localhost:8765`)
2. UI shows a button/link: "Click to set up browser identity"
3. User clicks → opens URL in their default browser
4. App captures the browser's real User-Agent header from the request
5. Saves to `config.json`
6. Server shuts down, UI confirms success

## Implementation

### Effort: Low-Medium (~1.5 hours)

### Dependencies

Need to add a lightweight HTTP server. Options:
- `tiny_http` - Simple, ~50KB, no async needed
- `rouille` - Simple wrapper around tiny_http
- Built-in with `std::net::TcpListener` - No new deps, slightly more code

**Recommendation:** Use `tiny_http` for simplicity.

### Changes Required

**1. Add dependency to `Cargo.toml`:**
```toml
tiny_http = "0.12"
```

**2. Add `user_agent` field to config:**
```json
{
  "search": {
    "user_agent": null
  }
}
```

**3. Create capture module** (`src/user_agent.rs` or in `src/search/mod.rs`):
```rust
use tiny_http::{Server, Response};

pub fn capture_user_agent() -> Result<String, String> {
    let server = Server::http("127.0.0.1:0")  // Port 0 = auto-assign
        .map_err(|e| format!("Failed to start server: {}", e))?;

    let port = server.server_addr().port();
    let url = format!("http://127.0.0.1:{}", port);

    // Open in default browser
    open_in_browser(&url)?;

    // Wait for single request (with timeout)
    if let Ok(Some(request)) = server.recv_timeout(Duration::from_secs(60)) {
        let user_agent = request.headers()
            .iter()
            .find(|h| h.field.as_str().eq_ignore_ascii_case("user-agent"))
            .map(|h| h.value.to_string())
            .unwrap_or_default();

        // Send response page
        let html = "<html><body><h1>Success!</h1><p>You can close this tab.</p></body></html>";
        let _ = request.respond(Response::from_string(html).with_header(
            "Content-Type: text/html".parse().unwrap()
        ));

        return Ok(user_agent);
    }

    Err("Timeout waiting for browser connection".to_string())
}
```

**4. Add UI trigger in `src/gui/app.rs`:**
- Button in settings or first-run dialog
- Shows status: "Waiting for browser..." → "Captured!" or error
- Could also auto-trigger on first search if not set

**5. Update `src/search/mod.rs`:**
- Load user_agent from SearchConfig
- Fall back to hardcoded default if not set

**6. Save to config.json:**
- After capture, write back to config.json

### Files to Modify

| File | Change |
|------|--------|
| `Cargo.toml` | Add `tiny_http` |
| `src/api/artwork.rs` | Add `user_agent: Option<String>` to SearchConfig |
| `src/search/mod.rs` | Add capture function, use config UA |
| `src/gui/app.rs` | Add UI button/trigger for capture |

### User Experience Flow

**Option A - Settings button:**
```
Settings panel:
  [Browser Identity: Not configured]
  [Configure Browser Identity] <- button

After click:
  [Browser Identity: Waiting...]
  (browser opens, user sees success page)
  [Browser Identity: Chrome/120.0.0.0 ✓]
```

**Option B - Auto on first search:**
```
User clicks Search
  → "Browser identity not set. Opening browser to configure..."
  → Browser opens, captures UA
  → Search proceeds automatically
```

Which UX flow do you prefer?
