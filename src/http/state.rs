use std::{env, error::Error, fs::File, io::Read, sync::Arc, time::Duration};

use hmac::{Hmac, Mac};
use reqwest::{Client, redirect::Policy};
use serde_json::{Value, json};
use sha2::Sha256;

use super::{params::Params, reply::Reply};
use crate::cache::{BoundedTtlCache, CacheConfig};

#[derive(Clone)]
pub struct AppState {
    pub(crate) http: Client,
    pub(crate) credentials: Arc<Value>,
    pub(crate) parse_cache: BoundedTtlCache<Reply>,
    pub(crate) download_cache: BoundedTtlCache<Reply>,
    key_mac: Hmac<Sha256>,
}

impl AppState {
    pub fn from_env() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut credentials = match env::var("JXPAN_CREDENTIALS_FILE") {
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
        for provider in ["mcloud", "cloud189"] {
            if credentials.get(provider).is_none() {
                credentials[provider] = json!({});
            }
            if !credentials[provider].is_object() {
                return Err("网盘凭据必须为 JSON 对象".into());
            }
        }
        for (provider, field, variables) in [
            (
                "mcloud",
                "authorization",
                ["MCLOUD_AUTHORIZATION", "MCLOUD_AUTH"],
            ),
            (
                "cloud189",
                "open_access_token",
                ["CLOUD189_TOKEN", "P189_TOKEN"],
            ),
        ] {
            if credentials[provider][field]
                .as_str()
                .is_none_or(str::is_empty)
            {
                for key in variables {
                    if let Ok(value) = env::var(key)
                        && !value.is_empty()
                    {
                        credentials[provider][field] = value.into();
                        break;
                    }
                }
            }
        }
        let http = Client::builder()
            .redirect(Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .pool_max_idle_per_host(4)
            .build()?;
        let config = CacheConfig {
            ttl: Duration::from_secs(600),
            max_entries: 64,
            max_bytes: 8 * 1024 * 1024,
            max_key_bytes: 64,
            max_in_flight: 8,
        };
        let key_mac = credential_mac(&credentials, uuid::Uuid::new_v4().as_bytes());
        Ok(Self {
            http,
            credentials: Arc::new(credentials),
            key_mac,
            parse_cache: BoundedTtlCache::new(config),
            download_cache: BoundedTtlCache::new(CacheConfig {
                ttl: Duration::from_secs(60),
                max_bytes: 256 * 1024,
                ..config
            }),
        })
    }

    pub(crate) fn cache_key(&self, params: &Params) -> String {
        request_key(self.key_mac.clone(), params)
    }
}

fn credential_mac(credentials: &Value, secret: &[u8]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC accepts any key length");
    mac.update(&serde_json::to_vec(credentials).expect("JSON Value is serializable"));
    mac
}

fn request_key(mut mac: Hmac<Sha256>, params: &Params) -> String {
    for name in ["url", "pwd", "type", "sid", "p189_token", "auth"] {
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
    let digest = mac.finalize().into_bytes();
    let mut key = String::with_capacity(64);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        key.push(HEX[(byte >> 4) as usize] as char);
        key.push(HEX[(byte & 15) as usize] as char);
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_isolate_all_private_and_directory_inputs() {
        let mac = credential_mac(
            &json!({"mcloud":{"authorization":"synthetic"}}),
            b"test-key",
        );
        let base = Params::query(Some(
            "url=share&pwd=pw&type=json&sid=s&id=f&fid=root&auth=a&p189_token=t",
        ));
        let key = request_key(mac.clone(), &base);
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
            assert_ne!(key, request_key(mac.clone(), &Params::query(Some(query))));
        }
        let different_account =
            credential_mac(&json!({"mcloud":{"authorization":"other"}}), b"test-key");
        assert_ne!(key, request_key(different_account, &base));
        assert_eq!(key, request_key(mac, &base));
    }
}
