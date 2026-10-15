//! Opt-in Cloud189 SHARE_SAVE kernel. No live I/O unless a caller supplies a transport.
//! Folder provisioning/listing are trusted caller capabilities, NOT guessed protocols.
use crate::compat::{js_string, truthy};
use crate::providers::{cloud189, cloud189_client::parse_json_safe};
use crate::tree::Walk;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    future::Future,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const WEB: &str = "https://cloud.189.cn/api";
const APP: &str = "https://api.cloud.189.cn";
const FORM: &str = "application/x-www-form-urlencoded; charset=UTF-8";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const MAX_BODY: usize = 2 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferError {
    Disabled,
    UnauthorizedScope,
    InvalidInput,
    MissingCredentials,
    Transport,
    InvalidJson,
    Oversized,
    Rejected,
    TaskFailed,
    PollLimit,
    UnresolvedResources,
}
impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cloud189 transfer: {self:?}")
    }
}
impl std::error::Error for TransferError {}
type Result<T> = std::result::Result<T, TransferError>;

/// Deliberately not Debug: headers can contain account credentials.
pub struct Request {
    pub url: String,
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}
pub trait Transport {
    fn fetch(&mut self, request: Request) -> impl Future<Output = Result<Value>> + Send;
}
pub trait Runtime {
    fn now_ms(&self) -> u64;
    fn random(&mut self) -> f64;
    fn uuid(&mut self) -> String;
    fn sleep(&mut self, delay: Duration) -> impl Future<Output = ()> + Send;
}
pub struct SystemRuntime;
impl Runtime for SystemRuntime {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64
    }
    fn random(&mut self) -> f64 {
        let bytes = uuid::Uuid::new_v4().into_bytes();
        let n = bytes[..6]
            .iter()
            .fold(0u64, |n, b| (n << 8) | u64::from(*b));
        n as f64 / 281_474_976_710_656.0
    }
    fn uuid(&mut self) -> String {
        uuid::Uuid::new_v4().to_string()
    }
    async fn sleep(&mut self, delay: Duration) {
        tokio::time::sleep(delay).await
    }
}

/// Reuses the caller's shared pool. The caller MUST configure Policy::none(),
/// no cookie jar and no default credentials. Reqwest cannot introspect that policy.
/// Redirects are errors; download links are never requested by this transport.
pub struct ReqwestTransport {
    client: reqwest::Client,
}
impl ReqwestTransport {
    pub fn from_no_redirect_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}
impl Transport for ReqwestTransport {
    async fn fetch(&mut self, request: Request) -> Result<Value> {
        let url = url::Url::parse(&request.url).map_err(|_| TransferError::InvalidInput)?;
        if url.scheme() != "https"
            || !matches!(url.host_str(), Some("cloud.189.cn" | "api.cloud.189.cn"))
            || url.port().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(TransferError::UnauthorizedScope);
        }
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|_| TransferError::InvalidInput)?;
        let mut builder = self
            .client
            .request(method, url)
            .timeout(Duration::from_secs(20));
        for (k, v) in request.headers {
            builder = builder.header(k, v);
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }
        let mut response = builder.send().await.map_err(|_| TransferError::Transport)?;
        if response.status().is_redirection() {
            return Err(TransferError::Rejected);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TransferError::Transport)?
        {
            if bytes.len().saturating_add(chunk.len()) > MAX_BODY {
                return Err(TransferError::Oversized);
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| TransferError::InvalidJson)?;
        parse_json_safe(text).map_err(|_| TransferError::InvalidJson)
    }
}

/// Instance-local, no Debug/serialization. Reuse existing account state; no login here.
pub struct Credentials {
    pub cookie_login_user: String,
    pub sson: String,
    pub session_key: String,
    pub session_secret: String,
}
impl Credentials {
    pub fn from_account(value: &Value) -> Self {
        let s = |k: &str| value[k].as_str().unwrap_or("").to_owned();
        Self {
            cookie_login_user: s("cookie_login_user"),
            sson: s("sson"),
            session_key: s("session_key"),
            session_secret: s("session_secret"),
        }
    }
}
#[derive(Clone, Copy)]
pub struct Options {
    pub enable_transfers: bool,
    /// Clamped to 1..=15; no unbounded task polling.
    pub max_polls: usize,
    pub poll_delay: Duration,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            enable_transfers: false,
            max_polls: 15,
            poll_delay: Duration::from_secs(2),
        }
    }
}
#[derive(Clone, Debug)]
pub struct ShareItem {
    pub file_id: String,
    pub file_name: String,
    pub is_folder: bool,
}
impl ShareItem {
    fn json(&self) -> Value {
        json!({"fileId":self.file_id,"fileName":self.file_name,"isFolder":if self.is_folder {1} else {0}})
    }
}

/// Opaque proof: created by this kernel's provision callback only. Not Clone.
pub struct IsolatedScope {
    owner: String,
    root: String,
    folder: String,
    name: String,
}
impl IsolatedScope {
    pub fn folder_id(&self) -> &str {
        &self.folder
    }
    pub fn folder_name(&self) -> &str {
        &self.name
    }
    pub fn authorized_root(&self) -> &str {
        &self.root
    }
}
/// IDs are bound to a complete owned-directory listing, never copied from share IDs.
/// Not Clone or deserializable: cannot forge/replay a persisted arbitrary deletion list.
pub struct Receipt {
    scope: IsolatedScope,
    task_id: Option<String>,
    items: Vec<ShareItem>,
    resources: Vec<String>,
    files: Vec<String>,
    pending_delete: Vec<String>,
    completed: bool,
    bound: bool,
    cleaned: bool,
}
impl Receipt {
    pub fn scope_id(&self) -> &str {
        self.scope.folder_id()
    }
    pub fn scope_name(&self) -> &str {
        self.scope.folder_name()
    }
    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }
    pub fn resource_ids(&self) -> &[String] {
        &self.resources
    }
    pub fn completed(&self) -> bool {
        self.completed
    }
    pub fn files_cleaned(&self) -> bool {
        self.cleaned
    }
    pub fn remaining_file_ids(&self) -> &[String] {
        &self.pending_delete
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cleanup {
    /// Receipt files removed; all directory skeletons deliberately retained.
    FilesRemovedDirectoriesRetained,
    /// Scope/task is available in receipt. Caller must arrange manual recovery.
    ManualInterventionRequired,
}
pub struct Outcome {
    pub receipt: Receipt,
    pub links: Vec<(String, String)>,
    pub failure: Option<TransferError>,
    pub cleanup: Cleanup,
}
#[derive(Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Complete,
    Pending,
    Failed,
}

pub struct TransferKernel<T, R> {
    pub transport: T,
    pub runtime: R,
    credentials: Credentials,
    options: Options,
    owner: String,
}
fn pairs(values: &[(&str, &str)]) -> String {
    let mut form = url::form_urlencoded::Serializer::new(String::new());
    form.extend_pairs(values.iter().copied());
    form.finish()
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 256
        && !id
            .chars()
            .any(|c| c.is_control() || matches!(c, ',' | '/' | '\\' | ';'))
}
fn success(value: &Value) -> bool {
    value.get("res_code") == Some(&json!(0))
}
impl<T: Transport, R: Runtime> TransferKernel<T, R> {
    pub fn new(transport: T, mut runtime: R, credentials: Credentials, options: Options) -> Self {
        let owner = runtime.uuid();
        Self {
            transport,
            runtime,
            credentials,
            options,
            owner,
        }
    }
    fn enabled(&self) -> Result<()> {
        if !self.options.enable_transfers {
            return Err(TransferError::Disabled);
        }
        if self.credentials.cookie_login_user.is_empty()
            || self.credentials.session_key.is_empty()
            || self.credentials.session_secret.is_empty()
        {
            return Err(TransferError::MissingCredentials);
        }
        Ok(())
    }
    fn scope_ok(&self, scope: &IsolatedScope) -> Result<()> {
        self.enabled()?;
        if scope.owner != self.owner {
            return Err(TransferError::UnauthorizedScope);
        }
        Ok(())
    }
    /// Caller explicitly authorizes the root and provisions a NEW, EMPTY child with
    /// this exact UUID name. Capability must attest fresh creation, not find-or-reuse.
    /// No provision callback is invoked when disabled. Frozen JS has no createFolder.
    pub async fn provision_scope<F, Fut>(
        &mut self,
        authorized_test_root: &str,
        provision: F,
    ) -> Result<IsolatedScope>
    where
        F: FnOnce(String, String) -> Fut,
        Fut: Future<Output = Result<String>>,
    {
        self.enabled()?;
        if !valid_id(authorized_test_root) || matches!(authorized_test_root, "-11" | "-12" | "0") {
            return Err(TransferError::UnauthorizedScope);
        }
        let nonce = self.runtime.uuid();
        let parsed = uuid::Uuid::parse_str(&nonce).map_err(|_| TransferError::InvalidInput)?;
        if parsed.get_version_num() != 4 || parsed.to_string() != nonce {
            return Err(TransferError::InvalidInput);
        }
        let name = format!("jxpan-transfer-{nonce}");
        let folder = tokio::time::timeout(
            Duration::from_secs(20),
            provision(authorized_test_root.to_owned(), name.clone()),
        )
        .await
        .map_err(|_| TransferError::Transport)??;
        if !valid_id(&folder)
            || folder == authorized_test_root
            || matches!(folder.as_str(), "-11" | "-12" | "0")
        {
            return Err(TransferError::UnauthorizedScope);
        }
        Ok(IsolatedScope {
            owner: self.owner.clone(),
            root: authorized_test_root.into(),
            folder,
            name,
        })
    }
    fn web_request(&mut self, path: &str, method: &'static str, body: Option<String>) -> Request {
        let timestamp = self.runtime.now_ms().to_string();
        let signature = cloud189::md5(&cloud189::web_signing_text(&timestamp));
        let mut cookie = format!("COOKIE_LOGIN_USER={}", self.credentials.cookie_login_user);
        if !self.credentials.sson.is_empty() {
            cookie.push_str(&format!("; SSON={}", self.credentials.sson));
        }
        let mut headers = vec![
            ("accept".into(), "application/json;charset=UTF-8".into()),
            ("appkey".into(), "600100422".into()),
            ("sign-type".into(), "1".into()),
            ("signature".into(), signature),
            ("timestamp".into(), timestamp),
            (
                "referer".into(),
                "https://cloud.189.cn/web/main/file/folder/-11".into(),
            ),
            ("user-agent".into(), UA.into()),
            ("cookie".into(), cookie),
        ];
        if method == "POST" {
            headers.push(("content-type".into(), FORM.into()));
        }
        Request {
            url: format!("{WEB}{path}"),
            method,
            headers,
            body,
        }
    }
    async fn fetch_json(&mut self, request: Request) -> Result<Value> {
        tokio::time::timeout(Duration::from_secs(20), self.transport.fetch(request))
            .await
            .map_err(|_| TransferError::Transport)?
    }
    /// Read-only prerequisite, retains deployed web-session behaviour. No storage writes.
    pub async fn get_web_session_key(&mut self) -> Result<String> {
        self.enabled()?;
        let path = format!(
            "/portal/v2/getUserBriefInfo.action?noCache={}",
            self.runtime.random()
        );
        let request = self.web_request(&path, "GET", None);
        let value = self.fetch_json(request).await?;
        value["sessionKey"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .ok_or(TransferError::Rejected)
    }
    pub async fn get_root_directory(&mut self, web_session_key: &str) -> Result<Value> {
        self.enabled()?;
        let mut request = self.web_request("/portal/v2/getRootDirectory.action", "GET", None);
        if !web_session_key.is_empty() {
            request
                .headers
                .push(("sessionkey".into(), web_session_key.into()));
        }
        let value = self.fetch_json(request).await?;
        if success(&value) && truthy(&value["fileId"]) {
            Ok(value)
        } else {
            Err(TransferError::Rejected)
        }
    }
    /// Exact confirmed batch wire shape. Folder isFolder=1 extends the existing
    /// schema; server folder-save support has NOT been live verified.
    async fn save_share_file(&mut self, share_id: &str, receipt: &Receipt) -> Result<String> {
        self.scope_ok(&receipt.scope)?;
        let infos = serde_json::to_string(
            &receipt
                .items
                .iter()
                .map(ShareItem::json)
                .collect::<Vec<_>>(),
        )
        .unwrap();
        let body = pairs(&[
            ("type", "SHARE_SAVE"),
            ("taskInfos", &infos),
            ("targetFolderId", receipt.scope_id()),
            ("shareId", share_id),
        ]);
        let request = self.web_request("/open/batch/createBatchTask.action", "POST", Some(body));
        let value = self.fetch_json(request).await?;
        if !success(&value) || !truthy(&value["taskId"]) {
            return Err(TransferError::Rejected);
        }
        let id = js_string(&value["taskId"]);
        if !valid_id(&id) {
            return Err(TransferError::Rejected);
        }
        Ok(id)
    }
    pub async fn check_task_status(&mut self, receipt: &Receipt) -> Result<TaskStatus> {
        self.scope_ok(&receipt.scope)?;
        let task_id = receipt.task_id().ok_or(TransferError::InvalidInput)?;
        let body = pairs(&[("taskId", task_id), ("type", "SHARE_SAVE")]);
        let request = self.web_request("/open/batch/checkBatchTask.action", "POST", Some(body));
        let value = self.fetch_json(request).await?;
        if !success(&value) {
            return Err(TransferError::Rejected);
        }
        if truthy(&value["errorCode"]) {
            return Ok(TaskStatus::Failed);
        }
        if value["taskStatus"] == json!(3) {
            Ok(TaskStatus::Complete)
        } else {
            Ok(TaskStatus::Pending)
        }
    }
    /// Sequential polling only; at most 15 requests, 2s delay each, 20s HTTP each.
    /// Runtime/transport implementations must uphold their bounded contract.
    async fn poll(&mut self, receipt: &mut Receipt) -> Result<()> {
        for _ in 0..self.options.max_polls.clamp(1, 15) {
            tokio::time::timeout(
                Duration::from_secs(3),
                self.runtime
                    .sleep(self.options.poll_delay.min(Duration::from_secs(2))),
            )
            .await
            .map_err(|_| TransferError::Transport)?;
            match self.check_task_status(receipt).await? {
                TaskStatus::Complete => {
                    receipt.completed = true;
                    return Ok(());
                }
                TaskStatus::Failed => return Err(TransferError::TaskFailed),
                TaskStatus::Pending => (),
            }
        }
        Err(TransferError::PollLimit)
    }
    /// Reuse crate::tree::walk (concurrency=1), with paginated own-directory list
    /// callback. The caller MUST propagate listing failures/truncation as Err:
    /// tree::walk alone swallows list errors. Never bind a partial/stale/share tree.
    fn bind_saved_tree(&self, receipt: &mut Receipt, tree: Walk) -> Result<()> {
        if !receipt.completed
            || tree.root_id != receipt.scope_id()
            || !tree.nodes.contains_key(receipt.scope_id())
        {
            return Err(TransferError::UnresolvedResources);
        }
        if tree.all_files.len().saturating_add(tree.all_folders.len()) > 1000 {
            return Err(TransferError::UnresolvedResources);
        }
        let dirs: HashSet<_> = tree
            .all_folders
            .iter()
            .filter_map(|v| v["fileId"].as_str())
            .collect();
        if tree.nodes.len() != dirs.len() + 1 || dirs.iter().any(|id| !tree.nodes.contains_key(*id))
        {
            return Err(TransferError::UnresolvedResources);
        }
        let mut ids = HashSet::new();
        for row in tree.all_files.iter().chain(&tree.all_folders) {
            let id = row["fileId"]
                .as_str()
                .ok_or(TransferError::UnresolvedResources)?;
            let parent = row["parentId"]
                .as_str()
                .ok_or(TransferError::UnresolvedResources)?;
            if !valid_id(id)
                || id == receipt.scope_id()
                || id == receipt.scope.authorized_root()
                || matches!(id, "-11" | "-12" | "0")
                || !ids.insert(id.to_owned())
                || (parent != receipt.scope_id() && !dirs.contains(parent))
            {
                return Err(TransferError::UnresolvedResources);
            }
            // Every row must follow a finite parent chain to the isolated root.
            let mut at = parent;
            let mut seen = HashSet::new();
            while at != receipt.scope_id() {
                if !seen.insert(at) {
                    return Err(TransferError::UnresolvedResources);
                }
                at = tree
                    .all_folders
                    .iter()
                    .find(|v| v["fileId"].as_str() == Some(at))
                    .and_then(|v| v["parentId"].as_str())
                    .ok_or(TransferError::UnresolvedResources)?;
            }
        }
        let top: Vec<_> = tree
            .all_files
            .iter()
            .chain(&tree.all_folders)
            .filter(|v| v["parentId"].as_str() == Some(receipt.scope_id()))
            .collect();
        if top.len() != receipt.items.len() {
            return Err(TransferError::UnresolvedResources);
        }
        for item in &receipt.items {
            let found = top
                .iter()
                .filter(|v| {
                    v["fileName"].as_str() == Some(&item.file_name)
                        && v["isFolder"].as_bool() == Some(item.is_folder)
                })
                .count();
            if found != 1 {
                return Err(TransferError::UnresolvedResources);
            }
        }
        // Never recursively delete directories: that could also remove a file
        // inserted by another writer after this snapshot. Only receipt file IDs.
        receipt.files = tree
            .all_files
            .iter()
            .map(|v| v["fileId"].as_str().unwrap().into())
            .collect();
        receipt.resources = receipt.files.clone();
        receipt.resources.extend(
            tree.all_folders
                .iter()
                .map(|v| v["fileId"].as_str().unwrap().to_owned()),
        );
        receipt.pending_delete = receipt.files.clone();
        receipt.bound = true;
        Ok(())
    }
    async fn delete_receipt_id(&mut self, receipt: &Receipt, id: &str) -> Result<()> {
        self.scope_ok(&receipt.scope)?;
        if !receipt.bound || !receipt.completed || !receipt.files.iter().any(|v| v == id) {
            return Err(TransferError::UnauthorizedScope);
        }
        let path = "/batchDeleteFile.action";
        let date =
            httpdate::fmt_http_date(UNIX_EPOCH + Duration::from_millis(self.runtime.now_ms()));
        let signature = cloud189::hmac_sha1(
            &self.credentials.session_secret,
            &cloud189::app_signing_text(&self.credentials.session_key, "POST", path, &date),
        )
        .map_err(|_| TransferError::MissingCredentials)?;
        let rand = format!(
            "{}_{}",
            (self.runtime.random() * 99999.0).floor() as u64 + 1,
            (self.runtime.random() * 9999999999.0).floor() as u64 + 1
        );
        let suffix = pairs(&[
            ("clientType", "TELEPC"),
            ("version", "6.2"),
            ("channelId", "web_cloud.189.cn"),
            ("rand", &rand),
        ]);
        let headers = vec![
            ("accept".into(), "application/json;charset=UTF-8".into()),
            ("content-type".into(), FORM.into()),
            ("date".into(), date),
            ("sessionkey".into(), self.credentials.session_key.clone()),
            ("signature".into(), signature),
            ("user-agent".into(), "desktop".into()),
            ("x-request-id".into(), self.runtime.uuid()),
        ];
        let request = Request {
            url: format!("{APP}{path}?{suffix}"),
            method: "POST",
            headers,
            body: Some(pairs(&[("fileIdList", id)])),
        };
        let value = self.fetch_json(request).await?;
        if success(&value) {
            Ok(())
        } else {
            Err(TransferError::Rejected)
        }
    }
    /// Idempotent in memory. A failed/uncertain request is not called cleaned and
    /// is not silently replayed. Caller can explicitly retry using this receipt.
    pub async fn cleanup(&mut self, receipt: &mut Receipt) -> Result<Cleanup> {
        self.scope_ok(&receipt.scope)?;
        if receipt.cleaned {
            return Ok(Cleanup::FilesRemovedDirectoriesRetained);
        }
        if !receipt.bound || !receipt.completed {
            return Err(TransferError::UnresolvedResources);
        }
        while let Some(id) = receipt.pending_delete.first().cloned() {
            self.delete_receipt_id(receipt, &id).await?;
            receipt.pending_delete.remove(0);
        }
        receipt.cleaned = true;
        Ok(Cleanup::FilesRemovedDirectoriesRetained)
    }
    /// Explicitly gated batch transfer + receipt mapping + owned downloads + cleanup.
    /// One item is saveAndDownload. On task success cleanup runs even if download
    /// returns None/Err. If listing cannot bind IDs, no speculative delete is issued.
    /// The caller must not cancel this future after save; persist the receipt/recovery
    /// location outside this kernel and provide bounded callbacks (<=20s each).
    pub async fn batch_save_share<L, LF, D, DF>(
        &mut self,
        scope: IsolatedScope,
        share_id: &str,
        items: Vec<ShareItem>,
        list_owned_tree: L,
        download_owned: D,
    ) -> Result<Outcome>
    where
        L: FnOnce(String) -> LF,
        LF: Future<Output = Result<Walk>>,
        D: Fn(String) -> DF,
        DF: Future<Output = Result<Option<String>>>,
    {
        self.scope_ok(&scope)?;
        let mut names = HashSet::new();
        if !valid_id(share_id)
            || items.is_empty()
            || items.len() > 100
            || items.iter().any(|i| {
                !valid_id(&i.file_id)
                    || i.file_name.is_empty()
                    || i.file_name.len() > 1024
                    || !names.insert(i.file_name.clone())
            })
        {
            return Err(TransferError::InvalidInput);
        }
        let mut receipt = Receipt {
            scope,
            task_id: None,
            items,
            resources: vec![],
            files: vec![],
            pending_delete: vec![],
            completed: false,
            bound: false,
            cleaned: false,
        };
        let preparation = async {
            self.get_web_session_key().await?;
            receipt.task_id = Some(self.save_share_file(share_id, &receipt).await?);
            self.poll(&mut receipt).await?;
            let tree = tokio::time::timeout(
                Duration::from_secs(20),
                list_owned_tree(receipt.scope_id().into()),
            )
            .await
            .map_err(|_| TransferError::UnresolvedResources)??;
            self.bind_saved_tree(&mut receipt, tree)
        }
        .await;
        if let Err(failure) = preparation {
            return Ok(Outcome {
                receipt,
                links: vec![],
                failure: Some(failure),
                cleanup: Cleanup::ManualInterventionRequired,
            });
        }
        let mut links = vec![];
        let mut failure = None;
        for id in &receipt.files {
            let result =
                tokio::time::timeout(Duration::from_secs(20), download_owned(id.clone())).await;
            match result {
                Ok(Ok(Some(link))) if !link.is_empty() => links.push((id.clone(), link)),
                Ok(Err(error)) => {
                    failure.get_or_insert(error);
                }
                _ => {
                    failure.get_or_insert(TransferError::Rejected);
                }
            }
        }
        let cleanup = match self.cleanup(&mut receipt).await {
            Ok(done) => done,
            Err(error) => {
                failure.get_or_insert(error);
                Cleanup::ManualInterventionRequired
            }
        };
        Ok(Outcome {
            receipt,
            links,
            failure,
            cleanup,
        })
    }
    pub async fn save_and_download<L, LF, D, DF>(
        &mut self,
        scope: IsolatedScope,
        share_id: &str,
        item: ShareItem,
        list_owned_tree: L,
        download_owned: D,
    ) -> Result<Outcome>
    where
        L: FnOnce(String) -> LF,
        LF: Future<Output = Result<Walk>>,
        D: Fn(String) -> DF,
        DF: Future<Output = Result<Option<String>>>,
    {
        self.batch_save_share(scope, share_id, vec![item], list_owned_tree, download_owned)
            .await
    }
}
