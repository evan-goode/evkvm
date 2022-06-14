mod config;

use anyhow::{Context, Error, anyhow};
use config::{Config, load_or_generate_identity};
use input::EventManager;
use log::LevelFilter;
use net::{self, Message, PROTOCOL_VERSION};
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::process;
use structopt::StructOpt;
use std::fs;
use std::convert::TryFrom;
use std::io::Write;
use std::sync::Arc;
use tokio::fs as tokio_fs;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::time;
use tokio_rustls::TlsConnector;
use rustls::ServerName;


struct Verifier {
    
}

impl Verifier {
    fn new() -> Self {
        Verifier {}
    }
}

impl rustls::client::ServerCertVerifier for Verifier {
    fn verify_server_cert(
        &self,
        end_identity: &rustls::Certificate,
        intermediates: &[rustls::Certificate],
        server_name: &rustls::ServerName,
        scts: &mut dyn Iterator<Item = &[u8]>, 
        ocsp_response: &[u8],
        now: std::time::SystemTime
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}

async fn run(
    server: &str,
    port: u16,
    identity: (rustls::Certificate, rustls::PrivateKey)
) -> Result<Infallible, Error> {
    let (cert, key) = identity;
    let verifier = Verifier::new();
    let config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_single_cert(vec! [cert], key)
        .expect("uh oh!");
    
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let stream = TcpStream::connect((server, port)).await?;
    let stream = BufReader::new(stream);
    let mut stream = connector
        .connect(ServerName::try_from(server)?, stream)
        .await
        .context("Failed to connect")?;

    log::info!("Connected to {}:{}", server, port);

    net::write_version(&mut stream, PROTOCOL_VERSION).await?;

    let version = net::read_version(&mut stream).await?;
    if version != PROTOCOL_VERSION {
        return Err(anyhow::anyhow!(
            "Incompatible protocol version (got {}, expecting {})",
            version,
            PROTOCOL_VERSION
        ));
    }

    let mut manager = EventManager::new().await?;
    loop {
        let message = time::timeout(net::MESSAGE_TIMEOUT, net::read_message(&mut stream))
            .await
            .context("Read timed out")??;
        match message {
            Message::Event(event) => manager.write(event).await?,
            Message::KeepAlive => {}
        }
    }
}

#[derive(StructOpt)]
#[structopt(name = "rkvm-client", about = "The rkvm client application")]
struct Args {
    #[structopt(help = "Path to configuration file")]
    #[cfg_attr(
        target_os = "linux",
        structopt(default_value = "/etc/rkvm/client.toml")
    )]
    config_path: PathBuf,
}


#[tokio::main]
async fn main() {
    env_logger::builder()
        .format_timestamp(None)
        .filter(None, LevelFilter::Info)
        .init();

    let args = Args::from_args();
    let config = match tokio_fs::read_to_string(&args.config_path).await {
        Ok(config) => config,
        Err(err) => {
            log::error!("Error loading config: {}", err);
            process::exit(1);
        }
    };

    let config: Config = match toml::from_str(&config) {
        Ok(config) => config,
        Err(err) => {
            log::error!("Error parsing config: {}", err);
            process::exit(1);
        }
    };

    let key = load_or_generate_identity(&config.certificate_path).unwrap();

    tokio::select! {
        result = run(&config.server.hostname, config.server.port, key) => {
            if let Err(err) = result {
                log::error!("Error: {:#}", err);
                process::exit(1);
            }
        }
        result = tokio::signal::ctrl_c() => {
            if let Err(err) = result {
                log::error!("Error setting up signal handler: {}", err);
                process::exit(1);
            }

            log::info!("Exiting on signal");
        }
    }
}
