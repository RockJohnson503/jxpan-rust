use auth::{AuthClock, AuthManager, AuthNetwork};
use futures_util::future::BoxFuture;
use http::params::Params;
use jxpan_rust::{auth, http, providers, storage};
use providers::{cloud189_auth as ca, mcloud_auth as ma};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use storage::Storage;

const NOW: u64 = 1_778_733_296_000;
struct TestClock(AtomicU64);
impl AuthClock for TestClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}
struct Step {
    path: &'static str,
    response: Option<ca::Response>,
}
struct MemoryNetwork {
    steps: Mutex<VecDeque<Step>>,
    requests: Mutex<Vec<ca::Request>>,
    calls: AtomicUsize,
}
impl MemoryNetwork {
    fn new(steps: Vec<Step>) -> Arc<Self> {
        Arc::new(Self {
            steps: Mutex::new(steps.into()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }
    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
    fn drained(&self) -> bool {
        self.steps.lock().unwrap().is_empty()
    }
}
impl AuthNetwork for MemoryNetwork {
    fn fetch(&self, request: ca::Request) -> BoxFuture<'_, auth::Result<ca::Response>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let step = self
                .steps
                .lock()
                .unwrap()
                .pop_front()
                .expect("unexpected memory network call");
            assert!(
                request.url.contains(step.path),
                "unexpected auth operation (URL redacted)"
            );
            if step.path == "getRsaPublicKey" || step.path == "user/thirdlogin" {
                let sign = request
                    .headers
                    .iter()
                    .find(|(k, _)| k == "mcloud-sign")
                    .unwrap();
                let parts: Vec<_> = sign.1.split(',').collect();
                assert_eq!(parts.len(), 3);
                assert!(parts[0].starts_with("2026-05-14 12:"));
                assert_eq!(parts[2].len(), 32);
                assert!(
                    !request
                        .headers
                        .iter()
                        .any(|(k, _)| k.eq_ignore_ascii_case("authorization")
                            || k.eq_ignore_ascii_case("cookie"))
                );
                if step.path == "getRsaPublicKey" {
                    assert_eq!(
                        request.body.as_deref(),
                        Some(r#"{"clientCode":"10701","type":1}"#)
                    );
                }
            }
            self.requests.lock().unwrap().push(request);
            step.response.ok_or(auth::AuthError::Network)
        })
    }
}
fn response(path: &'static str, value: Value) -> Step {
    text(path, &value.to_string())
}
fn text(path: &'static str, body: &str) -> Step {
    Step {
        path,
        response: Some(ca::Response {
            status: 200,
            headers: Vec::new(),
            text: body.into(),
        }),
    }
}
fn cloud_init() -> Vec<Step> {
    vec![
        text(
            "unifyLoginForPC",
            "lt=\"0123456789ABCDEF\";reqId=\"0123456789abcdefgh\";paramId=\"p\"",
        ),
        response(
            "getUUID.do",
            json!({"uuid":"https://open.e.189.cn/qr/synthetic","encryuuid":"synthetic-enc"}),
        ),
    ]
}
fn cloud_confirm() -> Vec<Step> {
    let mut session = response(
        "getSessionForPC",
        json!({"res_code":0,"sessionKey":"synthetic-key","sessionSecret":"synthetic-secret","accessToken":"synthetic-app"}),
    );
    session.response.as_mut().unwrap().headers.push((
        "set-cookie".into(),
        "COOKIE_LOGIN_USER=synthetic-user; Path=/".into(),
    ));
    vec![
        response(
            "qrcodeLoginState",
            json!({"status":0,"redirectUrl":"https://cloud.189.cn/synthetic"}),
        ),
        session,
        response(
            "getAccessTokenBySsKey",
            json!({"accessToken":"synthetic-open"}),
        ),
    ]
}
fn mobile_init() -> Vec<Step> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    fn tlv(tag: u8, bytes: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        if bytes.len() < 128 {
            out.push(bytes.len() as u8);
        } else {
            out.extend([0x82, (bytes.len() >> 8) as u8, bytes.len() as u8]);
        }
        out.extend(bytes);
        out
    }
    // Standards-valid synthetic SPKI, not the frozen nonstandard decoder fixture.
    let mut integer = vec![0];
    integer.extend([0xa5; 256]);
    let mut rsa = tlv(2, &integer);
    rsa.extend(tlv(2, &[1, 0, 1]));
    let mut bits = vec![0];
    bits.extend(tlv(0x30, &rsa));
    let mut spki = vec![
        0x30, 0x0d, 6, 9, 0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 1, 1, 1, 5, 0,
    ];
    spki.extend(tlv(3, &bits));
    vec![response(
        "getRsaPublicKey",
        json!({"success":true,"data":{"publicKey":STANDARD.encode(tlv(0x30, &spki))}}),
    )]
}
fn authorization(token: &str) -> String {
    providers::mcloud::encode_authorization(&format!("pc:15500001234:{token}")).unwrap()
}
fn params(key: &str, value: &str) -> Params {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair(key, value)
        .finish();
    Params::query(Some(&query))
}
async fn setup(
    initial: Value,
    steps: Vec<Step>,
) -> (AuthManager, Storage, Arc<MemoryNetwork>, Arc<TestClock>) {
    let db = Storage::open(":memory:", 64).await.unwrap();
    let net = MemoryNetwork::new(steps);
    let clock = Arc::new(TestClock(AtomicU64::new(NOW)));
    let manager = AuthManager::with_network(db.clone(), initial, net.clone(), clock.clone())
        .await
        .unwrap();
    (manager, db, net, clock)
}
async fn call(manager: &AuthManager, action: &str, p: &Params) -> Value {
    manager.handle(action, p, true).await.unwrap()
}

#[tokio::test]
async fn empty_status_does_not_claim_login_or_contact_network() {
    let (manager, _, network, _) = setup(json!({}), vec![]).await;
    let status = manager
        .handle("login_status", &Params::default(), false)
        .await
        .unwrap();
    assert_eq!(status["data"]["cloud189"]["logged_in"], false);
    assert_eq!(status["data"]["mcloud"]["logged_in"], false);
    assert_eq!(network.count(), 0);
    assert!(
        manager
            .handle("quark_qrcode", &Params::default(), true)
            .await
            .is_none()
    );
}
#[tokio::test]
async fn permission_gate_cannot_create_poll_save_or_logout() {
    let (manager, db, network, _) = setup(json!({}), vec![]).await;
    for action in [
        "cloud189_qrcode",
        "cloud189_check",
        "cloud189_manual_save",
        "cloud189_logout",
        "mcloud_qrcode",
        "mcloud_check",
        "mcloud_manual_save",
        "mcloud_logout",
    ] {
        assert_eq!(
            manager
                .handle(action, &Params::default(), false)
                .await
                .unwrap()["code"],
            403
        );
    }
    assert_eq!(manager.revision(), 0);
    assert!(db.get("c189_login_default").await.unwrap().is_none());
    assert_eq!(network.count(), 0);
}
#[tokio::test]
async fn manual_switch_persists_then_hot_swaps_and_restart_prefers_storage() {
    let (manager, db, net, clock) = setup(json!({"cloud189":{"accessToken":"synthetic-seed"},"mcloud":{"authorization":authorization("synthetic-seed")}}), vec![]).await;
    let old = manager.snapshot();
    assert_eq!(
        call(
            &manager,
            "cloud189_manual_save",
            &params("access_token", "synthetic-new")
        )
        .await["code"],
        200
    );
    assert_eq!(old["cloud189"]["accessToken"], "synthetic-seed");
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-new"
    );
    assert_eq!(
        db.get("c189_login_default").await.unwrap().unwrap()["access_token"],
        "synthetic-new"
    );
    let restarted = AuthManager::with_network(
        db,
        json!({"cloud189":{"accessToken":"synthetic-wrong"}}),
        net,
        clock,
    )
    .await
    .unwrap();
    assert_eq!(
        restarted.snapshot()["cloud189"]["accessToken"],
        "synthetic-new"
    );
}
#[tokio::test]
async fn logout_tombstone_blocks_seed_on_restart_for_both_providers() {
    let initial = json!({"cloud189":{"accessToken":"synthetic-seed"},"mcloud":{"authorization":authorization("synthetic-seed")}});
    let (manager, db, net, clock) = setup(initial.clone(), vec![]).await;
    for action in ["cloud189_logout", "mcloud_logout"] {
        assert_eq!(
            call(&manager, action, &Params::default()).await["code"],
            200
        );
    }
    let restarted = AuthManager::with_network(db.clone(), initial, net, clock)
        .await
        .unwrap();
    assert_eq!(restarted.snapshot()["cloud189"]["accessToken"], "");
    assert_eq!(restarted.snapshot()["mcloud"]["authorization"], "");
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["_jxpan_logged_out"],
        true
    );
}
#[tokio::test]
async fn cloud_qr_confirm_persists_and_removes_only_matching_provider_session() {
    let mut steps = cloud_init();
    steps.extend(mobile_init());
    steps.extend(cloud_confirm());
    steps.push(response(
        "user/thirdlogin",
        json!({"code":"200059541","data":{"result":{"resultCode":"200059541"}}}),
    ));
    let (manager, db, network, _) = setup(json!({}), steps).await;
    let cloud = call(&manager, "cloud189_qrcode", &Params::default()).await;
    let mobile = call(&manager, "mcloud_qrcode", &Params::default()).await;
    assert!(uuid::Uuid::parse_str(cloud["data"]["device_id"].as_str().unwrap()).is_ok());
    assert!(mobile["data"]["sid"].as_str().is_some());
    let p = params("device_id", cloud["data"]["device_id"].as_str().unwrap());
    let confirmed = call(&manager, "cloud189_check", &p).await;
    assert_eq!(confirmed["data"]["status"], "success");
    assert!(!confirmed.to_string().contains("synthetic-open"));
    assert_eq!(
        db.get("c189_login_default").await.unwrap().unwrap()["open_access_token"],
        "synthetic-open"
    );
    assert_eq!(
        call(&manager, "cloud189_check", &p).await["data"]["status"],
        "expired"
    );
    let mobile_p = params("device_id", mobile["data"]["device_id"].as_str().unwrap());
    assert_eq!(
        call(&manager, "mcloud_check", &mobile_p).await["data"]["status"],
        "waiting"
    );
    assert!(network.drained());
}
#[tokio::test]
async fn mobile_qr_confirm_redacts_authorization_and_persists() {
    let mut steps = mobile_init();
    steps.push(response(
        "user/thirdlogin",
        json!({"code":"200059541","data":{}}),
    ));
    steps.push(response(
        "user/thirdlogin",
        json!({"code":"200059548","data":{}}),
    ));
    steps.push(response(
        "user/thirdlogin",
        json!({"code":"0000","data":{"account":"15500001234","token":"synthetic-token"}}),
    ));
    let (manager, db, network, clock) = setup(json!({}), steps).await;
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    assert_eq!(qr["code"], 200);
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    for state in ["waiting", "scanned"] {
        let poll = call(&manager, "mcloud_check", &p).await;
        assert_eq!(poll["code"], 200);
        assert_eq!(poll["data"]["status"], state);
        assert_eq!(manager.revision(), 0);
        assert!(db.get("mcloud_login_default").await.unwrap().is_none());
    }
    let confirmed = call(&manager, "mcloud_check", &p).await;
    assert_eq!(confirmed["data"]["status"], "confirmed");
    assert_mobile_public(&confirmed, "synthetic-token");
    assert_eq!(manager.revision(), 1);
    assert_eq!(
        manager.snapshot()["mcloud"]["authorization"],
        authorization("synthetic-token")
    );
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["authorization"],
        authorization("synthetic-token")
    );
    let status = manager
        .handle("login_status", &Params::default(), false)
        .await
        .unwrap();
    assert_eq!(status["data"]["mcloud"]["account_mask"], "155****1234");
    assert!(!status.to_string().contains("synthetic-token"));
    assert!(!status.to_string().contains("15500001234"));
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    assert_mobile_public(&status, "synthetic-token");
    let restarted = AuthManager::with_network(
        db,
        json!({"mcloud":{"authorization":authorization("synthetic-wrong-seed")}}),
        network.clone(),
        clock,
    )
    .await
    .unwrap();
    assert_eq!(
        restarted.snapshot()["mcloud"]["authorization"],
        authorization("synthetic-token")
    );
    let status = call(&restarted, "login_status", &Params::default()).await;
    assert_eq!(status["data"]["mcloud"]["logged_in"], true);
    assert_eq!(status["data"]["mcloud"]["account_mask"], "155****1234");
    assert_mobile_public(&status, "synthetic-token");
    assert_eq!(network.count(), 4);
    assert!(network.drained());
}
#[tokio::test]
async fn qr_exact_expiry_and_provider_mismatch_never_poll_network() {
    let (manager, _, net, clock) = setup(json!({}), cloud_init()).await;
    let qr = call(&manager, "cloud189_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    clock.0.store(NOW + 300_000, Ordering::SeqCst);
    assert_eq!(
        call(&manager, "cloud189_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(net.count(), 2);
}
#[tokio::test]
async fn failed_confirmation_keeps_previous_credentials_and_qr() {
    let mut steps = cloud_init();
    steps.push(response(
        "qrcodeLoginState",
        json!({"status":0,"redirectUrl":"https://cloud.189.cn/synthetic"}),
    ));
    steps.push(response(
        "getSessionForPC",
        json!({"res_code":-1,"res_message":"synthetic-secret-do-not-echo"}),
    ));
    steps.push(response("qrcodeLoginState", json!({"status":-106})));
    let (manager, _, net, _) =
        setup(json!({"cloud189":{"accessToken":"synthetic-old"}}), steps).await;
    let qr = call(&manager, "cloud189_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    let failed = call(&manager, "cloud189_check", &p).await;
    assert_eq!(failed["code"], 502);
    assert!(!failed.to_string().contains("synthetic-secret"));
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-old"
    );
    assert_eq!(
        call(&manager, "cloud189_check", &p).await["data"]["status"],
        "waiting"
    );
    assert!(net.drained());
}
#[tokio::test]
async fn refresh_mobile_single_flight_preserves_failures_and_backoff() {
    let token = format!("a|b|c|{}", NOW + 86400000);
    let old = authorization(&token);
    let (manager, db, net, _) = setup(
        json!({"mcloud":{"authorization":old}}),
        vec![text(
            "authTokenRefresh.do",
            "<root><return>1</return></root>",
        )],
    )
    .await;
    let (a, b) = tokio::join!(manager.refresh_mcloud(), manager.refresh_mcloud());
    assert!(!a.unwrap());
    assert!(!b.unwrap());
    assert_eq!(net.count(), 1);
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], old);
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["mcloud_auto_refresh_failed_at"],
        NOW
    );
    assert!(!manager.refresh_mcloud().await.unwrap());
    assert_eq!(net.count(), 1);
}
#[tokio::test]
async fn refresh_mobile_success_hot_swaps_and_persists() {
    let old = authorization(&format!("a|b|c|{}", NOW + 86400000));
    let (manager, db, net, _) = setup(
        json!({"mcloud":{"authorization":old}}),
        vec![text(
            "authTokenRefresh.do",
            "<root><return>0</return><token>synthetic-new</token></root>",
        )],
    )
    .await;
    assert!(manager.refresh_mcloud().await.unwrap());
    assert_eq!(
        manager.snapshot()["mcloud"]["authorization"],
        authorization("synthetic-new")
    );
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["authorization"],
        authorization("synthetic-new")
    );
    assert_eq!(net.count(), 1);
}
#[tokio::test]
async fn invalid_manual_credential_does_not_overwrite_or_claim_success() {
    let (manager, db, net, _) = setup(json!({}), vec![]).await;
    assert_eq!(
        call(
            &manager,
            "mcloud_manual_save",
            &params("authorization", "not-base64")
        )
        .await["code"],
        400
    );
    assert_eq!(
        call(&manager, "cloud189_manual_save", &Params::default()).await["code"],
        400
    );
    assert_eq!(manager.revision(), 0);
    assert!(db.get("mcloud_login_default").await.unwrap().is_none());
    assert_eq!(net.count(), 0);
}
#[tokio::test]
async fn closed_storage_failure_keeps_hot_snapshot_unchanged() {
    let (manager, db, _, _) =
        setup(json!({"cloud189":{"accessToken":"synthetic-old"}}), vec![]).await;
    db.close().await.unwrap();
    assert_eq!(
        call(
            &manager,
            "cloud189_manual_save",
            &params("access_token", "synthetic-new")
        )
        .await["code"],
        500
    );
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-old"
    );
    assert_eq!(manager.revision(), 0);
}

struct DelayedNetwork {
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl AuthNetwork for DelayedNetwork {
    fn fetch(&self, request: ca::Request) -> BoxFuture<'_, auth::Result<ca::Response>> {
        Box::pin(async move {
            assert!(request.url == ma::REFRESH_URL);
            self.entered.notify_one();
            self.resume.notified().await;
            Ok(ca::Response {
                status: 200,
                headers: vec![],
                text: "<root><return>0</return><token>synthetic-stale-refresh</token></root>"
                    .into(),
            })
        })
    }
}
#[tokio::test]
async fn old_refresh_cannot_overwrite_concurrent_manual_account_switch() {
    let db = Storage::open(":memory:", 64).await.unwrap();
    let net = Arc::new(DelayedNetwork {
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let manager = AuthManager::with_network(
        db.clone(),
        json!({"mcloud":{"authorization":authorization(&format!("a|b|c|{}",NOW+1))}}),
        net.clone(),
        Arc::new(TestClock(AtomicU64::new(NOW))),
    )
    .await
    .unwrap();
    let m = manager.clone();
    let refresh = tokio::spawn(async move { m.refresh_mcloud().await });
    net.entered.notified().await;
    let new = authorization("synthetic-manual-new");
    assert_eq!(
        call(
            &manager,
            "mcloud_manual_save",
            &params("authorization", &new)
        )
        .await["code"],
        200
    );
    net.resume.notify_one();
    assert_eq!(refresh.await.unwrap(), Err(auth::AuthError::Conflict));
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], new);
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["authorization"],
        new
    );
}
#[tokio::test]
async fn cloud_refresh_single_flight_success_and_failure_retains_old() {
    let info = json!({"app_access_token":"synthetic-app","access_token":"synthetic-old","login_time":NOW/1000,"expires_at":NOW/1000+86400});
    let (manager,_,net,_) = setup(json!({"cloud189":{"loginInfo":info}}),vec![response("getSessionForPC",json!({"res_code":0,"sessionKey":"synthetic-key","sessionSecret":"synthetic-secret"})),response("getAccessTokenBySsKey",json!({"accessToken":"synthetic-refreshed"}))]).await;
    let (a, b) = tokio::join!(manager.refresh_cloud189(), manager.refresh_cloud189());
    assert!(a.unwrap());
    assert!(b.unwrap());
    assert_eq!(net.count(), 2);
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-refreshed"
    );
    let (manager, _, net, _) = setup(
        json!({"cloud189":{"loginInfo":info}}),
        vec![response("getSessionForPC", json!({"res_code":-1}))],
    )
    .await;
    assert!(!manager.refresh_cloud189().await.unwrap());
    assert!(!manager.refresh_cloud189().await.unwrap());
    assert_eq!(net.count(), 1);
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-old"
    );
}

struct YieldingNetwork(Arc<MemoryNetwork>);
impl AuthNetwork for YieldingNetwork {
    fn fetch(&self, request: ca::Request) -> BoxFuture<'_, auth::Result<ca::Response>> {
        Box::pin(async move {
            tokio::task::yield_now().await;
            self.0.fetch(request).await
        })
    }
}

#[tokio::test]
async fn cloud_refresh_errors_are_shared_without_negative_caching() {
    let db = Storage::open(":memory:", 64).await.unwrap();
    let net = MemoryNetwork::new(vec![
        Step {
            path: "getSessionForPC",
            response: None,
        },
        response(
            "getSessionForPC",
            json!({"res_code":0,"sessionKey":"synthetic-key","sessionSecret":"synthetic-secret"}),
        ),
        response(
            "getAccessTokenBySsKey",
            json!({"accessToken":"synthetic-retried"}),
        ),
    ]);
    let manager = AuthManager::with_network(
        db.clone(),
        json!({"cloud189":{"loginInfo":{"app_access_token":"synthetic-app","access_token":"synthetic-old","login_time":NOW/1000,"expires_at":NOW/1000+86400}}}),
        Arc::new(YieldingNetwork(net.clone())),
        Arc::new(TestClock(AtomicU64::new(NOW))),
    )
    .await
    .unwrap();
    let (a, b) = tokio::join!(manager.refresh_cloud189(), manager.refresh_cloud189());
    assert_eq!(a, Err(auth::AuthError::Network));
    assert_eq!(b, Err(auth::AuthError::Network));
    assert_eq!(net.count(), 1);
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-old"
    );
    assert_eq!(manager.revision(), 0);
    assert_eq!(db.get("c189_login_default").await.unwrap(), None);

    // Same clock and provider epoch: no 60-second wait is needed after Err.
    assert!(manager.refresh_cloud189().await.unwrap());
    assert_eq!(net.count(), 3);
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-retried"
    );
    assert_eq!(
        db.get("c189_login_default").await.unwrap().unwrap()["access_token"],
        "synthetic-retried"
    );
    assert!(!manager.refresh_cloud189().await.unwrap());
    assert_eq!(net.count(), 3);
    assert!(net.drained());
    db.close().await.unwrap();
}

#[tokio::test]
async fn mobile_refresh_storage_error_can_retry_without_negative_caching() {
    let old = authorization(&format!("a|b|c|{}", NOW + 86400000));
    let (manager, db, net, _) = setup(
        json!({"mcloud":{"authorization":old}}),
        vec![
            text(
                "authTokenRefresh.do",
                "<root><return>0</return><token>synthetic-new</token></root>",
            ),
            text(
                "authTokenRefresh.do",
                "<root><return>0</return><token>synthetic-retried</token></root>",
            ),
        ],
    )
    .await;
    db.with_connection(|db| {
        db.execute_batch(
            "CREATE TRIGGER fail_refresh_save BEFORE INSERT ON kv_store
             BEGIN SELECT RAISE(ABORT, 'synthetic'); END;",
        )
        .map_err(|_| storage::StorageError::Database)
    })
    .await
    .unwrap();
    assert_eq!(
        manager.refresh_mcloud().await,
        Err(auth::AuthError::Storage)
    );
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], old);
    assert_eq!(manager.revision(), 0);
    assert_eq!(db.get("mcloud_login_default").await.unwrap(), None);
    db.with_connection(|db| {
        db.execute_batch("DROP TRIGGER fail_refresh_save")
            .map_err(|_| storage::StorageError::Database)
    })
    .await
    .unwrap();
    assert!(manager.refresh_mcloud().await.unwrap());
    assert_eq!(net.count(), 2);
    assert_eq!(
        manager.snapshot()["mcloud"]["authorization"],
        authorization("synthetic-retried")
    );
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["authorization"],
        authorization("synthetic-retried")
    );
    assert!(net.drained());
    db.close().await.unwrap();
}

#[tokio::test]
async fn mobile_transport_failure_keeps_legacy_one_hour_backoff() {
    let old = authorization(&format!("a|b|c|{}", NOW + 86400000));
    let (manager, db, net, clock) = setup(
        json!({"mcloud":{"authorization":old}}),
        vec![
            Step {
                path: "authTokenRefresh.do",
                response: None,
            },
            text(
                "authTokenRefresh.do",
                "<root><return>0</return><token>synthetic-retried</token></root>",
            ),
        ],
    )
    .await;
    assert!(!manager.refresh_mcloud().await.unwrap());
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], old);
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["mcloud_auto_refresh_failed_at"],
        NOW
    );
    clock.0.store(NOW + 3_599_999, Ordering::SeqCst);
    assert!(!manager.refresh_mcloud().await.unwrap());
    assert_eq!(net.count(), 1);

    // Bypass the process-local Ok(false) cache, as a restart would, and verify
    // the exact boundary of Mobile's persisted provider backoff.
    let restarted = AuthManager::with_network(db.clone(), json!({}), net.clone(), clock.clone())
        .await
        .unwrap();
    clock.0.store(NOW + 3_600_000, Ordering::SeqCst);
    assert!(restarted.refresh_mcloud().await.unwrap());
    assert_eq!(net.count(), 2);
    let saved = db.get("mcloud_login_default").await.unwrap().unwrap();
    assert!(saved.get("mcloud_auto_refresh_failed_at").is_none());
    assert_eq!(saved["authorization"], authorization("synthetic-retried"));
    assert!(net.drained());
    db.close().await.unwrap();
}

#[tokio::test]
async fn legacy_string_storage_is_decoded_and_invalid_storage_fails_closed() {
    let db = Storage::open(":memory:", 64).await.unwrap();
    let info =
        json!({"access_token":"synthetic-legacy","login_time":NOW/1000,"expires_at":NOW/1000+60});
    db.put("c189_login_default", json!(info.to_string()), None)
        .await
        .unwrap();
    db.put(
        "mcloud_login_default",
        json!(authorization("synthetic-legacy")),
        None,
    )
    .await
    .unwrap();
    let net = MemoryNetwork::new(vec![]);
    let clock = Arc::new(TestClock(AtomicU64::new(NOW)));
    let manager = AuthManager::with_network(db.clone(), json!({}), net.clone(), clock.clone())
        .await
        .unwrap();
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-legacy"
    );
    assert_eq!(
        manager.snapshot()["mcloud"]["authorization"],
        authorization("synthetic-legacy")
    );
    db.put("c189_login_default", json!("invalid-json"), None)
        .await
        .unwrap();
    assert!(
        AuthManager::with_network(
            db,
            json!({"cloud189":{"accessToken":"synthetic-seed"}}),
            net,
            clock
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn public_metadata_masks_and_does_not_echo_untrusted_source_or_timestamp() {
    let info = json!({"access_token":"synthetic-token","source":"synthetic-token","login_time":NOW/1000,"expires_at":NOW/1000+60});
    let (manager,_,_,_) = setup(json!({"cloud189":{"loginInfo":info},"mcloud":{"loginInfo":{"authorization":authorization(&format!("a|b|c|{}",NOW-1)),"timestamp":"synthetic-token"}}}),vec![]).await;
    let status = manager
        .handle("login_status", &Params::default(), false)
        .await
        .unwrap();
    assert!(!status.to_string().contains("synthetic-token"));
    assert_eq!(status["data"]["mcloud"]["logged_in"], false);
    assert_eq!(status["data"]["mcloud"]["expired"], true);
}

#[tokio::test]
async fn qr_sessions_are_bounded_and_reclaimed_at_ttl() {
    let mut steps = Vec::new();
    for _ in 0..130 {
        steps.extend(cloud_init());
    }
    let (manager, _, net, clock) = setup(json!({}), steps).await;
    for _ in 0..128 {
        assert_eq!(
            call(&manager, "cloud189_qrcode", &Params::default()).await["code"],
            200
        );
    }
    assert_eq!(
        call(&manager, "cloud189_qrcode", &Params::default()).await["code"],
        429
    );
    clock.0.store(NOW + 300_000, Ordering::SeqCst);
    assert_eq!(
        call(&manager, "cloud189_qrcode", &Params::default()).await["code"],
        200
    );
    assert!(net.drained());
}

#[tokio::test]
async fn pending_qr_cannot_restore_authorization_after_logout() {
    let (manager, _, net, _) = setup(json!({}), cloud_init()).await;
    let qr = call(&manager, "cloud189_qrcode", &Params::default()).await;
    assert_eq!(
        call(&manager, "cloud189_logout", &Params::default()).await["code"],
        200
    );
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    assert_eq!(
        call(&manager, "cloud189_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(net.count(), 2);
}

#[tokio::test]
async fn default_constructor_only_initializes_and_has_no_network_side_effect() {
    let db = Storage::open(":memory:", 64).await.unwrap();
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let manager = AuthManager::new(db, http, json!({})).await.unwrap();
    assert_eq!(manager.revision(), 0);
}

#[tokio::test]
async fn private_flat_credentials_remain_compatible_with_existing_clients() {
    let (manager,_,net,_) = setup(json!({"cloud189":{"open_access_token":"synthetic-open","app_access_token":"synthetic-app","session_key":"synthetic-key","session_secret":"synthetic-secret"},"mcloud":{"authorization":"","loginInfo":""}}),vec![]).await;
    assert_eq!(
        manager.snapshot()["cloud189"]["open_access_token"],
        "synthetic-open"
    );
    assert_eq!(
        manager.snapshot()["cloud189"]["session_key"],
        "synthetic-key"
    );
    assert_eq!(
        manager.snapshot()["cloud189"]["accessToken"],
        "synthetic-open"
    );
    assert_eq!(
        call(
            &manager,
            "cloud189_manual_save",
            &params("access_token", "synthetic-new")
        )
        .await["code"],
        200
    );
    assert_eq!(
        manager.snapshot()["cloud189"]["open_access_token"],
        "synthetic-new"
    );
    assert!(manager.snapshot()["cloud189"].get("session_key").is_none());
    assert_eq!(net.count(), 0);
}

#[tokio::test]
async fn source_environment_seed_shape_and_logout_mask_all_aliases() {
    let initial = json!({"MCLOUD_AUTHORIZATION":authorization("synthetic-env"),"MCLOUD_ENABLED":"false","MCLOUD_USER_AGENT":"synthetic-agent","P189_TOKEN":"synthetic-env"});
    let (manager, db, net, clock) = setup(initial.clone(), vec![]).await;
    assert_eq!(manager.snapshot()["mcloud"]["enabled"], false);
    assert_eq!(manager.snapshot()["mcloud"]["userAgent"], "synthetic-agent");
    assert_eq!(
        manager.snapshot()["cloud189"]["open_access_token"],
        "synthetic-env"
    );
    for action in ["cloud189_logout", "mcloud_logout"] {
        assert_eq!(
            call(&manager, action, &Params::default()).await["code"],
            200
        );
    }
    let restarted = AuthManager::with_network(db, initial, net, clock)
        .await
        .unwrap();
    assert_eq!(restarted.snapshot()["cloud189"]["accessToken"], "");
    assert_eq!(restarted.snapshot()["mcloud"]["authorization"], "");
    assert!(restarted.snapshot().get("P189_TOKEN").is_none());
    assert!(restarted.snapshot().get("MCLOUD_AUTHORIZATION").is_none());
}

struct ExpiringNetwork {
    network: Arc<MemoryNetwork>,
    clock: Arc<TestClock>,
}
impl AuthNetwork for ExpiringNetwork {
    fn fetch(&self, request: ca::Request) -> BoxFuture<'_, auth::Result<ca::Response>> {
        Box::pin(async move {
            let expire = request.url.contains("getAccessTokenBySsKey");
            let result = self.network.fetch(request).await;
            if expire {
                self.clock.0.store(NOW + 300_000, Ordering::SeqCst);
            }
            result
        })
    }
}
#[tokio::test]
async fn qr_expiring_during_network_confirmation_never_persists() {
    let db = Storage::open(":memory:", 64).await.unwrap();
    let clock = Arc::new(TestClock(AtomicU64::new(NOW)));
    let mut steps = cloud_init();
    steps.extend(cloud_confirm());
    let network = Arc::new(ExpiringNetwork {
        network: MemoryNetwork::new(steps),
        clock: clock.clone(),
    });
    let manager = AuthManager::with_network(db.clone(), json!({}), network, clock)
        .await
        .unwrap();
    let qr = call(&manager, "cloud189_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    assert_eq!(
        call(&manager, "cloud189_check", &p).await["data"]["status"],
        "expired"
    );
    assert!(db.get("c189_login_default").await.unwrap().is_none());
    assert_eq!(manager.revision(), 0);
}

// Everything below uses synthetic accounts, keys and in-memory transports: no real login.
fn assert_mobile_public(value: &Value, token: &str) {
    let text = value.to_string();
    for private in ["15500001234", token, "Basic ", &authorization(token)] {
        assert!(
            !text.contains(private),
            "public mobile response leaked synthetic credentials"
        );
    }
}

fn mobile_confirm(token: &str) -> Step {
    response(
        "user/thirdlogin",
        json!({"code":"0000","data":{"account":"15500001234","token":token}}),
    )
}

#[tokio::test]
async fn mobile_rejected_keys_publish_no_qr_write_nothing_and_consume_no_session_capacity() {
    let invalid = [
        json!({"success":true,"data":{}}),
        json!({"success":false,"data":{"publicKey":"synthetic-secret"}}),
        json!({"success":true,"data":{"publicKey":"not-a-public-key"}}),
        json!({"success":true,"data":{"publicKey":""}}),
        json!({"success":true,"data":{"publicKey":123}}),
        json!({"success":"true","data":{}}),
    ];
    // More failures than the 128-session capacity also prove failed bootstraps
    // do not leave invisible sessions behind. Then a valid bootstrap must work.
    let mut steps: Vec<_> = (0..130)
        .map(|i| response("getRsaPublicKey", invalid[i % invalid.len()].clone()))
        .collect();
    steps.push(text("getRsaPublicKey", "synthetic-secret-not-json"));
    steps.push(Step {
        path: "getRsaPublicKey",
        response: None,
    });
    let mut denied = mobile_init().pop().unwrap();
    denied.response.as_mut().unwrap().status = 503;
    steps.push(denied);
    steps.extend(mobile_init());
    let (manager, db, net, _) = setup(json!({}), steps).await;
    for _ in 0..133 {
        let failed = call(&manager, "mcloud_qrcode", &Params::default()).await;
        assert_eq!(failed["code"], 502);
        assert_eq!(failed["success"], false);
        assert!(failed["data"].is_null());
        assert!(!failed.to_string().contains("synthetic-secret"));
        assert_eq!(manager.revision(), 0);
        assert!(db.get("mcloud_login_default").await.unwrap().is_none());
    }
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    assert_eq!(qr["code"], 200);
    assert!(uuid::Uuid::parse_str(qr["data"]["device_id"].as_str().unwrap()).is_ok());
    assert_eq!(net.count(), 134);
    assert!(net.drained());
}

// Independently generate the website response envelope rather than calling the
// production encrypt/decrypt helpers or changing any frozen protocol fixtures.
fn encrypted_mobile_confirmation(token: &str, json_wrapped: bool) -> String {
    use aes::cipher::{BlockEncrypt, BlockEncryptMut, KeyInit, KeyIvInit, block_padding::Pkcs7};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let mut data = json!({"account":"15500001234","token":token})
        .to_string()
        .into_bytes();
    let padding = 16 - data.len() % 16;
    data.extend(std::iter::repeat_n(padding as u8, padding));
    let cipher = aes::Aes128::new_from_slice(b"qPqDw263XgFgL3u8").unwrap();
    for block in data.chunks_exact_mut(16) {
        cipher.encrypt_block(aes::cipher::generic_array::GenericArray::from_mut_slice(
            block,
        ));
    }
    let hex: String = data.iter().map(|b| format!("{b:02x}")).collect();
    let envelope = json!({"code":"0000","data":hex}).to_string();
    let iv = [0x42; 16];
    let ciphertext = cbc::Encryptor::<aes::Aes256>::new(
        b"UqEZkrjCKfa02pP6jntzFmkzOz86zHUC".into(),
        (&iv).into(),
    )
    .encrypt_padded_vec_mut::<Pkcs7>(envelope.as_bytes());
    let mut wire = iv.to_vec();
    wire.extend(ciphertext);
    let encoded = STANDARD.encode(wire);
    if json_wrapped {
        serde_json::to_string(&encoded).unwrap()
    } else {
        encoded
    }
}

#[tokio::test]
async fn mobile_synthetic_cbc_hex_ecb_confirmation_persists_without_public_secrets() {
    for wrapped in [false, true] {
        let mut steps = mobile_init();
        steps.push(text(
            "user/thirdlogin",
            &encrypted_mobile_confirmation("synthetic-encrypted-token", wrapped),
        ));
        let (manager, db, net, clock) = setup(json!({}), steps).await;
        let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
        let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
        let confirmed = call(&manager, "mcloud_check", &p).await;
        assert_eq!(confirmed["code"], 200);
        assert_eq!(confirmed["data"]["status"], "confirmed");
        assert_mobile_public(&confirmed, "synthetic-encrypted-token");
        let saved = db.get("mcloud_login_default").await.unwrap().unwrap();
        assert_eq!(
            saved["authorization"],
            authorization("synthetic-encrypted-token")
        );
        assert_eq!(saved["source"], "qrcode");
        let restarted = AuthManager::with_network(db, json!({}), net.clone(), clock)
            .await
            .unwrap();
        assert_eq!(
            restarted.snapshot()["mcloud"]["authorization"],
            saved["authorization"]
        );
        assert_mobile_public(
            &call(&restarted, "login_status", &Params::default()).await,
            "synthetic-encrypted-token",
        );
        assert_eq!(
            call(&manager, "mcloud_check", &p).await["data"]["status"],
            "expired"
        );
        assert_eq!(manager.revision(), 1);
        assert_eq!(net.count(), 2);
        assert!(net.drained());
    }
}

#[tokio::test]
async fn mobile_unknown_malformed_and_transport_errors_are_502_not_waiting_and_keep_credentials() {
    let errors = [
        json!({"code":"unknown-synthetic-secret","message":"15500001234 Basic synthetic-secret"}),
        json!({"code":0,"data":{"account":"15500001234","token":"synthetic-secret"}}),
        json!({"code":"0000","data":{"account":"15500001234"}}),
        json!({"success":false,"code":"0000","data":{"account":"15500001234","token":"synthetic-secret"}}),
        json!({"code":"0000","data":{"result":{"resultCode":"200059541"},"account":"15500001234","token":"synthetic-secret"}}),
    ];
    let mut steps = mobile_init();
    steps.extend(
        errors
            .iter()
            .cloned()
            .map(|v| response("user/thirdlogin", v)),
    );
    steps.push(text(
        "user/thirdlogin",
        "not-json-or-base64-synthetic-secret",
    ));
    steps.push(Step {
        path: "user/thirdlogin",
        response: None,
    });
    let mut unavailable = mobile_confirm("synthetic-secret");
    unavailable.response.as_mut().unwrap().status = 503;
    steps.push(unavailable);
    steps.push(response(
        "user/thirdlogin",
        json!({"code":"200059541","data":{}}),
    ));
    let (manager, db, net, _) = setup(json!({}), steps).await;
    let old = authorization("synthetic-old");
    assert_eq!(
        call(
            &manager,
            "mcloud_manual_save",
            &params("authorization", &old)
        )
        .await["code"],
        200
    );
    let saved = db.get("mcloud_login_default").await.unwrap();
    let snapshot = manager.snapshot();
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    for _ in 0..errors.len() + 3 {
        let failed = call(&manager, "mcloud_check", &p).await;
        assert_eq!(failed["code"], 502);
        assert_eq!(failed["success"], false);
        assert_ne!(failed["data"]["status"], "waiting");
        assert_mobile_public(&failed, "synthetic-secret");
        assert!(!failed.to_string().contains("unknown-synthetic-secret"));
        assert_eq!(manager.snapshot(), snapshot);
        assert_eq!(manager.revision(), 1);
        assert_eq!(db.get("mcloud_login_default").await.unwrap(), saved);
    }
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "waiting"
    );
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], old);
    assert!(net.drained());
}

#[tokio::test]
async fn mobile_provider_expiry_removes_session_and_never_polls_it_again() {
    for code in ["200059542", "200059549"] {
        let mut steps = mobile_init();
        steps.push(response("user/thirdlogin", json!({"code":code,"data":{}})));
        let (manager, db, net, _) = setup(json!({}), steps).await;
        let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
        let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
        for _ in 0..3 {
            let expired = call(&manager, "mcloud_check", &p).await;
            assert_eq!(expired["data"]["status"], "expired");
            assert_eq!(expired["success"], false);
        }
        assert_eq!(manager.revision(), 0);
        assert!(db.get("mcloud_login_default").await.unwrap().is_none());
        assert_eq!(net.count(), 2);
        assert!(net.drained());
    }
}

fn mobile_request_header<'a>(request: &'a ca::Request, name: &str) -> &'a str {
    request
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .unwrap()
        .1
        .as_str()
}

fn decrypt_mobile_request(request: &ca::Request) -> (Vec<u8>, Value) {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    assert_eq!(request.url, ma::THIRD_LOGIN_URL);
    let encoded: String = serde_json::from_str(request.body.as_deref().unwrap()).unwrap();
    let bytes = STANDARD.decode(encoded).unwrap();
    let plaintext = cbc::Decryptor::<aes::Aes256>::new(
        b"UqEZkrjCKfa02pP6jntzFmkzOz86zHUC".into(),
        (&bytes[..16]).into(),
    )
    .decrypt_padded_vec_mut::<Pkcs7>(&bytes[16..])
    .unwrap();
    (
        bytes[..16].to_vec(),
        serde_json::from_slice(&plaintext).unwrap(),
    )
}

#[tokio::test]
async fn mobile_sessions_isolate_sid_device_wrapped_key_and_fresh_poll_iv() {
    let mut steps = mobile_init();
    steps.extend(mobile_init());
    for code in ["200059541", "200059548", "200059542", "200059541"] {
        steps.push(response("user/thirdlogin", json!({"code":code,"data":{}})));
    }
    let (manager, db, net, _) = setup(json!({}), steps).await;
    let a = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let b = call(&manager, "mcloud_qrcode", &Params::default()).await;
    assert_ne!(a["data"]["sid"], b["data"]["sid"]);
    assert_ne!(a["data"]["device_id"], b["data"]["device_id"]);
    let pa = params("device_id", a["data"]["device_id"].as_str().unwrap());
    let pb = params("device_id", b["data"]["device_id"].as_str().unwrap());
    assert_eq!(
        call(&manager, "mcloud_check", &pa).await["data"]["status"],
        "waiting"
    );
    assert_eq!(
        call(&manager, "mcloud_check", &pb).await["data"]["status"],
        "scanned"
    );
    assert_eq!(
        call(&manager, "mcloud_check", &pa).await["data"]["status"],
        "expired"
    );
    assert_eq!(
        call(&manager, "mcloud_check", &pa).await["data"]["status"],
        "expired"
    );
    assert_eq!(
        call(&manager, "mcloud_check", &pb).await["data"]["status"],
        "waiting"
    );
    {
        let requests = net.requests.lock().unwrap();
        assert_eq!(requests.len(), 6);
        let mut ivs = Vec::new();
        for (poll_index, key_index, qr) in [(2, 0, &a), (3, 1, &b), (4, 0, &a), (5, 1, &b)] {
            let request = &requests[poll_index];
            let (iv, body) = decrypt_mobile_request(request);
            assert!(
                !ivs.contains(&iv),
                "each poll must receive fresh IV entropy"
            );
            ivs.push(iv);
            assert_eq!(body["dycpwd"], qr["data"]["sid"]);
            assert_eq!(body["pintype"], 21);
            assert_eq!(
                mobile_request_header(request, "x-deviceinfo"),
                mobile_request_header(&requests[key_index], "x-deviceinfo")
            );
            let qr_url = qr["data"]["qr_url"].as_str().unwrap();
            let query: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(qr_url.split_once('?').unwrap().1.as_bytes())
                    .into_owned()
                    .collect();
            assert_eq!(query["sID"], qr["data"]["sid"].as_str().unwrap());
            assert!(mobile_request_header(request, "x-deviceinfo").contains(&query["dID"]));
            assert!(!mobile_request_header(request, "mcloud-skey").is_empty());
        }
        assert_ne!(
            mobile_request_header(&requests[2], "x-deviceinfo"),
            mobile_request_header(&requests[3], "x-deviceinfo")
        );
        assert_ne!(
            mobile_request_header(&requests[2], "mcloud-skey"),
            mobile_request_header(&requests[3], "mcloud-skey")
        );
        assert_eq!(
            mobile_request_header(&requests[2], "mcloud-skey"),
            mobile_request_header(&requests[4], "mcloud-skey")
        );
    }
    assert_eq!(manager.revision(), 0);
    assert!(db.get("mcloud_login_default").await.unwrap().is_none());
    assert!(net.drained());
}

#[tokio::test]
async fn mobile_ttl_accepts_last_millisecond_but_exact_expiry_removes_without_polling() {
    let mut steps = mobile_init();
    steps.push(response(
        "user/thirdlogin",
        json!({"code":"200059541","data":{}}),
    ));
    let (manager, db, net, clock) = setup(json!({}), steps).await;
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    assert_eq!(qr["data"]["expires_in"], 300);
    clock.0.store(NOW + 299_999, Ordering::SeqCst);
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "waiting"
    );
    clock.0.store(NOW + 300_000, Ordering::SeqCst);
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    // Even a wall-clock rollback cannot resurrect a removed QR session.
    clock.0.store(NOW, Ordering::SeqCst);
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(net.count(), 2);
    assert_eq!(manager.revision(), 0);
    assert!(db.get("mcloud_login_default").await.unwrap().is_none());
    assert!(net.drained());
}

struct PausedMobileNetwork {
    network: Arc<MemoryNetwork>,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
}
impl AuthNetwork for PausedMobileNetwork {
    fn fetch(&self, request: ca::Request) -> BoxFuture<'_, auth::Result<ca::Response>> {
        Box::pin(async move {
            if request.url == ma::THIRD_LOGIN_URL {
                self.entered.notify_one();
                self.resume.notified().await;
            }
            self.network.fetch(request).await
        })
    }
}
async fn paused_mobile_setup() -> (
    AuthManager,
    Storage,
    Arc<PausedMobileNetwork>,
    Arc<TestClock>,
) {
    let db = Storage::open(":memory:", 64).await.unwrap();
    let mut steps = mobile_init();
    steps.push(mobile_confirm("synthetic-paused-token"));
    let net = Arc::new(PausedMobileNetwork {
        network: MemoryNetwork::new(steps),
        entered: tokio::sync::Notify::new(),
        resume: tokio::sync::Notify::new(),
    });
    let clock = Arc::new(TestClock(AtomicU64::new(NOW)));
    let manager = AuthManager::with_network(db.clone(), json!({}), net.clone(), clock.clone())
        .await
        .unwrap();
    (manager, db, net, clock)
}
async fn wait_for_mobile_poll(net: &PausedMobileNetwork) {
    tokio::time::timeout(std::time::Duration::from_secs(5), net.entered.notified())
        .await
        .expect("mobile poll never entered network");
}

#[tokio::test]
async fn mobile_confirmation_expiring_in_network_cannot_publish_or_persist() {
    let (manager, db, net, clock) = paused_mobile_setup().await;
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    let expire = async {
        wait_for_mobile_poll(&net).await;
        clock.0.store(NOW + 300_000, Ordering::SeqCst);
        net.resume.notify_one();
    };
    let (result, ()) = tokio::join!(call(&manager, "mcloud_check", &p), expire);
    assert_eq!(result["data"]["status"], "expired");
    assert_mobile_public(&result, "synthetic-paused-token");
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(manager.revision(), 0);
    assert!(db.get("mcloud_login_default").await.unwrap().is_none());
    assert_eq!(net.network.count(), 2);
    assert!(net.network.drained());
}

#[tokio::test]
async fn mobile_logout_invalidates_pending_qr_and_restart_keeps_tombstone() {
    let initial = json!({"mcloud":{"authorization":authorization("synthetic-old")}});
    let (manager, db, net, clock) = setup(initial.clone(), mobile_init()).await;
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    assert_eq!(
        call(&manager, "mcloud_logout", &Params::default()).await["code"],
        200
    );
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], "");
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["_jxpan_logged_out"],
        true
    );
    let restarted = AuthManager::with_network(db, initial, net.clone(), clock)
        .await
        .unwrap();
    assert_eq!(restarted.snapshot()["mcloud"]["authorization"], "");
    assert_eq!(
        call(&restarted, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(manager.revision(), 1);
    assert_eq!(net.count(), 1);
    assert!(net.drained());
}

#[tokio::test]
async fn mobile_logout_during_confirmation_rejects_stale_network_credentials() {
    let (manager, db, net, _) = paused_mobile_setup().await;
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    let logout = async {
        wait_for_mobile_poll(&net).await;
        assert_eq!(
            call(&manager, "mcloud_logout", &Params::default()).await["code"],
            200
        );
        net.resume.notify_one();
    };
    let (result, ()) = tokio::join!(call(&manager, "mcloud_check", &p), logout);
    assert_eq!(result["code"], 409);
    assert_mobile_public(&result, "synthetic-paused-token");
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], "");
    assert_eq!(manager.revision(), 1);
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["_jxpan_logged_out"],
        true
    );
    assert_eq!(net.network.count(), 2);
    assert!(net.network.drained());
}

#[tokio::test]
async fn mobile_concurrent_checks_confirm_once_with_exactly_one_database_write() {
    let (manager, db, net, _) = paused_mobile_setup().await;
    db.with_connection(|db| {
        db.execute_batch(
            "CREATE TABLE mobile_write_audit (n INTEGER);
            CREATE TRIGGER count_mobile_save AFTER INSERT ON kv_store
            WHEN NEW.key = 'mcloud_login_default'
            BEGIN INSERT INTO mobile_write_audit VALUES (1); END;",
        )
        .map_err(|_| storage::StorageError::Database)
    })
    .await
    .unwrap();
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    let resume = async {
        wait_for_mobile_poll(&net).await;
        // join! has polled both checks before this branch releases the first.
        tokio::task::yield_now().await;
        net.resume.notify_one();
    };
    let (a, b, ()) = tokio::join!(
        call(&manager, "mcloud_check", &p),
        call(&manager, "mcloud_check", &p),
        resume
    );
    assert_eq!(a["data"]["status"], "confirmed");
    assert_eq!(b["data"]["status"], "expired");
    assert_mobile_public(&a, "synthetic-paused-token");
    assert_mobile_public(&b, "synthetic-paused-token");
    assert_eq!(manager.revision(), 1);
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["authorization"],
        authorization("synthetic-paused-token")
    );
    let writes: i64 = db
        .with_connection(|db| {
            db.query_row("SELECT count(*) FROM mobile_write_audit", [], |row| {
                row.get(0)
            })
            .map_err(|_| storage::StorageError::Database)
        })
        .await
        .unwrap();
    assert_eq!(writes, 1);
    assert_eq!(net.network.count(), 2);
    assert!(net.network.drained());
}

#[tokio::test]
async fn mobile_confirmation_storage_failure_keeps_old_snapshot_and_can_retry_same_qr() {
    let mut steps = mobile_init();
    steps.push(mobile_confirm("synthetic-new"));
    steps.push(mobile_confirm("synthetic-new"));
    let old = authorization("synthetic-old");
    let (manager, db, net, _) = setup(json!({"mcloud":{"authorization":old}}), steps).await;
    db.with_connection(|db| {
        db.execute_batch(
            "CREATE TRIGGER fail_mobile_confirmation BEFORE INSERT ON kv_store
            BEGIN SELECT RAISE(ABORT, 'synthetic'); END;",
        )
        .map_err(|_| storage::StorageError::Database)
    })
    .await
    .unwrap();
    let qr = call(&manager, "mcloud_qrcode", &Params::default()).await;
    let p = params("device_id", qr["data"]["device_id"].as_str().unwrap());
    let failed = call(&manager, "mcloud_check", &p).await;
    assert_eq!(failed["code"], 500);
    assert_mobile_public(&failed, "synthetic-new");
    assert_eq!(manager.snapshot()["mcloud"]["authorization"], old);
    assert_eq!(manager.revision(), 0);
    assert!(db.get("mcloud_login_default").await.unwrap().is_none());
    db.with_connection(|db| {
        db.execute_batch("DROP TRIGGER fail_mobile_confirmation")
            .map_err(|_| storage::StorageError::Database)
    })
    .await
    .unwrap();
    let confirmed = call(&manager, "mcloud_check", &p).await;
    assert_eq!(confirmed["data"]["status"], "confirmed");
    assert_mobile_public(&confirmed, "synthetic-new");
    assert_eq!(
        manager.snapshot()["mcloud"]["authorization"],
        authorization("synthetic-new")
    );
    assert_eq!(
        db.get("mcloud_login_default").await.unwrap().unwrap()["authorization"],
        authorization("synthetic-new")
    );
    assert_eq!(manager.revision(), 1);
    assert_eq!(
        call(&manager, "mcloud_check", &p).await["data"]["status"],
        "expired"
    );
    assert_eq!(net.count(), 3);
    assert!(net.drained());
}
