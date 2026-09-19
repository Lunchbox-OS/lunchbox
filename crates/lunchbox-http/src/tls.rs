//! Certificates for the management API (issue #156).
//!
//! Two sources, and the difference between them is who the browser trusts:
//!
//! - [`TlsMode::Files`] — a certificate somebody else issued. `tailscale cert`
//!   is the one that costs a family nothing: a real, publicly-trusted
//!   certificate for a `*.ts.net` name, renewed by a command. Let's Encrypt
//!   via a DNS-01 client and a home CA land here too. This is the mode with a
//!   padlock and no ceremony.
//! - [`TlsMode::SelfSigned`] — a certificate this device made for itself.
//!   Browsers show an interstitial the first time on each device. That is a
//!   real cost, and it buys the thing that actually matters here: the child on
//!   the same Wi-Fi can no longer read a password or a session cookie off the
//!   wire. The fingerprint is logged and readable through the companion, so
//!   the parent clicking through can check *which* certificate they are
//!   accepting rather than accepting whatever answers.
//!
//! The generated certificate is persisted, not minted per boot. A fingerprint
//! that changed on every restart would be a browser warning on every restart,
//! which is how a person learns to stop reading them.

use anyhow::{Context, Result};
use lunchbox_util::{ProtectedFile, ProtectedFiles};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::IpAddr;
use std::sync::Arc;
use tracing::info;

/// How long a generated certificate is good for.
///
/// Ten years, deliberately. Nothing checks this certificate's expiry except
/// the browser the parent already clicked past, and a device that quietly
/// stopped serving HTTPS one morning because a self-signed certificate aged
/// out would be a support call with no error message attached to it.
const SELF_SIGNED_VALID_YEARS: i32 = 10;

/// Build the rustls config for a listener, generating a certificate if that is
/// what the mode calls for.
///
/// `files` is where a generated certificate is kept — the state custodian's
/// directory, at a uid no activity has, because the private key is exactly the
/// kind of thing that must not be readable from a browser the child is using.
pub fn server_config(
    mode: &lunchbox_config::TlsMode,
    files: &Arc<dyn ProtectedFiles>,
    hostnames: &[String],
    addresses: &[IpAddr],
) -> Result<Option<Arc<ServerConfig>>> {
    use lunchbox_config::TlsMode;
    let pem = match mode {
        TlsMode::Off => return Ok(None),
        TlsMode::Files { cert, key } => {
            let cert_pem = std::fs::read_to_string(cert)
                .with_context(|| format!("reading TLS certificate {}", cert.display()))?;
            let key_pem = std::fs::read_to_string(key)
                .with_context(|| format!("reading TLS key {}", key.display()))?;
            format!("{cert_pem}\n{key_pem}")
        }
        TlsMode::SelfSigned => load_or_generate(files, hostnames, addresses)?,
    };

    let config = config_from_pem(&pem)?;
    Ok(Some(Arc::new(config)))
}

/// The stored certificate, or a fresh one if there is nothing usable there.
///
/// "Usable" is checked by parsing, not by trusting the file: a truncated write
/// or a half-migrated device would otherwise leave the API unable to listen at
/// all, and regenerating costs one browser warning where the alternative costs
/// the whole management surface.
fn load_or_generate(
    files: &Arc<dyn ProtectedFiles>,
    hostnames: &[String],
    addresses: &[IpAddr],
) -> Result<String> {
    if let Some(existing) = files.read(ProtectedFile::TlsCert)?
        && config_from_pem(&existing).is_ok()
    {
        info!(
            fingerprint = %fingerprint(&existing).unwrap_or_else(|| "?".into()),
            "Management API using its stored self-signed certificate",
        );
        return Ok(existing);
    }

    let mut names: Vec<String> = hostnames.to_vec();
    names.extend(addresses.iter().map(|a| a.to_string()));
    if names.is_empty() {
        names.push("localhost".to_string());
    }
    let mut params = rcgen::CertificateParams::new(names.clone())
        .context("building self-signed certificate parameters")?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "lunchbox management");
    params.not_after = rcgen::date_time_ymd(time_now_year() + SELF_SIGNED_VALID_YEARS, 1, 1);
    let key = rcgen::KeyPair::generate().context("generating a key pair")?;
    let cert = params
        .self_signed(&key)
        .context("self-signing the certificate")?;
    let pem = format!("{}\n{}", cert.pem(), key.serialize_pem());

    // Parse what we just made before storing it, so a bad generation is a
    // startup error rather than a certificate that fails on every connection.
    config_from_pem(&pem).context("the certificate we just generated does not load")?;
    files.write(ProtectedFile::TlsCert, &pem)?;
    info!(
        names = ?names,
        fingerprint = %fingerprint(&pem).unwrap_or_else(|| "?".into()),
        "Management API generated a self-signed certificate",
    );
    Ok(pem)
}

fn time_now_year() -> i32 {
    use chrono::Datelike;
    lunchbox_util::now().year()
}

fn config_from_pem(pem: &str) -> Result<ServerConfig> {
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<_, _>>()
        .context("parsing PEM certificates")?;
    anyhow::ensure!(!certs.is_empty(), "no certificate found in the PEM");

    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut reader)
        .context("parsing the PEM private key")?
        .context("no private key found in the PEM")?;

    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("building the TLS server config")
}

/// SHA-256 of the leaf certificate, colon-separated hex — the string a browser
/// shows in its "this certificate is not trusted" panel, so that what is in
/// the log can be compared against what is on the screen.
pub fn fingerprint(pem: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut reader = std::io::BufReader::new(pem.as_bytes());
    let cert = rustls_pemfile::certs(&mut reader).next()?.ok()?;
    let digest = Sha256::digest(&cert);
    Some(
        digest
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}
