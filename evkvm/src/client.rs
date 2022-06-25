use anyhow::{Context, Error};
use input::WriterManager;
use net::{self, Message, PROTOCOL_VERSION};
use rustls::ServerName;
use std::convert::Infallible;
use std::convert::TryFrom;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::TcpStream;
use tokio::time;
use tokio_rustls::rustls;

use crate::common::{Identity, get_cert_fingerprint};
use crate::config::{Sender, DEFAULT_PORT};

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
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        let fingerprint = get_cert_fingerprint(end_identity);

        let name = match &self.sender.nick {
            None => &self.sender.address,
            Some(nick) => nick,
        };

        let fingerprint_matches = match self.sender.fingerprint {
            Some(ref sender_fingerprint) => &fingerprint == sender_fingerprint,
            None => false,
        };

        if fingerprint_matches {
            let none: String = String::from("<none>");
            let fingerprint_display = self.sender.fingerprint.as_ref().unwrap_or(&none);
            log::info!(
                "Fingerprint {} did not match fingerprint expected for sender {}, {}!",
                fingerprint,
                name,
                fingerprint_display,
            );
            Err(rustls::Error::InvalidCertificateSignature)
        } else {
            log::info!("connected to {}", name);
            Ok(rustls::client::ServerCertVerified::assertion())
        }
    }
}

pub async fn run_client(
    senders: Vec<Sender>,
    identity: Identity,
) {
    let handles: Vec<_> = senders.into_iter().map(|sender| {
        let identity = identity.clone();
        client_handle_connection(sender, identity)
    }).collect();

    futures::future::join_all(handles).await;
}

async fn client_handle_connection(
    sender: Sender,
    identity: Identity,
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
    identity: Identity,
) -> Result<Infallible, Error> {
    let mut writer_manager = WriterManager::new().await;

    let (cert, key) = identity;
    let verifier = ServerVerifier::new(sender.clone());
    let config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_single_cert(vec! [cert], key)
        .expect("Invalid identity!");
    
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
            Message::KeepAlive => {},
        }
    }
}
