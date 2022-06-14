use input::Key;
use serde::Deserialize;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::PathBuf;

pub const DEFAULT_PORT: u16 = 5258;

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Sender {
    pub nick: Option<String>,
    pub address: String,
    pub port: Option<u16>,
    pub fingerprint: String,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Receiver {
    pub nick: Option<String>,
    pub fingerprint: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "kebab-case")]
pub struct Config {
    pub listen_address: Option<SocketAddr>,
    pub switch_keys: HashSet<Key>,
    pub identity_path: PathBuf,
    pub senders: Option<Vec<Sender>>,
    pub receivers: Option<Vec<Receiver>>,
}
