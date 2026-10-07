//! Local token: 32 random bytes in hexadecimal, in `local-token` next to `config.toml`.
//! Shared with the Activation plugin, never logged.

use std::path::Path;

pub struct LocalToken(pub(crate) String);

impl LocalToken {
    /// Reads the existing token back, or generates one if it is missing or unreadable
    /// (the plugin reads it again on every run, so replacing it is safe).
    pub fn load_or_create(dir: &Path) -> Result<LocalToken, String> {
        let path = dir.join("local-token");
        if let Ok(text) = std::fs::read_to_string(&path) {
            let text = text.trim();
            if text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Ok(LocalToken(text.to_owned()));
            }
        }
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| format!("randomness unavailable: {e}"))?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        // Atomic write: a plugin reading at the same time never sees an empty file.
        crate::token_store::write_private(&path, token.as_bytes())
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(LocalToken(token))
    }

    /// Checks the `Authorization: Bearer <token>` header, in constant time. The only check of
    /// the local token: cache, `/stats` and `/reload` all go through here.
    pub fn matches_header(&self, headers: &axum::http::HeaderMap) -> bool {
        let header = headers.get(axum::http::header::AUTHORIZATION);
        let Some(given) = header.and_then(|h| h.as_bytes().strip_prefix(b"Bearer ")) else {
            return false;
        };
        let expected = self.0.as_bytes();
        // The length (64) is not secret; only the content is compared in constant time.
        given.len() == expected.len()
            && given
                .iter()
                .zip(expected)
                .fold(0u8, |acc, (a, b)| acc | (a ^ b))
                == 0
    }
}
