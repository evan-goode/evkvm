use input::Key;
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;
use std::net::SocketAddr;
use anyhow::Error;

use figment::{Figment, providers::{Format, Toml}};

pub const DEFAULT_PORT: u16 = 5258;

const DEFAULT_CONFIG_TOML: &str = r#"
# Listen on all interfaces on port 5258
listen-address = "0.0.0.0:5258"

# Switch to next client by pressing both alt keys at the same time
switch-keys = ["LeftAlt", "RightAlt"]

identity-path = "/var/lib/evkvm/identity.pem"

senders = []
receivers = []
"#;

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Sender {
    pub nick: Option<String>,
    pub address: String,
    pub port: Option<u16>,
    pub fingerprint: Option<String>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Receiver {
    pub nick: Option<String>,
    pub fingerprint: Option<String>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub listen_address: SocketAddr,
    pub switch_keys: HashSet<Key>,
    pub identity_path: PathBuf,
    pub senders: Vec<Sender>,
    pub receivers: Vec<Receiver>,
}

impl Config {
    pub fn new(config_path: &PathBuf) -> Result<Config, Error> {
        let config: Config = Figment::new()
            .merge(Toml::string(DEFAULT_CONFIG_TOML))
            .merge(Toml::file(config_path))
            .extract()?;
        Ok(config)
    }
}
