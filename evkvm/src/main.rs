mod config;
mod common;
mod server;
mod client;

use anyhow::{Error, anyhow};
use clap::{Parser};
use config::Config;
use log::LevelFilter;
use rcgen::generate_simple_self_signed;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use tokio_rustls::rustls;

use common::{Identity, get_cert_fingerprint};
use server::run_server;
use client::run_client;

fn load_identity(
    certificate_path: &Path,
) -> Result<Option<Identity>, Error> {
    // Try loading the identity file at `certificate_path`. If no file exists, return None.

    let file = match std::fs::File::open(&certificate_path) {
        Ok(file) => file,
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None)
        },
        Err(e) => { return Err(e.into()); },
    };

    let mut reader = std::io::BufReader::new(file);
    let mut certificate: Option<rustls::Certificate> = None;
    let mut private_key: Option<rustls::PrivateKey> = None;
    loop {
        match rustls_pemfile::read_one(&mut reader).expect("cannot parse private file") {
            Some(rustls_pemfile::Item::X509Certificate(cert)) => {
                certificate = Some(rustls::Certificate(cert));
            },
            Some(rustls_pemfile::Item::PKCS8Key(key)) => {
                private_key = Some(rustls::PrivateKey(key));
            },
            None => { break; },
            _ => {},
        }
    }
    match (certificate, private_key) {
        (Some(cert), Some(key)) => Ok(Some((cert, key))),
        (Some(_), None) => Err(anyhow!("Identity file at {} is missing a certificate!", &certificate_path.display())),
        (None, Some(_)) => Err(anyhow!("Identity file at {} is missing a private key!", &certificate_path.display())),
        (None, None) => Err(anyhow!("Identity file at {} is missing both a certificate and a private key!", &certificate_path.display())),
    }
}

fn load_or_generate_identity(
    certificate_path: &Path,
) -> Result<Identity, Error> {
    // Try loading the identity file at `certificate_path`, or create a new one if no file exists.

    let identity = load_identity(certificate_path)?;
    match identity {
        // Use existing identity
        Some(identity) => Ok(identity),

        // Identity did not already exist, create it
        None => {
            let cert = generate_simple_self_signed([String::from("localhost")]).unwrap();

            let pem = cert.serialize_pem()?;
            let private_key_pem = cert.serialize_private_key_pem();

            std::fs::create_dir_all(certificate_path.parent().unwrap())?;
            let mut options = OpenOptions::new();
            options.write(true);
            options.create(true);
            options.mode(0o600);
            let mut keyfile = options.open(certificate_path)?;

            let _ = keyfile.write((&pem).as_bytes())?;
            let _ = keyfile.write((&private_key_pem).as_bytes())?;

            let certificate_der = cert.serialize_der()?;
            let private_key_der = cert.serialize_private_key_der();

            Ok((rustls::Certificate(certificate_der), rustls::PrivateKey(private_key_der)))
        },
    }
}


#[derive(clap::Subcommand)]
enum Verb {
    Fingerprint,
}

#[derive(clap::Parser)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(subcommand)]
    verb: Option<Verb>,

    #[clap(short, long, value_parser, default_value = "/etc/evkvm/config.toml")]
    config_path: PathBuf,
}

fn print_fingerprint(identity_path: &Path) {
    let identity = match load_identity(identity_path) {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            log::error!("{} does not exist. Run `evkvm` with no arguments to generate it.",
                        identity_path.display());
            process::exit(1);
        }
        Err(err) => {
            log::error!("Error loading identity: {}", err);
            process::exit(1);
        }
    };
    let (cert, _) = identity;
    let fingerprint = get_cert_fingerprint(&cert);
    println!("{}", fingerprint);
}

#[tokio::main]
async fn main() {
    env_logger::builder()
        .format_timestamp(None)
        .filter(None, LevelFilter::Info)
        .init();

    let args = Args::parse();
    
    let config = match Config::new(&args.config_path) {
        Ok(config) => config,
        Err(err) => {
            log::error!("Error reading config: {}", err);
            process::exit(1);
        },
    };

    match args.verb {
        Some(Verb::Fingerprint) => print_fingerprint(&config.identity_path),
        None => {
            let identity = match load_or_generate_identity(&config.identity_path) {
                Ok(identity) => identity,
                Err(err) => {
                    log::error!("Error loading or generating identity: {}", err);
                    process::exit(1);
                }
            };

            let should_run_server = !config.receivers.is_empty();
            let should_run_client = !config.senders.is_empty();

            if !(should_run_server || should_run_client) {
                log::error!("No senders or receivers specified, exiting.");
                process::exit(1);
            }

            tokio::select! {
                result = async {
                    run_server(
                        config.listen_address,
                        &config.switch_keys,
                        identity.clone(),
                        config.receivers
                    ).await
                }, if should_run_server => {
                    if let Err(err) = result {
                        log::error!("Error: {:#}", err);
                        process::exit(1);
                    }
                }

                _ = async {
                    run_client(config.senders, identity.clone()).await
                }, if should_run_client => {}

                result = tokio::signal::ctrl_c() => {
                    if let Err(err) = result {
                        log::error!("Error setting up signal handler: {}", err);
                        process::exit(1);
                    }

                    log::info!("Exiting on signal");
                }
            }
        }
    }
}
