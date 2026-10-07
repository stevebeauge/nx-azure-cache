//! Local Gateway for the Nx remote cache, backed by Azure Blob Storage.

mod access_log;
mod autostart;
mod cache;
mod config;
mod health;
mod identity;
mod journal;
mod server;
mod stats;
mod status;
mod store;
mod token;
mod token_store;
mod user;

const USAGE: &str = "usage: nx-azure-cache <serve|status [--json]|login [--device]|logout|whoami|install|uninstall|version>";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("serve") => server::serve().await,
        Some("status") => status::run(args.iter().any(|a| a == "--json")).await,
        Some("install") => autostart::install().await,
        Some("uninstall") => autostart::uninstall().await,
        Some(cmd @ ("login" | "logout" | "whoami")) => user::command(cmd, &args[1..]).await,
        Some("version") => {
            println!("nx-azure-cache {}", env!("CARGO_PKG_VERSION"));
            0
        }
        _ => {
            eprintln!("{USAGE}");
            1
        }
    };
    std::process::exit(code);
}
