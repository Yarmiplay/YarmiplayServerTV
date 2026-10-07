//! Headless Syncplay server for interop tests:
//! `cargo run --example syncplay_server -- <port> [password]`
//! The file relay is on, with its cache in the temp folder.

use std::sync::Arc;
use yarmiplayservertv_lib::relay::Relay;
use yarmiplayservertv_lib::syncplay::{
    Extensions, SyncplayAccess, SyncplayOptions, SyncplayServer,
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,yarmiplayservertv_lib=debug")
        .init();
    yarmiplayservertv_lib::net::install_crypto_provider();
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(8999);
    let password = args.next().unwrap_or_default();
    let opts = SyncplayOptions {
        access: if password.is_empty() {
            SyncplayAccess::Open
        } else {
            SyncplayAccess::Password
        },
        password,
        file_relay: true,
        ..SyncplayOptions::default()
    };
    let cache = std::env::temp_dir().join(format!("yarmiplay-relay-{port}"));
    let ext = Extensions {
        relay: Some(Relay::new(cache, 1 << 30, true)),
        ..Extensions::default()
    };
    let server = SyncplayServer::start(port, opts, Arc::default(), Arc::new(|| {}), ext)
        .await
        .expect("bind");
    println!("listening on {}", server.port);
    tokio::signal::ctrl_c().await.ok();
    server.stop();
}
