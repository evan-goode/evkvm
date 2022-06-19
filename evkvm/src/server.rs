use anyhow::{Context, Error};
use input::{Direction, Event, InputEvent, ReaderManager, WriterManager, Key, KeyKind};
use net::{self, Message, PROTOCOL_VERSION};
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::time;
use tokio_rustls::rustls;

use crate::config::Receiver;
use crate::common::{Identity, get_cert_fingerprint};

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
        let fingerprint = get_cert_fingerprint(end_identity);

        let receiver = self.receivers.iter().find(|&receiver|
            match receiver.fingerprint {
                Some(ref receiver_fingerprint) => receiver_fingerprint == &fingerprint,
                None => false,
            }
        );

        match receiver {
            None => {
                log::info!("Fingerprint \"{}\" not authorized!", fingerprint);
                Err(rustls::Error::InvalidCertificateSignature)
            },
            Some(receiver) => {
                let name = match &receiver.nick {
                    None => &fingerprint,
                    Some(nick) => nick,
                };
                log::info!("{} connected", name);
                Ok(rustls::server::ClientCertVerified::assertion())
            }
        }
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

pub async fn run_server<'a>(
    listen_address: SocketAddr,
    switch_keys: &HashSet<Key>,
    identity: Identity,
    receivers: Vec<Receiver>,
) -> Result<Infallible, Error> {
    let (cert, key) = identity;

    let verifier = ClientVerifier::new(receivers);
    let config = rustls::ServerConfig::builder()
        .with_safe_defaults()
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(vec! [cert], key)
        .expect("Identity is invalid.");
    
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

                if let Event::Input {
                    device_id,
                    input: InputEvent::Key { direction, kind: KeyKind::Key(key) },
                    syn: _
                } = event {
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
                                            device_id,
                                            input: release_input,
                                            syn: true,
                                        };
                                        writer_manager.write(release_event).await?;
                                    } else {
                                        let release_event = Event::Input {
                                            device_id,
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
                                            device_id,
                                            input: press_input,
                                            syn: true,
                                        };
                                        writer_manager.write(press_event).await?
                                    } else {
                                        let press_event = Event::Input {
                                            device_id,
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

                if current != 0 {
                    let idx = current - 1;
                    if clients[idx].send(event.clone()).is_ok() {
                        continue;
                    }

                    clients.remove(idx);
                    current = 0;
                }

                writer_manager.write(event).await?;
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
