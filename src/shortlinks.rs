//! Persistent short links on Storage's bounded SQLite owner; no second actor.
//! resolve returns ONLY a validated local root target (`/?...`), never an origin.
use crate::storage::{Storage, StorageError};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use url::Url;

pub const MAX_URL_BYTES: usize = 8192;
/// Reject only new targets at this bound; existing short links are never evicted.
pub const MAX_SHORT_LINKS: usize = 10_000;
pub const MAX_ATTEMPTS: usize = 10;
const ALPHABET: &[u8; 62] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

pub type Result<T> = std::result::Result<T, ShortLinkError>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortLinkError {
    InvalidUrl,
    InvalidCode,
    CollisionLimit,
    Capacity,
    Random,
    Storage(StorageError),
}
impl From<StorageError> for ShortLinkError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
impl std::fmt::Display for ShortLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidUrl => f.write_str("short link parser URL invalid"),
            Self::InvalidCode => f.write_str("short link code invalid"),
            Self::CollisionLimit => f.write_str("short link collision limit reached"),
            Self::Capacity => f.write_str("short link capacity reached"),
            Self::Random => f.write_str("short link randomness unavailable"),
            Self::Storage(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ShortLinkError {}

#[derive(Clone)]
pub struct ShortLinks {
    storage: Storage,
}
impl ShortLinks {
    pub async fn new(storage: Storage) -> Result<Self> {
        storage
            .with_connection(|db| {
                let tx = db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|_| StorageError::Database)?;
                tx.execute_batch(
                    "CREATE TABLE IF NOT EXISTS short_links (
                shortcode TEXT PRIMARY KEY,
                original_url TEXT NOT NULL,
                created_at INTEGER DEFAULT (strftime('%s', 'now')),
                access_count INTEGER DEFAULT 0,
                last_accessed INTEGER DEFAULT 0,
                expires_at INTEGER DEFAULT 0
            ); CREATE INDEX IF NOT EXISTS idx_shortlinks_expires ON short_links(expires_at);",
                )
                .map_err(|_| StorageError::Schema)?;
                {
                    let mut statement = tx
                        .prepare("PRAGMA table_info(short_links)")
                        .map_err(|_| StorageError::Schema)?;
                    let columns = statement
                        .query_map([], |row| {
                            Ok((
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                                row.get::<_, i64>(3)?,
                                row.get::<_, i64>(5)?,
                            ))
                        })
                        .map_err(|_| StorageError::Schema)?
                        .collect::<std::result::Result<Vec<_>, _>>()
                        .map_err(|_| StorageError::Schema)?;
                    for (name, kind) in [
                        ("shortcode", "TEXT"),
                        ("original_url", "TEXT"),
                        ("created_at", "INTEGER"),
                        ("access_count", "INTEGER"),
                        ("last_accessed", "INTEGER"),
                        ("expires_at", "INTEGER"),
                    ] {
                        let column = columns
                            .iter()
                            .find(|c| c.0 == name)
                            .ok_or(StorageError::Schema)?;
                        if !column.1.eq_ignore_ascii_case(kind)
                            || (name == "shortcode" && column.3 != 1)
                            || (name == "original_url" && column.2 != 1)
                        {
                            return Err(StorageError::Schema);
                        }
                    }
                    if columns.iter().any(|c| c.0 != "shortcode" && c.3 != 0) {
                        return Err(StorageError::Schema);
                    }
                }
                tx.commit().map_err(|_| StorageError::Database)
            })
            .await?;
        Ok(Self { storage })
    }

    /// Caller must check the supplied outer origin belongs to this service.
    /// Only safe parser requests are stored, without credentials or management actions.
    /// Reuses an identical canonical URL, including when the repository is full.
    pub async fn create(&self, original_url: &str) -> Result<String> {
        self.create_using(original_url, generate_shortcode).await
    }

    async fn create_using<F>(&self, original_url: &str, mut generate: F) -> Result<String>
    where
        F: FnMut() -> Result<String> + Send + 'static,
    {
        let target = validate_parser_url(original_url)?;
        let mut canonical = http_url(original_url)?;
        canonical.set_query(target.strip_prefix("/?"));
        let original_url: String = canonical.into();
        if original_url.len() > MAX_URL_BYTES {
            return Err(ShortLinkError::InvalidUrl);
        }
        self.storage
            .with_connection(move |db| {
                // Serialize reuse, count and insert across independent Storage owners.
                // No expiry/retention change: capacity never deletes historical links.
                let tx = db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|_| StorageError::Database)?;
                let existing = tx
                    .query_row(
                        "SELECT shortcode FROM short_links WHERE original_url = ?1
                         AND length(CAST(shortcode AS BLOB)) = 6
                         AND shortcode NOT GLOB '*[^A-Za-z0-9]*' LIMIT 1",
                        [&original_url],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(|_| StorageError::Database)?;
                if let Some(code) = existing {
                    return Ok(Ok(code));
                }
                let count: i64 = tx
                    .query_row("SELECT COUNT(*) FROM short_links", [], |row| row.get(0))
                    .map_err(|_| StorageError::Database)?;
                if count >= MAX_SHORT_LINKS as i64 {
                    return Ok(Err(ShortLinkError::Capacity));
                }
                for _ in 0..MAX_ATTEMPTS {
                    let code = match generate() {
                        Ok(code) => code,
                        Err(error) => return Ok(Err(error)),
                    };
                    if !valid_code(&code) {
                        return Ok(Err(ShortLinkError::InvalidCode));
                    }
                    // Conflict handling is atomic; unrelated targets never overwrite.
                    let inserted = tx
                        .execute(
                            "INSERT INTO short_links (shortcode, original_url, created_at)
                    VALUES (?1, ?2, strftime('%s', 'now')) ON CONFLICT(shortcode) DO NOTHING",
                            params![code, original_url],
                        )
                        .map_err(|_| StorageError::Database)?;
                    if inserted == 1 {
                        tx.commit().map_err(|_| StorageError::Database)?;
                        return Ok(Ok(code));
                    }
                }
                Ok(Err(ShortLinkError::CollisionLimit))
            })
            .await?
    }

    /// Returns a local root path+query. Build a fresh request on the trusted service
    /// origin, discarding the incoming /s query and body; do not append these params.
    /// expires_at is deliberately NOT enforced: the frozen function never checks it.
    pub async fn resolve(&self, code: &str) -> Result<Option<String>> {
        if !valid_code(code) {
            return Err(ShortLinkError::InvalidCode);
        }
        let code = code.to_owned();
        self.storage
            .with_connection(move |db| {
                let tx = db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|_| StorageError::Database)?;
                // Bound allocation even for corrupt/legacy rows, not just new inserts.
                let row = tx
                    .query_row(
                        "SELECT CASE WHEN length(CAST(original_url AS BLOB)) <= ?2
                THEN original_url ELSE NULL END FROM short_links WHERE shortcode = ?1",
                        params![code, MAX_URL_BYTES],
                        |r| r.get::<_, Option<String>>(0),
                    )
                    .optional()
                    .map_err(|_| StorageError::Database)?;
                let Some(original) = row else {
                    return Ok(Ok(None));
                };
                let Some(original) = original else {
                    return Ok(Err(ShortLinkError::InvalidUrl));
                };
                let target = match validate_parser_url(&original) {
                    Ok(target) => target,
                    Err(error) => return Ok(Err(error)),
                };
                tx.execute(
                    "UPDATE short_links SET access_count = access_count + 1,
                last_accessed = strftime('%s', 'now') WHERE shortcode = ?1",
                    [&code],
                )
                .map_err(|_| StorageError::Database)?;
                tx.commit().map_err(|_| StorageError::Database)?;
                Ok(Ok(Some(target)))
            })
            .await?
    }
}

fn valid_code(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_alphanumeric())
}
fn generate_shortcode() -> Result<String> {
    let mut bytes = [0u8; 64];
    getrandom::getrandom(&mut bytes).map_err(|_| ShortLinkError::Random)?;
    // Rejection sampling avoids modulo bias and has a fixed entropy/input bound.
    let code: String = bytes
        .iter()
        .filter(|&&b| b < 248)
        .take(6)
        .map(|&b| ALPHABET[(b % 62) as usize] as char)
        .collect();
    if code.len() == 6 {
        Ok(code)
    } else {
        Err(ShortLinkError::Random)
    }
}
fn http_url(input: &str) -> Result<Url> {
    if input.is_empty()
        || input.len() > MAX_URL_BYTES
        || input.chars().any(|c| c.is_control() || c.is_whitespace())
        || input.contains('\\')
    {
        return Err(ShortLinkError::InvalidUrl);
    }
    let url = Url::parse(input).map_err(|_| ShortLinkError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ShortLinkError::InvalidUrl);
    }
    // Reject even an empty userinfo marker rather than silently normalizing it.
    let authority = input
        .split_once("://")
        .ok_or(ShortLinkError::InvalidUrl)?
        .1
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    if authority.contains('@') {
        return Err(ShortLinkError::InvalidUrl);
    }
    Ok(url)
}

/// Validate a full parser URL and project to local params, not a redirect URL.
/// Unknown keys (including auth/token/action) reject, rather than being silently saved.
pub fn validate_parser_url(input: &str) -> Result<String> {
    let outer = http_url(input)?;
    if outer.path() != "/" || outer.fragment().is_some() {
        return Err(ShortLinkError::InvalidUrl);
    }
    // A decoded query has at most these 27 distinct allowed keys; duplicates use
    // URLSearchParams.get's first-value semantics, with a separate input count bound.
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (key, value) in outer.query_pairs() {
        if pairs.len() >= 64 || value.len() > 4096 || value.chars().any(|c| c.is_control()) {
            return Err(ShortLinkError::InvalidUrl);
        }
        let folder = key == "fid" || (1..=20).any(|i| key == format!("fid{i}"));
        if !folder && !matches!(key.as_ref(), "url" | "pwd" | "type" | "id" | "d" | "sid") {
            return Err(ShortLinkError::InvalidUrl);
        }
        pairs.push((key.into_owned(), value.into_owned()));
    }
    let get = |key: &str| {
        pairs
            .iter()
            .find(|p| p.0 == key)
            .map(|p| p.1.as_str())
            .unwrap_or("")
    };
    let share = http_url(get("url"))?;
    if share.port().is_some() {
        return Err(ShortLinkError::InvalidUrl);
    }
    let host = share.host_str().ok_or(ShortLinkError::InvalidUrl)?;
    // Only share entrypoints of the two supported providers, not provider API/login URLs.
    let cloud189 = matches!(host, "cloud.189.cn" | "h5.cloud.189.cn" | "m.cloud.189.cn")
        && (share.path().starts_with("/t/")
            || share.path().starts_with("/s/")
            || matches!(
                share.path(),
                "/web/share" | "/web/share.html" | "/share.html" | "/share"
            ))
        && crate::providers::cloud189::extract_share_info(get("url")).is_some();
    let mcloud = matches!(host, "yun.139.com" | "caiyun.139.com")
        && matches!(
            share.path(),
            "/shareweb/" | "/shareweb" | "/link/" | "/link"
        )
        && crate::providers::mcloud::extract_share_info(get("url")).is_some();
    if !cloud189 && !mcloud {
        return Err(ShortLinkError::InvalidUrl);
    }
    // Only mcloud's share navigation fragment is allowed. It may contain a
    // password query; other fragments cannot smuggle account credentials.
    let fragment_query = if let Some(fragment) = share.fragment() {
        let (path, query) = fragment.split_once('?').unwrap_or((fragment, ""));
        let id = path
            .strip_prefix("/w/i/")
            .ok_or(ShortLinkError::InvalidUrl)?;
        if !mcloud || id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
            return Err(ShortLinkError::InvalidUrl);
        }
        Some(query)
    } else {
        None
    };
    // Do not persist credentials hidden inside a nested provider query or fragment.
    for query in [share.query(), fragment_query].into_iter().flatten() {
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            let key = key.to_ascii_lowercase();
            if !matches!(
                key.as_str(),
                "code" | "sharecode" | "linkid" | "pwd" | "password"
            ) || value.chars().any(|c| c.is_control())
            {
                return Err(ShortLinkError::InvalidUrl);
            }
        }
    }
    if !matches!(get("type"), "" | "json" | "down" | "text") {
        return Err(ShortLinkError::InvalidUrl);
    }
    let mut output = url::form_urlencoded::Serializer::new(String::new());
    output.append_pair("url", get("url"));
    for key in ["pwd", "type"] {
        if !get(key).is_empty() {
            output.append_pair(key, get(key));
        }
    }
    let id = if get("id").is_empty() {
        get("d")
    } else {
        get("id")
    };
    if !id.is_empty() {
        output.append_pair("id", id);
    }
    if !get("sid").is_empty() {
        output.append_pair("sid", get("sid"));
    }
    let mut folders = Vec::new();
    for i in 0..=20 {
        let key = if i == 0 {
            "fid".to_owned()
        } else {
            format!("fid{i}")
        };
        let value = get(&key);
        if !value.is_empty() && !folders.contains(&value) {
            folders.push(value);
        }
    }
    // Preserve the frozen canonical spelling for up to 20 distinct folders.
    // All 21 accepted aliases need fid1 as well, otherwise fid21 would be lost.
    let offset = usize::from(folders.len() <= 20);
    for (i, folder) in folders.iter().enumerate() {
        let key = if i == 0 {
            "fid".to_owned()
        } else {
            format!("fid{}", i + offset)
        };
        output.append_pair(&key, folder);
    }
    let target = format!("/?{}", output.finish());
    if target.len() > MAX_URL_BYTES {
        return Err(ShortLinkError::InvalidUrl);
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;
    const URL: &str =
        "https://parser.example.invalid/?url=https%3A%2F%2Fcloud.189.cn%2Ft%2FSynthetic";
    #[tokio::test(flavor = "current_thread")]
    async fn bounded_collisions_and_error_injection() {
        let storage = Storage::open(":memory:", 16).await.unwrap();
        let links = ShortLinks::new(storage.clone()).await.unwrap();
        assert_eq!(
            links
                .create_using(URL, || Ok("AAAAAA".into()))
                .await
                .unwrap(),
            "AAAAAA"
        );
        assert_eq!(
            links
                .create_using(URL, || panic!("reuse must not generate another code"))
                .await
                .unwrap(),
            "AAAAAA"
        );
        let mut attempts = 0;
        let code = links
            .create_using(&format!("{URL}&id=second"), move || {
                attempts += 1;
                Ok(if attempts < 10 { "AAAAAA" } else { "BBBBBB" }.into())
            })
            .await
            .unwrap();
        assert_eq!(code, "BBBBBB");
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = calls.clone();
        assert_eq!(
            links
                .create_using(&format!("{URL}&id=collision-limit"), move || {
                    observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("AAAAAA".into())
                })
                .await,
            Err(ShortLinkError::CollisionLimit)
        );
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/shortlinks.json")).unwrap();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            fixture["create"][3]["operations"].as_array().unwrap().len()
        );
        assert_eq!(code, fixture["create"][4]["code"].as_str().unwrap());
        for row in fixture["generator"].as_array().unwrap() {
            assert_eq!(
                row["code"].as_str().unwrap(),
                (ALPHABET[row["index"].as_u64().unwrap() as usize] as char)
                    .to_string()
                    .repeat(6)
            );
        }
        assert_eq!(
            links
                .create_using(&format!("{URL}&id=random"), || Err(ShortLinkError::Random))
                .await,
            Err(ShortLinkError::Random)
        );
        // A trigger failure is NOT a collision and must not be retried.
        storage
            .with_connection(|db| {
                db.execute_batch(
                    "CREATE TRIGGER short_fail BEFORE INSERT ON short_links
            BEGIN SELECT RAISE(ABORT, 'synthetic'); END;",
                )
                .map_err(|_| StorageError::Database)
            })
            .await
            .unwrap();
        assert_eq!(
            links
                .create_using(&format!("{URL}&id=trigger"), || Ok("CCCCCC".into()))
                .await,
            Err(ShortLinkError::Storage(StorageError::Database))
        );
        storage.close().await.unwrap();
    }
    #[tokio::test(flavor = "current_thread")]
    async fn concurrent_collision_is_not_overwrite() {
        let storage = Storage::open(":memory:", 16).await.unwrap();
        let links = ShortLinks::new(storage.clone()).await.unwrap();
        let other = links.clone();
        let other_url = format!("{URL}&id=concurrent-second");
        let (a, b) = tokio::join!(
            links.create_using(URL, || Ok("AAAAAA".into())),
            other.create_using(&other_url, || Ok("AAAAAA".into()))
        );
        assert!(matches!(
            (&a, &b),
            (Ok(_), Err(ShortLinkError::CollisionLimit))
                | (Err(ShortLinkError::CollisionLimit), Ok(_))
        ));
        storage.close().await.unwrap();
    }
}
