//! Direct management of the Jellyfin SQLite database: the server owns
//! Jellyfin and ensures an administrator API key so it can always
//! automatically sign in, even if no credentials exist in the OS keyring.

use rand::Rng;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

#[derive(Debug, Clone, PartialEq)]
pub struct AdminSession {
    pub user_id: String,
    pub user_name: String,
    pub token: String,
}

pub fn db_path(jellyfin_root: &Path) -> PathBuf {
    jellyfin_root.join("data").join("data").join("jellyfin.db")
}

/// Look in the Jellyfin database for an existing administrator user, and
/// ensure an active API key exists for YarmiplayServerTV. Returns `Ok(Some(...))`
/// if an administrator user is found and an API key is provisioned, or
/// `Ok(None)` if the database does not exist yet or has no users.
pub fn ensure_admin_session(jellyfin_root: &Path) -> Result<Option<AdminSession>, String> {
    get_or_create_admin_session(jellyfin_root, false)
}

/// Force generation of a fresh administrator API key for YarmiplayServerTV in the
/// Jellyfin database, replacing any existing one (useful after a 401 error).
pub fn reprovision_admin_session(jellyfin_root: &Path) -> Result<Option<AdminSession>, String> {
    get_or_create_admin_session(jellyfin_root, true)
}

fn get_or_create_admin_session(
    jellyfin_root: &Path,
    force_reprovision: bool,
) -> Result<Option<AdminSession>, String> {
    let path = db_path(jellyfin_root);
    if !path.exists() {
        return Ok(None);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .map_err(|e| format!("open jellyfin.db: {e}"))?;

    conn.busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|e| format!("set busy timeout on jellyfin.db: {e}"))?;

    // Find the administrator user (Kind=0 is Administrator permission in Jellyfin)
    let admin_row: Option<(String, String)> = conn
        .query_row(
            "SELECT u.Id, u.Username FROM Users u \
             JOIN Permissions p ON u.Id = p.UserId \
             WHERE p.Kind = 0 AND p.Value = 1 \
             LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
        .or_else(|| {
            conn.query_row(
                "SELECT Id, Username FROM Users LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        });

    let Some((raw_id, user_name)) = admin_row else {
        return Ok(None);
    };

    let user_id = raw_id.replace('-', "").to_lowercase();

    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS ApiKeys (
            Id INTEGER PRIMARY KEY AUTOINCREMENT,
            DateCreated TEXT,
            DateLastActivity TEXT,
            Name TEXT,
            AccessToken TEXT
        );",
    );

    if !force_reprovision {
        let existing_api_key: Option<String> = conn
            .query_row(
                "SELECT AccessToken FROM ApiKeys WHERE (Name = 'YarmiplayServerTV' OR Name = 'NoireServerTV') AND AccessToken != '' LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok();

        if let Some(token) = existing_api_key {
            debug!(user = %user_name, "found existing API key in Jellyfin database");
            return Ok(Some(AdminSession {
                user_id,
                user_name,
                token,
            }));
        }
    }

    // Delete any old key if re-provisioning or updating
    let _ = conn.execute("DELETE FROM ApiKeys WHERE Name = 'YarmiplayServerTV' OR Name = 'NoireServerTV'", []);

    // Generate a 32-hex-character API key
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill(&mut bytes);
    let token = hex::encode(bytes);

    conn.execute(
        "INSERT INTO ApiKeys (DateCreated, DateLastActivity, Name, AccessToken) \
         VALUES (datetime('now'), datetime('now'), 'YarmiplayServerTV', ?1)",
        [&token],
    )
    .map_err(|e| format!("insert ApiKey: {e}"))?;

    info!(user = %user_name, "provisioned YarmiplayServerTV administrator key in Jellyfin database");
    Ok(Some(AdminSession {
        user_id,
        user_name,
        token,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_ensure_admin_session() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert_eq!(ensure_admin_session(root).unwrap(), None);

        let data_dir = root.join("data").join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        let db = data_dir.join("jellyfin.db");

        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE Users (Id TEXT PRIMARY KEY, Username TEXT);
             CREATE TABLE Permissions (Id INTEGER PRIMARY KEY, Kind INTEGER, Value INTEGER, UserId TEXT);
             INSERT INTO Users VALUES ('admin-uuid-1234', 'Admin');
             INSERT INTO Permissions VALUES (1, 0, 1, 'admin-uuid-1234');",
        )
        .unwrap();
        drop(conn);

        let session = ensure_admin_session(root).unwrap().unwrap();
        assert_eq!(session.user_name, "Admin");
        assert_eq!(session.user_id, "adminuuid1234");
        assert_eq!(session.token.len(), 32);

        // Idempotent: repeated call returns same token
        let session2 = ensure_admin_session(root).unwrap().unwrap();
        assert_eq!(session2, session);

        // Reprovision produces a fresh token
        let session3 = reprovision_admin_session(root).unwrap().unwrap();
        assert_eq!(session3.user_name, "Admin");
        assert_eq!(session3.user_id, "adminuuid1234");
        assert_ne!(session3.token, session.token);

        // Idempotent after reprovision
        let session4 = ensure_admin_session(root).unwrap().unwrap();
        assert_eq!(session4, session3);
    }
}
