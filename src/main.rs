mod advert;
mod ancs;
mod config;
mod desktop;
mod icons;
mod monitor;
mod otp;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::ancs::session::Options;
use crate::config::Config;

#[derive(Parser)]
#[command(
    version,
    about = "Mirrors iPhone notifications to the Linux desktop over Bluetooth LE (ANCS).",
    after_help = "Settings are read from ~/.config/ancs-linux/config.toml; these flags override them."
)]
struct Args {
    #[arg(long, value_name = "PATH", help = "Config file to read instead of ~/.config/ancs-linux/config.toml")]
    config: Option<PathBuf>,
    #[arg(long, help = "Bluetooth adapter to use, e.g. hci0 [default: BlueZ's default adapter]")]
    adapter: Option<String>,
    #[arg(long, help = "Name the iPhone sees [default: the adapter's alias]")]
    name: Option<String>,
    #[arg(
        long,
        allow_negative_numbers = true,
        help = "Popup timeout in milliseconds; 0 keeps popups until closed, -1 uses the desktop's setting"
    )]
    timeout_ms: Option<i32>,
    #[arg(long, help = "Use generic icons instead of looking up app icons on the App Store")]
    no_app_store_icons: bool,
    #[arg(long, value_name = "COUNTRY", help = "Second App Store region to search, e.g. ru")]
    app_store_country: Option<String>,
    #[arg(long, help = "Also show notifications that were already on the iPhone when it connected")]
    show_preexisting: bool,
    #[arg(
        long = "ignore-app",
        value_name = "BUNDLE_ID",
        help = "Do not mirror this app's notifications; repeat for more apps"
    )]
    ignored_apps: Vec<String>,
    #[arg(long, help = "Keep notifications on the iPhone when they are closed on the desktop")]
    no_dismiss_sync: bool,
    #[arg(short, long, help = "Log debug output; RUST_LOG overrides this")]
    verbose: bool,
}

impl Args {
    fn apply(&self, config: &mut Config) {
        if let Some(adapter) = &self.adapter {
            config.adapter = Some(adapter.clone());
        }
        if let Some(name) = &self.name {
            config.name = Some(name.clone());
        }
        if let Some(timeout_ms) = self.timeout_ms {
            config.timeout_ms = timeout_ms;
        }
        if self.no_app_store_icons {
            config.app_store_icons = false;
        }
        if let Some(country) = &self.app_store_country {
            config.app_store_country = Some(country.clone());
        }
        if self.show_preexisting {
            config.show_preexisting = true;
        }
        if self.no_dismiss_sync {
            config.dismiss_sync = false;
        }
        config.ignore_apps.extend(self.ignored_apps.iter().cloned());
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let default_filter = if args.verbose {
        "ancs_linux=debug"
    } else {
        "ancs_linux=info"
    };
    tracing_subscriber::fmt()
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_filter)),
        )
        .init();

    let config_path = args.config.clone().unwrap_or_else(config::default_path);
    let mut config = config::load(&config_path, args.config.is_some())?;
    args.apply(&mut config);

    let session = bluer::Session::new()
        .await
        .context("cannot reach bluetoothd")?;
    let adapter = match &config.adapter {
        Some(name) => session.adapter(name)?,
        None => session.default_adapter().await?,
    };
    if !adapter.is_powered().await? {
        info!("powering on {}", adapter.name());
        adapter.set_powered(true).await?;
    }
    if !adapter.is_pairable().await? {
        warn!(
            "{} is not pairable; an iPhone that is not paired yet cannot bond (bluetoothctl pairable on)",
            adapter.name()
        );
    }
    let name = match config.name {
        Some(name) => name,
        None => adapter.alias().await?,
    };

    let icons = icons::Icons::new(config.app_store_icons, config.app_store_country)?;
    let desktop = desktop::Desktop::connect(config.timeout_ms, icons, config.copy_codes)
        .await
        .context("cannot reach the desktop notification server")?;

    let options = Options {
        show_preexisting: config.show_preexisting,
        ignored_apps: config.ignore_apps.into(),
        dismiss_sync: config.dismiss_sync,
    };
    info!(
        "using adapter {} ({})",
        adapter.name(),
        adapter.address().await?
    );

    tokio::select! {
        result = monitor::run(adapter, desktop, name, options) => result,
        _ = tokio::signal::ctrl_c() => Ok(()),
    }
}
