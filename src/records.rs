//! Bounded private parse history and lifetime statistics on Storage's SQLite actor.
use crate::compat::truthy as js_truthy;
use crate::storage::{Result, Storage, StorageError, decrypt_from_kv, encrypt_for_kv};
use rusqlite::{Connection, TransactionBehavior, params};
use serde_json::{Value, json};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_RETENTION_DAYS: u16 = 7;
const DAY_MS: i64 = 86_400_000;
const MAX_TIME_MS: i64 = 253_402_300_799_999;
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
pub const MAX_PAGE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone)]
pub struct Records {
    storage: Storage,
}

impl Records {
    pub async fn new(storage: Storage) -> Result<Self> {
        Self::new_at(storage, now_ms()?).await
    }

    /// Injectable wall clock; schema creation and startup cleanup run on the actor.
    pub async fn new_at(storage: Storage, now_ms: i64) -> Result<Self> {
        validate_time(now_ms)?;
        storage.with_connection(move |db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|_| StorageError::Database)?;
            tx.execute_batch(
                "CREATE TABLE IF NOT EXISTS records_settings (
                    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                    log_retention_days INTEGER NOT NULL DEFAULT 7
                        CHECK(log_retention_days BETWEEN 1 AND 3650)
                 );
                 CREATE TABLE IF NOT EXISTS parse_records (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    created_ms INTEGER NOT NULL CHECK(created_ms >= 0),
                    success INTEGER NOT NULL CHECK(success IN (0, 1)),
                    cached INTEGER NOT NULL CHECK(cached IN (0, 1)),
                    search_text TEXT NOT NULL,
                    payload TEXT NOT NULL
                 );
                 CREATE INDEX IF NOT EXISTS parse_records_created_ms ON parse_records(created_ms);
                 CREATE INDEX IF NOT EXISTS parse_records_success_created_ms
                    ON parse_records(success, created_ms, id);
                 CREATE INDEX IF NOT EXISTS parse_records_search_cover
                    ON parse_records(created_ms, success, cached, search_text);
                 CREATE TABLE IF NOT EXISTS records_stats (
                    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
                    total INTEGER NOT NULL CHECK(typeof(total) = 'integer' AND total >= 0),
                    success INTEGER NOT NULL CHECK(typeof(success) = 'integer' AND success >= 0),
                    failed INTEGER NOT NULL CHECK(typeof(failed) = 'integer' AND failed >= 0),
                    cached INTEGER NOT NULL CHECK(typeof(cached) = 'integer' AND cached >= 0),
                    CHECK(total = success + failed AND cached <= total)
                 );"
            ).map_err(|_| StorageError::Schema)?;
            tx.execute(
                "INSERT OR IGNORE INTO records_settings(singleton, log_retention_days) VALUES(1, ?1)",
                [DEFAULT_RETENTION_DAYS],
            ).map_err(|_| StorageError::Database)?;
            // One-time upgrade can seed surviving rows only; lost old history is unrecoverable.
            tx.execute(
                "INSERT OR IGNORE INTO records_stats(singleton, total, success, failed, cached)
                 SELECT 1, COUNT(*), COALESCE(SUM(success), 0),
                        COALESCE(SUM(1-success), 0), COALESCE(SUM(cached), 0) FROM parse_records",
                [],
            ).map_err(|_| StorageError::Database)?;
            cleanup(&tx, now_ms)?;
            tx.commit().map_err(|_| StorageError::Database)
        }).await?;
        Ok(Self { storage })
    }

    pub async fn retention_days(&self) -> Result<u16> {
        self.storage.with_connection(|db| retention(db)).await
    }

    pub async fn set_retention_days(&self, days: u16) -> Result<()> {
        self.set_retention_days_at(days, now_ms()?).await
    }

    /// Setting and deletion commit together; increasing retention never restores rows.
    pub async fn set_retention_days_at(&self, days: u16, now_ms: i64) -> Result<()> {
        if !(1..=3650).contains(&days) {
            return Err(StorageError::InvalidTtl);
        }
        validate_time(now_ms)?;
        self.storage
            .with_connection(move |db| {
                let tx = db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|_| StorageError::Database)?;
                let changed = tx
                    .execute(
                        "UPDATE records_settings SET log_retention_days = ?1 WHERE singleton = 1",
                        [days],
                    )
                    .map_err(|_| StorageError::Database)?;
                if changed != 1 {
                    return Err(StorageError::Schema);
                }
                cleanup(&tx, now_ms)?;
                tx.commit().map_err(|_| StorageError::Database)
            })
            .await
    }

    /// Only this component's parse_records table is affected.
    pub async fn cleanup_at(&self, now_ms: i64) -> Result<usize> {
        validate_time(now_ms)?;
        self.storage
            .with_connection(move |db| cleanup(db, now_ms))
            .await
    }

    /// Returns a monotonic SQLite ID, not a collision-prone millisecond timestamp.
    pub async fn insert(&self, url: &str, pwd: &str, result: &Value, cached: bool) -> Result<i64> {
        self.insert_at(url, pwd, result, cached, now_ms()?).await
    }

    pub async fn insert_at(
        &self,
        url: &str,
        pwd: &str,
        result: &Value,
        cached: bool,
        now_ms: i64,
    ) -> Result<i64> {
        validate_time(now_ms)?;
        let success = classify(result);
        // Frozen request counters differ from history classification; cache hits count success.
        let counted_success = cached || result.get("success").is_some_and(js_truthy);
        let payload = make_payload(url, pwd, result)?;
        // Private search supports the original URL/password/data/message/code fields.
        let search_text = format!(
            "{} {} {} {} {}",
            url,
            pwd,
            payload["data"],
            payload["msg"].as_str().unwrap_or(""),
            payload["code"]
        )
        .to_lowercase();
        let payload = encrypt_for_kv(&payload)?;
        self.storage
            .with_connection(move |db| {
                let tx = db
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .map_err(|_| StorageError::Database)?;
                cleanup(&tx, now_ms)?;
                tx.execute(
                    "INSERT INTO parse_records(created_ms, success, cached, search_text, payload)
                 VALUES(?1, ?2, ?3, ?4, ?5)",
                    params![now_ms, success, cached, search_text, payload],
                )
                .map_err(|_| StorageError::Database)?;
                let id = tx.last_insert_rowid();
                let changed = tx
                    .execute(
                        "UPDATE records_stats SET total = total + 1, success = success + ?1,
                     failed = failed + ?2, cached = cached + ?3 WHERE singleton = 1",
                        params![counted_success, !counted_success, cached],
                    )
                    .map_err(|_| StorageError::Database)?;
                if changed != 1 {
                    return Err(StorageError::Schema);
                }
                tx.commit().map_err(|_| StorageError::Database)?;
                Ok(id)
            })
            .await
    }

    pub async fn stats(&self) -> Result<Value> {
        self.stats_at(now_ms()?).await
    }

    /// Lifetime counters survive retention, deletion, clear and process restarts.
    pub async fn stats_at(&self, now_ms: i64) -> Result<Value> {
        validate_time(now_ms)?;
        self.storage
            .with_connection(|db| {
                let (total, success, failed, cached): (i64, i64, i64, i64) = db.query_row(
                "SELECT total, success, failed, cached FROM records_stats WHERE singleton = 1", [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).map_err(|_| StorageError::Database)?;
                Ok(json!({"total": total, "success": success, "failed": failed, "cached": cached}))
            })
            .await
    }

    /// Frozen cache-hit branch increments lifetime counters without duplicating history.
    pub async fn record_cache_hit(&self) -> Result<()> {
        self.record_request_outcome(true, true).await
    }

    /// Count a coalesced response without duplicating its persisted history row.
    /// Joined failed loads must count failure, not the frozen ready-cache success branch.
    pub async fn record_request_outcome(&self, success: bool, cached: bool) -> Result<()> {
        if cached && !success {
            return Err(StorageError::InvalidJson);
        }
        self.storage.with_connection(move |db| {
            let changed = db.execute("UPDATE records_stats SET total=total+1, success=success+?1, failed=failed+?2, cached=cached+?3 WHERE singleton=1", params![success, !success, cached])
                .map_err(|_| StorageError::Database)?;
            if changed != 1 { return Err(StorageError::Schema); }
            Ok(())
        }).await
    }
    /// Physical history count, including rows awaiting scheduled retention cleanup.
    pub async fn count(&self) -> Result<u64> {
        self.storage
            .with_connection(|db| {
                db.query_row("SELECT COUNT(*) FROM parse_records", [], |row| row.get(0))
                    .map_err(|_| StorageError::Database)
            })
            .await
    }

    pub async fn delete(&self, id: i64) -> Result<bool> {
        self.storage
            .with_connection(move |db| {
                db.execute("DELETE FROM parse_records WHERE id = ?1", [id])
                    .map(|count| count != 0)
                    .map_err(|_| StorageError::Database)
            })
            .await
    }

    pub async fn clear(&self) -> Result<usize> {
        self.storage
            .with_connection(|db| {
                db.execute("DELETE FROM parse_records", [])
                    .map_err(|_| StorageError::Database)
            })
            .await
    }

    /// Delete oldest rows first; caller can repeat until zero without loading payloads.
    pub async fn delete_batch(&self, limit: u32) -> Result<usize> {
        if !(1..=1000).contains(&limit) {
            return Err(StorageError::InvalidJson);
        }
        self.storage
            .with_connection(move |db| {
                db.execute(
                    "DELETE FROM parse_records WHERE id IN
                       (SELECT id FROM parse_records ORDER BY created_ms, id LIMIT ?1)",
                    [limit],
                )
                .map_err(|_| StorageError::Database)
            })
            .await
    }

    /// SQL filters precede LIMIT/OFFSET. Only the selected page is decrypted.
    /// page=0 becomes 1; limit is clamped to 1..50; type=""/"all" means both.
    pub async fn list(&self, page: u32, limit: u32, kind: &str, search: &str) -> Result<Value> {
        self.list_at(page, limit, kind, search, now_ms()?).await
    }

    pub async fn list_at(
        &self,
        page: u32,
        limit: u32,
        kind: &str,
        search: &str,
        now_ms: i64,
    ) -> Result<Value> {
        validate_time(now_ms)?;
        let filter = match kind {
            "" | "all" => -1_i64,
            "success" => 1,
            "failed" => 0,
            _ => return Err(StorageError::InvalidJson),
        };
        // Bound allocation and query work for untrusted frontend inputs.
        if search.len() > 512 {
            return Err(StorageError::InvalidJson);
        }
        let search = search.trim().to_lowercase();
        let page = page.max(1);
        let limit = limit.clamp(1, 50);
        let offset = i64::from(page - 1) * i64::from(limit);
        self.storage
            .with_connection(move |db| {
                let cutoff = cutoff(db, now_ms)?;
                // Totals apply the search but precede the type filter, for frontend tabs.
                let (success_total, failed_total): (i64, i64) = db
                    .query_row(
                        "SELECT COALESCE(SUM(success), 0), COALESCE(SUM(1-success), 0)
                 FROM parse_records WHERE created_ms >= ?1
                   AND (?2 = '' OR instr(search_text, ?2) > 0)",
                        params![cutoff, search],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(|_| StorageError::Database)?;
                let total = match filter {
                    1 => success_total,
                    0 => failed_total,
                    _ => success_total + failed_total,
                };
                let mut statement = db
                    .prepare(
                        "SELECT id, created_ms, success, cached,
                            CASE WHEN length(CAST(payload AS BLOB)) <= ?6 THEN payload END
                 FROM parse_records WHERE created_ms >= ?1 AND (?2 = -1 OR success = ?2)
                   AND (?3 = '' OR instr(search_text, ?3) > 0)
                 ORDER BY created_ms DESC, id DESC LIMIT ?4 OFFSET ?5",
                    )
                    .map_err(|_| StorageError::Database)?;
                let mut rows = statement
                    .query(params![
                        cutoff,
                        filter,
                        search,
                        limit,
                        offset,
                        (MAX_RECORD_BYTES / 16 + 1) * 32
                    ])
                    .map_err(|_| StorageError::Database)?;
                let mut successful = Vec::new();
                let mut failed = Vec::new();
                let mut page_bytes = 1024;
                while let Some(row) = rows.next().map_err(|_| StorageError::Database)? {
                    let id: i64 = row.get(0).map_err(|_| StorageError::Database)?;
                    let created_ms: i64 = row.get(1).map_err(|_| StorageError::Database)?;
                    validate_time(created_ms)?;
                    let success: bool = row.get(2).map_err(|_| StorageError::Database)?;
                    let cached: bool = row.get(3).map_err(|_| StorageError::Database)?;
                    let payload: String = row
                        .get::<_, Option<String>>(4)
                        .map_err(|_| StorageError::Database)?
                        .ok_or(StorageError::InvalidJson)?;
                    let mut value = decrypt_from_kv(&payload)?;
                    let object = value.as_object_mut().ok_or(StorageError::InvalidJson)?;
                    // The previous component discarded reparse fields without a marker.
                    object.entry("truncated").or_insert(json!(true));
                    object.insert("id".into(), json!(id));
                    object.insert("timestamp".into(), json!(iso_timestamp(created_ms)));
                    object.insert("success".into(), json!(success));
                    object.insert("cached".into(), json!(cached));
                    page_bytes += bounded_size(&value, MAX_RECORD_BYTES + 1024)?;
                    if page_bytes > MAX_PAGE_BYTES {
                        return Err(StorageError::InvalidJson);
                    }
                    if success {
                        successful.push(value);
                    } else {
                        failed.push(value);
                    }
                }
                let response = json!({
                    "success": successful, "failed": failed, "total": total,
                    "successTotal": success_total, "failedTotal": failed_total,
                    "page": page, "limit": limit,
                    "pages": ((total + i64::from(limit) - 1) / i64::from(limit)).max(1)
                });
                bounded_size(&response, MAX_PAGE_BYTES)?;
                Ok(response)
            })
            .await
    }
}

fn retention(db: &Connection) -> Result<u16> {
    let days: i64 = db
        .query_row(
            "SELECT log_retention_days FROM records_settings WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|_| StorageError::Database)?;
    if !(1..=3650).contains(&days) {
        return Err(StorageError::Schema);
    }
    Ok(days as u16)
}

fn cutoff(db: &Connection, now_ms: i64) -> Result<i64> {
    Ok(now_ms - i64::from(retention(db)?) * DAY_MS)
}

fn cleanup(db: &Connection, now_ms: i64) -> Result<usize> {
    db.execute(
        "DELETE FROM parse_records WHERE created_ms < ?1",
        [cutoff(db, now_ms)?],
    )
    .map_err(|_| StorageError::Database)
}

fn now_ms() -> Result<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| StorageError::Clock)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| StorageError::Clock)
}

fn validate_time(now_ms: i64) -> Result<()> {
    if !(0..=MAX_TIME_MS).contains(&now_ms) {
        return Err(StorageError::Clock);
    }
    Ok(())
}

fn classify(result: &Value) -> bool {
    // Frozen history classification deliberately ignores result.success.
    result.get("code") == Some(&json!(200))
        || result.get("code").and_then(Value::as_str) == Some("success")
        || result.get("msg").and_then(Value::as_str) == Some("解析成功")
}

// A capped counting writer avoids serializing arbitrarily large input to a String.
struct SizeBudget(usize);
impl std::io::Write for SizeBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.0 {
            return Err(std::io::ErrorKind::FileTooLarge.into());
        }
        self.0 -= bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn bounded_size(value: &Value, max: usize) -> Result<usize> {
    let mut budget = SizeBudget(max);
    serde_json::to_writer(&mut budget, value).map_err(|_| StorageError::InvalidJson)?;
    Ok(max - budget.0)
}
fn account_key(key: &str) -> bool {
    if key.len() > 64 {
        return false;
    }
    let normalized: String = key
        .chars()
        .filter(|c| !matches!(c, '_' | '-'))
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "credentials"
            | "credential"
            | "authorization"
            | "cookie"
            | "cookies"
            | "setcookie"
            | "accesstoken"
            | "openaccesstoken"
            | "appaccesstoken"
            | "refreshtoken"
            | "sessiontoken"
            | "sessionkey"
            | "sessionsecret"
            | "accountpassword"
            | "loginpassword"
            | "privatekey"
            | "clientsecret"
            | "cookieloginuser"
            | "sson"
            | "sharesessioncookies"
            | "logininfo"
            | "mcloudskey"
    )
}
fn private_data(value: &Value, budget: &mut usize, depth: usize) -> Result<Value> {
    if depth > 64 {
        return Err(StorageError::InvalidJson);
    }
    let charge = |budget: &mut usize, bytes: usize| -> Result<()> {
        *budget = budget.checked_sub(bytes).ok_or(StorageError::InvalidJson)?;
        Ok(())
    };
    match value {
        Value::Array(values) => {
            charge(budget, 2)?;
            let mut output = Vec::new();
            for value in values {
                charge(budget, 1)?;
                output.push(private_data(value, budget, depth + 1)?);
            }
            Ok(Value::Array(output))
        }
        Value::Object(values) => {
            charge(budget, 2)?;
            let mut output = serde_json::Map::new();
            for (key, value) in values {
                if account_key(key) {
                    continue;
                }
                // UTF-8 length before clone; reserve worst-case JSON escaping for keys.
                charge(budget, key.len().saturating_mul(6).saturating_add(4))?;
                output.insert(key.clone(), private_data(value, budget, depth + 1)?);
            }
            Ok(Value::Object(output))
        }
        _ => {
            let size = bounded_size(value, *budget)?;
            charge(budget, size)?;
            Ok(value.clone())
        }
    }
}
fn make_payload(url: &str, pwd: &str, result: &Value) -> Result<Value> {
    // Reject oversized mandatory reparse/scalar fields rather than erasing them.
    let code = result
        .get("code")
        .filter(|v| !v.is_null())
        .unwrap_or(&Value::Null);
    let code = if code.is_null() {
        json!(200)
    } else {
        if code.is_array() || code.is_object() {
            return Err(StorageError::InvalidJson);
        }
        bounded_size(code, MAX_RECORD_BYTES)?;
        code.clone()
    };
    let msg = result
        .get("msg")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("解析成功");
    if url
        .len()
        .saturating_add(pwd.len())
        .saturating_add(msg.len())
        > MAX_RECORD_BYTES
    {
        return Err(StorageError::InvalidJson);
    }
    let mut payload = json!({"url": url, "pwd": pwd, "code": code, "msg": msg,
                             "data": null, "truncated": result["truncated"] == true});
    let base_size = bounded_size(&payload, MAX_RECORD_BYTES)?;
    let mut budget = MAX_RECORD_BYTES - base_size;
    if let Some(data) = result.get("data").filter(|v| js_truthy(v)) {
        match private_data(data, &mut budget, 0) {
            Ok(data) => payload["data"] = data,
            Err(_) => payload["truncated"] = json!(true),
        }
    }
    if bounded_size(&payload, MAX_RECORD_BYTES).is_err() {
        payload["data"] = Value::Null;
        payload["truncated"] = json!(true);
        bounded_size(&payload, MAX_RECORD_BYTES)?;
    }
    Ok(payload)
}

fn iso_timestamp(ms: i64) -> String {
    // Gregorian civil date from days since epoch (no new dependency).
    let z = ms / DAY_MS + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    let secs = ms / 1000 % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60,
        ms % 1000
    )
}
