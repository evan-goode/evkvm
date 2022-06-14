mod config;

use anyhow::{Context, Error, anyhow};
use config::{Sender, Receiver, Config, DEFAULT_PORT};
use hex::ToHex;
use input::{Direction, Event, InputEvent, ReaderManager, WriterManager, Key, KeyKind};
use log::LevelFilter;
use net::{self, Message, PROTOCOL_VERSION};
use rcgen::generate_simple_self_signed;
use ring::digest::{digest, SHA256};
use rustls_pemfile;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use structopt::StructOpt;
use tokio::fs as tokio_fs;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time;
use tokio_rustls::rustls;
use tokio::net::TcpStream;
use tokio::io::BufReader;
use rustls::ServerName;
use std::convert::TryFrom;
use std::time::Duration;

fn load_or_generate_identity(
    certificate_path: &Path
) -> Result<(rustls::Certificate, rustls::PrivateKey), Error> {
    let keyfile = std::fs::File::open(&certificate_path);
    match keyfile {
        Ok(file) => {
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
                (Some(cert), Some(key)) => Ok((cert, key)),
                (Some(_), None) => Err(anyhow!("Identity file at {} is missing a certificate!", &certificate_path.display())),
                (None, Some(_)) => Err(anyhow!("Identity file at {} is missing a private key!", &certificate_path.display())),
                (None, None) => Err(anyhow!("Identity file at {} is missing both a certificate and a private key!", &certificate_path.display())),
            }
        },
        Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => {
            let cert = generate_simple_self_signed([String::from("localhost")]).unwrap();

            let pem = cert.serialize_pem()?;
            let private_key_pem = cert.serialize_private_key_pem();

            std::fs::create_dir_all(certificate_path.parent().unwrap())?;
            let mut options = OpenOptions::new();
            options.write(true);
            options.create(true);
            options.mode(0o600);
            let mut keyfile = options.open(certificate_path)?;

            keyfile.write((&pem).as_bytes())?;
            keyfile.write((&private_key_pem).as_bytes())?;

            let certificate_der = cert.serialize_der()?;
            let private_key_der = cert.serialize_private_key_der();

            Ok((rustls::Certificate(certificate_der), rustls::PrivateKey(private_key_der)))
        },
        Err(e) => Err(anyhow::Error::new(e)),
    }
}

async fn server_handle_connection<T>(
    mut stream: T,
    mut receiver: UnboundedReceiver<Event>,
) -> Result<(), Error>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    net::write_version(&mut stream, PROTOCOL_VERSION).await?;

    let version = net::read_version(&mut stream).await?;
    if version != PROTOCOL_VERSION {
        return Err(anyhow::anyhow!(
            "Incompatible protocol version (got {}, expecting {})",
            version,
            PROTOCOL_VERSION
        ));
    }

    loop {
        // Send a keep alive message in intervals of half of the timeout just to be on the safe
        // side.
        let message = match time::timeout(net::MESSAGE_TIMEOUT / 2, receiver.recv()).await {
            Ok(Some(message)) => Message::Event(message),
            Ok(None) => return Ok(()),
            Err(_) => Message::KeepAlive,
        };

        time::timeout(
            net::MESSAGE_TIMEOUT,
            net::write_message(&mut stream, &message),
        )
        .await
        .context("Write timeout")??;
    }
}

struct ClientVerifier { receivers: Vec<Receiver> }

impl ClientVerifier {
    fn new(receivers: Vec<Receiver>) -> Self {
        ClientVerifier { receivers }
    }
}

impl<'a> rustls::server::ClientCertVerifier for ClientVerifier {
    fn client_auth_root_subjects(&self) -> Option<rustls::DistinguishedNames> {
        Some(vec! [])
    }
    fn client_auth_mandatory(&self) -> Option<bool> {
        Some(true)
    }
    fn verify_client_cert(
        &self,
        end_identity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _now: std::time::SystemTime
    ) -> Result<rustls::server::ClientCertVerified, rustls::Error> {

        let rustls::Certificate(certificate_bytes) = end_identity;
        let fingerprint_digest = digest(&SHA256, certificate_bytes);
        let fingerprint = fingerprint_digest.as_ref().encode_hex::<String>();

        let receiver = self.receivers.iter().find(|&receiver|
            receiver.fingerprint == fingerprint
        );

        match receiver {
            None => {
                log::info!("Fingerprint \"{}\" not authorized!", fingerprint);
                Err(rustls::Error::InvalidCertificateSignature)
            },
            Some(receiver) => {
                let name = match &receiver.nick {
                    None => &receiver.fingerprint,
                    Some(nick) => nick,
                };
                log::info!("{} connected", name);
                Ok(rustls::server::ClientCertVerified::assertion())
            }
        }
    }
}

async fn run_server<'a>(
    listen_address: SocketAddr,
    switch_keys: &HashSet<Key>,
    identity: (rustls::Certificate, rustls::PrivateKey),
    receivers: Vec<Receiver>,
) -> Result<Infallible, Error> {

    let (cert, key) = identity;

    let verifier = ClientVerifier::new(receivers);
    let config = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec! [cert], key)
        .expect("uh oh!");
    
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind(listen_address).await?;

    log::info!("Listening on {}", listen_address);

    let mut reader_manager = ReaderManager::new().await?;
    let mut writer_manager = WriterManager::new().await;

    let (client_sender, mut client_receiver) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (stream, address) = match listener.accept().await {
                Ok(sa) => sa,
                Err(err) => {
                    let _ = client_sender.send(Err(err));
                    return;
                }
            };

            let stream = match acceptor.accept(stream).await {
                Ok(stream) => stream,
                Err(err) => {
                    log::error!("{}: TLS error: {}", address, err);
                    continue;
                }
            };

            let (sender, receiver) = mpsc::unbounded_channel();

            if client_sender.send(Ok(sender)).is_err() {
                return;
            }

            tokio::spawn(async move {
                log::info!("{}: connected", address);
                let message = server_handle_connection(stream, receiver)
                    .await
                    .err()
                    .map(|err| format!(" ({})", err))
                    .unwrap_or_else(String::new);
                log::info!("{}: disconnected{}", address, message);
            });
        }
    });

    let mut clients: Vec<UnboundedSender<Event>> = Vec::new();
    let mut current = 0;
    let mut key_states: HashMap<_, _> = switch_keys
        .iter()
        .copied()
        .map(|key| (key, false))
        .collect();
    loop {
        tokio::select! {
            event = reader_manager.read() => {
                let event = event?;

                if let Event::Input { device_id, input, syn: _ } = event {
                    if let InputEvent::Key { direction, kind: KeyKind::Key(key) } = input {
                        if let Some(state) = key_states.get_mut(&key) {
                            *state = direction == Direction::Down;
                            if key_states.iter().filter(|(_, state)| **state).count() == key_states.len() {
                                let new_current = (current + 1) % (clients.len() + 1);

                                for (other_key, _) in key_states.iter() {
                                    // On current client, release all currently pressed keys from the combo
                                    // NOTE: This will NOT release other keys that are not part of the combo
                                    let release_input = InputEvent::Key {
                                        direction: Direction::Up,
                                        kind: KeyKind::Key(*other_key),
                                    };
                                    if current == 0 {
                                        let release_event = Event::Input {
                                            device_id: device_id,
                                            input: release_input,
                                            syn: true,
                                        };
                                        writer_manager.write(release_event).await?;
                                    } else {
                                        let release_event = Event::Input {
                                            device_id: device_id,
                                            input: release_input,
                                            syn: true,
                                        };
                                        let idx = current - 1;
                                        // We cannot remove broken client here, to not crash in next iteration,
                                        // and it will be removed later one anyways, therefore we just ignore error here
                                        let _ = clients[idx].send(release_event);
                                    }

                                    // On new client, press all currently pressed keys from the combo
                                    let press_input = InputEvent::Key {
                                        direction: Direction::Down,
                                        kind: KeyKind::Key(*other_key),
                                    };
                                    if new_current == 0 {
                                        let press_event = Event::Input {
                                            device_id: device_id,
                                            input: press_input,
                                            syn: true,
                                        };
                                        writer_manager.write(press_event).await?
                                    } else {
                                        let press_event = Event::Input {
                                            device_id: device_id,
                                            input: press_input,
                                            syn: true,
                                        };
                                        let idx = new_current - 1;
                                        let _ = clients[idx].send(press_event);
                                    }
                                }

                                current = new_current;
                                log::info!("Switching to client {}", current);
                            }
                        }
                    }
                }

                if current != 0 {
                    let idx = current - 1;
                    if clients[idx].send(event.clone()).is_ok() {
                        continue;
                    }

                    clients.remove(idx);
                    current = 0;
                }

                if let Event::Input { device_id: _, input: _, syn: _ } = event {
                    writer_manager.write(event).await?;
                }
            }
            sender = client_receiver.recv() => {
                let sender = sender.unwrap()?;
                for device in reader_manager.devices.values() {
                    sender.send(Event::NewDevice(device.clone()))?;
                }
                clients.push(sender);
            }
        }
    }
}

struct ServerVerifier { sender: Sender }

impl ServerVerifier {
    fn new(sender: Sender) -> Self {
        ServerVerifier { sender }
    }
}

impl rustls::client::ServerCertVerifier for ServerVerifier {
    fn verify_server_cert(
        &self,
        end_identity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>, 
        _ocsp_response: &[u8],
        _now: std::time::SystemTime
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        let rustls::Certificate(certificate_bytes) = end_identity;
        let fingerprint_digest = digest(&SHA256, certificate_bytes);
        let fingerprint = fingerprint_digest.as_ref().encode_hex::<String>();

        let name = match &self.sender.nick {
            None => &self.sender.address,
            Some(nick) => nick,
        };

        if &fingerprint == &self.sender.fingerprint {
            log::info!(
                "Fingerprint {} did not match fingerprint expected for sender {}, {}!",
                fingerprint,
                name,
                self.sender.fingerprint
            );
            Err(rustls::Error::InvalidCertificateSignature)
        } else {
            log::info!("connected to {}", name);
            Ok(rustls::client::ServerCertVerified::assertion())
        }
    }
}

async fn run_client(
    senders: Vec<Sender>,
    identity: (rustls::Certificate, rustls::PrivateKey),
) {
    let handles: Vec<_> = senders.into_iter().map(|sender| {
        let identity = identity.clone();
        handle_client_connection(sender, identity)
    }).collect();

    futures::future::join_all(handles).await;
}

async fn handle_client_connection(
    sender: Sender,
    identity: (rustls::Certificate, rustls::PrivateKey),
) -> Infallible {
    let mut last_msg: Option<String> = None;

    loop {
        if let Err(err) = client(sender.clone(), identity.clone()).await {
            let msg = err.to_string();
            if last_msg.as_ref() == Some(&msg) {
                log::error!("Error: {}", msg);
            }
            last_msg = Some(msg);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn client(
    sender: Sender,
    identity: (rustls::Certificate, rustls::PrivateKey),
) -> Result<Infallible, Error> {
    let mut writer_manager = WriterManager::new().await;

    let (cert, key) = identity;
    let verifier = ServerVerifier::new(sender.clone());
    let config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_single_cert(vec! [cert], key)
        .expect("uh oh!");
    
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

    let address = &sender.address[..];
    let port = sender.port.unwrap_or(DEFAULT_PORT);

    let stream = TcpStream::connect((address, port)).await?;
    let stream = BufReader::new(stream);
    let mut stream = connector
        .connect(ServerName::try_from(address)?, stream)
        .await
        .context("Failed to connect")?;

    log::info!("Connected to {}:{}", sender.address, port);

    net::write_version(&mut stream, PROTOCOL_VERSION).await?;

    let version = net::read_version(&mut stream).await?;
    if version != PROTOCOL_VERSION {
        return Err(anyhow::anyhow!(
            "Incompatible protocol version (got {}, expecting {})",
            version,
            PROTOCOL_VERSION
        ));
    }

    loop {
        let message = time::timeout(net::MESSAGE_TIMEOUT, net::read_message(&mut stream))
            .await
            .context("Read timed out")??;
        match message {
            Message::Event(event) => writer_manager.write(event).await?,
            Message::KeepAlive => {}
        }
    }
}

#[derive(StructOpt)]
#[structopt(name = "evkvm", about = "evdev kvm")]
struct Args {
    #[structopt(help = "Path to configuration file")]
    #[cfg_attr(
        target_os = "linux",
        structopt(default_value = "/etc/evkvm/config.toml")
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

    let identity = load_or_generate_identity(&config.identity_path).unwrap();

    let listen_address = config.listen_address;
    let switch_keys = config.switch_keys;
    let senders = config.senders;
    let receivers = config.receivers;


    let is_server = match receivers {
        Some(ref receivers) => !receivers.is_empty(),
        _ => false,
    };
    let is_client = match senders {
        Some(ref senders) => !senders.is_empty(),
        _ => false,
    };

    tokio::select! {
        result = async {
            match listen_address {
                Some(listen_address) => {
                    run_server(listen_address, &switch_keys, identity.clone(), receivers.unwrap()).await
                },
                None => {
                    Err(anyhow!("uh oh"))
                }
            }
        }, if is_server => {
            if let Err(err) = result {
                log::error!("Error: {:#}", err);
                process::exit(1);
            }
        }

        _ = async {
            run_client(senders.unwrap(), identity.clone()).await
        }, if is_client => {}

        result = tokio::signal::ctrl_c() => {
            if let Err(err) = result {
                log::error!("Error setting up signal handler: {}", err);
                process::exit(1);
            }

            log::info!("Exiting on signal");
        }
    }
}
