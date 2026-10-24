//! `whoami` without an Identity: the text stays on stdout, the exit code says "no session".

use std::path::PathBuf;
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_nx-azure-cache");

/// `whoami` isolated in an empty temporary config directory, file token store.
fn whoami(env: &[(&str, &str)]) -> Output {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir: PathBuf = std::env::temp_dir().join(format!(
        "nx-azure-cache-whoami-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(BIN);
    cmd.arg("whoami")
        .env("APPDATA", &dir)
        .env("XDG_CONFIG_HOME", &dir)
        .env("LOCALAPPDATA", &dir)
        .env("XDG_DATA_HOME", &dir)
        .env("NX_AZURE_CACHE_TOKEN_STORE", "file")
        .env_remove("NX_AZURE_CACHE_TENANT_ID")
        .env_remove("NX_AZURE_CACHE_CLIENT_ID");
    cmd.envs(env.iter().copied());
    let out = cmd.output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn not_configured_exits_1() {
    let out = whoami(&[]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("not signed in ("), "stdout: {stdout}");
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn no_stored_token_exits_1() {
    let out = whoami(&[
        (
            "NX_AZURE_CACHE_TENANT_ID",
            "00000000-0000-0000-0000-000000000001",
        ),
        (
            "NX_AZURE_CACHE_CLIENT_ID",
            "00000000-0000-0000-0000-000000000002",
        ),
    ]);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "not signed in\n");
    assert_eq!(out.status.code(), Some(1));
}
