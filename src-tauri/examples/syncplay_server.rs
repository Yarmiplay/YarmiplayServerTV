//! Headless Syncplay server for interop tests:
//! `cargo run --example syncplay_server -- <port> [password]`

use std::sync::Arc;
use yarmiplayservertv_lib::syncplay::{SyncplayOptions, SyncplayServer};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("info,yarmiplayservertv_lib=debug")
        .init();
    let mut args = std::env::args().skip(1);
    let port: u16 = args.next().and_then(|p| p.parse().ok()).unwrap_or(8999);
    let password = args.next().unwrap_or_default();
    let opts = SyncplayOptions {
        password,
        ..SyncplayOptions::default()
    };
    let server = SyncplayServer::start(port, opts, Arc::default(), Arc::new(|| {}))
        .await
        .expect("bind");
    println!("listening on {}", server.port);
    tokio::signal::ctrl_c().await.ok();
    server.stop();
}
