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
    let state = jxpan_rust::http::AppState::from_env()?;
    let listener = TcpListener::bind(address).await?;
    println!("jxpan-rust listening on {}", listener.local_addr()?);

    axum::serve(listener, jxpan_rust::http::router(state))
        .with_graceful_shutdown(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("failed to listen for shutdown signal: {error}");
            }
        })
        .await?;
    Ok(())
}
