use super::{AppState, params::Params, reply::Reply};
use crate::auth::Provider;
use crate::providers::{
    cloud189,
    cloud189_client::Cloud189Client,
    cloud189_service, mcloud,
    mcloud_client::MobileCloudClient,
    mcloud_service::{self, ServiceConfig},
};
use axum::http::StatusCode;
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

enum ParseOutcome {
    Direct(Reply),
    Parsed(Value, Option<(String, Vec<String>)>),
}

pub async fn run(state: AppState, params: Params, revision: u64, credentials: Arc<Value>) -> Reply {
    run_with_timeout(
        state,
        params,
        revision,
        credentials,
        std::time::Duration::from_secs(120),
    )
    .await
}
async fn run_with_timeout(
    state: AppState,
    params: Params,
    revision: u64,
    credentials: Arc<Value>,
    timeout: std::time::Duration,
) -> Reply {
    let fast = params.get("type") == "down" && !params.file_id().is_empty();
    // The provider deadline must not cancel a record write already accepted by
    // the SQLite actor and then submit a second failure record for that request.
    let outcome = match tokio::time::timeout(
        timeout,
        recover_auth(
            &state,
            &params,
            revision,
            credentials,
            |revision, credentials, rejected| {
                run_inner(
                    state.clone(),
                    params.clone(),
                    revision,
                    credentials,
                    rejected,
                )
            },
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) if fast => {
            return Reply::error(StatusCode::GATEWAY_TIMEOUT, "上游解析超时，请重试");
        }
        Err(_) => ParseOutcome::Parsed(
            json!({"code":504,"msg":"上游解析超时，请重试","success":false,"data":null}),
            None,
        ),
    };
    let mut reply = match outcome {
        ParseOutcome::Direct(reply) => reply,
        ParseOutcome::Parsed(value, index) => finish(&state, &params, revision, value, index).await,
    };
    // This loader belongs to its original cache key, even after a successful renewal.
    if state.auth.revision() != revision {
        reply.cacheable = false;
    }
    reply
}
fn request_provider(params: &Params) -> Option<Provider> {
    if params.get("action").starts_with("parse_cloud189_")
        || cloud189::extract_share_info(params.get("url")).is_some()
    {
        Some(Provider::Cloud)
    } else if params.get("action") == "parse_mcloud_subfolder"
        || mcloud::extract_share_info(params.get("url")).is_some()
    {
        Some(Provider::Mobile)
    } else {
        None
    }
}
fn successful(outcome: &ParseOutcome) -> bool {
    match outcome {
        ParseOutcome::Direct(reply) => reply.request_success,
        ParseOutcome::Parsed(value, _) => value["code"] == 200 && value["success"] == true,
    }
}
async fn recover_auth<F, Fut>(
    state: &AppState,
    params: &Params,
    mut revision: u64,
    mut credentials: Arc<Value>,
    mut attempt: F,
) -> ParseOutcome
where
    F: FnMut(u64, Arc<Value>, Arc<AtomicBool>) -> Fut,
    Fut: Future<Output = ParseOutcome>,
{
    let provider = request_provider(params);
    // Per-request external tokens are not evidence about the server-managed account.
    let managed = match provider {
        Some(Provider::Cloud) => {
            params.get("p189_token").is_empty() && params.get("c189_token").is_empty()
        }
        Some(Provider::Mobile) => {
            credentials["mcloud"]["enabled"] != false
                && credentials["mcloud"]["enabled"] != "false"
                && credentials["mcloud"]["authorization"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
        }
        None => false,
    };
    let mut lease = provider
        .filter(|_| managed)
        .and_then(|provider| state.auth.request_lease(provider, &credentials));
    if let Some(current) = &lease {
        if let Ok(Some((next_revision, next_credentials))) =
            state.auth.refresh_for_request(current, false).await
        {
            revision = next_revision;
            credentials = next_credentials;
        }
        lease = provider.and_then(|provider| state.auth.request_lease(provider, &credentials));
    }
    let rejected = Arc::new(AtomicBool::new(false));
    let outcome = attempt(revision, credentials, rejected.clone()).await;
    if successful(&outcome) || !rejected.load(Ordering::Relaxed) {
        return outcome;
    }
    let Some(lease) = lease else { return outcome };
    match state.auth.refresh_for_request(&lease, true).await {
        Ok(Some((revision, credentials))) => {
            // Exactly one retry, with a new per-attempt marker. Never recurse.
            attempt(revision, credentials, Arc::new(AtomicBool::new(false))).await
        }
        Ok(None) => outcome,
        Err(error) => {
            let message = if error == crate::auth::AuthError::Conflict {
                "认证状态已改变，请重试"
            } else {
                "认证续期暂时失败，请稍后重试"
            };
            let value = json!({"code":503,"success":false,"msg":message,"data":null});
            if params.get("type") == "down" && !params.file_id().is_empty() {
                ParseOutcome::Direct(Reply::error(StatusCode::SERVICE_UNAVAILABLE, message))
            } else {
                ParseOutcome::Parsed(value, None)
            }
        }
    }
}
async fn run_inner(
    state: AppState,
    params: Params,
    revision: u64,
    credentials: Arc<Value>,
    rejected: Arc<AtomicBool>,
) -> ParseOutcome {
    if params.get("action").starts_with("parse_cloud189_")
        || cloud189::extract_share_info(params.get("url")).is_some()
    {
        return cloud189(state, params, revision, credentials, rejected).await;
    }
    if params.get("action") == "parse_mcloud_subfolder"
        || mcloud::extract_share_info(params.get("url")).is_some()
    {
        return mobile(state, params, revision, credentials, rejected).await;
    }
    ParseOutcome::Parsed(
        json!({"code":501,"msg":"该网盘尚未迁移","success":false,"data":null}),
        None,
    )
}
async fn mobile(
    state: AppState,
    params: Params,
    revision: u64,
    credentials: Arc<Value>,
    rejected: Arc<AtomicBool>,
) -> ParseOutcome {
    let config = &credentials["mcloud"];
    let authorization = config["authorization"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(params.get("auth"))
        .to_owned();
    let configured = !authorization.is_empty();
    let enabled = config["enabled"] != false && config["enabled"] != "false";
    let client = MobileCloudClient::new(state.http.clone(), authorization)
        .with_limit(state.parse_settings.upstream())
        .with_auth_rejection(rejected);
    if params.get("type") == "down" && !params.file_id().is_empty() {
        if !enabled {
            return ParseOutcome::Direct(Reply::error(
                StatusCode::SERVICE_UNAVAILABLE,
                "移动云盘解析已禁用",
            ));
        }
        if !configured {
            return ParseOutcome::Direct(Reply::error(
                StatusCode::BAD_GATEWAY,
                "移动云盘凭据未配置",
            ));
        }
        let sid = if params.get("sid").is_empty() {
            mcloud::extract_share_info(params.get("url")).unwrap_or_default()
        } else {
            params.get("sid").to_owned()
        };
        if sid.is_empty() {
            return ParseOutcome::Direct(Reply::error(
                StatusCode::BAD_REQUEST,
                "缺少或无效的分享 ID",
            ));
        }
        if directory(&state, &params, revision, &sid) {
            return ParseOutcome::Direct(Reply::error(StatusCode::BAD_REQUEST, "目录无法下载"));
        }
        return ParseOutcome::Direct(match client.download_url(&sid, params.file_id()).await {
            Ok(Some(url)) => Reply::redirect(url),
            _ => Reply::error(StatusCode::BAD_GATEWAY, "移动云盘获取下载链接失败"),
        });
    }
    let (value, index) = if params.get("action") == "parse_mcloud_subfolder" {
        if !enabled || !configured {
            (
                json!({"code":if enabled {401} else {503},"msg":"移动云盘未配置或已禁用","success":false,"data":null}),
                None,
            )
        } else if !params.get("file_id").is_empty() || !params.file_id().is_empty() {
            // The legacy page sends file_id; resolve a single/root or nested target
            // through the existing bounded file lookup, not an empty fid-chain.
            mcloud_service::parse_indexed(
                &client,
                ServiceConfig {
                    enabled,
                    authorization_configured: configured,
                },
                params.get("url"),
                params.get("pwd"),
                &params.folders(),
                if params.get("file_id").is_empty() {
                    params.file_id()
                } else {
                    params.get("file_id")
                },
            )
            .await
        } else if let Some(link) = mcloud::extract_share_info(params.get("url")) {
            (
                mcloud_service::parse_with_fid_chain(
                    &client,
                    &link,
                    params.get("pwd"),
                    &params.folders(),
                    params.file_id(),
                )
                .await,
                None,
            )
        } else {
            (
                json!({"code":400,"msg":"缺少或无效的分享链接参数","success":false,"data":null}),
                None,
            )
        }
    } else {
        mcloud_service::parse_indexed(
            &client,
            ServiceConfig {
                enabled,
                authorization_configured: configured,
            },
            params.get("url"),
            params.get("pwd"),
            &params.folders(),
            params.file_id(),
        )
        .await
    };
    ParseOutcome::Parsed(value, index)
}
async fn cloud189(
    state: AppState,
    params: Params,
    revision: u64,
    account: Arc<Value>,
    rejected: Arc<AtomicBool>,
) -> ParseOutcome {
    let mut credentials = account["cloud189"].clone();
    let token = if params.get("c189_token").is_empty() {
        params.get("p189_token")
    } else {
        params.get("c189_token")
    };
    if !token.is_empty() {
        credentials["open_access_token"] = token.into();
    }
    let configured = credentials["open_access_token"]
        .as_str()
        .is_some_and(|s| !s.is_empty())
        || (credentials["session_key"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
            && credentials["session_secret"]
                .as_str()
                .is_some_and(|s| !s.is_empty()));
    if !configured {
        let value = json!({"code":401,"msg":"天翼云盘凭据未配置","success":false,"data":null});
        return if params.get("type") == "down" {
            ParseOutcome::Direct(Reply::error(StatusCode::BAD_GATEWAY, "天翼云盘凭据未配置"))
        } else {
            ParseOutcome::Parsed(value, None)
        };
    }
    let client = Cloud189Client::new(state.http.clone(), credentials)
        .with_limit(state.parse_settings.upstream())
        .with_auth_rejection(rejected);
    if params.get("type") == "down" && !params.file_id().is_empty() {
        if directory(&state, &params, revision, params.get("sid")) {
            return ParseOutcome::Direct(Reply::error(StatusCode::BAD_REQUEST, "目录无法下载"));
        }
        let url = match cloud189_service::fast_download(
            client,
            params.get("url"),
            params.get("pwd"),
            params.file_id(),
            params.get("sid"),
        )
        .await
        {
            Ok(url) => url,
            Err(value) => return ParseOutcome::Direct(Reply::fast_error(value)),
        };
        // Return the supplier URL directly. The client follows any CDN hop;
        // this server never issues Range GET or receives file/video bytes.
        return ParseOutcome::Direct(Reply::redirect(url));
    }
    let (value, index) = match params.get("action") {
        "parse_cloud189_file" => (
            cloud189_service::parse_file(
                client,
                params.get("share_id"),
                params.get("file_id"),
                params.get("file_name"),
                params.get("file_size"),
                params.get("pwd"),
                params.get("share_code"),
            )
            .await,
            None,
        ),
        "parse_cloud189_folder_item" => (
            cloud189_service::browse_folder_item(
                client,
                params.get("share_id"),
                params.get("folder_id"),
                params.get("target_file_id"),
                params.get("pwd"),
                if params.get("share_mode").is_empty() {
                    "1"
                } else {
                    params.get("share_mode")
                },
                params.get("share_code"),
            )
            .await,
            None,
        ),
        _ => {
            cloud189_service::parse_indexed(
                client,
                params.get("url"),
                params.get("pwd"),
                &params.folders(),
                params.file_id(),
            )
            .await
        }
    };
    ParseOutcome::Parsed(value, index)
}
fn directory(state: &AppState, params: &Params, revision: u64, sid: &str) -> bool {
    state
        .directories
        .get(&state.directory_key(params, revision, sid))
        .is_some_and(|ids| ids.iter().any(|id| id == params.file_id()))
}
async fn finish(
    state: &AppState,
    params: &Params,
    revision: u64,
    value: Value,
    index: Option<(String, Vec<String>)>,
) -> Reply {
    if let Some((sid, folders)) = index.filter(|_| state.auth.revision() == revision) {
        let weight = folders.capacity() * std::mem::size_of::<String>()
            + folders.iter().map(String::capacity).sum::<usize>();
        // Also index requests without an explicit sid; retain raw IDs, not name-keyed trees.
        for sid in [sid.as_str(), ""] {
            let copy = folders.clone();
            let _ = state
                .directories
                .get_or_init(
                    state.directory_key(params, revision, sid),
                    || async { copy },
                    move |_| Some(weight + 128),
                )
                .await;
        }
    }
    if params.get("action").is_empty()
        && state
            .records
            .insert(params.get("url"), params.get("pwd"), &value, false)
            .await
            .is_err()
    {
        return Reply::error(
            StatusCode::SERVICE_UNAVAILABLE,
            "解析记录保存失败，请稍后重试",
        );
    }
    let should_redirect = params.get("type") == "down"
        || (params.get("type") != "json" && state.account().1["redirect-url"] == true);
    if should_redirect
        && value["code"] == 200
        && value["data"]["is_folder"] != true
        && value["data"]["is_directory"] != true
        && value["data"]["list"].is_null()
        && let Some(url) = value["data"]["download_url"]
            .as_str()
            .filter(|s| !s.is_empty())
    {
        return Reply::redirect(url.to_owned());
    }
    Reply::json(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{http::reply::ReplyBody, storage::Storage};
    use std::time::Duration;

    #[tokio::test]
    async fn accepted_record_write_outlives_upstream_deadline_without_duplicate_accounting() {
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
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocked_storage = storage.clone();
        let blocker = tokio::spawn(async move {
            blocked_storage
                .with_connection(move |_| {
                    entered_tx.send(()).unwrap();
                    // Synthetic actor stall only; always released below before assertions.
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Ok(())
                })
                .await
        });
        entered_rx.await.unwrap();
        let params = Params::query(Some(
            "url=https%3A%2F%2Foffline.invalid%2Fdeadline&type=json",
        ));
        let (revision, credentials) = state.account();
        let parse = run_with_timeout(
            state.clone(),
            params,
            revision,
            credentials,
            Duration::from_millis(10),
        );
        tokio::pin!(parse);
        let first_poll = futures_util::poll!(&mut parse);
        tokio::time::sleep(Duration::from_millis(30)).await;
        let after_deadline = futures_util::poll!(&mut parse);
        release_tx.send(()).unwrap();
        assert!(first_poll.is_pending());
        assert!(after_deadline.is_pending());
        blocker.await.unwrap().unwrap();
        let reply = parse.await;
        let ReplyBody::Json { bytes, .. } = reply.body else {
            panic!("synthetic unsupported parse must return JSON")
        };
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            state.records.count().await.unwrap(),
            1,
            "one parse must persist one history row"
        );
        assert_eq!(
            value["code"], 501,
            "completed work must not become an upstream timeout"
        );
        assert_eq!(
            state.records.stats().await.unwrap(),
            json!({"total":1,"success":0,"failed":1,"cached":0})
        );
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn upstream_timeout_counts_once_and_fast_download_does_not_write_history() {
        let storage = Storage::open(":memory:", 16).await.unwrap();
        for key in ["c189_login_default", "mcloud_login_default"] {
            storage
                .put(key, json!({"_jxpan_logged_out":true}), None)
                .await
                .unwrap();
        }
        let state = AppState::new(storage, json!({}), "", "").await.unwrap();
        let upstream = state.parse_settings.upstream();
        let mut occupied = Vec::new();
        for _ in 0..upstream.concurrency() {
            occupied.push(upstream.acquire().await);
        }
        // All supplier slots remain occupied: both requests time out before send,
        // without reaching DNS, the internet, or any real account.
        let credentials = Arc::new(json!({"mcloud":{
            "enabled":true,"authorization":"Basic c3ludGhldGljOnRlc3Q6dG9rZW4="
        }}));
        for (query, status) in [
            (
                "url=https%3A%2F%2Fyun.139.com%2Fshareweb%2F%23%2Fw%2Fi%2Fabcdefghijkl&type=json",
                StatusCode::OK,
            ),
            (
                "url=https%3A%2F%2Fyun.139.com%2Fshareweb%2F%23%2Fw%2Fi%2Fabcdefghijkl&type=down&id=synthetic-file&sid=abcdefghijkl",
                StatusCode::GATEWAY_TIMEOUT,
            ),
        ] {
            let reply = run_with_timeout(
                state.clone(),
                Params::query(Some(query)),
                0,
                credentials.clone(),
                Duration::from_millis(10),
            )
            .await;
            assert_eq!(reply.status, status);
            assert!(!reply.cacheable);
            let ReplyBody::Json { bytes, .. } = reply.body else {
                panic!("timeout must return JSON")
            };
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(value["code"], 504);
            assert_eq!(state.records.count().await.unwrap(), 1);
            assert_eq!(
                state.records.stats().await.unwrap(),
                json!({"total":1,"success":0,"failed":1,"cached":0})
            );
        }
        drop(occupied);
        state.close().await.unwrap();
    }

    // All renewal traffic is a strict, in-memory transcript. Parse attempts below
    // are synthetic too: no test can accidentally contact a supplier endpoint.
    use crate::auth::{AuthClock, AuthManager, AuthNetwork};
    use crate::providers::cloud189_auth as ca;
    use futures_util::future::BoxFuture;
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicU64, AtomicUsize},
        },
    };

    const RECOVERY_NOW: u64 = 1_778_733_296_000;
    struct RecoveryClock(AtomicU64);
    impl RecoveryClock {
        fn advance(&self, millis: u64) {
            self.0.fetch_add(millis, Ordering::SeqCst);
        }
    }
    impl AuthClock for RecoveryClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
    struct RecoveryStep(&'static str, Option<String>);
    struct RecoveryNetwork {
        steps: Mutex<VecDeque<RecoveryStep>>,
        calls: AtomicUsize,
        gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    }
    impl AuthNetwork for RecoveryNetwork {
        fn fetch(&self, request: ca::Request) -> BoxFuture<'_, crate::auth::Result<ca::Response>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let RecoveryStep(path, response) = self
                    .steps
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("unexpected offline auth request");
                assert!(
                    request.url.contains(path),
                    "unexpected auth transcript operation"
                );
                if let Some((entered, release)) = &self.gate {
                    entered.notify_one();
                    release.notified().await;
                }
                tokio::task::yield_now().await;
                response
                    .map(|text| ca::Response {
                        status: 200,
                        headers: vec![],
                        text,
                    })
                    .ok_or(crate::auth::AuthError::Network)
            })
        }
    }
    impl RecoveryNetwork {
        fn assert_drained(&self, calls: usize) {
            assert_eq!(self.calls.load(Ordering::SeqCst), calls);
            assert!(self.steps.lock().unwrap().is_empty());
        }
    }
    fn renewal_steps() -> Vec<RecoveryStep> {
        vec![
            RecoveryStep(
                "getSessionForPC",
                Some(
                    json!({"res_code":0,
                "sessionKey":"offline-new-session", "sessionSecret":"offline-new-secret",
                "accessToken":"offline-new-app"})
                    .to_string(),
                ),
            ),
            RecoveryStep(
                "getAccessTokenBySsKey",
                Some(json!({"accessToken":"offline-new-open"}).to_string()),
            ),
        ]
    }
    fn cloud_info() -> Value {
        json!({"app_access_token":"offline-old-app", "access_token":"offline-old-open",
            "open_access_token":"offline-old-open", "session_key":"offline-old-session",
            "session_secret":"offline-old-secret", "login_time":RECOVERY_NOW/1000,
            "expires_at":RECOVERY_NOW/1000+86400})
    }
    fn mobile_auth(token: &str) -> String {
        mcloud::encode_authorization(&format!("pc:15500001234:{token}")).unwrap()
    }
    async fn recovery_fixture(
        provider: Option<Provider>,
        info: Value,
        steps: Vec<RecoveryStep>,
        gate: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    ) -> (AppState, Storage, Arc<RecoveryNetwork>, Arc<RecoveryClock>) {
        let storage = Storage::open(":memory:", 64).await.unwrap();
        for key in ["c189_login_default", "mcloud_login_default"] {
            storage
                .put(key, json!({"_jxpan_logged_out":true}), None)
                .await
                .unwrap();
        }
        // Tombstones exist before AppState construction, so private/env seeds
        // cannot populate either account. Initial config also pins non-secret knobs.
        let mut state = AppState::new(storage.clone(), json!({}), "", "")
            .await
            .unwrap();
        if let Some(provider) = provider {
            let key = if provider == Provider::Cloud {
                "c189_login_default"
            } else {
                "mcloud_login_default"
            };
            storage.put(key, info, None).await.unwrap();
        }
        let network = Arc::new(RecoveryNetwork {
            steps: Mutex::new(steps.into()),
            calls: AtomicUsize::new(0),
            gate,
        });
        let clock = Arc::new(RecoveryClock(AtomicU64::new(RECOVERY_NOW)));
        state.auth = AuthManager::with_network(
            storage.clone(),
            json!({"mcloud":{"enabled":true,"userAgent":"offline-test"}}),
            network.clone(),
            clock.clone(),
        )
        .await
        .unwrap();
        (state, storage, network, clock)
    }
    async fn rebuild_recovery_auth(
        state: &mut AppState,
        storage: &Storage,
        network: &Arc<RecoveryNetwork>,
        clock: &Arc<RecoveryClock>,
        enabled: Value,
    ) {
        state.auth = AuthManager::with_network(
            storage.clone(),
            json!({"mcloud":{"enabled":enabled,"userAgent":"offline-test"}}),
            network.clone(),
            clock.clone(),
        )
        .await
        .unwrap();
    }
    fn window_token(label: &str, days: u64) -> String {
        assert!(days <= 15);
        format!("{label}|offline|test|{}", RECOVERY_NOW + days * 86_400_000)
    }
    fn mobile_renewal_step(token: &str) -> RecoveryStep {
        RecoveryStep(
            "authTokenRefresh.do",
            Some(format!(
                "<root><return>0</return><token>{token}</token></root>"
            )),
        )
    }
    async fn rejected_request(state: &AppState, params: &Params) -> (ParseOutcome, usize) {
        let (revision, credentials) = state.account();
        let mut attempts = 0;
        let outcome = recover_auth(state, params, revision, credentials, |_, _, rejected| {
            attempts += 1;
            assert!(!rejected.load(Ordering::Relaxed));
            async move {
                rejected.store(true, Ordering::Relaxed);
                synthetic_outcome(401)
            }
        })
        .await;
        (outcome, attempts)
    }
    fn recovery_params(provider: Provider) -> Params {
        Params::query(Some(if provider == Provider::Cloud {
            "url=https%3A%2F%2Fcloud.189.cn%2Ft%2Foffline-share&type=json"
        } else {
            "action=parse_mcloud_subfolder&sid=offline-share&type=json"
        }))
    }
    fn synthetic_outcome(code: u16) -> ParseOutcome {
        ParseOutcome::Parsed(
            json!({"code":code,"success":code==200,
            "msg":"offline attempt","data":null}),
            None,
        )
    }
    fn outcome_value(outcome: ParseOutcome) -> Value {
        match outcome {
            ParseOutcome::Parsed(value, _) => value,
            ParseOutcome::Direct(reply) => {
                let ReplyBody::Json { bytes, .. } = reply.body else {
                    panic!("expected JSON")
                };
                serde_json::from_slice(&bytes).unwrap()
            }
        }
    }

    #[tokio::test]
    async fn disabled_mobile_bool_and_string_skip_renewal_on_parse_subfolder_and_fast_paths() {
        for enabled in [json!(false), json!("false")] {
            for query in [
                "url=https%3A%2F%2Fyun.139.com%2Fshareweb%2F%23%2Fw%2Fi%2Fabcdefghijkl&type=json",
                "action=parse_mcloud_subfolder&sid=abcdefghijkl&file_id=offline-file&type=json",
                "action=parse_mcloud_subfolder&sid=abcdefghijkl&id=offline-file&type=down",
            ] {
                let (mut state, storage, network, clock) = recovery_fixture(
                    Some(Provider::Mobile),
                    json!({"authorization":mobile_auth(&window_token("disabled", 1))}),
                    vec![],
                    None,
                )
                .await;
                rebuild_recovery_auth(&mut state, &storage, &network, &clock, enabled.clone())
                    .await;
                let params = Params::query(Some(query));
                assert!(request_provider(&params) == Some(Provider::Mobile));
                let (revision, credentials) = state.account();
                assert_eq!(credentials["mcloud"]["enabled"], enabled);
                let before = storage.get("mcloud_login_default").await.unwrap();
                // Even a regression in the disabled parse branch cannot send real HTTP.
                let upstream = state.parse_settings.upstream();
                let mut occupied = Vec::new();
                for _ in 0..upstream.concurrency() {
                    occupied.push(upstream.acquire().await);
                }
                let mut attempts = 0;
                let outcome = tokio::time::timeout(
                    Duration::from_millis(100),
                    recover_auth(
                        &state,
                        &params,
                        revision,
                        credentials.clone(),
                        |revision, credentials, rejected| {
                            attempts += 1;
                            run_inner(
                                state.clone(),
                                params.clone(),
                                revision,
                                credentials,
                                rejected,
                            )
                        },
                    ),
                )
                .await
                .expect("disabled parse must finish without an upstream slot");
                if params.get("type") == "down" {
                    let ParseOutcome::Direct(ref reply) = outcome else {
                        panic!("disabled fast download must return a direct error")
                    };
                    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
                    assert!(!reply.cacheable);
                }
                let value = outcome_value(outcome);
                assert_eq!(value["code"], 503);
                assert_eq!(value["success"], false);
                assert!(value["msg"].as_str().unwrap().contains("禁用"));
                assert_eq!(attempts, 1);
                assert_eq!(state.auth.revision(), revision);
                assert!(Arc::ptr_eq(&credentials, &state.auth.snapshot()));
                assert_eq!(storage.get("mcloud_login_default").await.unwrap(), before);
                assert_eq!(state.records.count().await.unwrap(), 0);
                network.assert_drained(0);
                drop(occupied);
                state.close().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn mobile_window_success_with_same_or_still_window_token_has_persisted_sixty_second_cooldown()
     {
        for same_token in [true, false] {
            let old = window_token("old", 1);
            let renewed = if same_token {
                old.clone()
            } else {
                window_token("new", 2)
            };
            let (mut state, storage, network, clock) = recovery_fixture(
                Some(Provider::Mobile),
                json!({"authorization":mobile_auth(&old)}),
                vec![mobile_renewal_step(&renewed), mobile_renewal_step(&renewed)],
                None,
            )
            .await;
            let params = recovery_params(Provider::Mobile);
            let original_revision = state.auth.revision();
            let mut saved = Value::Null;
            let mut stable_revision = 0;
            let mut stable_snapshot = state.auth.snapshot();
            for request in 0..8 {
                if request == 4 {
                    // A new manager has no in-memory refresh cache or provider epochs.
                    rebuild_recovery_auth(&mut state, &storage, &network, &clock, json!(true))
                        .await;
                    stable_revision = state.auth.revision();
                    stable_snapshot = state.auth.snapshot();
                }
                let (revision, credentials) = state.account();
                let mut attempts = 0;
                let outcome = recover_auth(
                    &state,
                    &params,
                    revision,
                    credentials,
                    |_, credentials, _| {
                        attempts += 1;
                        assert_eq!(
                            credentials["mcloud"]["authorization"],
                            mobile_auth(&renewed)
                        );
                        async { synthetic_outcome(200) }
                    },
                )
                .await;
                assert!(successful(&outcome));
                assert_eq!(attempts, 1);
                assert_eq!(network.calls.load(Ordering::SeqCst), 1);
                let current = storage.get("mcloud_login_default").await.unwrap().unwrap();
                assert_eq!(current["mcloud_auto_refreshed_at"], RECOVERY_NOW);
                assert_eq!(current["timestamp"], RECOVERY_NOW);
                assert_eq!(current["jxpan_auto_refresh_managed"], true);
                if request == 0 {
                    assert!(state.auth.revision() > original_revision);
                    saved = current;
                    stable_revision = state.auth.revision();
                    stable_snapshot = state.auth.snapshot();
                } else {
                    assert_eq!(current, saved);
                    assert_eq!(state.auth.revision(), stable_revision);
                    assert!(Arc::ptr_eq(&stable_snapshot, &state.auth.snapshot()));
                }
            }
            clock.advance(59_999);
            assert!(!state.auth.refresh_mcloud().await.unwrap());
            assert_eq!(network.calls.load(Ordering::SeqCst), 1);
            assert_eq!(
                storage.get("mcloud_login_default").await.unwrap().unwrap(),
                saved
            );
            assert_eq!(state.auth.revision(), stable_revision);
            assert!(Arc::ptr_eq(&stable_snapshot, &state.auth.snapshot()));
            clock.advance(1);
            assert!(
                !state.auth.refresh_mcloud().await.unwrap(),
                "same authorization is not progress"
            );
            let current = storage.get("mcloud_login_default").await.unwrap().unwrap();
            assert_eq!(current["mcloud_auto_refreshed_at"], RECOVERY_NOW + 60_000);
            assert_eq!(current["timestamp"], RECOVERY_NOW + 60_000);
            assert_eq!(current["authorization"], mobile_auth(&renewed));
            assert!(state.auth.revision() > stable_revision);
            network.assert_drained(2);
            state.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn mobile_rejection_refresh_without_token_progress_does_not_retry_or_loop() {
        let token = "offline-unknown-expiry";
        let (state, storage, network, _) = recovery_fixture(
            Some(Provider::Mobile),
            json!({"authorization":mobile_auth(token),"jxpan_auto_refresh_managed":true}),
            vec![mobile_renewal_step(token)],
            None,
        )
        .await;
        let params = recovery_params(Provider::Mobile);
        let (outcome, attempts) = rejected_request(&state, &params).await;
        assert_eq!(outcome_value(outcome)["code"], 401);
        assert_eq!(
            attempts, 1,
            "metadata-only success cannot justify a parse retry"
        );
        let (revision, snapshot) = state.account();
        let saved = storage.get("mcloud_login_default").await.unwrap();
        assert_eq!(
            saved.as_ref().unwrap()["mcloud_auto_refreshed_at"],
            RECOVERY_NOW
        );
        for _ in 0..4 {
            let (outcome, attempts) = rejected_request(&state, &params).await;
            let value = outcome_value(outcome);
            assert_eq!(value["code"], 503);
            assert!(value["msg"].as_str().unwrap().contains("暂时"));
            assert_eq!(attempts, 1);
            assert_eq!(state.auth.revision(), revision);
            assert!(Arc::ptr_eq(&snapshot, &state.auth.snapshot()));
            assert_eq!(storage.get("mcloud_login_default").await.unwrap(), saved);
        }
        network.assert_drained(1);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn continuous_auth_rejection_during_mobile_or_cloud_success_cooldown_returns_temporary_503()
     {
        for provider in [Provider::Mobile, Provider::Cloud] {
            for fast in [false, true] {
                let token = window_token("old", 1);
                let (info, steps, key, calls) = if provider == Provider::Mobile {
                    (
                        json!({"authorization":mobile_auth(&token)}),
                        vec![mobile_renewal_step(&window_token("renewed", 2))],
                        "mcloud_login_default",
                        1,
                    )
                } else {
                    (cloud_info(), renewal_steps(), "c189_login_default", 2)
                };
                let (mut state, storage, network, clock) =
                    recovery_fixture(Some(provider), info, steps, None).await;
                let params = if fast {
                    Params::query(Some(if provider == Provider::Mobile {
                        "action=parse_mcloud_subfolder&type=down&id=offline-file"
                    } else {
                        "action=parse_cloud189_subfolder&type=down&id=offline-file"
                    }))
                } else {
                    recovery_params(provider)
                };
                let (outcome, attempts) = rejected_request(&state, &params).await;
                // Mobile renews proactively; Cloud refreshes only after the first 401.
                assert_eq!(attempts, if provider == Provider::Mobile { 1 } else { 2 });
                assert_eq!(
                    outcome_value(outcome)["code"],
                    if provider == Provider::Mobile {
                        503
                    } else {
                        401
                    }
                );
                network.assert_drained(calls);
                let saved = storage.get(key).await.unwrap();
                for request in 0..4 {
                    if request == 2 {
                        rebuild_recovery_auth(&mut state, &storage, &network, &clock, json!(true))
                            .await;
                        clock.advance(59_999);
                    }
                    let (revision, snapshot) = state.account();
                    let (outcome, attempts) = rejected_request(&state, &params).await;
                    if fast {
                        let ParseOutcome::Direct(ref reply) = outcome else {
                            panic!("fast cooldown rejection must be a direct error")
                        };
                        assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
                        assert!(!reply.cacheable);
                    }
                    let value = outcome_value(outcome);
                    assert_eq!(value["code"], 503);
                    assert_eq!(value["success"], false);
                    let message = value["msg"].as_str().unwrap();
                    assert!(message.contains("暂时"));
                    assert!(!message.contains("登录") && !message.contains("重登"));
                    assert_eq!(attempts, 1);
                    assert_eq!(state.auth.revision(), revision);
                    assert!(Arc::ptr_eq(&snapshot, &state.auth.snapshot()));
                    assert_eq!(storage.get(key).await.unwrap(), saved);
                    network.assert_drained(calls);
                }
                state.close().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn mobile_manual_account_switch_drops_old_refresh_metadata_and_can_renew_immediately() {
        let old = window_token("old-account", 1);
        let new = window_token("new-account", 2);
        let new_auth = mcloud::encode_authorization(&format!("pc:15500005678:{new}")).unwrap();
        let (state, storage, network, _) = recovery_fixture(
            Some(Provider::Mobile),
            json!({"authorization":mobile_auth(&old)}),
            vec![mobile_renewal_step(&old), mobile_renewal_step(&new)],
            None,
        )
        .await;
        assert!(!state.auth.refresh_mcloud().await.unwrap());
        let old_saved = storage.get("mcloud_login_default").await.unwrap().unwrap();
        assert_eq!(old_saved["mcloud_auto_refreshed_at"], RECOVERY_NOW);
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("authorization", &new_auth)
            .finish();
        let response = state
            .auth
            .handle("mcloud_manual_save", &Params::query(Some(&query)), true)
            .await
            .unwrap();
        assert_eq!(response["code"], 200);
        let saved = storage.get("mcloud_login_default").await.unwrap().unwrap();
        assert_eq!(saved["authorization"], new_auth);
        for key in [
            "mcloud_auto_refreshed_at",
            "mcloud_auto_refresh_failed_at",
            "jxpan_auto_refresh_managed",
        ] {
            assert!(
                saved.get(key).is_none(),
                "manual account must not inherit {key}"
            );
            assert!(
                state.auth.snapshot()["mcloud"]["loginInfo"]
                    .get(key)
                    .is_none()
            );
        }
        let (outcome, attempts) =
            rejected_request(&state, &recovery_params(Provider::Mobile)).await;
        assert_eq!(outcome_value(outcome)["code"], 503);
        assert_eq!(attempts, 1);
        let saved = storage.get("mcloud_login_default").await.unwrap().unwrap();
        assert_eq!(saved["authorization"], new_auth);
        assert_eq!(saved["mcloud_auto_refreshed_at"], RECOVERY_NOW);
        network.assert_drained(2);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn rejected_future_cloud_credentials_renew_twice_and_retry_once_then_finish_once() {
        let (state, storage, network, _) =
            recovery_fixture(Some(Provider::Cloud), cloud_info(), renewal_steps(), None).await;
        let params = recovery_params(Provider::Cloud);
        let (original_revision, old) = state.account();
        let mut attempts = Vec::new();
        let outcome = recover_auth(
            &state,
            &params,
            original_revision,
            old.clone(),
            |rev, credentials, rejected| {
                attempts.push((rev, credentials.clone()));
                let first = attempts.len() == 1;
                async move {
                    if first {
                        rejected.store(true, Ordering::Relaxed);
                        synthetic_outcome(401)
                    } else {
                        ParseOutcome::Parsed(
                            json!({"code":200,"success":true,"data":{"is_folder":true}}),
                            Some(("offline-share".into(), vec!["offline-directory".into()])),
                        )
                    }
                }
            },
        )
        .await;
        assert_eq!(attempts.len(), 2);
        assert!(Arc::ptr_eq(&attempts[0].1, &old));
        assert_eq!(
            attempts[0].1["cloud189"]["session_key"],
            "offline-old-session"
        );
        assert_eq!(
            attempts[1].1["cloud189"]["session_key"],
            "offline-new-session"
        );
        assert_eq!(
            attempts[1].1["cloud189"]["open_access_token"],
            "offline-new-open"
        );
        assert!(attempts[1].0 > original_revision);
        assert_eq!(
            storage.get("c189_login_default").await.unwrap().unwrap()["open_access_token"],
            "offline-new-open"
        );
        assert_eq!(
            state.records.count().await.unwrap(),
            0,
            "attempts themselves must not account history"
        );
        let ParseOutcome::Parsed(value, index) = outcome else {
            panic!("expected parsed")
        };
        let reply = finish(&state, &params, original_revision, value, index).await;
        assert!(reply.request_success);
        assert_eq!(state.records.count().await.unwrap(), 1);
        assert_eq!(
            state.records.stats().await.unwrap(),
            json!({"total":1,"success":1,"failed":0,"cached":0})
        );
        for revision in [original_revision, state.auth.revision()] {
            for sid in ["offline-share", ""] {
                assert!(
                    state
                        .directories
                        .get(&state.directory_key(&params, revision, sid))
                        .is_none(),
                    "an old-revision loader must never publish a directory index"
                );
            }
        }
        network.assert_drained(2);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn app_only_cloud_proactively_recovers_session_and_open_before_first_attempt() {
        let mut info = cloud_info();
        for key in ["session_key", "session_secret", "open_access_token"] {
            info.as_object_mut().unwrap().remove(key);
        }
        info["access_token"] = json!("offline-old-app");
        let (state, _, network, _) =
            recovery_fixture(Some(Provider::Cloud), info, renewal_steps(), None).await;
        let (revision, credentials) = state.account();
        let mut calls = 0;
        let outcome = recover_auth(
            &state,
            &recovery_params(Provider::Cloud),
            revision,
            credentials,
            |rev, credentials, _| {
                calls += 1;
                assert!(rev > revision);
                assert_eq!(
                    credentials["cloud189"]["session_key"],
                    "offline-new-session"
                );
                assert_eq!(
                    credentials["cloud189"]["open_access_token"],
                    "offline-new-open"
                );
                async { synthetic_outcome(200) }
            },
        )
        .await;
        assert!(successful(&outcome));
        assert_eq!(calls, 1);
        network.assert_drained(2);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn unknown_mobile_expiry_rejection_forces_refresh_after_managed_marker_write() {
        let old = mobile_auth("offline-unknown-expiry");
        let (state, storage, network, _) = recovery_fixture(
            Some(Provider::Mobile),
            json!({"authorization":old}),
            vec![RecoveryStep(
                "authTokenRefresh.do",
                Some("<root><return>0</return><token>offline-renewed</token></root>".into()),
            )],
            None,
        )
        .await;
        let (revision, credentials) = state.account();
        let mut calls = 0;
        let outcome = recover_auth(
            &state,
            &recovery_params(Provider::Mobile),
            revision,
            credentials,
            |_, credentials, rejected| {
                calls += 1;
                let first = calls == 1;
                assert_eq!(
                    credentials["mcloud"]["authorization"],
                    if first {
                        old.clone()
                    } else {
                        mobile_auth("offline-renewed")
                    }
                );
                // Proactive use persists this marker without changing account generation.
                assert_eq!(
                    credentials["mcloud"]["loginInfo"]["jxpan_auto_refresh_managed"],
                    true
                );
                async move {
                    rejected.store(first, Ordering::Relaxed);
                    synthetic_outcome(if first { 401 } else { 200 })
                }
            },
        )
        .await;
        assert!(successful(&outcome));
        assert_eq!(
            calls, 2,
            "lease must be reacquired after proactive metadata write"
        );
        assert_eq!(
            storage.get("mcloud_login_default").await.unwrap().unwrap()["authorization"],
            mobile_auth("offline-renewed")
        );
        network.assert_drained(1);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn mobile_rejection_preserves_old_auth_and_persisted_one_hour_failure_backoff() {
        let old = mobile_auth("offline-unknown-expiry");
        let (state, storage, network, clock) = recovery_fixture(
            Some(Provider::Mobile),
            json!({"authorization":old}),
            vec![RecoveryStep("authTokenRefresh.do", None)],
            None,
        )
        .await;
        for now in [RECOVERY_NOW, RECOVERY_NOW + 3_599_999] {
            clock.0.store(now, Ordering::SeqCst);
            let (revision, credentials) = state.account();
            let mut calls = 0;
            let outcome = recover_auth(
                &state,
                &recovery_params(Provider::Mobile),
                revision,
                credentials,
                |_, _, rejected| {
                    calls += 1;
                    async move {
                        rejected.store(true, Ordering::Relaxed);
                        synthetic_outcome(401)
                    }
                },
            )
            .await;
            assert_eq!(outcome_value(outcome)["code"], 401);
            assert_eq!(calls, 1);
            assert_eq!(state.auth.snapshot()["mcloud"]["authorization"], old);
        }
        let saved = storage.get("mcloud_login_default").await.unwrap().unwrap();
        assert_eq!(saved["mcloud_auto_refresh_failed_at"], RECOVERY_NOW);
        assert_eq!(saved["authorization"], old);
        network.assert_drained(1);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn successful_fallback_and_non_auth_failures_never_force_renewal() {
        for (code, flag) in [(200, true), (502, false)] {
            let (state, _, network, _) =
                recovery_fixture(Some(Provider::Cloud), cloud_info(), vec![], None).await;
            let (revision, credentials) = state.account();
            let mut calls = 0;
            let outcome = recover_auth(
                &state,
                &recovery_params(Provider::Cloud),
                revision,
                credentials,
                |_, _, rejected| {
                    calls += 1;
                    async move {
                        rejected.store(flag, Ordering::Relaxed);
                        synthetic_outcome(code)
                    }
                },
            )
            .await;
            assert_eq!(outcome_value(outcome)["code"], code);
            assert_eq!(calls, 1);
            assert_eq!(state.auth.revision(), revision);
            network.assert_drained(0);
            state.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn external_cloud_tokens_and_unmanaged_mobile_auth_never_refresh_server_account() {
        for token in ["p189_token", "c189_token"] {
            let (state, _, network, _) =
                recovery_fixture(Some(Provider::Cloud), cloud_info(), vec![], None).await;
            let params = Params::query(Some(&format!(
                "action=parse_cloud189_subfolder&{token}=offline-external&type=json"
            )));
            let (revision, credentials) = state.account();
            let mut calls = 0;
            let outcome = recover_auth(&state, &params, revision, credentials, |_, _, rejected| {
                calls += 1;
                async move {
                    rejected.store(true, Ordering::Relaxed);
                    synthetic_outcome(401)
                }
            })
            .await;
            assert_eq!(outcome_value(outcome)["code"], 401);
            assert_eq!(calls, 1);
            network.assert_drained(0);
            state.close().await.unwrap();
        }
        let (state, _, network, _) = recovery_fixture(None, Value::Null, vec![], None).await;
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("action", "parse_mcloud_subfolder")
            .append_pair("auth", &mobile_auth("offline-external"))
            .finish();
        let (revision, credentials) = state.account();
        let mut calls = 0;
        let outcome = recover_auth(
            &state,
            &Params::query(Some(&query)),
            revision,
            credentials,
            |_, _, rejected| {
                calls += 1;
                async move {
                    rejected.store(true, Ordering::Relaxed);
                    synthetic_outcome(401)
                }
            },
        )
        .await;
        assert_eq!(outcome_value(outcome)["code"], 401);
        assert_eq!(calls, 1);
        network.assert_drained(0);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn second_auth_rejection_is_returned_without_a_third_attempt_or_refresh() {
        let (state, _, network, _) =
            recovery_fixture(Some(Provider::Cloud), cloud_info(), renewal_steps(), None).await;
        let (revision, credentials) = state.account();
        let mut calls = 0;
        let outcome = recover_auth(
            &state,
            &recovery_params(Provider::Cloud),
            revision,
            credentials,
            |_, _, rejected| {
                calls += 1;
                assert!(
                    !rejected.load(Ordering::Relaxed),
                    "each attempt gets a fresh marker"
                );
                async move {
                    rejected.store(true, Ordering::Relaxed);
                    synthetic_outcome(401)
                }
            },
        )
        .await;
        assert_eq!(outcome_value(outcome)["code"], 401);
        assert_eq!(calls, 2);
        network.assert_drained(2);
        state.close().await.unwrap();
    }

    #[tokio::test]
    async fn renewal_network_or_storage_failure_returns_temporary_503_and_retains_old_auth() {
        for (storage_failure, fast) in [(false, false), (true, false), (false, true), (true, true)]
        {
            let steps = if storage_failure {
                renewal_steps()
            } else {
                vec![RecoveryStep("getSessionForPC", None)]
            };
            let (state, storage, network, _) =
                recovery_fixture(Some(Provider::Cloud), cloud_info(), steps, None).await;
            if storage_failure {
                storage.with_connection(|db| {
                    db.execute_batch("CREATE TRIGGER offline_fail_auth BEFORE INSERT ON kv_store BEGIN SELECT RAISE(ABORT, 'offline'); END;")
                        .map_err(|_| crate::storage::StorageError::Database)
                }).await.unwrap();
            }
            let (revision, credentials) = state.account();
            let before = storage.get("c189_login_default").await.unwrap();
            let params = if fast {
                Params::query(Some(
                    "action=parse_cloud189_subfolder&type=down&id=offline-file",
                ))
            } else {
                recovery_params(Provider::Cloud)
            };
            let mut calls = 0;
            let outcome = recover_auth(
                &state,
                &params,
                revision,
                credentials.clone(),
                |_, _, rejected| {
                    calls += 1;
                    async move {
                        rejected.store(true, Ordering::Relaxed);
                        synthetic_outcome(401)
                    }
                },
            )
            .await;
            if fast {
                let ParseOutcome::Direct(ref reply) = outcome else {
                    panic!("fast download must return direct error")
                };
                assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
                assert!(!reply.cacheable);
            }
            let value = outcome_value(outcome);
            assert_eq!(value["code"], 503);
            let message = value["msg"].as_str().unwrap();
            assert!(message.contains("暂时"));
            assert!(!message.contains("登录") && !message.contains("重登"));
            assert_eq!(calls, 1);
            assert!(Arc::ptr_eq(&credentials, &state.auth.snapshot()));
            assert_eq!(state.auth.revision(), revision);
            assert_eq!(storage.get("c189_login_default").await.unwrap(), before);
            network.assert_drained(if storage_failure { 2 } else { 1 });
            state.close().await.unwrap();
        }
    }

    async fn change_cloud_account(state: &AppState, logout: bool) {
        let result = if logout {
            state
                .auth
                .handle("cloud189_logout", &Params::default(), true)
                .await
        } else {
            state
                .auth
                .handle(
                    "cloud189_manual_save",
                    &Params::query(Some("access_token=offline-other-account")),
                    true,
                )
                .await
        }
        .unwrap();
        assert_eq!(result["code"], 200);
    }

    #[tokio::test]
    async fn manual_switch_or_logout_before_or_after_attempt_cannot_retry_with_another_account() {
        for logout in [false, true] {
            for before_attempt in [false, true] {
                let (state, _, network, _) =
                    recovery_fixture(Some(Provider::Cloud), cloud_info(), vec![], None).await;
                let (revision, credentials) = state.account();
                if before_attempt {
                    change_cloud_account(&state, logout).await;
                }
                let mut calls = 0;
                let outcome = recover_auth(
                    &state,
                    &recovery_params(Provider::Cloud),
                    revision,
                    credentials,
                    |_, credentials, rejected| {
                        calls += 1;
                        let state = state.clone();
                        async move {
                            assert_eq!(
                                credentials["cloud189"]["open_access_token"],
                                "offline-old-open"
                            );
                            if !before_attempt {
                                change_cloud_account(&state, logout).await;
                            }
                            rejected.store(true, Ordering::Relaxed);
                            synthetic_outcome(401)
                        }
                    },
                )
                .await;
                assert_eq!(
                    outcome_value(outcome)["code"],
                    if before_attempt { 401 } else { 503 }
                );
                assert_eq!(calls, 1);
                network.assert_drained(0);
                state.close().await.unwrap();
            }
        }
    }

    #[tokio::test]
    async fn manual_switch_or_logout_during_refresh_discards_stale_renewal_without_retry() {
        for logout in [false, true] {
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            let (state, storage, network, _) = recovery_fixture(
                Some(Provider::Cloud),
                cloud_info(),
                renewal_steps(),
                Some((entered.clone(), release.clone())),
            )
            .await;
            let attempts = Arc::new(AtomicUsize::new(0));
            let task_state = state.clone();
            let calls = attempts.clone();
            let task = tokio::spawn(async move {
                let (revision, credentials) = task_state.account();
                recover_auth(
                    &task_state,
                    &recovery_params(Provider::Cloud),
                    revision,
                    credentials,
                    |_, _, rejected| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        async move {
                            rejected.store(true, Ordering::Relaxed);
                            synthetic_outcome(401)
                        }
                    },
                )
                .await
            });
            entered.notified().await;
            change_cloud_account(&state, logout).await;
            let switched = state.auth.snapshot();
            let saved = storage.get("c189_login_default").await.unwrap();
            release.notify_one();
            entered.notified().await;
            release.notify_one();
            let value = outcome_value(task.await.unwrap());
            assert_eq!(value["code"], 503);
            assert!(value["msg"].as_str().unwrap().contains("状态已改变"));
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            assert!(Arc::ptr_eq(&switched, &state.auth.snapshot()));
            assert_eq!(storage.get("c189_login_default").await.unwrap(), saved);
            network.assert_drained(2);
            state.close().await.unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_rejected_requests_share_one_renewal_and_each_retry_with_new_credentials() {
        let (state, _, network, _) =
            recovery_fixture(Some(Provider::Cloud), cloud_info(), renewal_steps(), None).await;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let request = || {
            let state = state.clone();
            let barrier = barrier.clone();
            async move {
                let (revision, credentials) = state.account();
                let mut calls = 0;
                let outcome = recover_auth(
                    &state,
                    &recovery_params(Provider::Cloud),
                    revision,
                    credentials,
                    |_, credentials, rejected| {
                        calls += 1;
                        let first = calls == 1;
                        let barrier = barrier.clone();
                        async move {
                            if first {
                                assert_eq!(
                                    credentials["cloud189"]["open_access_token"],
                                    "offline-old-open"
                                );
                                barrier.wait().await;
                                rejected.store(true, Ordering::Relaxed);
                                synthetic_outcome(401)
                            } else {
                                assert_eq!(
                                    credentials["cloud189"]["open_access_token"],
                                    "offline-new-open"
                                );
                                synthetic_outcome(200)
                            }
                        }
                    },
                )
                .await;
                assert!(successful(&outcome));
                assert_eq!(calls, 2);
            }
        };
        tokio::join!(request(), request());
        network.assert_drained(2);
        state.close().await.unwrap();
    }
}
