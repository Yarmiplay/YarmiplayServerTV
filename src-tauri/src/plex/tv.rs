//! plex.tv, on behalf of the host's own Plex account: linking the account
//! with a PIN code (the same flow Plex apps use), finding the local server in
//! the account's resources, and sharing libraries with friends.
//!
//! The sharing endpoints follow the shapes Plex's own web app uses; the spike
//! in the Plex plan re-checks them against a live account before release.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const PRODUCT: &str = "YarmiplayServerTV";

/// The `X-Plex-*` headers every request carries, to plex.tv and to Plex.
pub fn plex_headers(rb: reqwest::RequestBuilder, client_id: &str) -> reqwest::RequestBuilder {
    rb.header("Accept", "application/json")
        .header("X-Plex-Product", PRODUCT)
        .header("X-Plex-Version", env!("CARGO_PKG_VERSION"))
        .header("X-Plex-Client-Identifier", client_id)
        .header("X-Plex-Device-Name", crate::jellyfin::api::hostname())
}

#[derive(Clone)]
pub struct PlexTv {
    base: String,
    client: reqwest::Client,
    client_id: String,
    token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Pin {
    pub id: u64,
    pub code: String,
    #[serde(default)]
    pub expires_in: u64,
    #[serde(default)]
    pub auth_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Account {
    pub id: String,
    pub username: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Resource {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub client_identifier: String,
    #[serde(default)]
    pub provides: String,
    #[serde(default)]
    pub owned: bool,
    #[serde(default)]
    pub access_token: Option<String>,
}

/// A library share with one friend.
#[derive(Debug, Clone, PartialEq)]
pub struct Share {
    pub share_id: String,
    pub user_id: String,
    pub username: String,
}

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

/// `GET /api/servers/{machineId}` lists the server's sections with both the
/// local key and the id plex.tv uses for sharing.
fn parse_section_ids(xml: &str) -> Result<BTreeMap<String, u64>, String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("plex.tv sections: {e}"))?;
    Ok(doc
        .descendants()
        .filter(|n| n.has_tag_name("Section"))
        .filter_map(|n| {
            Some((
                n.attribute("key")?.to_string(),
                n.attribute("id")?.parse().ok()?,
            ))
        })
        .collect())
}

fn parse_share(v: &Value, invitee: &str) -> Share {
    let invited = v.get("invited").unwrap_or(&Value::Null);
    let username = Some(text(invited, "username"))
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| invitee.to_string());
    Share {
        share_id: text(v, "id"),
        user_id: Some(text(invited, "id"))
            .filter(|i| !i.is_empty())
            .unwrap_or_else(|| text(v, "invitedId")),
        username,
    }
}

impl PlexTv {
    pub fn new(client_id: &str, token: Option<String>) -> Self {
        Self::with_base("https://plex.tv", client_id, token)
    }

    pub fn with_base(base: &str, client_id: &str, token: Option<String>) -> Self {
        let client = crate::net::client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            base: base.trim_end_matches('/').to_string(),
            client,
            client_id: client_id.to_string(),
            token,
        }
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let rb = plex_headers(
            self.client.request(method, format!("{}{path}", self.base)),
            &self.client_id,
        );
        match &self.token {
            Some(t) => rb.header("X-Plex-Token", t),
            None => rb,
        }
    }

    async fn send(rb: reqwest::RequestBuilder, what: &str) -> Result<reqwest::Response, String> {
        let resp = rb
            .send()
            .await
            .map_err(|e| format!("{what}: plex.tv is not reachable ({e})"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        // plex.tv explains refusals as {"errors":[{"message": ...}]}.
        let message = serde_json::from_str::<Value>(&body).ok().and_then(|v| {
            v.pointer("/errors/0/message")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        Err(match (status.as_u16(), message) {
            (401, _) => format!("{what}: not signed in to plex.tv (401)"),
            (_, Some(m)) => format!("{what}: {m}"),
            _ => format!("{what}: {status}"),
        })
    }

    async fn json(rb: reqwest::RequestBuilder, what: &str) -> Result<Value, String> {
        Self::send(rb, what)
            .await?
            .json()
            .await
            .map_err(|e| format!("{what}: {e}"))
    }

    /// Start linking: the user enters `code` at plex.tv/link.
    pub async fn create_pin(&self) -> Result<Pin, String> {
        let rb = self
            .req(reqwest::Method::POST, "/api/v2/pins")
            .query(&[("strong", "false")]);
        Self::send(rb, "link code")
            .await?
            .json()
            .await
            .map_err(|e| format!("link code: {e}"))
    }

    /// The account token once the user entered the code, else None.
    pub async fn check_pin(&self, id: u64) -> Result<Option<String>, String> {
        let rb = self.req(reqwest::Method::GET, &format!("/api/v2/pins/{id}"));
        let pin: Pin = Self::send(rb, "link code")
            .await?
            .json()
            .await
            .map_err(|e| format!("link code: {e}"))?;
        Ok(pin.auth_token.filter(|t| !t.is_empty()))
    }

    pub async fn account(&self) -> Result<Account, String> {
        let v = Self::json(
            self.req(reqwest::Method::GET, "/api/v2/user"),
            "Plex account",
        )
        .await?;
        Ok(Account {
            id: text(&v, "id"),
            username: Some(text(&v, "username"))
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| text(&v, "title")),
        })
    }

    pub async fn resources(&self) -> Result<Vec<Resource>, String> {
        let rb = self
            .req(reqwest::Method::GET, "/api/v2/resources")
            .query(&[("includeHttps", "1")]);
        Self::send(rb, "Plex servers")
            .await?
            .json()
            .await
            .map_err(|e| format!("Plex servers: {e}"))
    }

    /// Local section key → plex.tv section id, for sharing.
    pub async fn section_ids(&self, machine_id: &str) -> Result<BTreeMap<String, u64>, String> {
        let rb = self
            .req(reqwest::Method::GET, &format!("/api/servers/{machine_id}"))
            .header("Accept", "application/xml");
        let xml = Self::send(rb, "Plex libraries")
            .await?
            .text()
            .await
            .map_err(|e| format!("Plex libraries: {e}"))?;
        parse_section_ids(&xml)
    }

    /// Invite `invitee` (plex.tv username or email) as a friend with access
    /// to the given sections; they accept it with their own account.
    pub async fn invite(
        &self,
        machine_id: &str,
        section_ids: &[u64],
        invitee: &str,
    ) -> Result<Share, String> {
        let body = json!({
            "machineIdentifier": machine_id,
            "librarySectionIds": section_ids,
            "invitedEmail": invitee,
            "settings": {
                "allowSync": false,
                "allowCameraUpload": false,
                "allowChannels": false,
                "filterMovies": "",
                "filterTelevision": "",
                "filterMusic": "",
            },
        });
        let rb = self
            .req(reqwest::Method::POST, "/api/v2/shared_servers")
            .json(&body);
        let v = Self::json(rb, "Plex invite").await?;
        Ok(parse_share(&v, invitee))
    }

    /// Change which sections an existing share includes.
    pub async fn update_share(
        &self,
        machine_id: &str,
        share_id: &str,
        section_ids: &[u64],
    ) -> Result<(), String> {
        let body = json!({
            "server_id": machine_id,
            "shared_server": { "library_section_ids": section_ids },
        });
        let rb = self
            .req(
                reqwest::Method::PUT,
                &format!("/api/servers/{machine_id}/shared_servers/{share_id}"),
            )
            .json(&body);
        Self::send(rb, "Plex share").await.map(drop)
    }

    /// Whether a share (pending or accepted) still exists.
    pub async fn share_exists(&self, share_id: &str) -> Result<bool, String> {
        let rb = self.req(
            reqwest::Method::GET,
            &format!("/api/v2/shared_servers/{share_id}"),
        );
        match Self::send(rb, "Plex share").await {
            Ok(_) => Ok(true),
            Err(e) if e.ends_with("404 Not Found") => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub async fn unshare(&self, share_id: &str) -> Result<(), String> {
        let rb = self.req(
            reqwest::Method::DELETE,
            &format!("/api/v2/shared_servers/{share_id}"),
        );
        match Self::send(rb, "Plex share").await {
            Ok(_) => Ok(()),
            // Already gone (the friend left, or the host removed it in Plex).
            Err(e) if e.ends_with("404 Not Found") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use std::sync::{Arc, Mutex};

    #[test]
    fn section_ids_come_from_the_server_listing() {
        let xml = r#"<?xml version="1.0"?>
            <MediaContainer machineIdentifier="m1">
              <Server name="Home" machineIdentifier="m1">
                <Section id="901" key="1" type="movie" title="Movies"/>
                <Section id="902" key="2" type="show" title="Shows"/>
                <Section key="3" title="Broken"/>
              </Server>
            </MediaContainer>"#;
        let ids = parse_section_ids(xml).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(ids["1"], 901);
        assert_eq!(ids["2"], 902);
    }

    #[test]
    fn shares_fall_back_to_the_invitee() {
        let s = parse_share(&json!({ "id": 55, "invitedId": 7 }), "alice");
        assert_eq!(
            s,
            Share {
                share_id: "55".into(),
                user_id: "7".into(),
                username: "alice".into()
            }
        );
        let s = parse_share(
            &json!({ "id": "56", "invited": { "id": 8, "username": "Bob" } }),
            "bob@example.com",
        );
        assert_eq!((s.user_id.as_str(), s.username.as_str()), ("8", "Bob"));
    }

    #[tokio::test]
    async fn linking_and_sharing() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let s2 = seen.clone();
        let base = serve(Arc::new(move |req: Request| {
            assert_eq!(
                req.headers.get("x-plex-product").map(String::as_str),
                Some(PRODUCT)
            );
            let reply = match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/api/v2/pins") => {
                    (201, r#"{"id":42,"code":"ABCD","expiresIn":900}"#.into())
                }
                ("GET", "/api/v2/pins/42") => {
                    (200, r#"{"id":42,"code":"ABCD","authToken":"acct"}"#.into())
                }
                ("GET", "/api/v2/user") => (200, r#"{"id":1,"uuid":"u","username":"host"}"#.into()),
                ("POST", "/api/v2/shared_servers") => (
                    200,
                    r#"{"id":77,"invited":{"id":5,"username":"alice"}}"#.into(),
                ),
                ("PUT", "/api/servers/m1/shared_servers/77") => (200, String::new()),
                ("DELETE", "/api/v2/shared_servers/77") => (204, String::new()),
                ("DELETE", "/api/v2/shared_servers/78") => (404, String::new()),
                ("DELETE", "/api/v2/shared_servers/79") => {
                    (400, r#"{"errors":[{"code":1,"message":"Nope"}]}"#.into())
                }
                _ => (404, String::new()),
            };
            s2.lock().unwrap().push(req);
            reply
        }))
        .await;

        let anon = PlexTv::with_base(&base, "cid", None);
        let pin = anon.create_pin().await.unwrap();
        assert_eq!((pin.id, pin.code.as_str()), (42, "ABCD"));
        assert_eq!(anon.check_pin(42).await.unwrap().as_deref(), Some("acct"));

        let tv = PlexTv::with_base(&base, "cid", Some("acct".into()));
        assert_eq!(tv.account().await.unwrap().username, "host");
        let share = tv.invite("m1", &[901, 902], "alice").await.unwrap();
        assert_eq!(share.share_id, "77");
        assert_eq!(share.user_id, "5");
        tv.update_share("m1", "77", &[901]).await.unwrap();
        tv.unshare("77").await.unwrap();
        tv.unshare("78").await.unwrap();
        assert_eq!(tv.unshare("79").await.unwrap_err(), "Plex share: Nope");

        let seen = seen.lock().unwrap();
        let invite = seen
            .iter()
            .find(|r| r.path == "/api/v2/shared_servers")
            .unwrap();
        let body: Value = serde_json::from_str(&invite.body).unwrap();
        assert_eq!(body["machineIdentifier"], "m1");
        assert_eq!(body["librarySectionIds"], json!([901, 902]));
        assert_eq!(body["invitedEmail"], "alice");
        assert_eq!(
            invite.headers.get("x-plex-token").map(String::as_str),
            Some("acct")
        );
    }
}
