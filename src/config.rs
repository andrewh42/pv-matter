//! Environment-driven configuration (plus `.env` in the working directory;
//! real environment variables take precedence). Owns every default.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use rs_matter::MATTER_PORT;

#[derive(Debug, Clone)]
pub struct Config {
    pub mqtt_host: String,
    pub mqtt_port: u16,
    /// Matter operational UDP port. Defaults to the standard 5540; override
    /// (e.g. 5541) to run a second rs-matter daemon on the same host — the
    /// node advertises this port over mDNS, so controllers still discover it.
    pub matter_port: u16,
    pub mqtt_username: Option<String>,
    pub mqtt_password: Option<String>,
    /// Matches the publisher's `MQTT_TOPIC_PREFIX`.
    pub topic_prefix: String,
    /// Pin a specific `<device-id>`; otherwise lock onto the first one seen.
    pub device_id: Option<String>,
    /// rs-matter fabric persistence (survives restarts, so the device isn't
    /// re-commissioned every run).
    pub storage_path: PathBuf,
    /// How long startup may wait for the retained `info` topic before
    /// proceeding with defaults.
    pub bootstrap_timeout: Duration,
}

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let Some(mqtt_host) = var("PV_MQTT_HOST") else {
            bail!("PV_MQTT_HOST is required (the MQTT broker sma-daemon publishes to)");
        };
        let mqtt_port = match var("PV_MQTT_PORT") {
            Some(p) => p.parse().context("PV_MQTT_PORT must be a port number")?,
            None => 1883,
        };
        let matter_port = match var("PV_MATTER_PORT") {
            Some(p) => p.parse().context("PV_MATTER_PORT must be a port number")?,
            None => MATTER_PORT,
        };
        let storage_path = match var("PV_STORAGE_PATH") {
            Some(p) => PathBuf::from(p),
            None => dirs_home()
                .context("cannot determine home directory; set PV_STORAGE_PATH")?
                .join(".pv-matter"),
        };
        let bootstrap_timeout = match var("PV_BOOTSTRAP_TIMEOUT") {
            Some(s) => Duration::from_secs(
                s.parse()
                    .context("PV_BOOTSTRAP_TIMEOUT must be whole seconds")?,
            ),
            None => Duration::from_secs(30),
        };

        Ok(Self {
            mqtt_host,
            mqtt_port,
            matter_port,
            mqtt_username: var("PV_MQTT_USERNAME"),
            mqtt_password: var("PV_MQTT_PASSWORD"),
            topic_prefix: var("PV_MQTT_TOPIC_PREFIX").unwrap_or_else(|| "pv-inverter".into()),
            device_id: var("PV_DEVICE_ID"),
            storage_path,
            bootstrap_timeout,
        })
    }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
