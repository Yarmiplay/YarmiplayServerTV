//! Jellyfin's `config/network.xml`: we own the port, HTTPS and remote-access
//! elements and leave every other element exactly as Jellyfin wrote it.

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct NetworkSettings {
    pub http_port: u16,
    pub https_port: u16,
    /// PFX path + password when HTTPS should be on.
    pub https: Option<(String, String)>,
}

impl NetworkSettings {
    fn managed(&self) -> Vec<(&'static str, String)> {
        let (enable, path, password) = match &self.https {
            Some((path, password)) => ("true", path.clone(), password.clone()),
            None => ("false", String::new(), String::new()),
        };
        vec![
            ("InternalHttpPort", self.http_port.to_string()),
            ("InternalHttpsPort", self.https_port.to_string()),
            ("PublicHttpPort", self.http_port.to_string()),
            ("PublicHttpsPort", self.https_port.to_string()),
            ("EnableHttps", enable.to_string()),
            ("RequireHttps", "false".to_string()),
            ("CertificatePath", path),
            ("CertificatePassword", password),
            ("EnableRemoteAccess", "true".to_string()),
            ("AutoDiscovery", "true".to_string()),
        ]
    }
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Return `existing` (or a fresh document) with the managed elements set.
pub fn patch_network_xml(existing: Option<&str>, settings: &NetworkSettings) -> String {
    let managed = settings.managed();
    let mut body: Vec<String> = Vec::new();
    let mut seen: Vec<&str> = Vec::new();

    if let Some(xml) = existing {
        let trimmed = xml.trim_start_matches('\u{feff}');
        if let Ok(doc) = roxmltree::Document::parse(trimmed) {
            let root = doc.root_element();
            if root.tag_name().name() == "NetworkConfiguration" {
                for child in root.children().filter(|n| n.is_element()) {
                    let name = child.tag_name().name();
                    if let Some((key, value)) = managed.iter().find(|(k, _)| *k == name) {
                        seen.push(key);
                        body.push(format!("  <{key}>{}</{key}>", escape(value)));
                    } else {
                        body.push(format!("  {}", &trimmed[child.range()]));
                    }
                }
            }
        }
    }
    for (key, value) in &managed {
        if !seen.contains(key) {
            body.push(format!("  <{key}>{}</{key}>", escape(value)));
        }
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<NetworkConfiguration xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\">\n{}\n</NetworkConfiguration>\n",
        body.join("\n")
    )
}

pub fn write_network_xml(config_dir: &Path, settings: &NetworkSettings) -> Result<(), String> {
    let path = config_dir.join("network.xml");
    let existing = std::fs::read_to_string(&path).ok();
    let next = patch_network_xml(existing.as_deref(), settings);
    if existing.as_deref() == Some(next.as_str()) {
        return Ok(());
    }
    crate::paths::write_atomic(&path, next.as_bytes())
}

/// PKCS#12 bundle (leaf + chain + key) for Jellyfin's Kestrel HTTPS listener.
pub fn pem_to_pfx(cert_pem: &str, key_pem: &str, password: &str) -> Result<Vec<u8>, String> {
    let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("no certificate in PEM".into());
    }
    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .map_err(|e| format!("private key PEM: {e}"))?
        .ok_or("no private key in PEM")?;
    let key_der = match key {
        rustls::pki_types::PrivateKeyDer::Pkcs8(k) => k.secret_pkcs8_der().to_vec(),
        _ => return Err("expected a PKCS#8 private key".into()),
    };
    let key = p12_keystore::PrivateKey::from_der(&key_der).map_err(|e| format!("private key: {e}"))?;
    let chain = certs
        .iter()
        .map(|c| p12_keystore::Certificate::from_der(c.as_ref()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("certificate: {e}"))?;
    let local_key_id: Vec<u8> = vec![1];
    let chain = p12_keystore::PrivateKeyChain::new(local_key_id, key, chain);
    let mut store = p12_keystore::KeyStore::new();
    store.add_entry("jellyfin", p12_keystore::KeyStoreEntry::PrivateKeyChain(chain));
    store.writer(password).write().map_err(|e| format!("PKCS#12: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXISTING: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<NetworkConfiguration xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:xsd="http://www.w3.org/2001/XMLSchema">
  <BaseUrl />
  <EnableHttps>false</EnableHttps>
  <InternalHttpPort>8096</InternalHttpPort>
  <KnownProxies>
    <string>10.0.0.2</string>
  </KnownProxies>
</NetworkConfiguration>"#;

    #[test]
    fn patch_keeps_unknown_elements_and_sets_ours() {
        let s = NetworkSettings { http_port: 9096, https_port: 9920, https: Some(("C:\\a&b\\cert.pfx".into(), "pw".into())) };
        let out = patch_network_xml(Some(EXISTING), &s);
        assert!(out.contains("<BaseUrl />"));
        assert!(out.contains("<string>10.0.0.2</string>"));
        assert!(out.contains("<InternalHttpPort>9096</InternalHttpPort>"));
        assert!(out.contains("<EnableHttps>true</EnableHttps>"));
        assert!(out.contains("<CertificatePath>C:\\a&amp;b\\cert.pfx</CertificatePath>"));
        assert_eq!(out.matches("<InternalHttpPort>").count(), 1);
        assert!(roxmltree::Document::parse(&out).is_ok());
        // Idempotent.
        assert_eq!(patch_network_xml(Some(&out), &s), out);
    }

    #[test]
    fn fresh_document_when_missing_or_garbage() {
        let s = NetworkSettings { http_port: 8096, https_port: 8920, https: None };
        for existing in [None, Some("not xml")] {
            let out = patch_network_xml(existing, &s);
            assert!(out.contains("<EnableHttps>false</EnableHttps>"));
            assert!(roxmltree::Document::parse(&out).is_ok());
        }
    }

    #[test]
    fn pfx_roundtrip() {
        let params = rcgen::CertificateParams::new(vec!["x.duckdns.org".into()]).unwrap();
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&key).unwrap();
        let pfx = pem_to_pfx(&cert.pem(), &key.serialize_pem(), "secret").unwrap();
        let store = p12_keystore::KeyStore::from_pkcs12(&pfx, "secret", p12_keystore::Pkcs12ImportPolicy::Strict).unwrap();
        let (_, chain) = store.private_key_chain().unwrap();
        assert_eq!(chain.certs().len(), 1);
    }
}
