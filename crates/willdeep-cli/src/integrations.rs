use anyhow::Result;
use clap::Subcommand;
use serde::Serialize;

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum IntegrationAction {
    /// Inspect optional Herdr terminal-multiplexer integration.
    Herdr {
        #[command(subcommand)]
        action: HerdrAction,
    },
}

#[derive(Clone, Debug, Subcommand)]
pub(crate) enum HerdrAction {
    /// Show Herdr CLI, Pane environment, and lifecycle reporting readiness.
    Status {
        /// Emit a machine-readable status object.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Serialize)]
struct HerdrStatus {
    cli_available: bool,
    cli_version: Option<String>,
    in_herdr: bool,
    pane_id: Option<String>,
    socket_configured: bool,
    lifecycle_reporting_ready: bool,
}

pub(crate) async fn handle(action: IntegrationAction) -> Result<()> {
    match action {
        IntegrationAction::Herdr {
            action: HerdrAction::Status { json },
        } => herdr_status(json).await,
    }
}

async fn herdr_status(json: bool) -> Result<()> {
    let binary = std::env::var_os("WILLDEEP_HERDR_BIN").unwrap_or_else(|| "herdr".into());
    let version = tokio::process::Command::new(binary)
        .arg("--version")
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let in_herdr = std::env::var_os("HERDR_ENV").is_some_and(|value| value == "1");
    let pane_id = std::env::var("HERDR_PANE_ID")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let status = HerdrStatus {
        cli_available: version.is_some(),
        cli_version: version,
        in_herdr,
        pane_id: pane_id.clone(),
        socket_configured: std::env::var_os("HERDR_SOCKET_PATH").is_some(),
        lifecycle_reporting_ready: in_herdr && pane_id.is_some(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&status)?);
    } else {
        println!(
            "Herdr CLI: {}",
            status.cli_version.as_deref().unwrap_or("not found")
        );
        println!("Inside Herdr: {}", status.in_herdr);
        println!(
            "Pane: {}",
            status.pane_id.as_deref().unwrap_or("not configured")
        );
        println!("Socket configured: {}", status.socket_configured);
        println!(
            "Lifecycle reporting ready: {}",
            status.lifecycle_reporting_ready
        );
    }
    Ok(())
}
