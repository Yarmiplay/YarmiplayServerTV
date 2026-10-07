//! Sharing this Jellyfin with the people on the Syncplay server. They sign in
//! with Quick Connect, approved by this app on behalf of one hidden, non-admin
//! guest account; turning sharing off disables that account, which signs
//! every guest out.

use super::api::JellyfinApi;
use rand::Rng;
use serde_json::{json, Value};

pub const GUEST_NAME: &str = "Syncplay guests";

fn random_password() -> String {
    let bytes: [u8; 24] = rand::thread_rng().gen();
    hex::encode(bytes)
}

/// Guest policy: hidden from the sign-in screen, never an administrator,
/// can't delete or manage anything, usable from outside the LAN.
pub fn apply_guest_policy(policy: &mut Value, enabled: bool) {
    if !policy.is_object() {
        *policy = json!({});
    }
    for (key, value) in [
        ("IsAdministrator", false),
        ("IsHidden", true),
        ("IsDisabled", !enabled),
        ("EnableRemoteAccess", true),
        ("EnableContentDeletion", false),
        ("EnableRemoteControlOfOtherUsers", false),
        ("EnableSharedDeviceControl", false),
        ("EnableLiveTvManagement", false),
        ("EnableSubtitleManagement", false),
        ("EnableLyricManagement", false),
        ("EnableCollectionManagement", false),
        ("EnableUserPreferenceAccess", false),
    ] {
        policy[key] = Value::Bool(value);
    }
    policy["EnableContentDeletionFromFolders"] = json!([]);
}

/// Find (by id, then by name) or create the guest account and set its policy.
/// Never touches the administrator. Returns the guest's user id, or None when
/// sharing is off and there is no guest account to disable.
pub async fn ensure_guest(
    api: &JellyfinApi,
    known_id: Option<&str>,
    admin_id: Option<&str>,
    enabled: bool,
) -> Result<Option<String>, String> {
    let users = api.users().await?;
    let not_admin = |id: &String| Some(id.as_str()) != admin_id;
    let existing = known_id
        .and_then(|k| users.iter().find(|(id, _)| id == k))
        .or_else(|| users.iter().find(|(_, name)| name == GUEST_NAME))
        .map(|(id, _)| id.clone())
        .filter(not_admin);
    let id = match existing {
        Some(id) => id,
        None => {
            if !enabled {
                return Ok(None);
            }
            api.create_user(GUEST_NAME, &random_password()).await?
        }
    };
    set_enabled(api, &id, enabled).await?;
    Ok(Some(id))
}

pub async fn set_enabled(api: &JellyfinApi, user_id: &str, enabled: bool) -> Result<(), String> {
    let mut policy = api.user_policy(user_id).await?;
    apply_guest_policy(&mut policy, enabled);
    api.set_policy(user_id, &policy).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use std::sync::{Arc, Mutex};

    #[test]
    fn policy_keeps_unknown_fields() {
        let mut p = json!({ "AuthenticationProviderId": "x", "IsAdministrator": true });
        apply_guest_policy(&mut p, false);
        assert_eq!(p["AuthenticationProviderId"], "x");
        assert_eq!(p["IsAdministrator"], false);
        assert_eq!(p["IsHidden"], true);
        assert_eq!(p["IsDisabled"], true);
        apply_guest_policy(&mut p, true);
        assert_eq!(p["IsDisabled"], false);
    }

    #[tokio::test]
    async fn guest_is_created_once_then_reused() {
        let created: Arc<Mutex<Vec<String>>> = Arc::default();
        let policies: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (c2, p2) = (created.clone(), policies.clone());
        let base = serve(Arc::new(move |req: Request| {
            match (req.method.as_str(), req.path.as_str()) {
                ("GET", "/Users") => {
                    let mut users = vec![json!({ "Id": "admin", "Name": "me" })];
                    if !c2.lock().unwrap().is_empty() {
                        users.push(json!({ "Id": "g1", "Name": GUEST_NAME }));
                    }
                    (200, Value::Array(users).to_string())
                }
                ("POST", "/Users/New") => {
                    let body: Value = serde_json::from_str(&req.body).unwrap();
                    assert_eq!(body["Name"], GUEST_NAME);
                    assert!(body["Password"].as_str().unwrap().len() >= 32);
                    c2.lock().unwrap().push("g1".into());
                    (200, r#"{"Id":"g1"}"#.into())
                }
                ("GET", "/Users/g1") => {
                    (200, r#"{"Policy":{"AuthenticationProviderId":"p"}}"#.into())
                }
                ("POST", "/Users/g1/Policy") => {
                    p2.lock()
                        .unwrap()
                        .push(serde_json::from_str(&req.body).unwrap());
                    (204, String::new())
                }
                _ => (404, String::new()),
            }
        }))
        .await;
        let api = JellyfinApi::with_base(&base, Some("t".into()));
        assert_eq!(
            ensure_guest(&api, None, Some("admin"), false)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            ensure_guest(&api, None, Some("admin"), true)
                .await
                .unwrap()
                .as_deref(),
            Some("g1")
        );
        assert_eq!(
            ensure_guest(&api, Some("g1"), Some("admin"), false)
                .await
                .unwrap()
                .as_deref(),
            Some("g1")
        );
        assert_eq!(created.lock().unwrap().len(), 1);
        let p = policies.lock().unwrap();
        assert_eq!(p[0]["IsDisabled"], false);
        assert_eq!(p[1]["IsDisabled"], true);
        assert_eq!(p[1]["AuthenticationProviderId"], "p");
    }

    #[tokio::test]
    async fn quick_connect_is_authorized_for_the_guest() {
        let base = serve(Arc::new(|req: Request| {
            if req.path == "/QuickConnect/Authorize"
                && req.query.get("code").map(String::as_str) == Some("123456")
                && req.query.get("userId").map(String::as_str) == Some("g1")
            {
                (200, "true".into())
            } else {
                (404, String::new())
            }
        }))
        .await;
        let api = JellyfinApi::with_base(&base, Some("t".into()));
        api.quick_connect_authorize("123456", "g1").await.unwrap();
        assert!(api
            .quick_connect_authorize("000000", "nobody")
            .await
            .is_err());
    }
}
