//! The small part of the Jellyfin REST API the control panel needs: the
//! first-run startup endpoints, sign-in, libraries and Quick Connect.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

#[derive(Clone)]
pub struct JellyfinApi {
    base: String,
    client: reqwest::Client,
    device_id: String,
    token: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct PublicInfo {
    #[serde(rename = "ServerName", default)]
    pub server_name: String,
    #[serde(rename = "Version", default)]
    pub version: String,
    #[serde(rename = "Id", default)]
    pub id: String,
    #[serde(rename = "StartupWizardCompleted", default)]
    pub startup_wizard_completed: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Library {
    pub name: String,
    pub collection_type: Option<String>,
    pub locations: Vec<String>,
    pub item_id: Option<String>,
}

pub struct Session {
    pub token: String,
    pub user_id: String,
    pub user_name: String,
}

fn quote(v: &str) -> String {
    v.replace('"', "")
}

impl JellyfinApi {
    pub fn new(port: u16, device_id: &str, token: Option<String>) -> Self {
        let client = crate::net::client_builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self {
            base: format!("http://127.0.0.1:{port}"),
            client,
            device_id: device_id.to_string(),
            token,
        }
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    #[cfg(test)]
    pub fn with_base(base: &str, token: Option<String>) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            client: crate::net::client_builder().build().unwrap(),
            device_id: "test".into(),
            token,
        }
    }

    fn auth_header(&self) -> String {
        let device = quote(&hostname());
        let mut h = format!(
            "MediaBrowser Client=\"YarmiplayServerTV\", Device=\"{device}\", DeviceId=\"{}\", Version=\"{}\"",
            quote(&self.device_id),
            env!("CARGO_PKG_VERSION")
        );
        if let Some(t) = &self.token {
            h.push_str(&format!(", Token=\"{}\"", quote(t)));
        }
        h
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.base))
            .header("Authorization", self.auth_header())
    }

    async fn send(rb: reqwest::RequestBuilder, what: &str) -> Result<reqwest::Response, String> {
        let resp = rb
            .send()
            .await
            .map_err(|e| format!("{what}: Jellyfin is not reachable ({e})"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let detail = body.trim();
        let detail = if detail.is_empty() || detail.len() > 300 {
            status.to_string()
        } else {
            format!("{status}: {detail}")
        };
        Err(match status.as_u16() {
            401 => format!("{what}: not signed in to Jellyfin (401)"),
            403 => format!("{what}: not allowed (403)"),
            _ => format!("{what}: {detail}"),
        })
    }

    pub async fn public_info(&self) -> Result<PublicInfo, String> {
        let r = Self::send(
            self.req(reqwest::Method::GET, "/System/Info/Public"),
            "server info",
        )
        .await?;
        r.json().await.map_err(|e| e.to_string())
    }

    /// The startup endpoints only work until the wizard has been completed.
    pub async fn run_startup(
        &self,
        server_name: &str,
        user: &str,
        password: &str,
    ) -> Result<(), String> {
        use reqwest::Method;
        Self::send(
            self.req(Method::POST, "/Startup/Configuration")
                .json(&json!({
                    "ServerName": server_name,
                    "UICulture": "en-US",
                    "MetadataCountryCode": "US",
                    "PreferredMetadataLanguage": "en",
                })),
            "startup configuration",
        )
        .await?;
        // Jellyfin creates the initial user lazily when it is first read.
        Self::send(self.req(Method::GET, "/Startup/User"), "startup user").await?;
        Self::send(
            self.req(Method::POST, "/Startup/User")
                .json(&json!({ "Name": user, "Password": password })),
            "create administrator",
        )
        .await?;
        Self::send(
            self.req(Method::POST, "/Startup/RemoteAccess")
                .json(&json!({ "EnableRemoteAccess": true, "EnableAutomaticPortMapping": false })),
            "remote access",
        )
        .await?;
        Self::send(self.req(Method::POST, "/Startup/Complete"), "finish setup").await?;
        Ok(())
    }

    pub async fn authenticate(&self, user: &str, password: &str) -> Result<Session, String> {
        let r = Self::send(
            self.req(reqwest::Method::POST, "/Users/AuthenticateByName")
                .json(&json!({ "Username": user, "Pw": password })),
            "sign in",
        )
        .await
        .map_err(|e| {
            if e.contains("401") {
                "Wrong Jellyfin username or password".to_string()
            } else {
                e
            }
        })?;
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        let token = v["AccessToken"]
            .as_str()
            .ok_or("sign in: no access token in reply")?
            .to_string();
        let user_id = v["User"]["Id"].as_str().unwrap_or_default().to_string();
        let user_name = v["User"]["Name"].as_str().unwrap_or(user).to_string();
        Ok(Session {
            token,
            user_id,
            user_name,
        })
    }

    pub async fn libraries(&self) -> Result<Vec<Library>, String> {
        let r = Self::send(
            self.req(reqwest::Method::GET, "/Library/VirtualFolders"),
            "libraries",
        )
        .await?;
        let v: Vec<Value> = r.json().await.map_err(|e| e.to_string())?;
        Ok(v.into_iter()
            .map(|f| Library {
                name: f["Name"].as_str().unwrap_or_default().to_string(),
                collection_type: f["CollectionType"].as_str().map(str::to_string),
                locations: f["Locations"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|l| l.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default(),
                item_id: f["ItemId"].as_str().map(str::to_string),
            })
            .collect())
    }

    pub async fn add_library(
        &self,
        name: &str,
        collection_type: &str,
        path: &str,
    ) -> Result<(), String> {
        let mut query = vec![
            ("name", name.to_string()),
            ("refreshLibrary", "true".into()),
            ("paths", path.to_string()),
        ];
        if !collection_type.is_empty() && collection_type != "mixed" {
            query.push(("collectionType", collection_type.to_string()));
        }
        Self::send(
            self.req(reqwest::Method::POST, "/Library/VirtualFolders")
                .query(&query)
                .json(&json!({ "LibraryOptions": {} })),
            "add library",
        )
        .await
        .map(|_| ())
    }

    /// Deletes the virtual folder only. Jellyfin does not remove the media files.
    pub async fn remove_library(&self, name: &str) -> Result<(), String> {
        Self::send(
            self.req(reqwest::Method::DELETE, "/Library/VirtualFolders")
                .query(&[("name", name), ("refreshLibrary", "true")]),
            "remove library",
        )
        .await
        .map(|_| ())
    }

    /// Drops virtual folders that have no media left. Returns the names removed.
    ///
    /// A library is empty when it has no folders, or every folder is missing or
    /// contains no media files. A folder that still has a file is kept even if
    /// Jellyfin has not indexed it yet. Nothing in those folders is deleted.
    pub async fn remove_empty_libraries(&self) -> Result<Vec<String>, String> {
        let libs = self.libraries().await?;
        let mut removed = Vec::new();
        for lib in libs {
            let name = lib.name.trim();
            if name.is_empty() || !library_locations_empty(&lib.locations) {
                continue;
            }
            self.remove_library(name).await?;
            removed.push(name.to_string());
        }
        Ok(removed)
    }

    pub async fn add_path(&self, library: &str, path: &str) -> Result<(), String> {
        Self::send(
            self.req(reqwest::Method::POST, "/Library/VirtualFolders/Paths")
                .query(&[("refreshLibrary", "true")])
                .json(&json!({ "Name": library, "PathInfo": { "Path": path } })),
            "add folder",
        )
        .await
        .map(|_| ())
    }

    pub async fn remove_path(&self, library: &str, path: &str) -> Result<(), String> {
        Self::send(
            self.req(reqwest::Method::DELETE, "/Library/VirtualFolders/Paths")
                .query(&[
                    ("name", library),
                    ("path", path),
                    ("refreshLibrary", "true"),
                ]),
            "remove folder",
        )
        .await
        .map(|_| ())
    }

    pub async fn rescan(&self) -> Result<(), String> {
        Self::send(
            self.req(reqwest::Method::POST, "/Library/Refresh"),
            "rescan",
        )
        .await
        .map(|_| ())
    }

    pub async fn enable_quick_connect(&self) -> Result<(), String> {
        let r = Self::send(
            self.req(reqwest::Method::GET, "/System/Configuration"),
            "server configuration",
        )
        .await?;
        let mut cfg: Value = r.json().await.map_err(|e| e.to_string())?;
        if cfg["QuickConnectAvailable"].as_bool() == Some(true) {
            return Ok(());
        }
        cfg["QuickConnectAvailable"] = Value::Bool(true);
        Self::send(
            self.req(reqwest::Method::POST, "/System/Configuration")
                .json(&cfg),
            "enable Quick Connect",
        )
        .await
        .map(|_| ())
    }

    /// Every user, hidden and disabled ones included: `(id, name)`.
    pub async fn users(&self) -> Result<Vec<(String, String)>, String> {
        let r = Self::send(self.req(reqwest::Method::GET, "/Users"), "users").await?;
        let v: Vec<Value> = r.json().await.map_err(|e| e.to_string())?;
        Ok(v.into_iter()
            .filter_map(|u| {
                Some((
                    u["Id"].as_str()?.to_string(),
                    u["Name"].as_str().unwrap_or_default().to_string(),
                ))
            })
            .collect())
    }

    /// Returns the new user's id.
    pub async fn create_user(&self, name: &str, password: &str) -> Result<String, String> {
        let r = Self::send(
            self.req(reqwest::Method::POST, "/Users/New")
                .json(&json!({ "Name": name, "Password": password })),
            "create user",
        )
        .await?;
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        v["Id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| "create user: no id in reply".into())
    }

    pub async fn user_policy(&self, user_id: &str) -> Result<Value, String> {
        let r = Self::send(
            self.req(reqwest::Method::GET, &format!("/Users/{user_id}")),
            "user",
        )
        .await?;
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        Ok(v["Policy"].clone())
    }

    pub async fn set_policy(&self, user_id: &str, policy: &Value) -> Result<(), String> {
        Self::send(
            self.req(reqwest::Method::POST, &format!("/Users/{user_id}/Policy"))
                .json(policy),
            "user policy",
        )
        .await
        .map(|_| ())
    }

    /// Approve a Quick Connect code on behalf of `user_id` (needs an admin token).
    pub async fn quick_connect_authorize(&self, code: &str, user_id: &str) -> Result<(), String> {
        let r = Self::send(
            self.req(reqwest::Method::POST, "/QuickConnect/Authorize")
                .query(&[("code", code), ("userId", user_id)]),
            "Quick Connect",
        )
        .await
        .map_err(|e| {
            if e.contains("404") || e.contains("Not Found") {
                "Unknown or expired Quick Connect code".to_string()
            } else {
                e
            }
        })?;
        let ok: Value = r.json().await.unwrap_or(Value::Bool(true));
        if ok.as_bool() == Some(false) {
            return Err("Jellyfin refused the Quick Connect code".into());
        }
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        Self::send(
            self.req(reqwest::Method::POST, "/System/Shutdown"),
            "shutdown",
        )
        .await
        .map(|_| ())
    }
}

/// Operating-system bookkeeping, not user media. A folder of only these is empty.
const NON_MEDIA_FILES: &[&str] = &[".DS_Store", "Thumbs.db", "desktop.ini", ".directory"];

/// True when there is nothing left to scan: no locations, or every location is
/// missing or a directory tree with no media files.
///
/// A path that cannot be read is kept. A disconnected drive often isn't
/// `NotFound`, and a library must not be dropped when we can't see inside it.
pub fn library_locations_empty(locations: &[String]) -> bool {
    locations.iter().all(|location| {
        let location = location.trim();
        location.is_empty() || location_gone_or_empty(Path::new(location))
    })
}

fn location_gone_or_empty(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
        Ok(meta) if meta.is_dir() => directory_has_no_media(path),
        Ok(_) => false,
    }
}

/// True when `path` contains no media. Read errors count as "has media".
fn directory_has_no_media(path: &Path) -> bool {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => return false,
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => return false,
        };
        if file_type.is_dir() {
            if !directory_has_no_media(&entry.path()) {
                return false;
            }
            continue;
        }
        let name = entry.file_name();
        let name = name.to_str().unwrap_or("");
        if NON_MEDIA_FILES
            .iter()
            .any(|ignored| name.eq_ignore_ascii_case(ignored))
        {
            continue;
        }
        // A file or a symlink is content. Symlinks are not followed.
        return false;
    }
    true
}

pub(crate) fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "YarmiplayServerTV".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn startup_flow_hits_endpoints_in_order() {
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let seen2 = seen.clone();
        let base = serve(Arc::new(move |req: Request| {
            seen2
                .lock()
                .unwrap()
                .push(format!("{} {}", req.method, req.path));
            assert!(req
                .headers
                .get("authorization")
                .is_some_and(|h| h.contains("Client=\"YarmiplayServerTV\"")));
            (200, "{}".into())
        }))
        .await;
        let api = JellyfinApi::with_base(&base, None);
        api.run_startup("Movie night", "admin", "pw").await.unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                "POST /Startup/Configuration",
                "GET /Startup/User",
                "POST /Startup/User",
                "POST /Startup/RemoteAccess",
                "POST /Startup/Complete"
            ]
        );
    }

    #[tokio::test]
    async fn authenticate_and_list_libraries() {
        let base = serve(Arc::new(|req: Request| match req.path.as_str() {
            "/Users/AuthenticateByName" => {
                let body: Value = serde_json::from_str(&req.body).unwrap();
                if body["Pw"] == "good" {
                    (200, r#"{"AccessToken":"tok123","User":{"Id":"u1","Name":"admin"}}"#.into())
                } else {
                    (401, String::new())
                }
            }
            "/Library/VirtualFolders" => {
                assert!(req.headers.get("authorization").unwrap().contains("Token=\"tok123\""));
                (200, r#"[{"Name":"Movies","CollectionType":"movies","Locations":["D:\\Movies"],"ItemId":"x"}]"#.into())
            }
            _ => (404, String::new()),
        }))
        .await;
        let api = JellyfinApi::with_base(&base, None);
        assert!(api
            .authenticate("admin", "bad")
            .await
            .err()
            .unwrap()
            .contains("Wrong"));
        let s = api.authenticate("admin", "good").await.unwrap();
        assert_eq!(s.token, "tok123");
        let api = JellyfinApi::with_base(&base, Some(s.token));
        let libs = api.libraries().await.unwrap();
        assert_eq!(libs[0].name, "Movies");
        assert_eq!(libs[0].locations, vec!["D:\\Movies"]);
    }

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn empty_locations_are_libraries_with_nothing_to_scan() {
        let root = tempfile::tempdir().unwrap();
        let empty = root.path().join("empty");
        let nested = empty.join("Season 01");
        std::fs::create_dir_all(&nested).unwrap();
        touch(&nested, ".DS_Store");
        touch(&empty, "Thumbs.db");

        let with_media = root.path().join("movies");
        std::fs::create_dir_all(with_media.join("extra")).unwrap();
        touch(&with_media.join("extra"), "Movie.mkv");
        let video = with_media.join("extra").join("Movie.mkv");

        let gone = root.path().join("does-not-exist");
        let file = root.path().join("not-a-folder.mkv");
        touch(root.path(), "not-a-folder.mkv");

        assert!(library_locations_empty(&[]));
        assert!(library_locations_empty(&[String::new(), "  ".into()]));
        assert!(library_locations_empty(&[empty.display().to_string()]));
        assert!(library_locations_empty(&[gone.display().to_string()]));
        assert!(!library_locations_empty(&[with_media
            .display()
            .to_string()]));
        assert!(!library_locations_empty(&[
            gone.display().to_string(),
            with_media.display().to_string(),
        ]));
        assert!(!library_locations_empty(&[file.display().to_string()]));
        assert!(video.is_file());
        assert!(nested.join(".DS_Store").is_file());

        #[cfg(unix)]
        {
            let link_dir = root.path().join("link");
            std::fs::create_dir(&link_dir).unwrap();
            std::os::unix::fs::symlink(&video, link_dir.join("link.mkv")).unwrap();
            assert!(!library_locations_empty(&[link_dir.display().to_string()]));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_directory_is_kept() {
        if unsafe { libc::geteuid() } == 0 {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("locked");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0)).unwrap();
        let empty = library_locations_empty(&[dir.display().to_string()]);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!empty, "a directory we cannot read is not empty");
    }

    #[tokio::test]
    async fn remove_empty_libraries_only_deletes_the_library_entry() {
        let root = tempfile::tempdir().unwrap();
        let empty = root.path().join("empty");
        std::fs::create_dir_all(empty.join("Season 02")).unwrap();
        touch(&empty.join("Season 02"), "desktop.ini");
        let movies = root.path().join("movies");
        std::fs::create_dir_all(&movies).unwrap();
        let video = movies.join("Movie.mkv");
        touch(&movies, "Movie.mkv");
        let gone = root.path().join("gone");

        let body = serde_json::json!([
            {"Name":"Empty","CollectionType":"tvshows","Locations":[empty],"ItemId":"a"},
            {"Name":"Movies","CollectionType":"movies","Locations":[movies, gone],"ItemId":"b"},
            {"Name":"Missing","CollectionType":"music","Locations":[gone],"ItemId":"c"},
            {"Name":"NoFolders","Locations":[],"ItemId":"d"},
            {"Name":"  ","Locations":[gone],"ItemId":"e"},
        ])
        .to_string();

        let seen: Arc<Mutex<Vec<Request>>> = Arc::default();
        let seen2 = seen.clone();
        let base = serve(Arc::new(move |req: Request| {
            let reply = match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/Library/VirtualFolders") => (200, body.clone()),
                ("DELETE", "/Library/VirtualFolders") => (200, String::new()),
                _ => (500, format!("unexpected {} {}", req.method, req.path)),
            };
            seen2.lock().unwrap().push(req);
            reply
        }))
        .await;

        let api = JellyfinApi::with_base(&base, Some("tok".into()));
        let removed = api.remove_empty_libraries().await.unwrap();
        assert_eq!(removed, vec!["Empty", "Missing", "NoFolders"]);

        let seen = seen.lock().unwrap();
        let deletes: Vec<&Request> = seen.iter().filter(|r| r.method == "DELETE").collect();
        assert_eq!(deletes.len(), 3);
        assert!(deletes.iter().all(|r| r.path == "/Library/VirtualFolders"));
        assert!(deletes
            .iter()
            .all(|r| r.query.get("refreshLibrary").map(String::as_str) == Some("true")));
        assert_eq!(
            deletes
                .iter()
                .filter_map(|r| r.query.get("name").map(String::as_str))
                .collect::<Vec<_>>(),
            vec!["Empty", "Missing", "NoFolders"]
        );
        assert!(video.is_file(), "media files stay on disk");
        assert!(empty.join("Season 02").join("desktop.ini").is_file());
        assert!(movies.is_dir());
    }
}
