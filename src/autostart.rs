//! `install` and `uninstall` commands: user-level automatic start of the Gateway, from
//! wherever the binary was copied.
//!
//! - Windows: scheduled task at logon, without a window.
//! - Linux: `systemd --user` unit.

use std::path::{Path, PathBuf};
use std::process::Command;

const NAME: &str = "nx-azure-cache";

/// Sets up (or updates) the automatic start and restarts the Gateway.
pub async fn install() -> i32 {
    report(if cfg!(windows) {
        install_windows().await
    } else {
        install_linux()
    })
}

/// Stops the Gateway and removes the automatic start; no effect if it is absent.
pub async fn uninstall() -> i32 {
    report(if cfg!(windows) {
        uninstall_windows().await
    } else {
        uninstall_linux()
    })
}

fn report(result: Result<String, String>) -> i32 {
    match result {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => {
            eprintln!("nx-azure-cache: {e}");
            1
        }
    }
}

fn current_exe() -> Result<PathBuf, String> {
    std::env::current_exe().map_err(|e| format!("binary path not found: {e}"))
}

/// Runs a system command; fails if it exits with an error.
fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let out = Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    Err(format!("{program} {}: {}", args.join(" "), stderr.trim()))
}

// --- Windows ---------------------------------------------------------------

async fn install_windows() -> Result<String, String> {
    let exe = current_exe()?;
    let user = format!(
        "{}\\{}",
        std::env::var("USERDOMAIN").unwrap_or_default(),
        std::env::var("USERNAME").map_err(|_| "USERNAME is not set")?
    );
    // schtasks reads the XML as UTF-16 with a BOM, as its header declares.
    let xml: Vec<u8> = std::iter::once(0xFEFF)
        .chain(task_xml(&exe, &user).encode_utf16())
        .flat_map(u16::to_le_bytes)
        .collect();
    let file = std::env::temp_dir().join(format!("{NAME}-task.xml"));
    std::fs::write(&file, xml).map_err(|e| format!("{}: {e}", file.display()))?;
    // Stop any running instance, so that the new path takes effect.
    stop_windows().await?;
    let created = run(
        "schtasks",
        &[
            "/Create",
            "/F",
            "/TN",
            NAME,
            "/XML",
            &file.to_string_lossy(),
        ],
    );
    let _ = std::fs::remove_file(&file);
    created?;
    run("schtasks", &["/Run", "/TN", NAME])?;
    Ok(format!(
        "scheduled task \"{NAME}\" set up ({} serve), Gateway started",
        exe.display()
    ))
}

async fn uninstall_windows() -> Result<String, String> {
    stop_windows().await?;
    if run("schtasks", &["/Query", "/TN", NAME]).is_err() {
        return Ok(format!("no task \"{NAME}\": nothing to do"));
    }
    run("schtasks", &["/Delete", "/F", "/TN", NAME])?;
    Ok(format!("task \"{NAME}\" removed, Gateway stopped"))
}

/// Stops the process holding the port, if it answers like a Gateway; its `conhost` exits
/// with it, which ends the task. No `schtasks /End`: it only kills `conhost`, and the
/// orphaned Gateway (which ignores its lost stdout) would keep the port.
async fn stop_windows() -> Result<(), String> {
    let port = crate::config::Config::load(&crate::config::config_dir()?)?.port;
    if !crate::server::is_gateway(port).await {
        return Ok(());
    }
    let script = format!(
        "Get-NetTCPConnection -LocalAddress 127.0.0.1 -LocalPort {port} -State Listen | \
         ForEach-Object {{ Stop-Process -Id $_.OwningProcess -Force -PassThru | Wait-Process }}"
    );
    run(
        "powershell",
        &["-NoProfile", "-NonInteractive", "-Command", &script],
    )
}

/// Task at logon of `user`. `conhost --headless` runs the console binary without opening a
/// window; no time limit and no stop on battery.
/// `Parallel`: a `/Run` right after `/End` is not ignored; a duplicate `serve` exits at
/// once, the port acting as the lock.
fn task_xml(exe: &Path, user: &str) -> String {
    let exe = xml_escape(&exe.display().to_string());
    let user = xml_escape(user);
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Local Gateway for the Nx remote cache</Description></RegistrationInfo>
  <Triggers>
    <LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>LeastPrivilege</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>Parallel</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>%SystemRoot%\System32\conhost.exe</Command>
      <Arguments>--headless "{exe}" serve</Arguments>
    </Exec>
  </Actions>
</Task>
"#
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// --- Linux -----------------------------------------------------------------

/// `~/.config/systemd/user/nx-azure-cache.service` (or under `$XDG_CONFIG_HOME`).
fn unit_path() -> Result<PathBuf, String> {
    let config = crate::config::config_dir()?;
    let base = config.parent().ok_or("config directory without a parent")?;
    Ok(base.join("systemd/user").join(format!("{NAME}.service")))
}

fn install_linux() -> Result<String, String> {
    let exe = current_exe()?;
    let path = unit_path()?;
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(&path, unit(&exe)).map_err(|e| format!("{}: {e}", path.display()))?;
    let service = format!("{NAME}.service");
    run("systemctl", &["--user", "daemon-reload"])?;
    run("systemctl", &["--user", "enable", &service])?;
    // `restart` rather than `start`: an already running instance picks up the new path.
    run("systemctl", &["--user", "restart", &service])?;
    Ok(format!(
        "unit {} set up ({} serve), Gateway started",
        path.display(),
        exe.display()
    ))
}

fn uninstall_linux() -> Result<String, String> {
    let path = unit_path()?;
    if !path.exists() {
        return Ok(format!("no unit {}: nothing to do", path.display()));
    }
    run(
        "systemctl",
        &["--user", "disable", "--now", &format!("{NAME}.service")],
    )?;
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    run("systemctl", &["--user", "daemon-reload"])?;
    Ok(format!("unit {} removed, Gateway stopped", path.display()))
}

/// Unit content. Path in quotes (spaces) and `%` doubled (specifiers).
fn unit(exe: &Path) -> String {
    let exe = exe.display().to_string().replace('%', "%%");
    format!(
        "[Unit]
Description=nx-azure-cache, local Gateway for the Nx remote cache

[Service]
ExecStart=\"{exe}\" serve
Restart=on-failure

[Install]
WantedBy=default.target
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_runs_serve_and_restarts_on_failure() {
        let text = unit(Path::new("/opt/my tools/nx-azure-cache"));
        assert_eq!(
            text,
            "[Unit]
Description=nx-azure-cache, local Gateway for the Nx remote cache

[Service]
ExecStart=\"/opt/my tools/nx-azure-cache\" serve
Restart=on-failure

[Install]
WantedBy=default.target
"
        );
        assert!(unit(Path::new("/a%b/x")).contains("\"/a%%b/x\" serve"));
    }

    #[test]
    fn task_runs_serve_without_a_window() {
        let xml = task_xml(Path::new(r"C:\Tools & co\nx-azure-cache.exe"), r"DOM\dev");
        assert!(xml.contains(r#"--headless "C:\Tools &amp; co\nx-azure-cache.exe" serve"#));
        assert!(xml.contains(r"<UserId>DOM\dev</UserId>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
    }
}
