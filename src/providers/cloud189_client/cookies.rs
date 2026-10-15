//! Response-cookie expiry and extraction.
use std::time::SystemTime;

fn deletion_cookie(cookie: &str, now: SystemTime) -> bool {
    let mut max_age = None;
    let mut expires = None;
    for attribute in cookie.split(';').skip(1) {
        let Some((name, value)) = attribute.trim().split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("max-age") {
            if let Ok(age) = value.trim().parse::<i64>() {
                max_age = Some(age);
            }
        } else if name.trim().eq_ignore_ascii_case("expires")
            && let Ok(date) = httpdate::parse_http_date(value.trim())
        {
            expires = Some(date);
        }
    }
    max_age
        .map(|age| age <= 0)
        .unwrap_or_else(|| expires.is_some_and(|date| date <= now))
}
pub(super) fn extract_cookies(raw: &[String], now: SystemTime) -> Vec<String> {
    let extract = |cookie: &str| -> Option<String> {
        if deletion_cookie(cookie, now) {
            return None;
        }
        let pair = cookie.split(';').next()?.trim();
        let (name, value) = pair.split_once('=')?;
        if name.is_empty() || value.is_empty() {
            None
        } else {
            Some(pair.to_owned())
        }
    };
    // reqwest exposes individual Set-Cookie headers. If a server coalesces them,
    // split only before another cookie-pair, never the comma inside Expires.
    raw.iter()
        .flat_map(|header| {
            let mut start = 0;
            let mut fields = Vec::new();
            for (index, _) in header.match_indices(',') {
                let next = header[index + 1..].trim_start();
                let name = next.split(['=', ';', ',']).next().unwrap_or("");
                if !name.is_empty()
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
                    && next.as_bytes().get(name.len()) == Some(&b'=')
                {
                    fields.push(&header[start..index]);
                    start = index + 1;
                }
            }
            fields.push(&header[start..]);
            fields
        })
        .filter_map(extract)
        .collect()
}
