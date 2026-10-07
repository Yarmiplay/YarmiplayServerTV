//! Native Syncplay 1.7-compatible server, plus the YarmiplayTV extensions.

pub mod ext;
pub mod mux;
pub mod protocol;
pub mod room;
pub mod server;

pub use room::{RoomInfo, SyncplayOptions};
pub use server::{AuthorizeFn, Extensions, SyncplayServer};

impl From<&crate::config::SyncplaySettings> for SyncplayOptions {
    fn from(s: &crate::config::SyncplaySettings) -> Self {
        Self {
            password: s.password.clone(),
            motd: s.motd.clone(),
            isolate_rooms: s.isolate_rooms,
            disable_chat: s.disable_chat,
            disable_ready: s.disable_ready,
            max_chat_message_length: s.max_chat_message_length as usize,
            max_username_length: s.max_username_length as usize,
            vanilla_mode: s.vanilla_mode,
            file_relay: s.relay_effective(),
        }
    }
}
