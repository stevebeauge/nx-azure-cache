//! Persistence of the `user` Identity's refresh token only: the system keyring by default,
//! a 0600 file on explicit opt-in (`token_store = "file"`). Blocking calls: in the Gateway,
//! go through `spawn_blocking`.

use std::path::{Path, PathBuf};

const SERVICE: &str = "nx-azure-cache";
const ACCOUNT: &str = "refresh_token";

#[derive(Debug, Clone, PartialEq)]
pub enum TokenStore {
    Keyring,
    File(PathBuf),
}

impl TokenStore {
    /// Storage designated by `token_store`, the file living in `dir` (next to `config.toml`).
    pub fn from_config(token_store: &str, dir: &Path) -> Result<Self, String> {
        match token_store {
            "keyring" => Ok(Self::Keyring),
            "file" => Ok(Self::File(dir.join("refresh_token"))),
            other => Err(format!(
                "token_store = {other:?}: allowed values are `keyring` (default) or `file`"
            )),
        }
    }

    /// All possible locations, for a complete logout: a login made with `file` must not
    /// survive a logout run with another config.
    pub fn all(dir: &Path) -> [Self; 2] {
        [Self::Keyring, Self::File(dir.join("refresh_token"))]
    }

    /// Checks the storage is usable, to fail before a login rather than after.
    pub fn check(&self) -> Result<(), String> {
        self.load().map(|_| ())
    }

    pub fn load(&self) -> Result<Option<String>, String> {
        let bytes = match self {
            Self::Keyring => match entry()?.get_secret() {
                Ok(b) => b,
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(e) => return Err(keyring_err(e)),
            },
            Self::File(p) => match std::fs::read(p) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(file_err(p, e)),
            },
        };
        // Unreadable storage counts as no session: a login will be asked again.
        Ok(String::from_utf8(bytes).ok().filter(|s| !s.is_empty()))
    }

    /// Stores bytes: Credential Manager caps a secret at 2560 bytes, which a refresh token
    /// (~1.7 KB) encoded as UTF-16 would exceed.
    pub fn store(&self, refresh_token: &str) -> Result<(), String> {
        match self {
            Self::Keyring => entry()?
                .set_secret(refresh_token.as_bytes())
                .map_err(keyring_err),
            Self::File(p) => write_private(p, refresh_token.as_bytes()).map_err(|e| file_err(p, e)),
        }
    }

    pub fn clear(&self) -> Result<(), String> {
        match self {
            Self::Keyring => match entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
                Err(e) => Err(keyring_err(e)),
            },
            Self::File(p) => match std::fs::remove_file(p) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(file_err(p, e)),
            },
        }
    }
}

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, ACCOUNT).map_err(keyring_err)
}

fn keyring_err(e: keyring::Error) -> String {
    format!(
        "system keyring unavailable ({e}): set `token_store = \"file\"` in config.toml \
         (or NX_AZURE_CACHE_TOKEN_STORE=file); the refresh token is then stored in clear, \
         in a 0600 file"
    )
}

fn file_err(p: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", p.display())
}

/// Through a uniquely named temporary file then a rename: a reader sees the old token or the
/// new one, never a truncated file, even if two writes overlap.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().expect("storage path without a parent");
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension(format!("{}.tmp", crate::user::random_b64::<9>()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        opts.mode(0o600);
    }
    let written = opts.open(&tmp).and_then(|mut f| f.write_all(bytes));
    match written.and_then(|()| std::fs::rename(&tmp, path)) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyring_is_the_default_and_file_an_opt_in() {
        let dir = Path::new("/conf/nx-azure-cache");
        assert_eq!(
            TokenStore::from_config(&crate::config::Config::default().token_store, dir),
            Ok(TokenStore::Keyring)
        );
        assert_eq!(
            TokenStore::from_config("file", dir),
            Ok(TokenStore::File(dir.join("refresh_token")))
        );
        // An unknown value is rejected rather than falling back to a default.
        let e = TokenStore::from_config("File", dir).unwrap_err();
        assert!(e.contains("`keyring` (default) or `file`"), "{e}");
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nx-azure-cache-test-{name}-{}",
            crate::user::random_b64::<6>()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn file_0600_round_trip_then_clear() {
        let dir = temp_dir("store");
        let store = TokenStore::from_config("file", &dir).unwrap();
        assert_eq!(store.load(), Ok(None));
        store.store("rt-1").unwrap();
        store.store("rt-2").unwrap();
        assert_eq!(store.load().unwrap().as_deref(), Some("rt-2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.join("refresh_token")), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        // No temporary file left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        store.clear().unwrap();
        store.clear().unwrap();
        assert_eq!(store.load(), Ok(None));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn overlapping_writes_never_truncate_the_file() {
        let dir = temp_dir("atomic");
        let store = TokenStore::from_config("file", &dir).unwrap();
        let values: Vec<String> = (0..4).map(|i| i.to_string().repeat(4096)).collect();
        store.store(&values[0]).unwrap();
        std::thread::scope(|s| {
            for v in &values {
                let store = store.clone();
                s.spawn(move || (0..50).for_each(|_| store.store(v).unwrap()));
            }
            for _ in 0..200 {
                match store.load() {
                    Ok(seen) => {
                        let seen = seen.unwrap();
                        assert!(values.contains(&seen), "read {} bytes", seen.len());
                    }
                    // On Windows, opening the file while it is being replaced can fail
                    // (access denied): a transient error, never truncated content.
                    Err(e) => assert!(cfg!(windows) && e.contains("os error 5"), "{e}"),
                }
            }
        });
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
