//! Pinned mutual TLS for the clipboard connection.
//!
//! Neither side validates a certificate chain. Each compares the peer's leaf
//! certificate, byte for byte, against the one it already trusts: the host
//! pins the client certificate against Sunshine's paired-device list, and the
//! client pins the host's own certificate. Names and validity dates are
//! ignored because Sunshine's certificates are self-signed with no SAN. The
//! signature checks stay real, so a peer must still prove it holds the key.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, CryptoProvider};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::moonlight::identity::ClientIdentity;

/// SNI name to pass to the connector; ignored by the pinned verifier.
pub const SERVER_NAME: &str = "dusk-clipboard";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(ring::default_provider())
}

fn certs_of(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>, String> {
    rustls_pemfile::certs(&mut &*pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "certificate PEM is unreadable".to_string())
}

/// The pinned leaf: the first certificate in the PEM.
fn leaf_of(pem: &str) -> Result<CertificateDer<'static>, String> {
    certs_of(pem.as_bytes())?
        .into_iter()
        .next()
        .ok_or_else(|| "no certificate in PEM".to_string())
}

/// A's side: present `identity`, accept only a server whose leaf cert DER equals `host_cert_pem`'s.
pub fn client_config(
    identity: &ClientIdentity,
    host_cert_pem: &str,
) -> Result<Arc<rustls::ClientConfig>, String> {
    let provider = provider();
    let pinned = leaf_of(host_cert_pem)?;
    let chain = certs_of(&identity.cert_pem)?;
    if chain.is_empty() {
        return Err("no client certificate".into());
    }
    let key = rustls_pemfile::private_key(&mut identity.key_pem.as_slice())
        .map_err(|_| "client key is unreadable".to_string())?
        .ok_or_else(|| "no client key".to_string())?;

    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer { pinned, provider }))
        .with_client_auth_cert(chain, key)
        .map_err(|e| e.to_string())?;
    // A resumed session skips certificate verification, so every connection
    // does a full handshake and the pin is checked each time.
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(config))
}

/// B's side: present Sunshine's cert and key, require a client cert whose DER equals one of `paired_pems`.
pub fn server_config(
    server_cert_pem: &str,
    server_key_pem: &str,
    paired_pems: &[String],
) -> Result<Arc<rustls::ServerConfig>, String> {
    let provider = provider();
    let chain = certs_of(server_cert_pem.as_bytes())?;
    if chain.is_empty() {
        return Err("no server certificate".into());
    }
    let key = rustls_pemfile::private_key(&mut server_key_pem.as_bytes())
        .map_err(|_| "server key is unreadable".to_string())?
        .ok_or_else(|| "no server key".to_string())?;
    // A paired entry that does not parse is skipped: it can never match.
    let paired = paired_pems
        .iter()
        .filter_map(|pem| leaf_of(pem).ok())
        .collect();

    let mut config = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_client_cert_verifier(Arc::new(PinnedClients { paired, provider }))
        .with_single_cert(chain, key)
        .map_err(|e| e.to_string())?;
    // A resumed TLS 1.3 session skips `verify_client_cert`, so hand out no
    // tickets and keep no session state: the pin runs on every connection,
    // however the config is reused.
    config.session_storage = Arc::new(rustls::server::NoServerSessionStorage {});
    config.send_tls13_tickets = 0;
    Ok(Arc::new(config))
}

#[derive(Debug)]
struct PinnedServer {
    pinned: CertificateDer<'static>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.pinned.as_ref() {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "host certificate is not the pinned one".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[derive(Debug)]
struct PinnedClients {
    paired: Vec<CertificateDer<'static>>,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for PinnedClients {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        if self
            .paired
            .iter()
            .any(|c| c.as_ref() == end_entity.as_ref())
        {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "client certificate is not a paired device".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::wire::{line, Wire};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio_rustls::{TlsAcceptor, TlsConnector};

    struct Pair {
        cert: String,
        key: String,
    }

    fn make() -> Pair {
        let ck = rcgen::generate_simple_self_signed(vec!["dusk-test".to_string()]).unwrap();
        Pair {
            cert: ck.cert.pem(),
            key: ck.key_pair.serialize_pem(),
        }
    }

    fn identity(p: &Pair) -> ClientIdentity {
        ClientIdentity {
            cert_pem: p.cert.clone().into_bytes(),
            key_pem: p.key.clone().into_bytes(),
        }
    }

    /// Both sides finish the handshake and one Hello line crosses each way.
    async fn run(
        server: Arc<rustls::ServerConfig>,
        client: Arc<rustls::ClientConfig>,
    ) -> Result<(), String> {
        let (a, b) = tokio::io::duplex(16 * 1024);
        let hello = line(&Wire::Hello { version: 1 });

        let server_side = async {
            let tls = TlsAcceptor::from(server)
                .accept(b)
                .await
                .map_err(|e| e.to_string())?;
            let (rd, mut wr) = tokio::io::split(tls);
            wr.write_all(hello.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            wr.flush().await.map_err(|e| e.to_string())?;
            let mut got = String::new();
            BufReader::new(rd)
                .read_line(&mut got)
                .await
                .map_err(|e| e.to_string())?;
            if got == hello {
                Ok(())
            } else {
                Err("no hello from client".to_string())
            }
        };
        let client_side = async {
            let name = ServerName::try_from(SERVER_NAME).unwrap();
            let tls = TlsConnector::from(client)
                .connect(name, a)
                .await
                .map_err(|e| e.to_string())?;
            let (rd, mut wr) = tokio::io::split(tls);
            wr.write_all(hello.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            wr.flush().await.map_err(|e| e.to_string())?;
            let mut got = String::new();
            BufReader::new(rd)
                .read_line(&mut got)
                .await
                .map_err(|e| e.to_string())?;
            if got == hello {
                Ok(())
            } else {
                Err("no hello from host".to_string())
            }
        };
        let (s, c) = tokio::join!(server_side, client_side);
        s.and(c)
    }

    async fn handshake(host: &Pair, client: &Pair, paired: &[&Pair]) -> Result<(), String> {
        let paired: Vec<String> = paired.iter().map(|p| p.cert.clone()).collect();
        let server = server_config(&host.cert, &host.key, &paired)?;
        let client = client_config(&identity(client), &host.cert)?;
        run(server, client).await
    }

    // Throwaway RSA-2048 self-signed fixtures (no SAN), generated with openssl
    // for tests only. The server key is PKCS#1, the client key PKCS#8.
    const RSA_SERVER_CERT: &str = include_str!("testdata/server_cert.pem");
    const RSA_SERVER_KEY: &str = include_str!("testdata/server_key_pkcs1.pem");
    const RSA_CLIENT_CERT: &str = include_str!("testdata/client_cert.pem");
    const RSA_CLIENT_KEY: &str = include_str!("testdata/client_key_pkcs8.pem");

    #[tokio::test]
    async fn rsa_pkcs1_and_pkcs8_keys_complete_the_handshake() {
        assert!(RSA_SERVER_KEY.contains("BEGIN RSA PRIVATE KEY"));
        assert!(RSA_CLIENT_KEY.contains("BEGIN PRIVATE KEY"));
        let server = server_config(
            RSA_SERVER_CERT,
            RSA_SERVER_KEY,
            &[RSA_CLIENT_CERT.to_string()],
        )
        .unwrap();
        let client = client_config(
            &ClientIdentity {
                cert_pem: RSA_CLIENT_CERT.as_bytes().to_vec(),
                key_pem: RSA_CLIENT_KEY.as_bytes().to_vec(),
            },
            RSA_SERVER_CERT,
        )
        .unwrap();
        run(server, client).await.unwrap();
    }

    #[tokio::test]
    async fn a_reused_server_config_checks_the_pin_every_time() {
        let (host, client, stranger) = (make(), make(), make());
        let server =
            server_config(&host.cert, &host.key, std::slice::from_ref(&client.cert)).unwrap();
        let good = client_config(&identity(&client), &host.cert).unwrap();
        run(server.clone(), good.clone()).await.unwrap();
        run(server.clone(), good).await.unwrap();
        let bad = client_config(&identity(&stranger), &host.cert).unwrap();
        assert!(run(server, bad).await.is_err());
    }

    #[tokio::test]
    async fn pinned_pair_completes_the_handshake() {
        let (host, client) = (make(), make());
        assert!(handshake(&host, &client, &[&client]).await.is_ok());
    }

    #[tokio::test]
    async fn an_unpaired_client_is_refused() {
        let (host, client, stranger) = (make(), make(), make());
        let server =
            server_config(&host.cert, &host.key, std::slice::from_ref(&client.cert)).unwrap();
        let config = client_config(&identity(&stranger), &host.cert).unwrap();
        assert!(run(server, config).await.is_err());
    }

    #[tokio::test]
    async fn a_host_with_a_different_cert_is_refused() {
        let (host, other_host, client) = (make(), make(), make());
        let paired = vec![client.cert.clone()];
        let server = server_config(&other_host.cert, &other_host.key, &paired).unwrap();
        let config = client_config(&identity(&client), &host.cert).unwrap();
        assert!(run(server, config).await.is_err());
    }

    #[tokio::test]
    async fn a_client_without_a_cert_is_refused() {
        let (host, client) = (make(), make());
        let paired = vec![client.cert.clone()];
        let server = server_config(&host.cert, &host.key, &paired).unwrap();
        let provider = provider();
        let bare = rustls::ClientConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServer {
                pinned: leaf_of(&host.cert).unwrap(),
                provider,
            }))
            .with_no_client_auth();
        assert!(run(server, Arc::new(bare)).await.is_err());
    }
}
