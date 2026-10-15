//! Official OAuth REST owned-directory capabilities, NOT the frozen WebAPI.
//! See docs/CLOUD189_TRANSFER_CAPABILITIES.md for protocol evidence and limits.
use crate::{providers::cloud189, tree};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    future::Future,
    sync::Arc,
    time::{Duration, SystemTime},
};

const ORIGIN: &str = "https://api.cloud.189.cn";
const LIST: &str = "/listFiles.action";
const CREATE: &str = "/createFolder.action";
const DEADLINE: Duration = Duration::from_secs(20);
pub const MAX_BODY: usize = 2 * 1024 * 1024;
const PAGE_SIZE: usize = 100;
const MAX_RESOURCES: usize = 1000;
const MAX_NODES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Disabled,
    UnauthorizedScope,
    MissingCredentials,
    InvalidInput,
    Transport,
    Http,
    Oversized,
    UnsupportedXml,
    InvalidResponse,
    ChangedListing,
    RepeatedResource,
    Limit,
    ExistingName,
    NotEmpty,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cloud189 owned capability: {self:?}")
    }
}
impl std::error::Error for Error {}
// The transfer kernel's Result alias is intentionally private.
pub type Result<T> = std::result::Result<T, Error>;

/// OAuth credentials from the official REST docs. NOT SessionKey/SessionSecret.
/// Deliberately no Debug or serialization and no account/login loader.
pub struct OAuthCredentials {
    access_token: String,
    app_secret: String,
}
impl OAuthCredentials {
    pub fn new(access_token: String, app_secret: String) -> Self {
        Self {
            access_token,
            app_secret,
        }
    }
}
#[derive(Clone, Copy, Default)]
pub struct Options {
    pub enable_owned_read: bool,
    /// Separate explicit authorization for one fresh child creation per callback.
    pub enable_provision: bool,
}
/// Deliberately no Debug: headers contain credentials.
pub struct Request {
    pub url: String,
    pub method: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
}
pub trait Transport {
    fn fetch(&mut self, request: Request) -> impl Future<Output = Result<Response>> + Send;
}

/// Owns a cloneable/shared pool built HERE so redirect policy cannot be forged by
/// passing an arbitrary reqwest client. No cookie jar or default credentials.
#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}
impl ReqwestTransport {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .https_only(true)
            .timeout(DEADLINE)
            .build()
            .map_err(|_| Error::Transport)?;
        Ok(Self { client })
    }
}
/// Public for offline inspection. Only the two verified endpoints are permitted.
pub fn validate_target(request: &Request) -> Result<()> {
    let url = url::Url::parse(&request.url).map_err(|_| Error::InvalidInput)?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.cloud.189.cn")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(
            (request.method, url.path()),
            ("GET", LIST) | ("POST", CREATE)
        )
    {
        return Err(Error::UnauthorizedScope);
    }
    Ok(())
}
impl Transport for ReqwestTransport {
    async fn fetch(&mut self, request: Request) -> Result<Response> {
        validate_target(&request)?;
        // Whole operation, including streamed body, is bounded by the deadline.
        tokio::time::timeout(DEADLINE, async {
            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|_| Error::InvalidInput)?;
            let mut builder = self.client.request(method, request.url).timeout(DEADLINE);
            for (key, value) in request.headers {
                builder = builder.header(key, value);
            }
            if let Some(body) = request.body {
                builder = builder.body(body);
            }
            let mut response = builder.send().await.map_err(|_| Error::Transport)?;
            if !response.status().is_success() {
                return Err(Error::Http);
            }
            if response
                .content_length()
                .is_some_and(|n| n > MAX_BODY as u64)
            {
                return Err(Error::Oversized);
            }
            let status = response.status().as_u16();
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::Transport)? {
                if body.len().saturating_add(chunk.len()) > MAX_BODY {
                    return Err(Error::Oversized);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(Response { status, body })
        })
        .await
        .map_err(|_| Error::Transport)?
    }
}

fn valid_id(id: &str) -> bool {
    // Official ID type is Long. Reject roots, negatives, aliases, leading zeros.
    id.parse::<i64>()
        .is_ok_and(|n| n > 0 && n.to_string() == id)
}
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 1024
        && !name
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
}
fn isolated_name(name: &str) -> bool {
    name.strip_prefix("jxpan-transfer-")
        .and_then(|s| uuid::Uuid::parse_str(s).ok().map(|u| (s, u)))
        .is_some_and(|(s, u)| u.get_version_num() == 4 && u.to_string() == s)
}
fn form(values: &[(&str, &str)]) -> String {
    let mut encoded = url::form_urlencoded::Serializer::new(String::new());
    encoded.extend_pairs(values.iter().copied());
    encoded.finish()
}
/// Reuse the existing exact method/path/date assembly and HMAC implementation.
/// The official OAuth variant changes ONLY the leading field name, not its value.
/// SessionKey authentication for these endpoints remains unverified/unimplemented.
pub fn oauth_signing_text(token: &str, method: &str, path: &str, date: &str) -> String {
    cloud189::app_signing_text(token, method, path, date).replacen("SessionKey=", "AccessToken=", 1)
}

pub struct OwnedClient<T> {
    transport: T,
    credentials: OAuthCredentials,
    options: Options,
    authorized_root: String,
    verified_scopes: HashSet<String>,
    /// A failed/ambiguous attempt also burns its name. Never automatically retry.
    attempted_names: HashSet<String>,
}
impl<T: Transport> OwnedClient<T> {
    pub fn new(
        transport: T,
        credentials: OAuthCredentials,
        options: Options,
        authorized_root: String,
    ) -> Result<Self> {
        if !valid_id(&authorized_root) {
            return Err(Error::UnauthorizedScope);
        }
        Ok(Self {
            transport,
            credentials,
            options,
            authorized_root,
            verified_scopes: HashSet::new(),
            attempted_names: HashSet::new(),
        })
    }
    fn enabled(&self, write: bool) -> Result<()> {
        if !self.options.enable_owned_read || (write && !self.options.enable_provision) {
            return Err(Error::Disabled);
        }
        if self.credentials.access_token.is_empty() || self.credentials.app_secret.is_empty() {
            return Err(Error::MissingCredentials);
        }
        Ok(())
    }
    fn request(&self, path: &'static str, method: &'static str, data: String) -> Result<Request> {
        let date = httpdate::fmt_http_date(SystemTime::now());
        let signature = cloud189::hmac_sha1(
            &self.credentials.app_secret,
            &oauth_signing_text(&self.credentials.access_token, method, path, &date),
        )
        .map_err(|_| Error::MissingCredentials)?;
        let mut headers = vec![
            ("AccessToken".into(), self.credentials.access_token.clone()),
            ("Signature".into(), signature),
            ("Date".into(), date),
            ("Accept".into(), "application/xml".into()),
        ];
        let (url, body) = if method == "GET" {
            (format!("{ORIGIN}{path}?{data}"), None)
        } else {
            headers.push((
                "Content-Type".into(),
                "application/x-www-form-urlencoded; charset=UTF-8".into(),
            ));
            (format!("{ORIGIN}{path}"), Some(data))
        };
        Ok(Request {
            url,
            method,
            headers,
            body,
        })
    }
    async fn fetch(&mut self, request: Request) -> Result<Xml> {
        validate_target(&request)?;
        let response = tokio::time::timeout(DEADLINE, self.transport.fetch(request))
            .await
            .map_err(|_| Error::Transport)??;
        if !(200..300).contains(&response.status) {
            return Err(Error::Http);
        }
        if response.body.len() > MAX_BODY {
            return Err(Error::Oversized);
        }
        let text = std::str::from_utf8(&response.body).map_err(|_| Error::UnsupportedXml)?;
        parse_xml(text)
    }
    /// Complete own-account listing. No share-list request or share ID mapping.
    async fn list_directory(&mut self, folder: &str) -> Result<Vec<Value>> {
        self.list_bounded(folder, MAX_RESOURCES, MAX_RESOURCES, &HashSet::new())
            .await
    }
    async fn list_bounded(
        &mut self,
        folder: &str,
        resource_budget: usize,
        folder_budget: usize,
        prior_ids: &HashSet<String>,
    ) -> Result<Vec<Value>> {
        let mut folders = 0;
        self.enabled(false)?;
        if !valid_id(folder) {
            return Err(Error::UnauthorizedScope);
        }
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        let mut expected = None;
        let mut revision = None;
        for page in 1..=11 {
            let page_num = page.to_string();
            let request = self.request(
                LIST,
                "GET",
                form(&[
                    ("folderId", folder),
                    ("fileType", "0"),
                    ("mediaType", "0"),
                    ("mediaAttr", "0"),
                    ("iconOption", "0"),
                    ("orderBy", "filename"),
                    ("descending", "false"),
                    ("pageNum", &page_num),
                    ("pageSize", "100"),
                ]),
            )?;
            let root = self.fetch(request).await?;
            if root.name != "listFiles" {
                return Err(Error::InvalidResponse);
            }
            root.only(&["lastRev", "fileList"])?;
            let rev = root.scalar("lastRev")?.to_owned();
            if rev.parse::<u64>().is_err() {
                return Err(Error::InvalidResponse);
            }
            let list = root.child("fileList")?;
            list.only(&["count", "folder", "file"])?;
            let count = list
                .scalar("count")?
                .parse::<usize>()
                .map_err(|_| Error::InvalidResponse)?;
            if count > MAX_RESOURCES || count > resource_budget {
                return Err(Error::Limit);
            }
            if expected.is_some_and(|n| n != count) || revision.as_ref().is_some_and(|r| r != &rev)
            {
                return Err(Error::ChangedListing);
            }
            expected = Some(count);
            revision = Some(rev);
            let items: Vec<_> = list
                .children
                .iter()
                .filter(|n| matches!(n.name.as_str(), "folder" | "file"))
                .collect();
            // count is documented TOTAL, not this page's length. No hasMore guess.
            if items.len() != PAGE_SIZE.min(count.saturating_sub(rows.len())) {
                return Err(Error::InvalidResponse);
            }
            for item in items {
                let row = item_row(item, folder)?;
                let id = row["fileId"].as_str().ok_or(Error::InvalidResponse)?;
                if id == folder
                    || id == self.authorized_root
                    || prior_ids.contains(id)
                    || !seen.insert(id.to_owned())
                {
                    return Err(Error::RepeatedResource);
                }
                if row["isFolder"] == json!(true) {
                    folders += 1;
                }
                if folders > folder_budget {
                    return Err(Error::Limit);
                }
                rows.push(row);
            }
            if rows.len() == count {
                return Ok(rows);
            }
        }
        Err(Error::Limit)
    }
    pub async fn list_authorized_root(&mut self) -> Result<Vec<Value>> {
        let root = self.authorized_root.clone();
        self.list_directory(&root).await
    }
    /// Adapter for TransferKernel::provision_scope's (root, UUID-name) callback.
    /// New only: pre-list -> POST exactly once -> parent re-list -> child empty.
    /// Failure after POST requires manual inspection; never retry/delete/reuse.
    pub async fn provision_new_empty(&mut self, root: &str, name: &str) -> Result<String> {
        self.enabled(true)?;
        if root != self.authorized_root || !valid_id(root) || !isolated_name(name) {
            return Err(Error::UnauthorizedScope);
        }
        if !self.attempted_names.insert(name.to_owned()) {
            return Err(Error::ExistingName);
        }
        let before = self.list_directory(root).await?;
        if before.iter().any(|r| r["fileName"].as_str() == Some(name)) {
            return Err(Error::ExistingName);
        }
        let request = self.request(
            CREATE,
            "POST",
            form(&[("parentFolderId", root), ("folderName", name)]),
        )?;
        let created = self.fetch(request).await?;
        if created.name != "folder" {
            return Err(Error::InvalidResponse);
        }
        created.only(&["id", "name", "createDate", "rev"])?;
        if created.children.iter().any(|n| !n.children.is_empty()) {
            return Err(Error::InvalidResponse);
        }
        let id = created.scalar("id")?.to_owned();
        if !valid_id(&id)
            || id == root
            || created.scalar("name")? != name
            || before.iter().any(|r| r["fileId"].as_str() == Some(&id))
        {
            return Err(Error::InvalidResponse);
        }
        let after = self.list_directory(root).await?;
        let named: Vec<_> = after
            .iter()
            .filter(|r| r["fileName"].as_str() == Some(name))
            .collect();
        if named.len() != 1
            || named[0]["fileId"].as_str() != Some(&id)
            || named[0]["isFolder"] != json!(true)
            || named[0]["parentId"].as_str() != Some(root)
        {
            return Err(Error::InvalidResponse);
        }
        if !self.list_directory(&id).await?.is_empty() {
            return Err(Error::NotEmpty);
        }
        self.verified_scopes.insert(id.clone());
        Ok(id)
    }
    /// Only fresh, verified scopes from this client instance may be traversed.
    /// External error/accounting state prevents tree::walk's catch-and-empty and
    /// max-node admission behavior from turning a partial result into success.
    pub async fn walk_owned(&mut self, scope: &str) -> Result<tree::Walk> {
        self.enabled(false)?;
        if !self.verified_scopes.contains(scope) {
            return Err(Error::UnauthorizedScope);
        }
        struct State<'a, T> {
            client: &'a mut OwnedClient<T>,
            error: Option<Error>,
            ids: HashSet<String>,
            folders: usize,
            resources: usize,
        }
        let state = Arc::new(tokio::sync::Mutex::new(State {
            client: self,
            error: None,
            ids: HashSet::from([scope.to_owned()]),
            folders: 1,
            resources: 0,
        }));
        let walk = tree::walk(
            |folder| {
                let state = Arc::clone(&state);
                async move {
                    let mut state = state.lock().await;
                    if let Some(error) = state.error {
                        return Err(error);
                    }
                    let result = async {
                        let prior_ids = state.ids.clone();
                        let resource_budget = MAX_RESOURCES - state.resources;
                        let folder_budget = MAX_NODES - state.folders;
                        let rows = state
                            .client
                            .list_bounded(&folder, resource_budget, folder_budget, &prior_ids)
                            .await?;
                        for row in &rows {
                            let id = row["fileId"].as_str().ok_or(Error::InvalidResponse)?;
                            if !state.ids.insert(id.to_owned()) {
                                return Err(Error::RepeatedResource);
                            }
                            state.resources += 1;
                            if row["isFolder"] == json!(true) {
                                state.folders += 1;
                            }
                            if state.resources > MAX_RESOURCES || state.folders > MAX_NODES {
                                return Err(Error::Limit);
                            }
                        }
                        Ok(rows)
                    }
                    .await;
                    if let Err(error) = result {
                        state.error = Some(error);
                    }
                    result
                }
            },
            scope,
            "",
            1,
            MAX_NODES,
        )
        .await;
        let state = state.lock().await;
        if let Some(error) = state.error {
            return Err(error);
        }
        if walk.nodes.len() != state.folders
            || walk.visited != state.folders
            || walk.all_files.len() + walk.all_folders.len() != state.resources
        {
            return Err(Error::Limit);
        }
        Ok(walk)
    }
}

fn item_row(item: &Xml, parent: &str) -> Result<Value> {
    item.only(&[
        "id",
        "name",
        "size",
        "createDate",
        "lastOpTime",
        "rev",
        "md5",
        "mediaType",
        "path",
        "parentFolderId",
    ])?;
    if item.children.iter().any(|n| !n.children.is_empty()) {
        return Err(Error::InvalidResponse);
    }
    let id = item.scalar("id")?;
    let name = item.scalar("name")?;
    if !valid_id(id) || !valid_name(name) {
        return Err(Error::InvalidResponse);
    }
    if item.children.iter().any(|n| n.name == "parentFolderId")
        && item.scalar("parentFolderId")? != parent
    {
        return Err(Error::InvalidResponse);
    }
    let folder = item.name == "folder";
    let size = if folder {
        0
    } else {
        item.scalar("size")?
            .parse::<u64>()
            .map_err(|_| Error::InvalidResponse)?
    };
    // Parent attestation is listFiles(folderId)'s documented direct-child context,
    // not an invented folder.parentFolderId response requirement.
    Ok(json!({"fileId":id, "fileName":name, "isFolder":folder, "fileSize":size, "parentId":parent}))
}

/// Deliberately restricted XML subset; no DTD, entity declarations, CDATA,
/// namespaces, attributes, comments, or external resolution. Unsupported shapes
/// FAIL CLOSED. This is not a permissive general-purpose XML parser.
struct Xml {
    name: String,
    text: String,
    children: Vec<Xml>,
}
impl Xml {
    fn child(&self, name: &str) -> Result<&Xml> {
        let mut matches = self.children.iter().filter(|n| n.name == name);
        let node = matches.next().ok_or(Error::InvalidResponse)?;
        if matches.next().is_some() {
            return Err(Error::InvalidResponse);
        }
        Ok(node)
    }
    fn scalar(&self, name: &str) -> Result<&str> {
        let child = self.child(name)?;
        if !child.children.is_empty() {
            return Err(Error::InvalidResponse);
        }
        Ok(&child.text)
    }
    fn only(&self, names: &[&str]) -> Result<()> {
        if !self.text.trim().is_empty()
            || self
                .children
                .iter()
                .any(|n| !names.contains(&n.name.as_str()))
        {
            return Err(Error::InvalidResponse);
        }
        // Repeated scalar fields (even unused metadata) are ambiguous.
        let mut seen = HashSet::new();
        for node in &self.children {
            if !matches!(node.name.as_str(), "folder" | "file") && !seen.insert(&node.name) {
                return Err(Error::InvalidResponse);
            }
        }
        Ok(())
    }
}
fn unescape(text: &str) -> Result<String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        let end = rest.find(';').ok_or(Error::UnsupportedXml)?;
        let entity = &rest[..end];
        let c = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).map_err(|_| Error::UnsupportedXml)?
                } else if let Some(decimal) = entity.strip_prefix('#') {
                    decimal.parse::<u32>().map_err(|_| Error::UnsupportedXml)?
                } else {
                    return Err(Error::UnsupportedXml);
                };
                char::from_u32(number).ok_or(Error::UnsupportedXml)?
            }
        };
        if c.is_control() && !matches!(c, '\n' | '\r' | '\t') {
            return Err(Error::UnsupportedXml);
        }
        out.push(c);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}
fn parse_xml(text: &str) -> Result<Xml> {
    if text.len() > MAX_BODY {
        return Err(Error::Oversized);
    }
    if text.contains("]]>")
        || text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        return Err(Error::UnsupportedXml);
    }
    let mut rest = text.trim_start_matches('\u{feff}').trim();
    if rest.starts_with("<?xml") {
        let end = rest.find("?>").ok_or(Error::UnsupportedXml)?;
        let declaration = &rest[..end + 2];
        if !matches!(
            declaration,
            "<?xml version=\"1.0\"?>"
                | "<?xml version=\"1.0\" encoding=\"UTF-8\"?>"
                | "<?xml version='1.0' encoding='UTF-8'?>"
        ) {
            return Err(Error::UnsupportedXml);
        }
        rest = &rest[end + 2..];
    }
    let mut stack: Vec<Xml> = Vec::new();
    let mut root = None;
    let mut elements = 0;
    while !rest.is_empty() {
        if !rest.starts_with('<') {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = &rest[..end];
            if let Some(node) = stack.last_mut() {
                node.text.push_str(&unescape(text)?);
            } else if !text.trim().is_empty() {
                return Err(Error::UnsupportedXml);
            }
            rest = &rest[end..];
            continue;
        }
        let end = rest.find('>').ok_or(Error::UnsupportedXml)?;
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        let closing = tag.starts_with('/');
        let empty = tag.ends_with('/');
        let name = if closing {
            &tag[1..]
        } else if empty {
            &tag[..tag.len() - 1]
        } else {
            tag
        };
        if name.is_empty()
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || !name.as_bytes()[0].is_ascii_alphabetic()
            || (closing && empty)
        {
            return Err(Error::UnsupportedXml);
        }
        if !closing {
            elements += 1;
            if stack.len() >= 12 || elements > 20000 || (stack.is_empty() && root.is_some()) {
                return Err(Error::UnsupportedXml);
            }
            stack.push(Xml {
                name: name.into(),
                text: String::new(),
                children: Vec::new(),
            });
        }
        if closing || empty {
            let node = stack.pop().ok_or(Error::UnsupportedXml)?;
            if node.name != name {
                return Err(Error::UnsupportedXml);
            }
            if let Some(parent) = stack.last_mut() {
                parent.children.push(node);
            } else if root.replace(node).is_some() {
                return Err(Error::UnsupportedXml);
            }
        }
    }
    if !stack.is_empty() {
        return Err(Error::UnsupportedXml);
    }
    root.ok_or(Error::UnsupportedXml)
}
