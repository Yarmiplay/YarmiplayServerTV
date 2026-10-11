//! Sharing this Plex with the people on the Syncplay server. A YarmiplayTV
//! user asks with their plex.tv username; the app invites them as a Plex
//! friend to the libraries the host picked, and they accept with their own
//! account, so the host never sees their token. Only invites this app made
//! are tracked and removed; the host's own Plex friends are never touched.

use super::tv::PlexTv;
use crate::config::PlexInvite;
use std::collections::BTreeMap;

/// plex.tv section ids for the picked libraries (None picks every library).
pub fn section_ids(picked: Option<&[String]>, ids: &BTreeMap<String, u64>) -> Vec<u64> {
    match picked {
        None => ids.values().copied().collect(),
        Some(keys) => keys.iter().filter_map(|k| ids.get(k).copied()).collect(),
    }
}

/// Plex's `customConnections` with the relay URLs added in front of whatever
/// the host had set themselves.
pub fn with_relay(original: &str, relay: &[String]) -> String {
    let mut all: Vec<&str> = relay.iter().map(String::as_str).collect();
    for url in original.split(',').map(str::trim).filter(|u| !u.is_empty()) {
        if !all.contains(&url) {
            all.push(url);
        }
    }
    all.join(",")
}

/// Invite `username`, or return the existing invite for them.
pub async fn invite(
    tv: &PlexTv,
    machine_id: &str,
    sections: &[u64],
    username: &str,
    syncplay_name: &str,
    existing: &[PlexInvite],
) -> Result<PlexInvite, String> {
    if let Some(known) = existing
        .iter()
        .find(|i| i.username.eq_ignore_ascii_case(username))
    {
        return Ok(known.clone());
    }
    if sections.is_empty() {
        return Err("No Plex libraries are shared with Syncplay users".into());
    }
    let share = tv.invite(machine_id, sections, username).await?;
    if share.share_id.is_empty() {
        return Err("plex.tv did not confirm the invite".into());
    }
    Ok(PlexInvite {
        plex_user_id: share.user_id,
        username: share.username,
        syncplay_name: syncplay_name.to_string(),
        share_id: share.share_id,
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    })
}

/// Remove every tracked share; returns the ones that couldn't be removed.
pub async fn unshare_all(tv: &PlexTv, invites: &[PlexInvite]) -> (Vec<PlexInvite>, Option<String>) {
    let mut kept = Vec::new();
    let mut error = None;
    for i in invites {
        if let Err(e) = tv.unshare(&i.share_id).await {
            error = Some(e);
            kept.push(i.clone());
        }
    }
    (kept, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::testutil::{serve, Request};
    use std::sync::{Arc, Mutex};

    #[test]
    fn picked_libraries_map_to_plex_tv_ids() {
        let ids = BTreeMap::from([("1".to_string(), 901), ("2".to_string(), 902)]);
        assert_eq!(section_ids(None, &ids), vec![901, 902]);
        let picked = vec!["2".to_string(), "9".to_string()];
        assert_eq!(section_ids(Some(&picked), &ids), vec![902]);
        assert!(section_ids(Some(&[]), &ids).is_empty());
    }

    #[test]
    fn relay_urls_go_first_and_the_hosts_stay() {
        let relay = vec!["https://me.duckdns.org:8999".to_string()];
        assert_eq!(with_relay("", &relay), "https://me.duckdns.org:8999");
        assert_eq!(
            with_relay("http://nas:32400, https://me.duckdns.org:8999", &relay),
            "https://me.duckdns.org:8999,http://nas:32400"
        );
    }

    #[tokio::test]
    async fn invites_are_made_once_and_removed() {
        let calls: Arc<Mutex<Vec<String>>> = Arc::default();
        let c2 = calls.clone();
        let base = serve(Arc::new(move |req: Request| {
            c2.lock()
                .unwrap()
                .push(format!("{} {}", req.method, req.path));
            match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/api/v2/shared_servers") => (
                    200,
                    r#"{"id":77,"invited":{"id":5,"username":"alice"}}"#.into(),
                ),
                ("DELETE", "/api/v2/shared_servers/77") => (204, String::new()),
                _ => (500, String::new()),
            }
        }))
        .await;
        let tv = PlexTv::with_base(&base, "cid", Some("acct".into()));

        assert!(invite(&tv, "m1", &[], "alice", "Alice", &[]).await.is_err());
        let made = invite(&tv, "m1", &[901], "alice", "Alice", &[])
            .await
            .unwrap();
        assert_eq!(made.share_id, "77");
        assert_eq!(made.syncplay_name, "Alice");
        let again = invite(
            &tv,
            "m1",
            &[901],
            "ALICE",
            "Alice",
            std::slice::from_ref(&made),
        )
        .await
        .unwrap();
        assert_eq!(again, made);

        let broken = PlexInvite {
            share_id: "99".into(),
            ..made.clone()
        };
        let (kept, error) = unshare_all(&tv, &[made, broken.clone()]).await;
        assert_eq!(kept, vec![broken]);
        assert!(error.is_some());
        assert_eq!(
            *calls.lock().unwrap(),
            vec![
                "POST /api/v2/shared_servers",
                "DELETE /api/v2/shared_servers/77",
                "DELETE /api/v2/shared_servers/99",
            ]
        );
    }
}
