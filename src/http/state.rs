use super::{params::Params, reply::Reply};
use crate::{
    admin::Administrator,
    auth::AuthManager,
    cache::{BoundedTtlCache, CacheConfig},
    records::Records,
    settings::ParseSettings,
    shortlinks::ShortLinks,
    storage::Storage,
};
use hmac::{Hmac, Mac};
use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};
use sha2::Sha256;
use std::{
    env,
    error::Error,
    fs::File,
    io::Read,
    sync::{Arc, Mutex},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
#[derive(Clone)]
pub struct AppState {
    pub(crate) http: Client,
    pub(crate) auth: AuthManager,
    pub(crate) admin: Administrator,
    pub(crate) records: Records,
    pub(crate) parse_settings: ParseSettings,
    pub(crate) requests: Arc<tokio::sync::Semaphore>,
    pub(crate) shortlinks: ShortLinks,
    pub(crate) parse_cache: BoundedTtlCache<Reply>,
    pub(crate) download_cache: BoundedTtlCache<Reply>,
    pub(crate) directories: BoundedTtlCache<Vec<String>>,
    pub(crate) storage: Storage,
    pub(crate) public_origin: Option<url::Url>,
    cache_revision: Arc<Mutex<u64>>,
    key_mac: Hmac<Sha256>,
}
impl AppState {
    pub async fn from_env() -> Result<Self> {
        let credentials = match env::var("JXPAN_CREDENTIALS_FILE") {
            Ok(path) => {
                let mut data = Vec::new();
                File::open(path)
                    .map_err(|_| "无法读取凭据文件")?
                    .take(64 * 1024 + 1)
                    .read_to_end(&mut data)
                    .map_err(|_| "无法读取凭据文件")?;
                if data.len() > 64 * 1024 {
                    return Err("凭据文件超过大小上限".into());
                }
                serde_json::from_slice::<Value>(&data).map_err(|_| "凭据文件不是有效 JSON")?
            }
            Err(env::VarError::NotPresent) => json!({}),
            Err(_) => return Err("凭据文件路径编码无效".into()),
        };
        if !credentials.is_object() {
            return Err("凭据配置必须为 JSON 对象".into());
        }
        for name in ["cloud189", "mcloud"] {
            if credentials.get(name).is_some_and(|v| !v.is_object()) {
                return Err("网盘凭据必须为 JSON 对象".into());
            }
        }
        let public_origin = match env::var("JXPAN_PUBLIC_ORIGIN") {
            Ok(value) => {
                let url = url::Url::parse(&value).map_err(|_| "公开服务地址无效")?;
                if !matches!(url.scheme(), "https" | "http")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.path() != "/"
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err("公开服务地址必须是 HTTP(S) origin".into());
                }
                Some(url)
            }
            Err(env::VarError::NotPresent) => None,
            Err(_) => return Err("公开服务地址编码无效".into()),
        };
        let path = env::var("JXPAN_DB_PATH").unwrap_or_else(|_| "jxpan.sqlite".into());
        let user = env::var("JXPAN_ADMIN_USER")
            .or_else(|_| env::var("admin"))
            .unwrap_or_default();
        let pass = env::var("JXPAN_ADMIN_PASSWORD")
            .or_else(|_| env::var("pass"))
            .unwrap_or_default();
        let mut state =
            Self::new(Storage::open(path, 128).await?, credentials, &user, &pass).await?;
        state.public_origin = public_origin;
        Ok(state)
    }
    /// Explicit setup for embedded use and isolated tests; never reads credential files.
    pub async fn new(storage: Storage, initial: Value, user: &str, password: &str) -> Result<Self> {
        let http = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(4)
            .build()?;
        let auth = AuthManager::new(storage.clone(), http.clone(), initial).await?;
        let records = Records::new(storage.clone()).await?;
        let shortlinks = ShortLinks::new(storage.clone()).await?;
        let parse_settings = ParseSettings::new(storage.clone()).await?;
        let config = CacheConfig {
            ttl: Duration::from_secs(600),
            max_entries: 64,
            max_bytes: 8 * 1024 * 1024,
            max_key_bytes: 64,
            max_in_flight: 8,
        };
        Ok(Self {
            http,
            auth,
            admin: Administrator::new(storage.clone(), user, password),
            records,
            parse_settings,
            requests: Arc::new(tokio::sync::Semaphore::new(64)),
            shortlinks,
            storage,
            public_origin: None,
            cache_revision: Arc::new(Mutex::new(0)),
            key_mac: Hmac::<Sha256>::new_from_slice(uuid::Uuid::new_v4().as_bytes())
                .expect("HMAC accepts any key length"),
            parse_cache: BoundedTtlCache::new(config),
            download_cache: BoundedTtlCache::new(CacheConfig {
                ttl: Duration::from_secs(600),
                max_bytes: 256 * 1024,
                ..config
            }),
            directories: BoundedTtlCache::new(CacheConfig {
                max_bytes: 256 * 1024,
                ..config
            }),
        })
    }
    /// Revision and credentials are one atomic snapshot. Old in-flight requests retain
    /// their own account, but cannot populate a new account's keys after an auth change.
    pub(crate) fn account(&self) -> (u64, Arc<Value>) {
        let mut previous = self.cache_revision.lock().expect("cache revision poisoned");
        let (revision, snapshot) = self.auth.versioned_snapshot();
        if *previous != revision {
            self.parse_cache.clear();
            self.download_cache.clear();
            self.directories.clear();
            *previous = revision;
        }
        (revision, snapshot)
    }
    pub(crate) fn cache_key(&self, params: &Params, revision: u64) -> String {
        request_key(self.key_mac.clone(), params, revision)
    }
    pub(crate) fn directory_key(&self, params: &Params, revision: u64, sid: &str) -> String {
        let mut mac = self.key_mac.clone();
        mac.update(b"directory");
        mac.update(&revision.to_be_bytes());
        for value in [
            params.get("url"),
            params.get("pwd"),
            params.get("auth"),
            params.get("p189_token"),
            params.get("c189_token"),
            sid,
        ] {
            mac.update(&(value.len() as u64).to_be_bytes());
            mac.update(value.as_bytes());
        }
        hex(mac)
    }
    /// Reclaims expired records even without requests; only the parse history table.
    pub async fn maintain(&self) -> crate::storage::Result<usize> {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| crate::storage::StorageError::Clock)?
            .as_millis();
        self.records
            .cleanup_at(i64::try_from(ms).map_err(|_| crate::storage::StorageError::Clock)?)
            .await
    }
    pub async fn close(&self) -> crate::storage::Result<()> {
        self.storage.close().await
    }
}
fn request_key(mut mac: Hmac<Sha256>, params: &Params, revision: u64) -> String {
    mac.update(&revision.to_be_bytes());
    for name in [
        "action",
        "url",
        "pwd",
        "type",
        "sid",
        "p189_token",
        "c189_token",
        "auth",
        "share_id",
        "share_code",
        "share_mode",
        "folder_id",
        "target_file_id",
        "file_id",
        "file_name",
        "file_size",
    ] {
        let value = params.get(name).as_bytes();
        mac.update(&(value.len() as u64).to_be_bytes());
        mac.update(value);
    }
    let id = params.file_id().as_bytes();
    mac.update(&(id.len() as u64).to_be_bytes());
    mac.update(id);
    for folder in params.folders() {
        mac.update(&(folder.len() as u64).to_be_bytes());
        mac.update(folder.as_bytes());
    }
    hex(mac)
}
fn hex(mac: Hmac<Sha256>) -> String {
    let mut key = String::with_capacity(64);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in mac.finalize().into_bytes() {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 15) as usize] as char);
    }
    key
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_keys_isolate_all_private_directory_and_revision_inputs() {
        let mac = Hmac::<Sha256>::new_from_slice(b"synthetic").unwrap();
        let base = Params::query(Some(
            "url=share&pwd=pw&type=json&sid=s&id=f&fid=root&auth=a&p189_token=t",
        ));
        let key = request_key(mac.clone(), &base, 0);
        assert_eq!(key.len(), 64);
        for query in [
            "url=other",
            "url=share&pwd=other",
            "url=share&type=down",
            "url=share&sid=other",
            "url=share&id=other",
            "url=share&fid=other",
            "url=share&auth=other",
            "url=share&p189_token=other",
        ] {
            assert_ne!(
                key,
                request_key(mac.clone(), &Params::query(Some(query)), 0)
            );
        }
        assert_ne!(key, request_key(mac.clone(), &base, 1));
        assert_eq!(key, request_key(mac, &base, 0));
    }
}
