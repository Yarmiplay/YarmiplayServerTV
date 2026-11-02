//! Headless Syncplay server for interop tests:
//! `cargo run --example syncplay_server -- <port> [password] [--voice|--no-voice]`
//! The file relay is on, with its cache in the temp folder.
//! Voice chat uses this same Syncplay port. There is no extra port to open.

use std::sync::Arc;
use yarmiplayservertv_lib::config::{self, HELP};
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        println!("syncplay_server <port> [password] [--voice|--no-voice]");
        println!("Voice uses that Syncplay port. No extra port to open.");
        return;
    }
    let voice = match config::voice_override(
        &args,
        std::env::var("YARMIPLAYSERVERTV_VOICE").ok().as_deref(),
    ) {
        Ok(v) => v.unwrap_or(true),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };
    let mut positional = Vec::new();
    for arg in &args {
        if arg == "--voice" || arg == "--no-voice" {
            continue;
        }
        positional.push(arg.as_str());
    }
    let port: u16 = positional
        .first()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8999);
    let password = positional.get(1).copied().unwrap_or("").to_string();
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
    if voice {
        server.ensure_voice().await;
    } else {
        server.stop_voice();
    }
    println!(
        "listening on {} — voice {} on this Syncplay port (no extra port to open)",
        server.port,
        if voice { "enabled" } else { "disabled" }
    );
    tokio::signal::ctrl_c().await.ok();
    server.stop();
}
