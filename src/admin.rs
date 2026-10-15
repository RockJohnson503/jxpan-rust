//! Private administrator sessions. Only token digests are persisted; one active session.
use crate::storage::{Storage, StorageError, Update};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

const TTL: i64 = 604_800;
const SESSION_KEY: &str = "rust_admin_session";

type Result<T> = std::result::Result<T, StorageError>;
struct Inner {
    storage: Storage,
    mac: Hmac<Sha256>,
    login_tag: Option<Vec<u8>>,
    next_login: Mutex<Instant>,
}
#[derive(Clone)]
pub struct Administrator(Arc<Inner>);
impl Administrator {
    pub fn new(storage: Storage, username: &str, password: &str) -> Self {
        let mac = Hmac::<Sha256>::new_from_slice(uuid::Uuid::new_v4().as_bytes())
            .expect("HMAC accepts any key length");
        let login_tag = (!username.is_empty() && !password.is_empty()).then(|| {
            login_mac(mac.clone(), username, password)
                .finalize()
                .into_bytes()
                .to_vec()
        });
        Self(Arc::new(Inner {
            storage,
            mac,
            login_tag,
            next_login: Mutex::new(Instant::now()),
        }))
    }
    pub fn configured(&self) -> bool {
        self.0.login_tag.is_some()
    }
    /// Globally rate-limited to one attempt per second without unbounded IP maps.
    pub async fn login(&self, username: &str, password: &str) -> Result<Option<String>> {
        let Some(tag) = &self.0.login_tag else {
            return Ok(None);
        };
        let mut next = self.0.next_login.lock().await;
        if Instant::now() < *next {
            return Ok(None);
        }
        *next = Instant::now() + Duration::from_secs(1);
        drop(next);
        if login_mac(self.0.mac.clone(), username, password)
            .verify_slice(tag)
            .is_err()
        {
            return Ok(None);
        }
        let token = URL_SAFE_NO_PAD.encode(uuid::Uuid::new_v4().as_bytes())
            + &URL_SAFE_NO_PAD.encode(uuid::Uuid::new_v4().as_bytes());
        self.0
            .storage
            .put(
                SESSION_KEY,
                json!({"digest":digest(&token),"expires_at":now()?+TTL}),
                Some(TTL),
            )
            .await?;
        Ok(Some(token))
    }
    pub async fn authorized(&self, cookie: &str) -> Result<bool> {
        if !self.configured() {
            return Ok(false);
        }
        let Some(token) = token(cookie) else {
            return Ok(false);
        };
        let Some(session) = self.0.storage.get(SESSION_KEY).await? else {
            return Ok(false);
        };
        Ok(valid(&self.0.mac, &session, token, now()?))
    }
    /// Old cookies cannot log out a concurrently replaced session.
    pub async fn logout(&self, cookie: &str) -> Result<()> {
        let Some(token) = token(cookie).map(str::to_owned) else {
            return Ok(());
        };
        let mac = self.0.mac.clone();
        let now = now()?;
        self.0
            .storage
            .update(SESSION_KEY, move |current| {
                Ok(
                    if current
                        .as_ref()
                        .is_some_and(|v| valid(&mac, v, &token, now))
                    {
                        Update::Delete
                    } else {
                        Update::Keep
                    },
                )
            })
            .await?;
        Ok(())
    }
}
fn login_mac(mut mac: Hmac<Sha256>, user: &str, password: &str) -> Hmac<Sha256> {
    for value in [user, password] {
        mac.update(&(value.len() as u64).to_be_bytes());
        mac.update(value.as_bytes());
    }
    mac
}
fn digest(token: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes()))
}
fn valid(mac: &Hmac<Sha256>, session: &Value, token: &str, now: i64) -> bool {
    let Some(stored) = session["digest"].as_str() else {
        return false;
    };
    if session["expires_at"]
        .as_i64()
        .is_none_or(|expiry| expiry <= now)
    {
        return false;
    }
    let mut expected = mac.clone();
    expected.update(stored.as_bytes());
    let tag = expected.finalize().into_bytes();
    let mut candidate = mac.clone();
    candidate.update(digest(token).as_bytes());
    candidate.verify_slice(&tag).is_ok()
}
fn token(cookie: &str) -> Option<&str> {
    let value = cookie
        .split(';')
        .filter_map(|part| part.trim().split_once('='))
        .find(|(key, _)| *key == "admin_token")?
        .1;
    (value.len() == 44
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_')))
    .then_some(value)
}
fn now() -> Result<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StorageError::Clock)
        .and_then(|d| i64::try_from(d.as_secs()).map_err(|_| StorageError::Clock))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn persistent_digests_expiry_logout_and_disabled_admin() {
        let storage = Storage::open(":memory:", 16).await.unwrap();
        let admin = Administrator::new(storage.clone(), "synthetic", "synthetic-pass");
        let token = admin
            .login("synthetic", "synthetic-pass")
            .await
            .unwrap()
            .unwrap();
        let cookie = format!("other=x; admin_token={token}; unrelated=x");
        assert!(admin.authorized(&cookie).await.unwrap());
        assert!(!admin.authorized("admin_token=invalid").await.unwrap());
        let stored = storage.get(SESSION_KEY).await.unwrap().unwrap();
        assert!(!stored.to_string().contains(&token));
        let restarted = Administrator::new(storage.clone(), "synthetic", "synthetic-pass");
        assert!(restarted.authorized(&cookie).await.unwrap());
        assert!(
            admin
                .login("synthetic", "synthetic-pass")
                .await
                .unwrap()
                .is_none()
        );
        restarted.logout(&cookie).await.unwrap();
        assert!(!admin.authorized(&cookie).await.unwrap());
        storage
            .put(
                SESSION_KEY,
                json!({"digest":digest(&token),"expires_at":now().unwrap()}),
                None,
            )
            .await
            .unwrap();
        assert!(!admin.authorized(&cookie).await.unwrap());
        assert!(
            !Administrator::new(storage.clone(), "", "")
                .authorized(&cookie)
                .await
                .unwrap()
        );
        storage.close().await.unwrap();
    }
}
