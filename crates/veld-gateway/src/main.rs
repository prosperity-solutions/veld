//! veld-gateway binary — thin wrapper over the `veld_gateway` library.

use anyhow::{Context, Result};
use veld_gateway::config::GatewayConfig;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,veld_gateway=info".into()),
        )
        .init();

    let config_path = parse_args()?;
    let config = GatewayConfig::load(config_path.as_deref())?;

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?
        .block_on(veld_gateway::server::run(config))
}

/// `veld-gateway [--config <path>]`.
fn parse_args() -> Result<Option<String>> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => Ok(None),
        Some("--config") => Ok(Some(args.next().context("--config requires a path")?)),
        Some("--help" | "-h") => {
            println!(
                "veld-gateway — public web gateway for Veld sharing\n\n\
                 USAGE: veld-gateway [--config <path>]\n\n\
                 Configuration is env-var-first (VELD_GATEWAY_DOMAIN, VELD_GATEWAY_TOKEN, …);\n\
                 see docs/gateway.md for the full reference."
            );
            std::process::exit(0);
        }
        Some(other) => anyhow::bail!("unknown argument `{other}` (try --help)"),
    }
}
