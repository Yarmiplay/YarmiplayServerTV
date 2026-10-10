//! Syncplay wire constants and helpers (JSON object per line, `\r\n`).

use serde_json::Value;

/// Reported to clients as the server's `realversion`; clients gate features
/// (shared playlists >= 1.4.0, chat and feature lists >= 1.5.0) on it.
pub const SERVER_VERSION: &str = "1.7.6";
pub const MAX_ROOM_NAME_LENGTH: usize = 35;
/// Keeps the Hello reply well under [`MAX_LINE_LENGTH`].
pub const MAX_MOTD_LENGTH: usize = 2000;
pub const MAX_FILENAME_LENGTH: usize = 250;
pub const PLAYLIST_MAX_CHARACTERS: usize = 10_000;
pub const PLAYLIST_MAX_ITEMS: usize = 250;
pub const PROTOCOL_TIMEOUT: f64 = 12.5;
pub const MAX_LINE_LENGTH: usize = 64 * 1024;
const PING_MOVING_AVERAGE_WEIGHT: f64 = 0.85;

pub fn md5_hex(text: &str) -> String {
    use md5::{Digest, Md5};
    hex::encode(Md5::digest(text.as_bytes()))
}

/// Truncate to at most `max` characters (not bytes).
pub fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub fn line(value: &Value) -> String {
    let mut s = value.to_string();
    s.push_str("\r\n");
    s
}

/// Port of Syncplay's `PingService`: round-trip time and forward delay estimate.
#[derive(Debug, Default, Clone)]
pub struct PingService {
    rtt: f64,
    fd: f64,
    avr_rtt: f64,
}

impl PingService {
    /// `timestamp` is our own earlier `latencyCalculation`, echoed back by the client.
    pub fn receive(&mut self, timestamp: f64, sender_rtt: f64, now: f64) {
        if timestamp == 0.0 {
            return;
        }
        self.rtt = now - timestamp;
        if self.rtt < 0.0 || sender_rtt < 0.0 {
            return;
        }
        if self.avr_rtt == 0.0 {
            self.avr_rtt = self.rtt;
        }
        self.avr_rtt = self.avr_rtt * PING_MOVING_AVERAGE_WEIGHT
            + self.rtt * (1.0 - PING_MOVING_AVERAGE_WEIGHT);
        self.fd = if sender_rtt < self.rtt {
            self.avr_rtt / 2.0 + (self.rtt - sender_rtt)
        } else {
            self.avr_rtt / 2.0
        };
    }

    pub fn forward_delay(&self) -> f64 {
        self.fd
    }

    pub fn rtt(&self) -> f64 {
        self.rtt
    }
}

pub fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_python_hashlib() {
        assert_eq!(md5_hex("secret"), "5ebe2294ecd0e0f08eab7690d2a6ee69");
    }

    #[test]
    fn truncate_counts_chars() {
        assert_eq!(truncate("héllo", 2), "hé");
    }

    #[test]
    fn ping_forward_delay() {
        let mut p = PingService::default();
        p.receive(10.0, 0.05, 10.1);
        assert!((p.rtt() - 0.1).abs() < 1e-9);
        assert!(p.forward_delay() > 0.05);
        p.receive(0.0, 0.0, 99.0);
        assert!((p.rtt() - 0.1).abs() < 1e-9, "zero timestamp is ignored");
    }
}
