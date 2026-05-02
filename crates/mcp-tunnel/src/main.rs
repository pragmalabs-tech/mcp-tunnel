mod auth;
mod config;
mod relay;

#[tokio::main]
async fn main() {
    let cfg = config::load();
    let (app, port) = relay::build_relay_app(cfg);

    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .unwrap_or_else(|e| {
            eprintln!("error: failed to bind on port {port}: {e}");
            std::process::exit(1);
        });

    let actual_port = listener.local_addr().unwrap().port();
    println!(
        "  {} mcp-tunnel listening on :{actual_port}",
        colored::Colorize::green("ready")
    );

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    let shutdown_trigger = shutdown_tx.clone();
    tokio::spawn(async move {
        let ctrl_c = tokio::signal::ctrl_c();

        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            let mut sigterm = signal(SignalKind::terminate()).expect("Failed to register SIGTERM");
            tokio::select! {
                _ = ctrl_c => {},
                _ = sigterm.recv() => {},
            }
        }

        #[cfg(not(unix))]
        {
            ctrl_c.await.expect("Failed to listen for ctrl-c");
        }

        eprintln!("[mcp-tunnel] Received shutdown signal, draining...");
        let _ = shutdown_trigger.send(true);
    });

    let shutdown_for_server = shutdown_tx.subscribe();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let mut rx = shutdown_for_server;
                let _ = rx.changed().await;
            })
            .await
            .expect("Relay server failed");
    });

    let _ = shutdown_rx.changed().await;
    eprintln!("[mcp-tunnel] Shutdown complete.");
}
