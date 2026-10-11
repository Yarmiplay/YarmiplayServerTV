//! The small part of the local Plex Media Server API the control panel needs:
//! identity, libraries and one preference. Everything is plain HTTP to
//! 127.0.0.1, so it works the same for every way Plex can be installed
//! (including from inside the Snap, which can't read Plex's files).

use serde::Serialize;
use serde_json::Value;

#[derive(Clone)]
pub struct PlexApi {
    base: String,
    client: reqwest::Client,
    client_id: String,
    token: Option<String>,
}

/// `GET /identity`, answered without a token.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Identity {
    pub machine_id: String,
    pub version: String,
    /// Signed in to a plex.tv account (Plex's own first-run setup done).
    pub claimed: bool,
}

/// A library section, as the UI shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    /// The section key on this server (e.g. "1").
    pub key: String,
    pub name: String,
    /// movie | show | artist | photo
    pub kind: String,
    pub locations: Vec<String>,
    #[serde(skip)]
    agent: String,
    #[serde(skip)]
    scanner: String,
    #[serde(skip)]
    language: String,
}

/// What kind of content a new library holds, as offered in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryKind {
    Movies,
    Shows,
    Music,
    Photos,
    OtherVideos,
}

impl LibraryKind {
    pub fn parse(s: &str) -> Result<Self, String> {
        Ok(match s {
            "movies" => Self::Movies,
            "shows" => Self::Shows,
            "music" => Self::Music,
            "photos" => Self::Photos,
            "other" => Self::OtherVideos,
            _ => return Err(format!("Unknown kind of library: {s}")),
        })
    }

    /// Plex's section type, metadata agent, scanner and language for this
    /// kind, as Plex 1.42 accepts them (it answers 400 to other mixes).
    /// Libraries without online metadata use the "no language" code `xn`.
    fn plex(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::Movies => ("movie", "tv.plex.agents.movie", "Plex Movie", "en-US"),
            Self::Shows => ("show", "tv.plex.agents.series", "Plex TV Series", "en-US"),
            Self::Music => ("artist", "tv.plex.agents.music", "Plex Music", "en-US"),
            Self::Photos => (
                "photo",
                "com.plexapp.agents.none",
                "Plex Photo Scanner",
                "xn",
            ),
            Self::OtherVideos => ("movie", "tv.plex.agents.none", "Plex Video Files", "xn"),
        }
    }
}

fn container(v: &Value) -> &Value {
    v.get("MediaContainer").unwrap_or(&Value::Null)
}

fn text(v: &Value, key: &str) -> String {
    match v.get(key) {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => String::new(),
    }
}

fn parse_identity(v: &Value) -> Identity {
    let c = container(v);
    Identity {
        machine_id: text(c, "machineIdentifier"),
        version: text(c, "version"),
        claimed: match c.get("claimed") {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => s == "1" || s == "true",
            Some(Value::Number(n)) => n.as_u64() == Some(1),
            _ => false,
        },
    }
}

fn parse_libraries(v: &Value) -> Vec<Library> {
    container(v)
        .get("Directory")
        .and_then(Value::as_array)
        .map(|dirs| {
            dirs.iter()
                .map(|d| Library {
                    key: text(d, "key"),
                    name: text(d, "title"),
                    kind: text(d, "type"),
                    locations: d
                        .get("Location")
                        .and_then(Value::as_array)
                        .map(|l| l.iter().map(|l| text(l, "path")).collect())
                        .unwrap_or_default(),
                    agent: text(d, "agent"),
                    scanner: text(d, "scanner"),
                    language: text(d, "language"),
                })
                .filter(|l| !l.key.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

impl PlexApi {
    pub fn new(port: u16, client_id: &str, token: Option<String>) -> Self {
        Self::with_base(&format!("http://127.0.0.1:{port}"), client_id, token)
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
        let rb = super::tv::plex_headers(
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
            .map_err(|e| format!("{what}: Plex is not reachable ({e})"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        Err(match status.as_u16() {
            401 => format!("{what}: not signed in to Plex (401)"),
            403 => format!("{what}: not allowed (403)"),
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

    pub async fn identity(&self) -> Result<Identity, String> {
        let v = Self::json(self.req(reqwest::Method::GET, "/identity"), "server info").await?;
        Ok(parse_identity(&v))
    }

    /// The server's name as set in Plex.
    pub async fn friendly_name(&self) -> Result<String, String> {
        let v = Self::json(self.req(reqwest::Method::GET, "/"), "server name").await?;
        Ok(text(container(&v), "friendlyName"))
    }

    pub async fn libraries(&self) -> Result<Vec<Library>, String> {
        let v = Self::json(
            self.req(reqwest::Method::GET, "/library/sections"),
            "libraries",
        )
        .await?;
        Ok(parse_libraries(&v))
    }

    async fn library(&self, key: &str) -> Result<Library, String> {
        self.libraries()
            .await?
            .into_iter()
            .find(|l| l.key == key)
            .ok_or_else(|| "That library no longer exists".to_string())
    }

    pub async fn add_library(
        &self,
        name: &str,
        kind: LibraryKind,
        path: &str,
    ) -> Result<(), String> {
        let (section_type, agent, scanner, language) = kind.plex();
        let rb = self
            .req(reqwest::Method::POST, "/library/sections")
            .query(&[
                ("name", name),
                ("type", section_type),
                ("agent", agent),
                ("scanner", scanner),
                ("language", language),
                ("location", path),
            ]);
        Self::send(rb, "add library").await.map(drop)
    }

    pub async fn remove_library(&self, key: &str) -> Result<(), String> {
        let rb = self.req(
            reqwest::Method::DELETE,
            &format!("/library/sections/{}", urlencode(key)),
        );
        Self::send(rb, "remove library").await.map(drop)
    }

    /// Plex replaces a section's folders as a whole, so send the full list.
    async fn set_locations(&self, lib: &Library, locations: &[String]) -> Result<(), String> {
        let mut query: Vec<(&str, &str)> = vec![
            ("agent", &lib.agent),
            ("scanner", &lib.scanner),
            ("language", &lib.language),
        ];
        query.retain(|(_, v)| !v.is_empty());
        query.extend(locations.iter().map(|l| ("location", l.as_str())));
        let rb = self
            .req(
                reqwest::Method::PUT,
                &format!("/library/sections/{}", urlencode(&lib.key)),
            )
            .query(&query);
        Self::send(rb, "update library folders").await.map(drop)
    }

    pub async fn add_path(&self, key: &str, path: &str) -> Result<(), String> {
        let lib = self.library(key).await?;
        if lib.locations.iter().any(|l| l == path) {
            return Ok(());
        }
        let mut locations = lib.locations.clone();
        locations.push(path.to_string());
        self.set_locations(&lib, &locations).await
    }

    pub async fn remove_path(&self, key: &str, path: &str) -> Result<(), String> {
        let lib = self.library(key).await?;
        let locations: Vec<String> = lib
            .locations
            .iter()
            .filter(|l| *l != path)
            .cloned()
            .collect();
        if locations.is_empty() {
            return Err("A library needs at least one folder".into());
        }
        self.set_locations(&lib, &locations).await
    }

    pub async fn rescan(&self) -> Result<(), String> {
        let rb = self.req(reqwest::Method::GET, "/library/sections/all/refresh");
        Self::send(rb, "scan libraries").await.map(drop)
    }

    pub async fn pref(&self, name: &str) -> Result<Option<String>, String> {
        let v = Self::json(self.req(reqwest::Method::GET, "/:/prefs"), "Plex settings").await?;
        Ok(container(&v)
            .get("Setting")
            .and_then(Value::as_array)
            .and_then(|s| s.iter().find(|s| text(s, "id") == name))
            .map(|s| text(s, "value")))
    }

    pub async fn set_pref(&self, name: &str, value: &str) -> Result<(), String> {
        let rb = self
            .req(reqwest::Method::PUT, "/:/prefs")
            .query(&[(name, value)]);
        Self::send(rb, "Plex settings").await.map(drop)
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    #[test]
    fn identity_parses_every_claimed_spelling() {
        let id = parse_identity(&json!({ "MediaContainer": {
            "claimed": true, "machineIdentifier": "m1", "version": "1.41.0"
        } }));
        assert_eq!(
            id,
            Identity {
                machine_id: "m1".into(),
                version: "1.41.0".into(),
                claimed: true
            }
        );
        assert!(parse_identity(&json!({ "MediaContainer": { "claimed": "1" } })).claimed);
        assert!(!parse_identity(&json!({ "MediaContainer": { "claimed": "0" } })).claimed);
        assert!(!parse_identity(&json!({})).claimed);
    }

    fn sections_json() -> String {
        json!({ "MediaContainer": { "Directory": [
            { "key": "1", "title": "Movies", "type": "movie", "agent": "tv.plex.agents.movie",
              "scanner": "Plex Movie", "language": "en-US",
              "Location": [{ "id": 1, "path": "D:\\Movies" }, { "id": 2, "path": "E:\\More" }] },
            { "key": 2, "title": "Music", "type": "artist", "Location": [] },
        ] } })
        .to_string()
    }

    #[test]
    fn libraries_parse() {
        let libs = parse_libraries(&serde_json::from_str(&sections_json()).unwrap());
        assert_eq!(libs.len(), 2);
        assert_eq!(libs[0].name, "Movies");
        assert_eq!(libs[0].locations, vec!["D:\\Movies", "E:\\More"]);
        assert_eq!(libs[1].key, "2");
        assert!(libs[1].locations.is_empty());
    }

    #[tokio::test]
    async fn library_changes_send_what_plex_expects() {
        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let s2 = seen.clone();
        let base = serve(Arc::new(move |req: Request| {
            assert_eq!(
                req.headers.get("x-plex-token").map(String::as_str),
                Some("tok")
            );
            assert_eq!(
                req.headers
                    .get("x-plex-client-identifier")
                    .map(String::as_str),
                Some("cid")
            );
            let reply = match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/library/sections") => (200, sections_json()),
                _ => (200, String::new()),
            };
            s2.lock().unwrap().push(req);
            reply
        }))
        .await;
        let api = PlexApi::with_base(&base, "cid", Some("tok".into()));

        api.add_library("Home", LibraryKind::OtherVideos, "D:\\Home")
            .await
            .unwrap();
        api.add_library("Shows", LibraryKind::Shows, "D:\\TV")
            .await
            .unwrap();
        api.add_path("1", "F:\\New").await.unwrap();
        api.remove_path("1", "E:\\More").await.unwrap();
        assert_eq!(
            api.remove_path("2", "x").await.unwrap_err(),
            "A library needs at least one folder"
        );
        api.remove_library("1").await.unwrap();

        let seen = seen.lock().unwrap();
        let home = &seen[0];
        assert_eq!(home.query["agent"], "tv.plex.agents.none");
        assert_eq!(home.query["scanner"], "Plex Video Files");
        assert_eq!(home.query["language"], "xn");
        let add = &seen[1];
        assert_eq!(
            (add.method.as_str(), add.path.as_str()),
            ("POST", "/library/sections")
        );
        assert_eq!(add.query["type"], "show");
        assert_eq!(add.query["agent"], "tv.plex.agents.series");
        assert_eq!(add.query["scanner"], "Plex TV Series");
        assert_eq!(add.query["location"], "D:\\TV");
        assert_eq!(add.query["language"], "en-US");

        let puts: Vec<&Request> = seen.iter().filter(|r| r.method == "PUT").collect();
        assert_eq!(puts.len(), 2);
        assert_eq!(puts[0].path, "/library/sections/1");
        assert_eq!(puts[0].query["agent"], "tv.plex.agents.movie");
        let last = seen.last().unwrap();
        assert_eq!(
            (last.method.as_str(), last.path.as_str()),
            ("DELETE", "/library/sections/1")
        );
    }

    #[tokio::test]
    async fn unauthorized_is_reported_as_401() {
        let base = serve(Arc::new(|_req: Request| (401, String::new()))).await;
        let api = PlexApi::with_base(&base, "cid", Some("old".into()));
        assert!(api.libraries().await.unwrap_err().contains("(401)"));
    }
}
