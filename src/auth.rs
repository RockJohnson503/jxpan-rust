//! Private credential lifecycle. HTTP callers must authorize before invoking mutations.
use crate::{
    cache::{BoundedTtlCache, CacheConfig},
    http::params::Params,
    providers::{cloud189_auth as ca, mcloud_auth as ma},
    storage::Storage,
};
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex as AsyncMutex;

const TTL_MS: u64 = 300_000;
const MAX_SESSIONS: usize = 128;
const MAX_BODY: usize = 2 * 1024 * 1024;
pub type Result<T> = std::result::Result<T, AuthError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthError {
    Storage,
    InvalidCredential,
    Network,
    Conflict,
    Capacity,
}
impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "auth operation failed: {self:?}")
    }
}
impl std::error::Error for AuthError {}

/// Injection point for memory transcripts. Implementations must bound bytes/time,
/// forbid arbitrary hosts, and avoid logging requests, responses or upstream errors.
pub trait AuthNetwork: Send + Sync {
    fn fetch(&self, request: ca::Request) -> BoxFuture<'_, Result<ca::Response>>;
}
pub trait AuthClock: Send + Sync {
    fn now_ms(&self) -> u64;
}
struct Clock;
impl AuthClock for Clock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
}
struct KernelClock(Arc<dyn AuthClock>);
impl ca::Runtime for KernelClock {
    fn now_ms(&self) -> u64 {
        self.0.now_ms()
    }
    fn random(&mut self) -> f64 {
        let mut r = ca::SystemRuntime;
        ca::Runtime::random(&mut r)
    }
    fn uuid(&mut self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
}
struct Adapter(Arc<dyn AuthNetwork>);
impl ca::Transport for Adapter {
    async fn fetch(
        &mut self,
        request: ca::Request,
    ) -> std::result::Result<ca::Response, ca::AuthError> {
        self.0
            .fetch(request)
            .await
            .map_err(|_| ca::AuthError::Transport)
    }
}
struct ProductionNetwork {
    cloud: ca::ReqwestTransport,
    http: reqwest::Client,
}
impl AuthNetwork for ProductionNetwork {
    fn fetch(&self, request: ca::Request) -> BoxFuture<'_, Result<ca::Response>> {
        Box::pin(async move {
            tokio::time::timeout(Duration::from_secs(20), async {
                if ![ma::KEY_URL, ma::THIRD_LOGIN_URL, ma::REFRESH_URL]
                    .contains(&request.url.as_str())
                {
                    ca::allowed_url(&request.url).map_err(|_| AuthError::Network)?;
                    return ca::Transport::fetch(&mut self.cloud.clone(), request)
                        .await
                        .map_err(|_| AuthError::Network);
                }
                // The supplied pool MUST disable automatic redirects and cookie jars.
                // Mobile auth endpoints do not require redirects; reject all 3xx.
                let mut builder = self.http.request(reqwest::Method::POST, &request.url);
                for (key, value) in request.headers {
                    builder = builder.header(key, value);
                }
                let mut response = builder
                    .body(request.body.unwrap_or_default())
                    .send()
                    .await
                    .map_err(|_| AuthError::Network)?;
                if response.url().as_str() != request.url
                    || response.status().is_redirection()
                    || response
                        .content_length()
                        .is_some_and(|n| n > MAX_BODY as u64)
                {
                    return Err(AuthError::Network);
                }
                let status = response.status().as_u16();
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|_| AuthError::Network)? {
                    if bytes.len() + chunk.len() > MAX_BODY {
                        return Err(AuthError::Network);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(ca::Response {
                    status,
                    headers: Vec::new(),
                    text: String::from_utf8_lossy(&bytes).into_owned(),
                })
            })
            .await
            .map_err(|_| AuthError::Network)?
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provider {
    Cloud,
    Mobile,
}
impl Provider {
    fn index(self) -> usize {
        if self == Self::Cloud { 0 } else { 1 }
    }
    fn name(self) -> &'static str {
        if self == Self::Cloud {
            "cloud189"
        } else {
            "mcloud"
        }
    }
    fn key(self) -> &'static str {
        if self == Self::Cloud {
            "c189_login_default"
        } else {
            "mcloud_login_default"
        }
    }
}
/// A request can adopt renewed credentials, never a different login/account.
#[derive(Clone)]
pub(crate) struct CredentialLease {
    provider: Provider,
    generation: u64,
    snapshot: Arc<Value>,
}
#[derive(Clone)]
enum QrData {
    Cloud(Value),
    Mobile(ma::Session),
}
struct QrSession {
    provider: Provider,
    epoch: u64,
    expires: u64,
    deadline: std::time::Instant,
    data: QrData,
    poll: AsyncMutex<()>,
}
impl QrSession {
    fn expired(&self, now: u64) -> bool {
        now >= self.expires || std::time::Instant::now() >= self.deadline
    }
}
struct State {
    snapshot: Arc<Value>,
    revision: u64,
    epochs: [u64; 2],
    generations: [u64; 2],
    info: [Option<Value>; 2],
    sessions: HashMap<String, Arc<QrSession>>,
}
struct Inner {
    storage: Storage,
    state: Mutex<State>,
    commit: [AsyncMutex<()>; 2],
    refresh_operation: [AsyncMutex<()>; 2],
    network: Arc<dyn AuthNetwork>,
    clock: Arc<dyn AuthClock>,
    refresh: BoundedTtlCache<Result<bool>>,
    timezone_offset: i64,
    operations: tokio::sync::Semaphore,
    commits: Arc<tokio::sync::Semaphore>,
}
#[derive(Clone)]
pub struct AuthManager {
    inner: Arc<Inner>,
}

impl AuthManager {
    /// `initial` is a private, already-loaded configuration, not a filename. No network I/O.
    pub async fn new(storage: Storage, http: reqwest::Client, initial: Value) -> Result<Self> {
        let network = ProductionNetwork {
            cloud: ca::ReqwestTransport::with_client(http.clone()),
            http,
        };
        Self::with_network(storage, initial, Arc::new(network), Arc::new(Clock)).await
    }
    /// Memory-only test adapter; supplying one does not enable or contact production endpoints.
    pub async fn with_network(
        storage: Storage,
        mut initial: Value,
        network: Arc<dyn AuthNetwork>,
        clock: Arc<dyn AuthClock>,
    ) -> Result<Self> {
        if !initial.is_object() {
            initial = json!({});
        }
        if !initial["mcloud"].is_object() {
            initial["mcloud"] = json!({});
        }
        for (env, field) in [
            ("MCLOUD_ENABLED", "enabled"),
            ("MCLOUD_USER_AGENT", "userAgent"),
        ] {
            if initial["mcloud"].get(field).is_none() {
                let value = initial[env]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| std::env::var(env).ok());
                if let Some(value) = value {
                    initial["mcloud"][field] = if field == "enabled" {
                        json!(value != "false")
                    } else {
                        json!(value)
                    };
                }
            }
        }
        let offset = initial["auth_timezone_offset_seconds"]
            .as_i64()
            .unwrap_or(28800)
            .clamp(-86400, 86400);
        let now = clock.now_ms();
        let mut info: [Option<Value>; 2] = [None, None];
        for provider in [Provider::Cloud, Provider::Mobile] {
            let stored = storage
                .get(provider.key())
                .await
                .map_err(|_| AuthError::Storage)?;
            let value = match stored {
                Some(v) => {
                    let decoded = decode_stored(v, provider)?;
                    if decoded["_jxpan_logged_out"] == true {
                        None
                    } else {
                        Some(normalize(provider, decoded, now)?)
                    }
                }
                None => seed(&initial, provider, now)?,
            };
            info[provider.index()] = value;
            project(&mut initial, provider, info[provider.index()].as_ref());
        }
        Ok(Self {
            inner: Arc::new(Inner {
                storage,
                state: Mutex::new(State {
                    snapshot: Arc::new(initial),
                    revision: 0,
                    epochs: [0, 0],
                    generations: [0, 0],
                    info,
                    sessions: HashMap::new(),
                }),
                commit: [AsyncMutex::new(()), AsyncMutex::new(())],
                refresh_operation: [AsyncMutex::new(()), AsyncMutex::new(())],
                network,
                clock,
                operations: tokio::sync::Semaphore::new(8),
                commits: Arc::new(tokio::sync::Semaphore::new(8)),
                refresh: BoundedTtlCache::new(CacheConfig {
                    ttl: Duration::from_secs(60),
                    max_entries: 16,
                    max_bytes: 4096,
                    max_key_bytes: 64,
                    max_in_flight: 2,
                }),
                timezone_offset: offset,
            }),
        })
    }
    pub fn snapshot(&self) -> Arc<Value> {
        Arc::clone(
            &self
                .inner
                .state
                .lock()
                .expect("auth state poisoned")
                .snapshot,
        )
    }
    pub fn revision(&self) -> u64 {
        self.inner
            .state
            .lock()
            .expect("auth state poisoned")
            .revision
    }
    /// Read revision and credentials under one lock to prevent mixed-account cache keys.
    pub fn versioned_snapshot(&self) -> (u64, Arc<Value>) {
        let state = self.inner.state.lock().expect("auth state poisoned");
        (state.revision, state.snapshot.clone())
    }
    fn current(&self, provider: Provider) -> (u64, Option<Value>) {
        let state = self.inner.state.lock().expect("auth state poisoned");
        (
            state.epochs[provider.index()],
            state.info[provider.index()].clone(),
        )
    }
    fn kernel(&self) -> ca::AuthKernel<Adapter, KernelClock> {
        ca::AuthKernel {
            transport: Adapter(self.inner.network.clone()),
            runtime: KernelClock(self.inner.clock.clone()),
        }
    }
    async fn fetch_mobile(&self, request: ma::Request) -> Result<ca::Response> {
        self.inner
            .network
            .fetch(ca::Request {
                url: request.url.into(),
                method: request.method,
                headers: request.headers.into_iter().collect(),
                body: Some(request.body),
                redirect: "manual",
            })
            .await
    }
    fn signature_input(&self) -> Result<(String, String)> {
        let mut bytes = [0; 16];
        getrandom::getrandom(&mut bytes).map_err(|_| AuthError::Network)?;
        Ok((
            timestamp(self.inner.clock.now_ms(), self.inner.timezone_offset),
            ma::nonce(&bytes),
        ))
    }
    /// Persistence precedes publication. Async commit locks serialize only storage/CAS,
    /// never network. A provider epoch rejects stale refresh and old QR confirmation.
    async fn save(
        &self,
        provider: Provider,
        info: Option<Value>,
        expected: Option<u64>,
        qr: Option<&str>,
    ) -> Result<()> {
        // Once the write is accepted, complete persistence+publication even if
        // the HTTP waiter disconnects between the worker reply and hot swap.
        let manager = self.clone();
        let qr = qr.map(str::to_owned);
        let permit = self
            .inner
            .commits
            .clone()
            .try_acquire_owned()
            .map_err(|_| AuthError::Capacity)?;
        tokio::spawn(async move {
            let _permit = permit;
            manager
                .save_inner(provider, info, expected, qr.as_deref())
                .await
        })
        .await
        .map_err(|_| AuthError::Storage)?
    }
    async fn save_inner(
        &self,
        provider: Provider,
        info: Option<Value>,
        expected: Option<u64>,
        qr: Option<&str>,
    ) -> Result<()> {
        let info = info
            .map(|v| normalize(provider, v, self.inner.clock.now_ms()))
            .transpose()?;
        let _commit = self.inner.commit[provider.index()].lock().await;
        {
            let state = self.inner.state.lock().expect("auth state poisoned");
            if expected.is_some_and(|e| state.epochs[provider.index()] != e)
                || qr.is_some_and(|id| {
                    state
                        .sessions
                        .get(id)
                        .is_none_or(|s| s.expired(self.inner.clock.now_ms()))
                })
            {
                return Err(AuthError::Conflict);
            }
        }
        self.inner
            .storage
            .put(
                provider.key(),
                info.clone()
                    .unwrap_or_else(|| json!({"_jxpan_logged_out":true})),
                None,
            )
            .await
            .map_err(|_| AuthError::Storage)?;
        let mut state = self.inner.state.lock().expect("auth state poisoned");
        let mut snapshot = (*state.snapshot).clone();
        project(&mut snapshot, provider, info.as_ref());
        state.snapshot = Arc::new(snapshot);
        state.info[provider.index()] = info;
        state.epochs[provider.index()] += 1;
        // Only a renewal retains request identity. QR/manual login and logout invalidate leases.
        if expected.is_none() || qr.is_some() {
            state.generations[provider.index()] += 1;
        }
        state.revision += 1;
        if let Some(id) = qr {
            state.sessions.remove(id);
        } else if expected.is_none() {
            state.sessions.retain(|_, s| s.provider != provider);
        }
        Ok(())
    }
    fn insert_qr(&self, provider: Provider, epoch: u64, data: QrData) -> Result<String> {
        let now = self.inner.clock.now_ms();
        let mut state = self.inner.state.lock().expect("auth state poisoned");
        state.sessions.retain(|_, s| !s.expired(now));
        if state.epochs[provider.index()] != epoch {
            return Err(AuthError::Conflict);
        }
        if state.sessions.len() >= MAX_SESSIONS {
            return Err(AuthError::Capacity);
        }
        let id = uuid::Uuid::new_v4().to_string();
        state.sessions.insert(
            id.clone(),
            Arc::new(QrSession {
                provider,
                epoch,
                expires: now.saturating_add(TTL_MS),
                deadline: std::time::Instant::now() + Duration::from_millis(TTL_MS),
                data,
                poll: AsyncMutex::new(()),
            }),
        );
        Ok(id)
    }
    async fn qrcode(&self, provider: Provider) -> Result<Value> {
        let epoch = self.current(provider).0;
        if provider == Provider::Cloud {
            let qr = self
                .kernel()
                .init_qr_login()
                .await
                .map_err(|_| AuthError::Network)?;
            let url = qr["qrUrl"].clone();
            let id = self.insert_qr(provider, epoch, QrData::Cloud(qr))?;
            Ok(
                json!({"code":200,"msg":"获取二维码成功","success":true,"data":{"qr_url":url,"device_id":id,"expires_in":300,"interval":3}}),
            )
        } else {
            let mut session = ma::Session::from_uuids(
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
                &uuid::Uuid::new_v4().to_string(),
            );
            let (time, nonce) = self.signature_input()?;
            let response = self
                .fetch_mobile(ma::web_key_request(&session, &time, &nonce))
                .await?;
            if response.status != 200 {
                return Err(AuthError::Network);
            }
            let value: Value =
                serde_json::from_str(&response.text).map_err(|_| AuthError::Network)?;
            let (n, _) = ma::web_key(&value).map_err(|_| AuthError::Network)?;
            let width = n.len();
            let length = width
                .checked_sub(session.aes_key.len() + 3)
                .filter(|n| (8..=512).contains(n))
                .ok_or(AuthError::Network)?;
            let mut padding = vec![0; length];
            getrandom::getrandom(&mut padding).map_err(|_| AuthError::Network)?;
            ma::finish_web_key(&mut session, &value, &padding).map_err(|_| AuthError::Network)?;
            if session.mcloud_skey.is_empty() {
                return Err(AuthError::Network);
            }
            let mut response = session.qr_response();
            let id = self.insert_qr(provider, epoch, QrData::Mobile(session))?;
            response["data"]["device_id"] = json!(id);
            Ok(response)
        }
    }
    async fn check(&self, provider: Provider, id: &str) -> Result<Value> {
        let session = {
            let mut state = self.inner.state.lock().expect("auth state poisoned");
            state
                .sessions
                .retain(|_, s| !s.expired(self.inner.clock.now_ms()));
            state
                .sessions
                .get(id)
                .filter(|s| s.provider == provider)
                .cloned()
        };
        let Some(session) = session else {
            return Ok(ma::poll_missing_session());
        };
        let _poll = session.poll.lock().await;
        if session.expired(self.inner.clock.now_ms())
            || self.current(provider).0 != session.epoch
            || !self
                .inner
                .state
                .lock()
                .expect("auth state poisoned")
                .sessions
                .contains_key(id)
        {
            return Ok(ma::poll_missing_session());
        }
        let mut result = match &session.data {
            QrData::Cloud(qr) => self
                .kernel()
                .check_and_login(qr)
                .await
                .map_err(|_| AuthError::Network)?,
            QrData::Mobile(qr) => {
                let (time, nonce) = self.signature_input()?;
                let mut iv = [0; 16];
                getrandom::getrandom(&mut iv).map_err(|_| AuthError::Network)?;
                let request =
                    ma::web_poll_request(qr, &time, &nonce, &iv).map_err(|_| AuthError::Network)?;
                let response = self.fetch_mobile(request).await?;
                if response.status != 200 {
                    return Err(AuthError::Network);
                }
                ma::web_poll_response_text(&response.text).map_err(|_| AuthError::Network)?
            }
        };
        if session.expired(self.inner.clock.now_ms()) {
            return Ok(ma::poll_missing_session());
        }
        let info = match provider {
            Provider::Cloud => result["data"].get("loginInfo").cloned(),
            Provider::Mobile => result["data"]["authorization"].as_str().map(|auth| json!({"authorization":auth,"phone":result["data"]["phone"],"timestamp":self.inner.clock.now_ms(),"source":"qrcode"})),
        };
        if let Some(info) = info {
            if let Err(error) = self
                .save(provider, Some(info), Some(session.epoch), Some(id))
                .await
            {
                if error == AuthError::Conflict && session.expired(self.inner.clock.now_ms()) {
                    return Ok(ma::poll_missing_session());
                }
                return Err(error);
            }
            result = json!({"code":200,"msg":"登录成功","success":true,"data":{"status":if provider == Provider::Cloud {"success"} else {"confirmed"}}});
        } else if result["data"]["status"] == "expired" {
            self.inner
                .state
                .lock()
                .expect("auth state poisoned")
                .sessions
                .remove(id);
        }
        // Never return provider error text: an upstream echo may contain secrets.
        if result["success"] == false && result["data"]["status"] != "expired" {
            return Err(AuthError::Network);
        }
        let state = result["data"]["status"].as_str().unwrap_or("polling");
        let state = if [
            "waiting",
            "scanned",
            "expired",
            "polling",
            "success",
            "confirmed",
        ]
        .contains(&state)
        {
            state
        } else {
            "polling"
        };
        Ok(
            json!({"code":if state == "expired" {400} else {200},"success":state != "expired","msg":state,"data":{"status":state}}),
        )
    }
    /// Automatic renewal is mandatory on the managed parse/download path. Status
    /// and initialization remain read-only; only use of an account can renew it.
    pub async fn refresh_cloud189(&self) -> Result<bool> {
        self.refresh(Provider::Cloud).await
    }
    pub async fn refresh_mcloud(&self) -> Result<bool> {
        self.refresh(Provider::Mobile).await
    }
    pub(crate) fn request_lease(
        &self,
        provider: Provider,
        snapshot: &Arc<Value>,
    ) -> Option<CredentialLease> {
        let state = self.inner.state.lock().expect("auth state poisoned");
        (Arc::ptr_eq(snapshot, &state.snapshot) && state.info[provider.index()].is_some()).then(
            || CredentialLease {
                provider,
                generation: state.generations[provider.index()],
                snapshot: snapshot.clone(),
            },
        )
    }
    fn lease_snapshot(&self, lease: &CredentialLease) -> Result<(u64, Arc<Value>)> {
        let state = self.inner.state.lock().expect("auth state poisoned");
        if state.generations[lease.provider.index()] != lease.generation {
            return Err(AuthError::Conflict);
        }
        Ok((state.revision, state.snapshot.clone()))
    }
    pub(crate) async fn refresh_for_request(
        &self,
        lease: &CredentialLease,
        rejected: bool,
    ) -> Result<Option<(u64, Arc<Value>)>> {
        let provider = lease.provider;
        let (epoch, info) = {
            let state = self.inner.state.lock().expect("auth state poisoned");
            if state.generations[provider.index()] != lease.generation {
                return Err(AuthError::Conflict);
            }
            if rejected && credential_changed(provider, &lease.snapshot, &state.snapshot) {
                return Ok(Some((state.revision, state.snapshot.clone())));
            }
            (
                state.epochs[provider.index()],
                state.info[provider.index()].clone(),
            )
        };
        let Some(info) = info else { return Ok(None) };
        if !rejected && provider == Provider::Cloud {
            let missing = ["session_key", "session_secret", "open_access_token"]
                .iter()
                .any(|key| info[*key].as_str().is_none_or(str::is_empty));
            let expired = info["expires_at"]
                .as_u64()
                .is_some_and(|t| self.inner.clock.now_ms() / 1000 >= t);
            if !missing && !expired {
                return Ok(None);
            }
        }
        let result = self.refresh_expected(provider, epoch, info, rejected).await;
        let latest = self.lease_snapshot(lease)?;
        if credential_changed(provider, &lease.snapshot, &latest.1) || (!rejected && result.is_ok())
        {
            return Ok(Some(latest));
        }
        result?;
        Ok(None)
    }
    async fn refresh(&self, provider: Provider) -> Result<bool> {
        let (epoch, info) = self.current(provider);
        let Some(info) = info else { return Ok(false) };
        self.refresh_expected(provider, epoch, info, false).await
    }
    async fn refresh_expected(
        &self,
        provider: Provider,
        epoch: u64,
        info: Value,
        rejected: bool,
    ) -> Result<bool> {
        let now = self.inner.clock.now_ms();
        let cooling_down = if provider == Provider::Cloud {
            info["last_on_demand_refresh_at"]
                .as_u64()
                .is_some_and(|t| now / 1000 < t.saturating_add(60))
        } else {
            // Persisted cooldown survives both renewal epochs and process restarts,
            // including a supplier claiming success without advancing the token.
            info["mcloud_auto_refreshed_at"]
                .as_u64()
                .is_some_and(|t| now < t.saturating_add(60_000))
        };
        if cooling_down {
            return if rejected {
                Err(AuthError::Network)
            } else {
                Ok(false)
            };
        }
        let manager = self.clone();
        let result = self
            .inner
            .refresh
            .get_or_init(
                format!("{}:{epoch}:{}", provider.name(), u8::from(rejected)),
                move || async move {
                    // Window and rejection-triggered renewals must not race each other.
                    let _operation = manager.inner.refresh_operation[provider.index()]
                        .lock()
                        .await;
                    if manager.current(provider).0 != epoch {
                        return Err(AuthError::Conflict);
                    }
                    if provider == Provider::Cloud {
                        let updated = manager
                            .kernel()
                            .refresh_on_demand(&info)
                            .await
                            .map_err(|_| AuthError::Network)?;
                        let Some(updated) = updated else {
                            return Ok(false);
                        };
                        manager
                            .save(provider, Some(updated), Some(epoch), None)
                            .await?;
                        Ok(true)
                    } else {
                        let plan = ma::prepare_refresh_for_rejection(
                            Some(&info),
                            manager.inner.clock.now_ms() as i64,
                            rejected,
                        );
                        let response = if let Some(request) = plan.request.clone() {
                            manager.fetch_mobile(request).await.ok()
                        } else {
                            None
                        };
                        let result = ma::finish_refresh(
                            plan,
                            response
                                .as_ref()
                                .map(|r| (r.status == 200, r.text.as_str())),
                        );
                        if let Some(updated) = result.write_info {
                            manager
                                .save(provider, Some(updated), Some(epoch), None)
                                .await?;
                        }
                        Ok(result.refreshed)
                    }
                },
                // Share errors with current waiters, but allow the next request to retry.
                // Mobile's persisted provider backoff remains an ordinary Ok(false).
                |result| result.as_ref().ok().map(|_| 16),
            )
            .await
            .map_err(|_| AuthError::Capacity)?;
        *result
    }
    pub async fn handle(
        &self,
        action: &str,
        params: &Params,
        allow_secrets: bool,
    ) -> Option<Value> {
        let provider = match action {
            "cloud189_qrcode"
            | "cloud189_check"
            | "cloud189_status"
            | "cloud189_manual_save"
            | "cloud189_logout" => Provider::Cloud,
            "mcloud_qrcode" | "mcloud_check" | "mcloud_manual_save" | "mcloud_logout" => {
                Provider::Mobile
            }
            "login_status" => {
                return Some(
                    json!({"code":200,"success":true,"msg":"登录状态","data":{"cloud189":self.status(Provider::Cloud),"mcloud":self.status(Provider::Mobile)}}),
                );
            }
            _ => return None,
        };
        if action == "cloud189_status" {
            return Some(json!({"code":200,"success":true,"data":self.status(provider)}));
        }
        if !allow_secrets {
            return Some(error(403, "需要授权"));
        }
        let Ok(_permit) = self.inner.operations.try_acquire() else {
            return Some(error(429, "授权请求数量超过限制"));
        };
        let result = if action.ends_with("_qrcode") {
            self.qrcode(provider).await
        } else if action.ends_with("_check") {
            self.check(provider, params.get("device_id")).await
        } else if action.ends_with("_logout") {
            self.save(provider, None, None, None)
                .await
                .map(|_| success())
        } else {
            let now = self.inner.clock.now_ms();
            let value = if provider == Provider::Cloud {
                if !params.get("login_info").is_empty() {
                    serde_json::from_str(params.get("login_info"))
                        .map_err(|_| AuthError::InvalidCredential)
                } else {
                    Ok(
                        json!({"access_token":params.get("access_token"),"open_access_token":params.get("access_token"),"has_valid_open_token":true,"login_time":now/1000,"expires_at":now/1000+30*86400,"source":"manual"}),
                    )
                }
            } else {
                Ok(
                    json!({"authorization":params.get("authorization"),"source":"manual","timestamp":now}),
                )
            };
            match value {
                Ok(value) => self
                    .save(provider, Some(value), None, None)
                    .await
                    .map(|_| success()),
                Err(e) => Err(e),
            }
        };
        Some(result.unwrap_or_else(|e| match e {
            AuthError::InvalidCredential => error(400, "凭据格式无效"),
            AuthError::Conflict => error(409, "授权状态已改变，请重试"),
            AuthError::Capacity => error(429, "会话或请求数量超过限制"),
            AuthError::Storage => error(500, "凭据持久化失败"),
            AuthError::Network => error(502, "授权服务请求失败"),
        }))
    }
    fn status(&self, provider: Provider) -> Value {
        let (_, info) = self.current(provider);
        let Some(info) = info else {
            return json!({"logged_in":false,"configured":false});
        };
        let mut out = json!({"configured":true,"logged_in":true,"verified":false});
        if provider == Provider::Cloud {
            out = ca::login_status(Some(&info), self.inner.clock.now_ms())["data"].clone();
            out["configured"] = json!(true);
            out["verified"] = json!(false);
            let source = out["source"].as_str().unwrap_or("configured");
            out["source"] = json!(if ["qrcode", "manual", "seed", "on_demand_refresh"]
                .contains(&source)
            {
                source
            } else {
                "configured"
            });
        } else {
            // Decode only to mask the account; never include authorization/token.
            let auth = info["authorization"].as_str().unwrap_or("").trim();
            let encoded = auth.split_whitespace().last().unwrap_or("");
            if let Ok(decoded) = crate::providers::mcloud::decode_base64_latin1(encoded) {
                if let Some(phone) = decoded.split(':').nth(1) {
                    out["account_mask"] = json!(mask(phone));
                }
                if let Some(expiry) = decoded
                    .splitn(3, ':')
                    .nth(2)
                    .and_then(|t| t.split('|').nth(3))
                    .and_then(|v| v.parse::<u64>().ok())
                    .filter(|v| *v > 0)
                {
                    let expired = expiry <= self.inner.clock.now_ms();
                    out["expires_at_ms"] = json!(expiry);
                    out["expired"] = json!(expired);
                    out["logged_in"] = json!(!expired);
                }
            }
            if let Some(ts) = info["timestamp"].as_u64() {
                out["timestamp"] = json!(ts);
            }
        }
        out
    }
}
fn success() -> Value {
    json!({"code":200,"msg":"已保存","success":true})
}
fn error(code: u16, message: &str) -> Value {
    json!({"code":code,"msg":message,"success":false,"data":null})
}
fn decode_stored(v: Value, provider: Provider) -> Result<Value> {
    if let Some(s) = v.as_str() {
        if let Ok(v) = serde_json::from_str(s) {
            return Ok(v);
        }
        if provider == Provider::Mobile {
            return Ok(json!({"authorization":s}));
        }
        return Err(AuthError::InvalidCredential);
    }
    Ok(v)
}
fn credential_changed(provider: Provider, before: &Value, after: &Value) -> bool {
    let keys: &[&str] = if provider == Provider::Cloud {
        &[
            "session_key",
            "session_secret",
            "open_access_token",
            "app_access_token",
            "cookie_login_user",
            "sson",
        ]
    } else {
        &["authorization"]
    };
    keys.iter()
        .any(|key| before[provider.name()][*key] != after[provider.name()][*key])
}
fn normalize(provider: Provider, mut v: Value, now: u64) -> Result<Value> {
    if provider == Provider::Cloud {
        ca::validate_login_info(&v).map_err(|_| AuthError::InvalidCredential)?;
        if v["access_token"].as_str().is_none_or(str::is_empty) {
            v["access_token"] = ["open_access_token", "app_access_token"]
                .iter()
                .find_map(|k| v[*k].as_str().filter(|s| !s.is_empty()).map(|s| json!(s)))
                .unwrap_or(json!(""));
        }
    } else {
        if v.is_string() {
            v = json!({"authorization":v});
        }
        let auth = v["authorization"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| v["auth"].as_str())
            .unwrap_or("");
        if auth.is_empty() || auth.len() > 16384 || auth.chars().any(char::is_control) {
            return Err(AuthError::InvalidCredential);
        }
        let auth = auth.trim();
        let encoded = if auth
            .get(..5)
            .is_some_and(|s| s.eq_ignore_ascii_case("basic"))
        {
            auth[5..].trim()
        } else {
            auth
        };
        let decoded = crate::providers::mcloud::decode_base64_latin1(encoded)
            .map_err(|_| AuthError::InvalidCredential)?;
        let mut parts = decoded.splitn(3, ':');
        if parts.next().is_none_or(str::is_empty)
            || parts.next().is_none_or(str::is_empty)
            || parts.next().is_none_or(str::is_empty)
        {
            return Err(AuthError::InvalidCredential);
        }
        let auth = auth.to_owned();
        v["authorization"] = json!(auth);
        if v.get("timestamp").is_none() {
            v["timestamp"] = json!(now);
        }
    }
    Ok(v)
}
fn env_seed(initial: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| {
            initial[*name]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        })
        .or_else(|| {
            names
                .iter()
                .find_map(|name| std::env::var(name).ok().filter(|s| !s.is_empty()))
        })
}
fn seed(initial: &Value, provider: Provider, now: u64) -> Result<Option<Value>> {
    if let Some(value) = initial.get(provider.key()).filter(|v| !v.is_null()) {
        return Ok(Some(normalize(
            provider,
            decode_stored(value.clone(), provider)?,
            now,
        )?));
    }
    let config = &initial[provider.name()];
    if let Some(info) = ["loginInfo", "login_info"]
        .iter()
        .filter_map(|k| config.get(*k))
        .find(|v| !v.is_null() && v.as_str() != Some(""))
    {
        return Ok(Some(normalize(
            provider,
            decode_stored(info.clone(), provider)?,
            now,
        )?));
    }
    if provider == Provider::Mobile {
        let auth = ["authorization", "auth"]
            .iter()
            .find_map(|k| {
                config[*k]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
            })
            .or_else(|| {
                env_seed(
                    initial,
                    &["MCLOUD_AUTHORIZATION", "MCLOUD_AUTH", "mcloud_auth"],
                )
            });
        return auth
            .map(|auth| {
                normalize(
                    provider,
                    json!({"authorization":auth,"timestamp":now,"source":"seed"}),
                    now,
                )
            })
            .transpose();
    }
    if ["access_token", "open_access_token", "app_access_token"]
        .iter()
        .any(|k| config[*k].as_str().is_some_and(|s| !s.is_empty()))
    {
        let mut info = config.clone();
        if info.get("login_time").is_none() {
            info["login_time"] = json!(now / 1000);
        }
        if info.get("expires_at").is_none() {
            info["expires_at"] = json!(now / 1000 + 30 * 86400);
        }
        info["source"] = json!("seed");
        return Ok(Some(normalize(provider, info, now)?));
    }
    let token = config["accessToken"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            env_seed(
                initial,
                &[
                    "P189_TOKEN",
                    "p189_token",
                    "CLOUD189_TOKEN",
                    "cloud189_token",
                ],
            )
        });
    token.map(|token| normalize(provider, json!({"access_token":token,"open_access_token":token,"has_valid_open_token":true,"login_time":now/1000,"expires_at":now/1000+30*86400,"source":"seed"}), now)).transpose()
}
fn project(snapshot: &mut Value, provider: Provider, info: Option<&Value>) {
    if !snapshot[provider.name()].is_object() {
        snapshot[provider.name()] = json!({});
    }
    let target = &mut snapshot[provider.name()];
    target["loginInfo"] = info.cloned().unwrap_or(Value::Null);
    // Remove stale private-file credential aliases on every account switch/logout.
    if let Some(obj) = target.as_object_mut() {
        for key in [
            "login_info",
            "access_token",
            "app_access_token",
            "open_access_token",
            "refresh_token",
            "session_key",
            "session_secret",
            "cookie_login_user",
            "sson",
            "share_session_cookies",
            "auth",
            "cookie",
        ] {
            obj.remove(key);
        }
    }
    if provider == Provider::Cloud {
        target["accessToken"] = info.map(|v| v["access_token"].clone()).unwrap_or(json!(""));
        if let Some(info) = info {
            for key in [
                "access_token",
                "app_access_token",
                "open_access_token",
                "refresh_token",
                "session_key",
                "session_secret",
                "cookie_login_user",
                "sson",
                "share_session_cookies",
            ] {
                if let Some(value) = info.get(key) {
                    target[key] = value.clone();
                }
            }
        }
    } else {
        target["authorization"] = info
            .map(|v| v["authorization"].clone())
            .unwrap_or(json!(""));
    }
    if let Some(obj) = snapshot.as_object_mut() {
        obj.remove(provider.key());
        let aliases: &[&str] = if provider == Provider::Mobile {
            &[
                "MCLOUD_AUTHORIZATION",
                "MCLOUD_AUTH",
                "mcloud_auth",
                "MCLOUD_COOKIE",
            ]
        } else {
            &[
                "P189_TOKEN",
                "p189_token",
                "CLOUD189_TOKEN",
                "cloud189_token",
            ]
        };
        for key in aliases {
            obj.remove(*key);
        }
    }
}
fn mask(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < 7 {
        return "***".into();
    }
    format!(
        "{}****{}",
        chars[..3].iter().collect::<String>(),
        chars[chars.len() - 4..].iter().collect::<String>()
    )
}
fn timestamp(ms: u64, offset: i64) -> String {
    // httpdate supplies Gregorian UTC conversion; offset follows deployed runtime-local time.
    let secs = (ms / 1000) as i64 + offset;
    let date = httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_secs(secs.max(0) as u64));
    let p: Vec<_> = date.split_whitespace().collect();
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == p[2])
    .unwrap_or(0)
        + 1;
    format!("{}-{month:02}-{} {}", p[3], p[1], p[4])
}
