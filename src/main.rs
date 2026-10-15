mod config;

use std::{env, error::Error};

use tokio::net::TcpListener;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let address = match env::var("JXPAN_LISTEN_ADDR") {
        Ok(value) => config::listen_address(Some(&value))?,
        Err(env::VarError::NotPresent) => config::listen_address(None)?,
        Err(error) => return Err(error.into()),
    };
    let state = jxpan_rust::http::AppState::from_env().await?;
    let listener = TcpListener::bind(address).await?;
    println!("jxpan-rust listening on {}", listener.local_addr()?);

    let (stop, stopped) = tokio::sync::watch::channel(false);
    let maintenance_state = state.clone();
    let mut maintenance_stop = stopped.clone();
    let maintenance = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = maintenance_stop.changed() => break,
                _ = interval.tick() => {
                    if maintenance_state.maintain().await.is_err() {
                        eprintln!("parse-history retention cleanup failed");
                    }
                }
            }
        }
    });
    let server_state = state.clone();
    let served = axum::serve(listener, jxpan_rust::http::router(server_state))
        .with_graceful_shutdown(async move {
            shutdown_signal().await;
            stop.send_replace(true);
        })
        .await;
    drop(stopped);
    maintenance.abort();
    let _ = maintenance.await;
    state.close().await?;
    served?;
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = signal(SignalKind::terminate()).expect("cannot register SIGTERM");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
