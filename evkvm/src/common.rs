use ring::digest::{digest, SHA256};
use tokio_rustls::rustls;
use hex::ToHex;

pub type Identity = (rustls::Certificate, rustls::PrivateKey);

pub fn get_cert_fingerprint(cert: &rustls::Certificate) -> String {
    let rustls::Certificate(certificate_bytes) = cert;
    let fingerprint_digest = digest(&SHA256, certificate_bytes);
    fingerprint_digest.as_ref().encode_hex::<String>()
}
