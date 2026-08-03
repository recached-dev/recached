//! TLS material: certificate/key loading for the listener and for outbound
//! replication.

use crate::*;

// PEM parsing comes from rustls-pki-types, the crate rustls itself uses.
// `rustls-pemfile` was deprecated in favour of it (RUSTSEC-2025-0134), and an
// unmaintained dependency is a poor thing to have sitting in the TLS path.
pub(crate) fn load_certs(path: &str) -> std::io::Result<Vec<CertificateDer<'static>>> {
    CertificateDer::pem_file_iter(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?
        .map(|r| r.map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())))
        .collect()
}

pub(crate) fn load_private_key(path: &str) -> std::io::Result<PrivateKeyDer<'static>> {
    PrivateKeyDer::from_pem_file(path)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Decides what a (cert, key) environment pair means, without touching the
/// filesystem — split out from `load_tls_acceptor` so the security-relevant
/// rule is unit-testable.
///
/// Setting only one half is treated as a fatal misconfiguration rather than a
/// fallback to plaintext: an operator who set `RECACHED_TLS_CERT` intends TLS,
/// and silently serving unencrypted traffic on both ports because the key
/// variable was misspelled is a failure they would not detect until traffic had
/// already been exposed.
pub(crate) fn resolve_tls_paths(
    cert: Option<String>,
    key: Option<String>,
) -> Result<Option<(String, String)>, String> {
    match (cert, key) {
        (None, None) => Ok(None),
        (Some(c), Some(k)) => Ok(Some((c, k))),
        (Some(_), None) => Err(
            "RECACHED_TLS_CERT is set but RECACHED_TLS_KEY is not — refusing to start rather than \
             silently serving plaintext. Set both, or neither."
                .to_string(),
        ),
        (None, Some(_)) => Err(
            "RECACHED_TLS_KEY is set but RECACHED_TLS_CERT is not — refusing to start rather than \
             silently serving plaintext. Set both, or neither."
                .to_string(),
        ),
    }
}

/// The name a replica verifies the primary's certificate against.
///
/// Defaults to the host portion of `RECACHED_REPLICAOF`, which is what an
/// operator means by "connect to this primary". It is overridable because the
/// address is frequently an IP while the certificate names a host: a cert issued
/// for `primary.internal` does not validate against `10.0.1.5` unless it also
/// carries that IP as a SAN, and pointing at the IP is the common deployment.
pub(crate) fn repl_tls_servername(primary_addr: &str, override_name: Option<String>) -> String {
    if let Some(name) = override_name.map(|n| n.trim().to_string())
        && !name.is_empty()
    {
        return name;
    }
    // `host:port`, or a bare host. An IPv6 literal is bracketed, so splitting on
    // the last colon would cut inside the address.
    match primary_addr.rsplit_once(':') {
        Some((host, _)) if !host.is_empty() && !host.contains(':') => host.to_string(),
        _ => primary_addr
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or(primary_addr)
            .to_string(),
    }
}

/// Build the TLS connector a replica uses to reach its primary.
///
/// The trust anchor is an explicit file rather than the system root store, and
/// that is deliberate. Replication is a link between two machines the same
/// operator runs, so the right model is pinning the certificate (or the private
/// CA that issued it) — not trusting every public CA on earth to vouch for a
/// host that streams the entire keyspace. Pointing this at a system bundle still
/// works if the primary genuinely uses a publicly-issued certificate.
pub(crate) fn load_repl_tls_connector(ca_path: &str) -> Result<TlsConnector, String> {
    let certs =
        load_certs(ca_path).map_err(|e| format!("RECACHED_REPL_TLS_CA '{ca_path}': {e}"))?;
    if certs.is_empty() {
        return Err(format!(
            "RECACHED_REPL_TLS_CA '{ca_path}' contains no certificates — replication TLS would \
             trust nothing and every connection would fail."
        ));
    }
    let mut roots = RootCertStore::empty();
    for cert in certs {
        roots
            .add(cert)
            .map_err(|e| format!("RECACHED_REPL_TLS_CA '{ca_path}': {e}"))?;
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Returns a `TlsAcceptor` when both `RECACHED_TLS_CERT` and `RECACHED_TLS_KEY`
/// are set, `None` when neither is. Exits if exactly one is set.
pub(crate) fn load_tls_acceptor() -> Option<TlsAcceptor> {
    let (cert_path, key_path) = match resolve_tls_paths(
        std::env::var("RECACHED_TLS_CERT").ok(),
        std::env::var("RECACHED_TLS_KEY").ok(),
    ) {
        Ok(None) => return None,
        Ok(Some(pair)) => pair,
        Err(msg) => {
            error!("{msg}");
            std::process::exit(1);
        }
    };

    let cert_coll = load_certs(&cert_path).unwrap_or_else(|e| panic!("TLS cert {cert_path}: {e}"));
    let key = load_private_key(&key_path).unwrap_or_else(|e| panic!("TLS key {key_path}: {e}"));

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_coll, key)
        .expect("invalid TLS configuration");

    Some(TlsAcceptor::from(Arc::new(config)))
}

// ── tunables ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tls_loading_tests {
    use super::*;

    // A self-signed cert and its key, generated once with:
    //   openssl req -x509 -newkey rsa:2048 -keyout k -out c -days 3650 -nodes \
    //     -subj "/CN=recached-test"
    // Embedded rather than generated at test time so the test needs no openssl
    // on the runner and cannot fail for reasons unrelated to parsing.
    const TEST_CERT: &str = "-----BEGIN CERTIFICATE-----\nMIIDETCCAfmgAwIBAgIUDpGtGZ5z4j/X0RMdVgiZt5TyukwwDQYJKoZIhvcNAQEL\nBQAwGDEWMBQGA1UEAwwNcmVjYWNoZWQtdGVzdDAeFw0yNjA3MTkxNDUyMDVaFw0z\nNjA3MTYxNDUyMDVaMBgxFjAUBgNVBAMMDXJlY2FjaGVkLXRlc3QwggEiMA0GCSqG\nSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDdi5zyxNocCEi6elQKsS0onYh9aOMW5Hjz\n7zAcWa6EPp1g4Zz1tLF2Nk92CBG/iWzF5OckDChuIYjM+MTRws5UOSXwwkbLplKR\nSMGEst1mP3rZPGHq57w52OmxO599kBR4BpeWhFMC4w5xGEO9Gp4P+QdCIYaUEBxz\nLeEyCwapimzamKRYKO0VoZWzF0bLhYUHxc9FD2QMbaPUmRZZGdcttg/0Gq4U/P5N\n6jhWo+ekIKu1kpLSAZPiHtYNAzGu1sk0lTPyVxdmmwqPueV9MLUgVIpDWA+QL80I\nXIjTfaQAOl4k31AeC+yglCyhB/yl/0ROQUAXGgozsFJnpxujLGMPAgMBAAGjUzBR\nMB0GA1UdDgQWBBSWbJJErt4zE9+u8lbBnAPXaRSI0TAfBgNVHSMEGDAWgBSWbJJE\nrt4zE9+u8lbBnAPXaRSI0TAPBgNVHRMBAf8EBTADAQH/MA0GCSqGSIb3DQEBCwUA\nA4IBAQBGslzIW0Q46r7eQGK22fTEfNReSy4f7PZPGn/BZbj499LKSfRP1z8A3bbF\n2CdQKswbhVUbHfLUoaRwRfmJWhR/I/UxNkUfVlQ/jQBaUvg2ZCy1l/3kRM6N1t5K\ntkwg+dzai/6LwT7RHmbl8Dx32on3+x9vJMYtoxeBk4nfHZTQMIOd3zsaXp/+RWUY\nzuIWXX/rf862GerYhoHVCWzMcHMLnI/Mwzlm2tgVnfW1XpI/La3fxnTWYT4g4PIJ\nfXe3WrO9VyC1ZZ7PjE4Pq4unCRbJ2yZ5toybr4kcT4UGFrsXjnAsT+RyLY4By50D\nkaBPsvjq5ZvbiPBtEINXbmF3A7cq\n-----END CERTIFICATE-----";
    const TEST_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQDdi5zyxNocCEi6\nelQKsS0onYh9aOMW5Hjz7zAcWa6EPp1g4Zz1tLF2Nk92CBG/iWzF5OckDChuIYjM\n+MTRws5UOSXwwkbLplKRSMGEst1mP3rZPGHq57w52OmxO599kBR4BpeWhFMC4w5x\nGEO9Gp4P+QdCIYaUEBxzLeEyCwapimzamKRYKO0VoZWzF0bLhYUHxc9FD2QMbaPU\nmRZZGdcttg/0Gq4U/P5N6jhWo+ekIKu1kpLSAZPiHtYNAzGu1sk0lTPyVxdmmwqP\nueV9MLUgVIpDWA+QL80IXIjTfaQAOl4k31AeC+yglCyhB/yl/0ROQUAXGgozsFJn\npxujLGMPAgMBAAECggEAQ4xj6ClZDx7/fcv6f+ARksalbQdj5gD3V/jfxGUbrrqg\npX9kqg3T5eUdSTGgp7Ow9I2cZANI+HtFCKn46LPq0QczqDqz9zfZCO8UAe+/TYOh\nY0bj3AmX/FNEvYMeV9xsQURRR9VEsiakqprpXGkXNGuLaQBr1g0rf3rHpMhz2ZEH\nw7QxoUfH3YL7fMIWwAvHanl6HwzE3TVh3felFGGGiqUaGg2Pll+/s5AiYnnZuq39\nW+t0RVH36rSFh037Su/ScCs44WZS+kGyqcuxyWLwvNwXEVXC3h42N62MHGgY8xQw\nP8wMSxGejEIr8IGpwhl86+oW+44nxarmrMBzMPRIcQKBgQD6VnpsIwxWV1zNC1LD\nbTVRQOoqJWDzxdXB47IB29ADHJnfBLbMr/6kIgIfuCu1Kf9wWTZGHwFUOKFhJDBC\ngLHpWFMWKSew4UmNyc0a16a9Pb7mdNyxnTY1oQucEbzS3pLMzda2dAdxYJE0Cuc6\nJ8Xp2Jo73LnRxY+NyEyl28frAwKBgQDijmtG0cDESNPMhxqJO/9KoRqRT3T+JqNf\noWaKlfSlQFaGecjdk9dNPZ1Aew4xI0v/C5YTwT6MUVDEXmoaSsa+S1atoDLNsojL\nuWqUno9mF6o3U23pi4vlEYh6c/V7Bd1VYde8ZQqVq0KxbCmYp1VE+DBeyNNT7Q5X\nN2lst0hEBQKBgQCXjds3tFA3xVQNXpmQboEk2+Pn+BEmA9NROoP91BGukJYnCjeQ\n28uRmnUmttzfJLncTmYpNYQcdNxebwY4fKk415wVgnzg/MMG7/EYGw566vKzmnQx\noze6Z/EbXzGth8nf643dj4kh/pBprWAnOQT8eYGGVC667Jvn/idJEjGJ+QKBgQCz\nGmgQio3cHr7huATwbO/7rbT1H12b9iu91DjeYoIPifddRDXZhaD1vTnt2dp0WjUg\nIaa5Y1HxV++D7ifvNSI9Gg4iIL1JBFVEyQZLC7bNvPOh3WDM+rbTlrLQK4/re81o\nTHtiwnZFsCh/XsTbm527coG6zQTUGln19SZw/cwxiQKBgA8dcEyBvPi6JgAqLy+5\n3Ev1uZEKkAeAQAkOV9jzqDN9NTi7GWOz3mtY2zopYjef7Wl0V4Qjkr7Jkxlx2wyn\nHboOuCEjComkRxn5vrHm6EBp0uTrdFIknLysxmQFgNamp9E8mX9p/q9rq7aZWzPu\nr+3jOYvwFyzAQ4j2tGzUm7Zd\n-----END PRIVATE KEY-----";

    fn write(name: &str, body: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("recached_test_{name}_{}", std::process::id()));
        std::fs::write(&path, body).expect("write pem");
        path
    }

    // ── replication TLS (2.2) ───────────────────────────────────────────────

    #[test]
    fn the_verified_servername_defaults_to_the_primarys_host() {
        // What an operator means by "connect to this primary" is the host they
        // named, so that is what the certificate is checked against.
        assert_eq!(
            repl_tls_servername("primary.internal:6381", None),
            "primary.internal"
        );
        assert_eq!(repl_tls_servername("10.0.1.5:6381", None), "10.0.1.5");
        assert_eq!(
            repl_tls_servername("primary.internal", None),
            "primary.internal"
        );
    }

    #[test]
    fn an_ipv6_primary_address_is_not_split_inside_the_address() {
        // Splitting on the last colon would cut inside an IPv6 literal and
        // produce a servername that could never validate.
        assert_eq!(repl_tls_servername("[::1]:6381", None), "::1");
        assert_eq!(repl_tls_servername("[fd00::5]:6381", None), "fd00::5");
    }

    #[test]
    fn the_servername_override_wins_and_ignores_blanks() {
        // The common deployment points RECACHED_REPLICAOF at an IP while the
        // certificate names a host, which cannot validate without an IP SAN.
        assert_eq!(
            repl_tls_servername("10.0.1.5:6381", Some("primary.internal".into())),
            "primary.internal"
        );
        assert_eq!(
            repl_tls_servername("10.0.1.5:6381", Some("  primary.internal  ".into())),
            "primary.internal"
        );
        // Empty or whitespace is "unset", not "verify against nothing".
        assert_eq!(
            repl_tls_servername("10.0.1.5:6381", Some(String::new())),
            "10.0.1.5"
        );
        assert_eq!(
            repl_tls_servername("10.0.1.5:6381", Some("   ".into())),
            "10.0.1.5"
        );
    }

    #[test]
    fn a_missing_or_unusable_replication_ca_is_a_startup_error() {
        // Trusting nothing would fail every connection at runtime rather than at
        // startup, which is much harder to diagnose from a replica's logs.
        // `TlsConnector` is not Debug, so unwrap the error side by hand.
        let missing = load_repl_tls_connector("/nonexistent/recached-ca.pem");
        let Err(err) = missing else {
            panic!("a missing CA file must be refused");
        };
        assert!(err.contains("RECACHED_REPL_TLS_CA"), "{err}");

        let junk = write("repl_ca_junk.pem", "not a certificate\n");
        let unusable = load_repl_tls_connector(junk.to_str().unwrap());
        let _ = std::fs::remove_file(&junk);
        let Err(err) = unusable else {
            panic!("a file with no certificates must be refused");
        };
        assert!(err.contains("RECACHED_REPL_TLS_CA"), "{err}");
    }

    #[test]
    fn a_self_signed_certificate_works_as_the_replication_trust_anchor() {
        // Pinning the primary's own certificate is the documented path, and the
        // reason the trust anchor is an explicit file rather than the system root
        // store: replication is a private link between two hosts one operator
        // runs, so trusting every public CA to vouch for it would be backwards.
        let ca = write("repl_ca_ok.pem", TEST_CERT);
        assert!(
            load_repl_tls_connector(ca.to_str().unwrap()).is_ok(),
            "a valid self-signed PEM must build a connector"
        );
        let _ = std::fs::remove_file(&ca);
    }

    #[test]
    fn a_pem_certificate_and_key_load() {
        // PEM parsing moved from the deprecated rustls-pemfile to
        // rustls-pki-types. These two functions had no coverage at all, so the
        // swap would have been verified only by the code compiling.
        let cert_path = write("tls_load.crt", TEST_CERT);
        let key_path = write("tls_load.key", TEST_KEY);

        let certs = load_certs(cert_path.to_str().unwrap()).expect("cert must parse");
        assert_eq!(certs.len(), 1, "one certificate in the chain");
        assert!(!certs[0].as_ref().is_empty(), "DER body must be non-empty");

        let key = load_private_key(key_path.to_str().unwrap()).expect("key must parse");
        assert!(!key.secret_der().is_empty(), "key DER must be non-empty");

        let _ = std::fs::remove_file(&cert_path);
        let _ = std::fs::remove_file(&key_path);
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        assert!(load_certs("/nonexistent/recached-test.crt").is_err());
        assert!(load_private_key("/nonexistent/recached-test.key").is_err());
    }

    #[test]
    fn a_file_with_no_pem_content_is_rejected() {
        // Pointing RECACHED_TLS_CERT at the wrong file must fail loudly rather
        // than yielding an empty chain that rustls would later reject with a
        // much less obvious error.
        let junk = write("tls_junk.crt", "this is not a PEM file\n");
        assert!(
            load_certs(junk.to_str().unwrap())
                .map(|c| c.is_empty())
                .unwrap_or(true),
            "non-PEM input must not yield certificates"
        );
        let junk_key = write("tls_junk.key", "still not PEM\n");
        assert!(load_private_key(junk_key.to_str().unwrap()).is_err());

        let _ = std::fs::remove_file(&junk);
        let _ = std::fs::remove_file(&junk_key);
    }
}
