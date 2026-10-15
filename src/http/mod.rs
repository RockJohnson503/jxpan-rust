//! HTTP composition: routes, embedded assets, and private request adapters.
mod actions;
mod admin;
mod admission;
mod home;
pub mod params;
mod parse;
mod presentation;
mod reply;
mod request;
mod security;
mod shortlinks;
mod state;

use admin::admin;
use admission::admission;
use axum::{
    Router, middleware,
    routing::{get, post},
};
use home::home;
use presentation::{options, qrcode_asset};
use shortlinks::{create_short, short};
pub use state::AppState;

const HOME: &[u8] = include_bytes!("../../web/index.html");
const ADMIN_LOGIN: &[u8] = include_bytes!("../../web/admin-login.html");
const ADMIN: &[u8] = include_bytes!("../../web/admin.html");
const QRCODE: &[u8] = include_bytes!("../../web/vendor/qrcode.js");
const FORM_LIMIT: usize = 64 * 1024;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(home).post(home).options(options))
        .route("/admin", get(admin).post(admin).options(options))
        .route("/admin/", get(admin).post(admin).options(options))
        .route("/assets/qrcode.js", get(qrcode_asset))
        .route("/api/create-short", post(create_short).options(options))
        .route("/s/{code}", get(short).post(short).options(options))
        .route("/s/{code}/{*rest}", get(short).post(short).options(options))
        .layer(axum::extract::DefaultBodyLimit::max(FORM_LIMIT))
        .layer(middleware::from_fn_with_state(state.clone(), admission))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{home::parsed, params::Params};
    use axum::{body::to_bytes, http::StatusCode};
    use serde_json::{Value, json};

    #[tokio::test(flavor = "current_thread")]
    async fn coalesced_failed_parses_count_two_failures_without_duplicate_history() {
        use crate::storage::Storage;
        use futures_util::{future::join, poll};
        let storage = Storage::open(":memory:", 16).await.unwrap();
        for key in ["c189_login_default", "mcloud_login_default"] {
            storage
                .put(key, json!({"_jxpan_logged_out":true}), None)
                .await
                .unwrap();
        }
        let state = AppState::new(storage.clone(), json!({}), "", "")
            .await
            .unwrap();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let actor = storage.clone();
        let blocker = tokio::spawn(async move {
            actor
                .with_connection(move |_| {
                    started_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    Ok(())
                })
                .await
                .unwrap();
        });
        started_rx.await.unwrap();
        let query = Some("url=https%3A%2F%2Funmigrated.invalid%2Fshare&type=json");
        let mut first = Box::pin(parsed(state.clone(), Params::query(query)));
        assert!(poll!(first.as_mut()).is_pending());
        let mut second = Box::pin(parsed(state.clone(), Params::query(query)));
        assert!(poll!(second.as_mut()).is_pending());
        release_tx.send(()).unwrap();
        let (first, second) = join(first, second).await;
        for response in [first, second] {
            assert_eq!(response.status(), StatusCode::OK);
            let value: Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap())
                    .unwrap();
            assert_eq!(value["code"], 501);
            assert_eq!(value["success"], false);
        }
        blocker.await.unwrap();
        assert_eq!(
            state.records.stats().await.unwrap(),
            json!({"total":2,"success":0,"failed":2,"cached":0})
        );
        assert_eq!(state.records.count().await.unwrap(), 1);
        state.close().await.unwrap();
    }
}
