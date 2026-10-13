//! Offline compatibility primitives for the pinned deployed MobileCloudParser.
//! No HTTP, credential storage, logging, login, refresh, save or delete operations.
use crate::compat::{decode_uri_component, truthy};
use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig, general_purpose::STANDARD},
};
use serde_json::Value;

pub const DEPLOYMENT_SHA256: &str =
    "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118";

fn js_space(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}')
}
fn selected_auth(v: &Value) -> Option<&str> {
    let selected = if v.is_object() || v.is_array() {
        v.get("authorization")
            .filter(|x| truthy(x))
            .or_else(|| v.get("auth").filter(|x| truthy(x)))?
    } else {
        v
    };
    selected.as_str().filter(|s| !s.is_empty())
}
fn strip_basic(s: &str) -> &str {
    if s.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("basic"))
        && s[5..].chars().next().is_some_and(js_space)
    {
        s[5..].trim_start_matches(js_space)
    } else {
        s
    }
}
/// Account-extractor normalization followed by byte-preserving atob decoding.
pub fn decode_authorization(authorization: &str) -> Result<String, &'static str> {
    decode_base64_latin1(strip_basic(authorization.trim_matches(js_space)))
}
/// WHATWG atob: ASCII whitespace only; missing padding and nonzero trailing bits allowed.
/// Bytes are mapped to U+0000..U+00FF, NOT decoded as UTF-8.
pub fn decode_base64_latin1(s: &str) -> Result<String, &'static str> {
    let mut b: Vec<u8> = s
        .bytes()
        .filter(|b| !matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
        .collect();
    if b.len().is_multiple_of(4) {
        for _ in 0..2 {
            if b.last() == Some(&b'=') {
                b.pop();
            }
        }
    }
    if b.len() % 4 == 1
        || b.iter()
            .any(|b| !b.is_ascii_alphanumeric() && !matches!(b, b'+' | b'/'))
    {
        return Err("invalid base64");
    }
    let engine = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let bytes = engine.decode(b).map_err(|_| "invalid base64")?;
    Ok(bytes.into_iter().map(char::from).collect())
}
/// btoa-compatible encoding; rejects code points outside the Latin-1 byte range.
pub fn encode_authorization(decoded: &str) -> Result<String, &'static str> {
    let bytes: Result<Vec<u8>, _> = decoded
        .chars()
        .map(|c| u8::try_from(c as u32).map_err(|_| "not Latin-1"))
        .collect();
    Ok(format!("Basic {}", STANDARD.encode(bytes?)))
}
/// Exact extractAccountFromAuth behavior: invalid input => None, decode error => "default".
/// The first pipe segment is split on colon; index 1 is returned, even when empty.
pub fn extract_account_from_auth(input: &Value) -> Option<String> {
    let s = selected_auth(input)?;
    match decode_authorization(s) {
        Ok(decoded) => {
            let first = decoded.split('|').next().unwrap_or("");
            Some(first.split(':').nth(1).unwrap_or(first).to_owned())
        }
        Err(_) => Some("default".into()),
    }
}
/// buildHeaders' authorization field only. Unlike account extraction this does not
/// accept objects: deployed .replace throws for truthy non-string credentials.
pub fn normalize_authorization_header(s: &str) -> Option<String> {
    if s.is_empty() {
        return None;
    }
    let clean: String = s.chars().filter(|c| !js_space(*c)).collect();
    Some(if let Some(rest) = clean.strip_prefix("Basic") {
        format!("Basic {rest}")
    } else {
        clean
    })
}

fn match_marker<'a>(s: &'a str, marker: &str, question: bool) -> Option<&'a str> {
    let lower = s.to_ascii_lowercase();
    let mut start = 0;
    while let Some(relative) = lower[start..].find(marker) {
        let begin = start + relative + marker.len();
        let len = s.as_bytes()[begin..]
            .iter()
            .take_while(|c| c.is_ascii_alphanumeric())
            .count();
        let end = begin + len;
        if len > 0
            && (end == s.len()
                || s.as_bytes()[end] == b'&'
                || (question && s.as_bytes()[end] == b'?'))
        {
            return Some(&s[begin..end]);
        }
        start = begin;
    }
    None
}
/// Compatibility recognition, NOT a hostname/security validator. Fallback regexes
/// accept bare /w/i/ and linkId= in arbitrary text, including foreign hosts.
/// Performs one all-or-nothing decodeURIComponent and preserves pattern priority.
pub fn extract_share_info(input: &str) -> Option<String> {
    let trimmed = input.trim_matches(js_space);
    let decoded = decode_uri_component(trimmed);
    let s = decoded.as_deref().unwrap_or(trimmed);
    let groups: [(&[&str], bool); 3] = [
        (
            &[
                "https://yun.139.com/shareweb/#/w/i/",
                "http://yun.139.com/shareweb/#/w/i/",
                "https://caiyun.139.com/shareweb/#/w/i/",
                "http://caiyun.139.com/shareweb/#/w/i/",
            ],
            true,
        ),
        (
            &[
                "https://yun.139.com/shareweb/?linkid=",
                "http://yun.139.com/shareweb/?linkid=",
                "https://caiyun.139.com/shareweb/?linkid=",
                "http://caiyun.139.com/shareweb/?linkid=",
            ],
            false,
        ),
        (
            &[
                "https://yun.139.com/link/?linkid=",
                "http://yun.139.com/link/?linkid=",
                "https://caiyun.139.com/link/?linkid=",
                "http://caiyun.139.com/link/?linkid=",
            ],
            false,
        ),
    ];
    let mut found = None;
    for (markers, question) in groups {
        // Each deployed regex alternates hosts/schemes, thus earliest match wins.
        found = markers
            .iter()
            .filter_map(|m| match_marker(s, m, question))
            .min_by_key(|m| m.as_ptr() as usize);
        if found.is_some() {
            break;
        }
    }
    let id = found
        .or_else(|| match_marker(s, "/w/i/", true))
        .or_else(|| match_marker(s, "linkid=", false))?;
    Some(id.strip_suffix("https").unwrap_or(id).to_owned())
}
/// Strict JS comparisons; notably coType:"1", resType:"1", isDir:1 are false.
/// JSON null is an error in JS; callers must reject it before this object API.
pub fn try_is_folder(file: &Value) -> Result<bool, &'static str> {
    if file.is_null() {
        Err("file is null")
    } else {
        Ok(is_folder(file))
    }
}
pub fn is_folder(file: &Value) -> bool {
    let one = |v: Option<&Value>| v.and_then(Value::as_f64) == Some(1.0);
    one(file.get("coType"))
        || one(file.get("resType"))
        || file.get("isDir") == Some(&Value::Bool(true))
        || file.get("isDir").and_then(Value::as_str) == Some("1")
        || file.get("type").and_then(Value::as_str) == Some("folder")
}

/// Current deployment's function named md5 is NOT RFC 1321 MD5. Preserve its
/// extra b += aa on every round, sparse JS words, and big-endian hex output.
/// This is a compatibility checksum, not a cryptographic primitive.
pub fn deployment_md5(input: &str) -> String {
    const K: [u32; 64] = [
        3614090360, 3905402710, 606105819, 3250441966, 4118548399, 1200080426, 2821735955,
        4249261313, 1770035416, 2336552879, 4294925233, 2304563134, 1804603682, 4254626195,
        2792965006, 1236535329, 4129170786, 3225465664, 643717713, 3921069994, 3593408605,
        38016083, 3634488961, 3889429448, 568446438, 3275163606, 4107603335, 1163531501,
        2850285829, 4243563512, 1735328473, 2368359562, 4294588738, 2272392833, 1839030562,
        4259657740, 2763975236, 1272893353, 4139469664, 3200236656, 681279174, 3936430074,
        3572445317, 76029189, 3654602809, 3873151461, 530742520, 3299628645, 4096336452,
        1126891415, 2878612391, 4237533241, 1700485571, 2399980690, 4293915773, 2240044497,
        1873313359, 4264355552, 2734768916, 1309151649, 4149444226, 3174756917, 718787259,
        3951481745,
    ];
    const S: [u32; 16] = [7, 12, 17, 22, 5, 9, 14, 20, 4, 11, 16, 23, 6, 10, 15, 21];
    let bytes = input.as_bytes();
    let len = bytes.len();
    let last = ((len + 8) / 64 + 1) * 16 - 2;
    let mut words = vec![None; last + 1];
    for (i, byte) in bytes.iter().enumerate() {
        words[i / 4] = Some(words[i / 4].unwrap_or(0u32) | ((*byte as u32) << ((i % 4) * 8)));
    }
    words[len / 4] = Some(words[len / 4].unwrap_or(0) | (128 << ((len % 4) * 8)));
    words[last] = Some((len as u32).wrapping_mul(8));
    let (mut a, mut b, mut c, mut d) = (1732584193u32, 4023233417u32, 2562383102u32, 271733878u32);
    for k in (0..words.len()).step_by(16) {
        let (aa, bb, cc, dd) = (a, b, c, d);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | (!b & d), i)
            } else if i < 32 {
                ((d & b) | (!d & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * i) % 16)
            };
            let rotated = words
                .get(k + g)
                .copied()
                .flatten()
                .map(|w| {
                    f.wrapping_add(a)
                        .wrapping_add(K[i])
                        .wrapping_add(w)
                        .rotate_left(S[(i / 16) * 4 + i % 4])
                })
                .unwrap_or(0);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated).wrapping_add(aa);
        }
        a = a.wrapping_add(aa);
        b = b.wrapping_add(bb);
        c = c.wrapping_add(cc);
        d = d.wrapping_add(dd);
    }
    format!("{a:08x}{b:08x}{c:08x}{d:08x}")
}
/// pythonQuoteExact processes UTF-16 units individually; astral characters become
/// TWO U+FFFD encodings, rather than one UTF-8 code point.
pub fn python_quote_exact(input: &str) -> String {
    let mut out = String::new();
    for unit in input.encode_utf16() {
        if unit <= 127 && (unit as u8).is_ascii_alphanumeric() {
            out.push(char::from(unit as u8));
        } else {
            let ch = char::from_u32(unit as u32).unwrap_or('\u{fffd}');
            let mut buf = [0; 4];
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                use std::fmt::Write;
                let _ = write!(out, "%{b:02X}");
            }
        }
    }
    out
}
/// Deterministic inner generateMcloudSign. json must be the EXACT JSON.stringify
/// output (including key order and escaping), None means a JS-falsy body.
/// Timestamp is runtime-local YYYY-MM-DD HH:mm:ss; nonce is 16 base62 chars.
/// The deployed deviceId and version arguments do not affect the signature.
pub fn generate_mcloud_sign(json: Option<&str>, timestamp: &str, nonce: &str) -> String {
    let quoted = json.map(python_quote_exact).unwrap_or_default();
    let mut sorted = quoted.into_bytes();
    sorted.sort_unstable();
    let r = deployment_md5(&STANDARD.encode(sorted));
    let c = deployment_md5(&format!("{timestamp}:{nonce}"));
    let sign = deployment_md5(&format!("{r}{c}")).to_ascii_uppercase();
    format!("{timestamp},{nonce},{sign}")
}
