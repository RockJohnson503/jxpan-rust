//! Stored credential decoding, validation, seed precedence, and snapshot projection.
use super::{AuthError, Provider, Result, ca};
use serde_json::{Value, json};

pub(super) fn decode_stored(v: Value, provider: Provider) -> Result<Value> {
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
pub(super) fn credential_changed(provider: Provider, before: &Value, after: &Value) -> bool {
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
pub(super) fn normalize(provider: Provider, mut v: Value, now: u64) -> Result<Value> {
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
pub(super) fn seed(initial: &Value, provider: Provider, now: u64) -> Result<Option<Value>> {
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
pub(super) fn project(snapshot: &mut Value, provider: Provider, info: Option<&Value>) {
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
