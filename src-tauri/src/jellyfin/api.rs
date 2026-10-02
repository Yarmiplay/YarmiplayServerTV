//! The small part of the Jellyfin REST API the control panel needs: the
//! first-run startup endpoints, sign-in, libraries and Quick Connect.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

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
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .expect("reqwest client");
        Self { base: format!("http://127.0.0.1:{port}"), client, device_id: device_id.to_string(), token }
    }

    #[cfg(test)]
    pub fn with_base(base: &str, token: Option<String>) -> Self {
        Self { base: base.trim_end_matches('/').to_string(), client: reqwest::Client::new(), device_id: "test".into(), token }
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
        let resp = rb.send().await.map_err(|e| format!("{what}: Jellyfin is not reachable ({e})"))?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let body = resp.text().await.unwrap_or_default();
        let detail = body.trim();
        let detail = if detail.is_empty() || detail.len() > 300 { status.to_string() } else { format!("{status}: {detail}") };
        Err(match status.as_u16() {
            401 => format!("{what}: not signed in to Jellyfin (401)"),
            403 => format!("{what}: not allowed (403)"),
            _ => format!("{what}: {detail}"),
        })
    }

    pub async fn public_info(&self) -> Result<PublicInfo, String> {
        let r = Self::send(self.req(reqwest::Method::GET, "/System/Info/Public"), "server info").await?;
        r.json().await.map_err(|e| e.to_string())
    }

    /// The startup endpoints only work until the wizard has been completed.
    pub async fn run_startup(&self, server_name: &str, user: &str, password: &str) -> Result<(), String> {
        use reqwest::Method;
        Self::send(
            self.req(Method::POST, "/Startup/Configuration").json(&json!({
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
            self.req(Method::POST, "/Startup/User").json(&json!({ "Name": user, "Password": password })),
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
            self.req(reqwest::Method::POST, "/Users/AuthenticateByName").json(&json!({ "Username": user, "Pw": password })),
            "sign in",
        )
        .await
        .map_err(|e| if e.contains("401") { "Wrong Jellyfin username or password".to_string() } else { e })?;
        let v: Value = r.json().await.map_err(|e| e.to_string())?;
        let token = v["AccessToken"].as_str().ok_or("sign in: no access token in reply")?.to_string();
        let user_id = v["User"]["Id"].as_str().unwrap_or_default().to_string();
        let user_name = v["User"]["Name"].as_str().unwrap_or(user).to_string();
        Ok(Session { token, user_id, user_name })
    }

    pub async fn libraries(&self) -> Result<Vec<Library>, String> {
        let r = Self::send(self.req(reqwest::Method::GET, "/Library/VirtualFolders"), "libraries").await?;
        let v: Vec<Value> = r.json().await.map_err(|e| e.to_string())?;
        Ok(v.into_iter()
            .map(|f| Library {
                name: f["Name"].as_str().unwrap_or_default().to_string(),
                collection_type: f["CollectionType"].as_str().map(str::to_string),
                locations: f["Locations"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                item_id: f["ItemId"].as_str().map(str::to_string),
            })
            .collect())
    }

    pub async fn add_library(&self, name: &str, collection_type: &str, path: &str) -> Result<(), String> {
        let mut query = vec![("name", name.to_string()), ("refreshLibrary", "true".into()), ("paths", path.to_string())];
        if !collection_type.is_empty() && collection_type != "mixed" {
            query.push(("collectionType", collection_type.to_string()));
        }
        Self::send(
            self.req(reqwest::Method::POST, "/Library/VirtualFolders").query(&query).json(&json!({ "LibraryOptions": {} })),
            "add library",
        )
        .await
        .map(|_| ())
    }

    pub async fn remove_library(&self, name: &str) -> Result<(), String> {
        Self::send(
            self.req(reqwest::Method::DELETE, "/Library/VirtualFolders").query(&[("name", name), ("refreshLibrary", "true")]),
            "remove library",
        )
        .await
        .map(|_| ())
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
                .query(&[("name", library), ("path", path), ("refreshLibrary", "true")]),
            "remove folder",
        )
        .await
        .map(|_| ())
    }

    pub async fn rescan(&self) -> Result<(), String> {
        Self::send(self.req(reqwest::Method::POST, "/Library/Refresh"), "rescan").await.map(|_| ())
    }

    pub async fn enable_quick_connect(&self) -> Result<(), String> {
        let r = Self::send(self.req(reqwest::Method::GET, "/System/Configuration"), "server configuration").await?;
        let mut cfg: Value = r.json().await.map_err(|e| e.to_string())?;
        if cfg["QuickConnectAvailable"].as_bool() == Some(true) {
            return Ok(());
        }
        cfg["QuickConnectAvailable"] = Value::Bool(true);
        Self::send(self.req(reqwest::Method::POST, "/System/Configuration").json(&cfg), "enable Quick Connect")
            .await
            .map(|_| ())
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        Self::send(self.req(reqwest::Method::POST, "/System/Shutdown"), "shutdown").await.map(|_| ())
    }
}

fn hostname() -> String {
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
            seen2.lock().unwrap().push(format!("{} {}", req.method, req.path));
            assert!(req.headers.get("authorization").is_some_and(|h| h.contains("Client=\"YarmiplayServerTV\"")));
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
        assert!(api.authenticate("admin", "bad").await.err().unwrap().contains("Wrong"));
        let s = api.authenticate("admin", "good").await.unwrap();
        assert_eq!(s.token, "tok123");
        let api = JellyfinApi::with_base(&base, Some(s.token));
        let libs = api.libraries().await.unwrap();
        assert_eq!(libs[0].name, "Movies");
        assert_eq!(libs[0].locations, vec!["D:\\Movies"]);
    }
}
