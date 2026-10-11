//! Managed (controlled) rooms, byte-compatible with the official server's
//! `RoomPasswordProvider` (`syncplay/utils.py`): a room named
//! `+<name>:<HASH>` can only be controlled by users who prove the password
//! that, with the server's salt, produced `HASH`.

use rand::Rng;
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// `+` + `:` + the 12-character hash.
pub const CONTROLLED_SUFFIX_LENGTH: usize = 14;

/// Split `+<name>:<HASH>` into its name and hash (`^\+(.*):(\w{12})$`).
pub fn is_controlled(room: &str) -> Option<(&str, &str)> {
    let rest = room.strip_prefix('+')?;
    let (base, hash) = rest.rsplit_once(':')?;
    let word = |c: char| c.is_alphanumeric() || c == '_';
    (hash.chars().count() == 12 && hash.chars().all(word)).then_some((base, hash))
}

/// `re.match(r"[A-Z]{2}-\d{3}-\d{3}", password)`: only the start has to match.
pub fn password_valid(password: &str) -> bool {
    let b = password.as_bytes();
    b.len() >= 10
        && b[..2].iter().all(u8::is_ascii_uppercase)
        && b[2] == b'-'
        && b[3..6].iter().all(u8::is_ascii_digit)
        && b[6] == b'-'
        && b[7..10].iter().all(u8::is_ascii_digit)
}

pub fn room_hash(base: &str, password: &str, salt: &str) -> String {
    let salt = hex::encode(Sha256::digest(salt.as_bytes()));
    let provisional = hex::encode(Sha256::digest([base.as_bytes(), salt.as_bytes()].concat()));
    let full =
        Sha1::digest([provisional.as_bytes(), salt.as_bytes(), password.as_bytes()].concat());
    hex::encode(full)[..12].to_uppercase()
}

pub fn controlled_name(base: &str, password: &str, salt: &str) -> String {
    format!("+{base}:{}", room_hash(base, password, salt))
}

/// Whether `password` is the one `room` (a controlled room name) was made with.
pub fn check(room: &str, password: &str, salt: &str) -> bool {
    is_controlled(room).is_some_and(|(base, hash)| room_hash(base, password, salt) == hash)
}

/// A room password like `AB-123-456`.
pub fn generate_password() -> String {
    let mut rng = rand::thread_rng();
    let letters: String = (0..2).map(|_| rng.gen_range(b'A'..=b'Z') as char).collect();
    format!(
        "{letters}-{:03}-{:03}",
        rng.gen_range(0..1000),
        rng.gen_range(0..1000)
    )
}

/// Ten random uppercase letters, like the official server's default salt.
pub fn generate_salt() -> String {
    let mut rng = rand::thread_rng();
    (0..10)
        .map(|_| rng.gen_range(b'A'..=b'Z') as char)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_matches_the_official_server() {
        // Computed with syncplay/utils.py RoomPasswordProvider._computeRoomHash.
        assert_eq!(
            room_hash("movie night", "AB-123-456", "QWERTYUIOP"),
            "D6A85289CDB5"
        );
        assert_eq!(
            room_hash("élan:x", "ZZ-000-999", "SALTSALTSA"),
            "805B34B73CDD"
        );
        assert_eq!(
            controlled_name("movie night", "AB-123-456", "QWERTYUIOP"),
            "+movie night:D6A85289CDB5"
        );
    }

    #[test]
    fn controlled_names_are_parsed() {
        assert_eq!(
            is_controlled("+a:D6A85289CDB5"),
            Some(("a", "D6A85289CDB5"))
        );
        assert_eq!(
            is_controlled("+élan:x:805B34B73CDD"),
            Some(("élan:x", "805B34B73CDD"))
        );
        assert_eq!(is_controlled("+:D6A85289CDB5"), Some(("", "D6A85289CDB5")));
        assert_eq!(is_controlled("a:D6A85289CDB5"), None);
        assert_eq!(is_controlled("+a:D6A85289CDB"), None);
        assert_eq!(is_controlled("+a:D6A85289CDB5-"), None);
        assert_eq!(is_controlled("+a"), None);
        assert!(check("+élan:x:805B34B73CDD", "ZZ-000-999", "SALTSALTSA"));
        assert!(!check("+élan:x:805B34B73CDD", "ZZ-000-998", "SALTSALTSA"));
        assert!(!check("élan:x", "ZZ-000-999", "SALTSALTSA"));
    }

    #[test]
    fn passwords() {
        assert!(password_valid("AB-123-456"));
        assert!(password_valid("AB-123-4567"));
        assert!(!password_valid("ab-123-456"));
        assert!(!password_valid("AB-12-3456"));
        assert!(!password_valid("AB-123-45"));
        assert!(!password_valid(""));
        for _ in 0..50 {
            let p = generate_password();
            assert!(password_valid(&p) && p.len() == 10, "{p}");
            let s = generate_salt();
            assert!(
                s.len() == 10 && s.bytes().all(|b| b.is_ascii_uppercase()),
                "{s}"
            );
        }
    }
}
