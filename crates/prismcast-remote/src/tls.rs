//! TLS for the native WebSocket transport (WS-003, ADR-0022).
//!
//! TLS is terminated in `prismcast-remote` at the WS accept loop with
//! `rustls` (pure-Rust ring provider — the aws-lc-rs C build is excluded by
//! feature pinning) via `tokio-rustls`:
//!
//! - [`WsTlsConfig`] points the **server** at operator-provided PEM files
//!   (leaf-first certificate chain; PKCS#8 or PKCS#1 private key). There is
//!   deliberately no production self-signed generation — that needs
//!   persistence and a trust UX story (ADR-0022 §b); tests generate throwaway
//!   certificates with `rcgen`.
//! - [`ClientTlsConfig`] builds the **client** trust model: native system
//!   roots, plus an optional extra CA bundle for private deployments, plus an
//!   explicit warn-logged danger switch that disables verification.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tracing::{debug, warn};

/// Server-side TLS configuration: operator-provided PEM certificate chain
/// and private key. `wss://` is enabled by setting
/// [`WsServerConfig::tls`](crate::ws::WsServerConfig::tls) to `Some` of this.
#[derive(Debug, Clone)]
pub struct WsTlsConfig {
    /// PEM file with the certificate chain, **leaf first**, intermediates
    /// after (the standard order produced by ACME clients).
    pub cert_path: PathBuf,
    /// PEM file with the private key (PKCS#8 `PRIVATE KEY` or PKCS#1
    /// `RSA PRIVATE KEY`).
    pub key_path: PathBuf,
}

/// Errors loading TLS material or building rustls configs. Every variant
/// names the offending path where one exists.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    /// A PEM file could not be read.
    #[error("failed to read PEM file {path}: {source}")]
    Io {
        /// The file that failed.
        path: PathBuf,
        /// The underlying I/O error.
        source: io::Error,
    },
    /// The certificate PEM parsed no certificates or held malformed DER.
    #[error("invalid certificate chain in {path}: {source}")]
    InvalidCert {
        /// The certificate chain file.
        path: PathBuf,
        /// The PEM/DER decoding error.
        source: rustls::pki_types::pem::Error,
    },
    /// The certificate chain file contained no certificates.
    #[error("no certificates found in {path}: the chain must be leaf-first PEM")]
    EmptyCertChain {
        /// The certificate chain file.
        path: PathBuf,
    },
    /// The key PEM held no private key or malformed key material.
    #[error("invalid private key in {path}: {source}")]
    InvalidKey {
        /// The private key file.
        path: PathBuf,
        /// The PEM/DER decoding error.
        source: rustls::pki_types::pem::Error,
    },
    /// The key PEM parsed but contained no private key section.
    #[error("no private key found in {path}: expected PKCS#8 or PKCS#1 PEM")]
    MissingKey {
        /// The private key file.
        path: PathBuf,
    },
    /// A certificate in the extra CA bundle failed webpki parsing.
    #[error("invalid CA certificate in {path}: {source}")]
    InvalidCa {
        /// The extra CA bundle file.
        path: PathBuf,
        /// The rustls certificate error.
        source: rustls::Error,
    },
    /// rustls rejected otherwise valid-looking material (e.g. key does not
    /// match the leaf certificate).
    #[error("rustls configuration error: {0}")]
    Rustls(#[from] rustls::Error),
}

impl TlsError {
    fn pem(path: &std::path::Path, for_cert: bool, source: rustls::pki_types::pem::Error) -> Self {
        use rustls::pki_types::pem::Error as PemError;
        match source {
            PemError::Io(source) => Self::Io {
                path: path.to_path_buf(),
                source,
            },
            PemError::NoItemsFound if !for_cert => Self::MissingKey {
                path: path.to_path_buf(),
            },
            other if for_cert => Self::InvalidCert {
                path: path.to_path_buf(),
                source: other,
            },
            other => Self::InvalidKey {
                path: path.to_path_buf(),
                source: other,
            },
        }
    }
}

impl WsTlsConfig {
    /// Loads the PEM chain (leaf first) and the private key, and builds a
    /// rustls [`TlsAcceptor`] for the WS accept loop. Called once per server
    /// bind, not per connection.
    pub fn acceptor(&self) -> Result<TlsAcceptor, TlsError> {
        let chain = load_cert_chain(&self.cert_path)?;
        let key = load_private_key(&self.key_path)?;
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)?;
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

/// Loads a leaf-first PEM certificate chain; fails on an empty chain.
fn load_cert_chain(path: &std::path::Path) -> Result<Vec<CertificateDer<'static>>, TlsError> {
    let items = CertificateDer::pem_file_iter(path).map_err(|e| TlsError::pem(path, true, e))?;
    let mut chain = Vec::new();
    for item in items {
        chain.push(item.map_err(|e| TlsError::pem(path, true, e))?);
    }
    if chain.is_empty() {
        return Err(TlsError::EmptyCertChain {
            path: path.to_path_buf(),
        });
    }
    Ok(chain)
}

/// Loads the first private key section found in the PEM file (PKCS#8 or
/// PKCS#1; SEC1 EC keys are also accepted by the parser).
fn load_private_key(path: &std::path::Path) -> Result<PrivateKeyDer<'static>, TlsError> {
    PrivateKeyDer::from_pem_file(path).map_err(|e| TlsError::pem(path, false, e))
}

/// Client-side TLS trust configuration (used by the future ws_client/CLI
/// wiring; ADR-0022 §d).
#[derive(Debug, Clone, Default)]
pub struct ClientTlsConfig {
    /// Optional PEM bundle with an extra certificate authority to trust on
    /// top of the system roots — for private/self-signed deployments.
    pub extra_ca_path: Option<PathBuf>,
    /// Disables certificate verification entirely. Warn-logged on use;
    /// exists for diagnostics only, never a default.
    pub danger_accept_invalid_certs: bool,
}

/// Builds a rustls [`TlsConnector`] from the native system roots plus the
/// configured extras. A partially unreadable system store is not fatal: the
/// failures are logged and whatever loaded is used.
pub fn client_connector(config: &ClientTlsConfig) -> Result<TlsConnector, TlsError> {
    let mut roots = RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for error in &native.errors {
        warn!(%error, "failed to load part of the system certificate store; continuing");
    }
    let (added, ignored) = roots.add_parsable_certificates(native.certs);
    debug!(added, ignored, "loaded native system certificate roots");
    if let Some(path) = &config.extra_ca_path {
        let chain = load_cert_chain(path)?;
        for cert in chain {
            roots.add(cert).map_err(|source| TlsError::InvalidCa {
                path: path.clone(),
                source,
            })?;
        }
    }

    let builder = ClientConfig::builder();
    let builder = if config.danger_accept_invalid_certs {
        warn!(
            "TLS certificate verification is DISABLED (danger_accept_invalid_certs); \
               traffic is encrypted but not authenticated"
        );
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(DangerAcceptAll))
    } else {
        builder.with_root_certificates(roots)
    };
    Ok(TlsConnector::from(Arc::new(builder.with_no_client_auth())))
}

/// Certificate verifier that accepts everything. Only constructed behind
/// [`ClientTlsConfig::danger_accept_invalid_certs`], which is warn-logged.
#[derive(Debug)]
struct DangerAcceptAll;

impl ServerCertVerifier for DangerAcceptAll {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Ok(HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes an rcgen-generated CA + leaf into a fresh tempdir; returns the
    /// paths. Never committed material (ADR-0022 §b).
    struct TestPki {
        dir: PathBuf,
        cert_path: PathBuf,
        key_path: PathBuf,
    }

    impl TestPki {
        fn generate() -> Self {
            use rcgen::{BasicConstraints, CertificateParams, DnType, IsCa, KeyPair};

            let dir =
                std::env::temp_dir().join(format!("prismcast-tls-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("create tempdir");

            let ca_key = KeyPair::generate().expect("ca key");
            let mut ca_params = CertificateParams::new(Vec::new()).expect("ca params");
            ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            ca_params
                .distinguished_name
                .push(DnType::CommonName, "prismcast test ca");
            let ca_cert = ca_params.self_signed(&ca_key).expect("ca cert");

            let leaf_key = KeyPair::generate().expect("leaf key");
            let leaf_params =
                CertificateParams::new(vec!["localhost".to_string(), "127.0.0.1".to_string()])
                    .expect("leaf params");
            let leaf_cert = leaf_params
                .signed_by(&leaf_key, &ca_cert, &ca_key)
                .expect("leaf cert");

            let cert_path = dir.join("cert.pem");
            std::fs::write(&cert_path, format!("{}{}", leaf_cert.pem(), ca_cert.pem()))
                .expect("write chain");
            let key_path = dir.join("key.pem");
            std::fs::write(&key_path, leaf_key.serialize_pem()).expect("write key");
            let ca_pem_path = dir.join("ca.pem");
            std::fs::write(&ca_pem_path, ca_cert.pem()).expect("write ca");
            Self {
                dir,
                cert_path,
                key_path,
            }
        }

        fn server_config(&self) -> WsTlsConfig {
            WsTlsConfig {
                cert_path: self.cert_path.clone(),
                key_path: self.key_path.clone(),
            }
        }
    }

    impl Drop for TestPki {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn acceptor_loads_leaf_first_chain_and_pkcs8_key() {
        let pki = TestPki::generate();
        pki.server_config().acceptor().expect("acceptor");
    }

    #[test]
    fn missing_key_path_is_typed_and_names_the_path() {
        let pki = TestPki::generate();
        let config = WsTlsConfig {
            cert_path: pki.cert_path.clone(),
            key_path: pki.dir.join("does-not-exist.pem"),
        };
        match config.acceptor() {
            Err(TlsError::Io { path, .. }) => assert!(path.ends_with("does-not-exist.pem")),
            Err(other) => panic!("expected Io naming the path, got {other}"),
            Ok(_) => panic!("expected Io naming the path, got an acceptor"),
        }
    }

    #[test]
    fn garbage_cert_pem_is_typed_and_names_the_path() {
        let pki = TestPki::generate();
        let garbage = pki.dir.join("garbage.pem");
        std::fs::write(&garbage, b"not a pem file at all").expect("write garbage");
        let config = WsTlsConfig {
            cert_path: garbage.clone(),
            key_path: pki.key_path.clone(),
        };
        match config.acceptor() {
            Err(TlsError::EmptyCertChain { path }) | Err(TlsError::InvalidCert { path, .. }) => {
                assert_eq!(path, garbage)
            }
            Err(other) => panic!("expected typed cert error naming the path, got {other}"),
            Ok(_) => panic!("expected typed cert error, got an acceptor"),
        }
    }

    #[test]
    fn garbage_key_pem_reports_missing_key() {
        let pki = TestPki::generate();
        let garbage = pki.dir.join("garbage-key.pem");
        std::fs::write(
            &garbage,
            b"-----BEGIN CERTIFICATE-----\n-----END CERTIFICATE-----\n",
        )
        .expect("write garbage");
        let config = WsTlsConfig {
            cert_path: pki.cert_path.clone(),
            key_path: garbage.clone(),
        };
        match config.acceptor() {
            Err(TlsError::MissingKey { path }) | Err(TlsError::InvalidKey { path, .. }) => {
                assert_eq!(path, garbage)
            }
            Err(other) => panic!("expected typed key error naming the path, got {other}"),
            Ok(_) => panic!("expected typed key error, got an acceptor"),
        }
    }

    /// A client rooted at the test CA completes a real TLS handshake
    /// against the server acceptor; the danger verifier accepts the same
    /// handshake without any trusted root.
    #[tokio::test]
    async fn client_connector_handshakes_with_extra_ca_and_danger_mode() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let pki = TestPki::generate();
        let acceptor = pki.server_config().acceptor().expect("acceptor");
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut tls = acceptor.accept(stream).await.expect("server handshake");
            let mut buf = [0u8; 4];
            tls.read_exact(&mut buf).await.expect("read");
            tls.write_all(&buf).await.expect("echo");
        });

        let ca_path = pki.dir.join("ca.pem");
        let connector = client_connector(&ClientTlsConfig {
            extra_ca_path: Some(ca_path),
            danger_accept_invalid_certs: false,
        })
        .expect("connector");
        let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let server_name = ServerName::try_from("localhost").expect("name").to_owned();
        let mut tls = connector
            .connect(server_name, stream)
            .await
            .expect("client handshake");
        tls.write_all(b"ping").await.expect("write");
        let mut buf = [0u8; 4];
        tls.read_exact(&mut buf).await.expect("read");
        assert_eq!(&buf, b"ping");
        drop(tls);
        server.await.expect("server task");

        // Without the extra CA the untrusted leaf must be rejected...
        let acceptor = pki.server_config().acceptor().expect("acceptor");
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let _ = acceptor.accept(stream).await;
        });
        let strict = client_connector(&ClientTlsConfig::default()).expect("connector");
        let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let server_name = ServerName::try_from("localhost").expect("name").to_owned();
        assert!(strict.connect(server_name.clone(), stream).await.is_err());
        server.await.expect("server task");

        // ...and the danger verifier must accept it.
        let acceptor = pki.server_config().acceptor().expect("acceptor");
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let tls = acceptor.accept(stream).await.expect("handshake");
            drop(tls);
        });
        let danger = client_connector(&ClientTlsConfig {
            extra_ca_path: None,
            danger_accept_invalid_certs: true,
        })
        .expect("connector");
        let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
        let tls = danger
            .connect(server_name, stream)
            .await
            .expect("danger handshake must succeed");
        drop(tls);
        server.await.expect("server task");
    }

    #[test]
    fn native_root_loading_survives_and_is_additive() {
        // Default config must build a connector on any CI host, whatever the
        // system store looks like (partially unreadable stores log + skip).
        client_connector(&ClientTlsConfig::default()).expect("connector");
    }
}
