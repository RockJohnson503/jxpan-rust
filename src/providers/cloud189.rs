//! Offline compatibility primitives for the current deployed Cloud189Parser.
//! This is not an HTTP client or a security/domain validation boundary.

use crate::compat::decode_uri_component;
use hmac::{Hmac, Mac};
use md5::{Digest, Md5};
use regex::Regex;
use sha1::Sha1;
use std::{fmt, sync::OnceLock};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareInfo {
    pub share_code: String,
    /// The original string after scheme prefixing and one successful decodeURIComponent.
    pub share_url: String,
    /// JavaScript RegExp.toString(), or `url_param_code`.
    pub matched_pattern: String,
}

/// Matches the deployed method, including unanchored domain matching and permissive
/// `code` fallback on unrelated hosts. Do not use this to authorize network requests.
pub fn extract_share_info(input: &str) -> Option<ShareInfo> {
    let prefixed = if input.starts_with("http://") || input.starts_with("https://") {
        input.to_owned()
    } else {
        format!("https://{input}")
    };
    let share_url = decode_uri_component(&prefixed).unwrap_or(prefixed);
    static PATTERNS: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let patterns = PATTERNS.get_or_init(|| {
        [
            (
                r"(?i-u:cloud\.189\.cn/web/share\?)[^\n\r\u{2028}\u{2029}]*(?i-u:code=([a-zA-Z0-9]+))",
                r"/cloud\.189\.cn\/web\/share\?.*code=([a-zA-Z0-9]+)/i",
            ),
            (
                r"(?i-u:cloud\.189\.cn/t/([a-zA-Z0-9]+))",
                r"/cloud\.189\.cn\/t\/([a-zA-Z0-9]+)/i",
            ),
            (
                r"(?i-u:h5\.cloud\.189\.cn/t/([a-zA-Z0-9]+))",
                r"/h5\.cloud\.189\.cn\/t\/([a-zA-Z0-9]+)/i",
            ),
            (
                r"(?i-u:m\.cloud\.189\.cn/t/([a-zA-Z0-9]+))",
                r"/m\.cloud\.189\.cn\/t\/([a-zA-Z0-9]+)/i",
            ),
            (
                r"(?i-u:cloud\.189\.cn/s/([a-zA-Z0-9_-]+))",
                r"/cloud\.189\.cn\/s\/([a-zA-Z0-9_-]+)/i",
            ),
        ]
        .into_iter()
        .map(|(pattern, label)| (Regex::new(pattern).expect("fixed share regex"), label))
        .collect()
    });
    for (pattern, label) in patterns {
        if let Some(captures) = pattern.captures(&share_url) {
            return Some(ShareInfo {
                share_code: captures[1].to_owned(),
                share_url,
                matched_pattern: (*label).to_owned(),
            });
        }
    }
    let parsed = Url::parse(&share_url).ok()?;
    // URLSearchParams.get returns the FIRST matching key, even if empty.
    let (_, code) = parsed.query_pairs().find(|(key, _)| key == "code")?;
    if code.encode_utf16().count() <= 5 {
        return None;
    }
    Some(ShareInfo {
        share_code: code.into_owned(),
        share_url,
        matched_pattern: "url_param_code".to_owned(),
    })
}

fn hex(bytes: &[u8]) -> String {
    use fmt::Write;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("String writes cannot fail");
    }
    output
}

/// UTF-8 MD5, lowercase, always 32 hex digits.
pub fn md5(input: &str) -> String {
    hex(&Md5::digest(input.as_bytes()))
}

/// Deployed BigInt("0x" + md5).toString(16), NOT fixed-width hexadecimal.
pub fn java_md5(input: &str) -> String {
    let digest = md5(input);
    let trimmed = digest.trim_start_matches('0');
    if trimmed.is_empty() {
        "0".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureError {
    EmptyKey,
}

impl fmt::Display for SignatureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WebCrypto HMAC keys must not be empty")
    }
}

impl std::error::Error for SignatureError {}

/// UTF-8 HMAC-SHA1, lowercase, exactly 40 hex digits. An empty key is rejected
/// like the original WebCrypto importKey, although mathematical HMAC permits it.
pub fn hmac_sha1(key: &str, message: &str) -> Result<String, SignatureError> {
    if key.is_empty() {
        return Err(SignatureError::EmptyKey);
    }
    let mut mac =
        Hmac::<Sha1>::new_from_slice(key.as_bytes()).expect("HMAC accepts any nonempty key");
    mac.update(message.as_bytes());
    Ok(hex(&mac.finalize().into_bytes()))
}

/// Raw concatenation in deployment order: do not sort or URL-encode these values.
pub fn open_download_signing_text(
    token: &str,
    timestamp: &str,
    file_id: &str,
    share_id: &str,
) -> String {
    format!("AccessToken={token}&Timestamp={timestamp}&dt=1&fileId={file_id}&shareId={share_id}")
}

/// The deployed listing signature excludes pagination, password and shareMode.
pub fn open_list_signing_text(
    token: &str,
    timestamp: &str,
    folder_id: &str,
    share_id: &str,
) -> String {
    format!("AccessToken={token}&Timestamp={timestamp}&fileId={folder_id}&shareId={share_id}")
}

/// Caller supplies the exact HTTP Date, method and path (no query/body).
/// No case conversion or date reformatting is performed.
pub fn app_signing_text(session_key: &str, method: &str, request_uri: &str, date: &str) -> String {
    format!("SessionKey={session_key}&Operate={method}&RequestURI={request_uri}&Date={date}")
}

/// Web portal uses fixed-width MD5 of appkey + timestamp + sign-type.
pub fn web_signing_text(timestamp: &str) -> String {
    format!("600100422{timestamp}1")
}
