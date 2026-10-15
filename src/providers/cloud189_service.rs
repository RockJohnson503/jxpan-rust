//! Partial, read-only Cloud189 orchestration; not the deployment's complete login/transfer flow.
//! Returned direct links are data, never fetched here. Credential/config gates belong to the caller.
use crate::compat::{first_truthy, js_string};
use crate::providers::{
    cloud189,
    cloud189_client::{Cloud189Client, Cloud189Error},
};
use crate::tree::{
    TreeError, Walk, is_folder, nested_tree_complete, normalize_entry, walk_complete,
};
use serde_json::{Value, json};
use std::future::Future;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadMethod {
    Open,
    AppShared,
}
fn error(code: u16, msg: &str) -> Value {
    json!({"code":code,"msg":msg,"success":false,"data":null})
}
// Only classified, local messages cross the service boundary; never format upstream errors.
fn upstream_error(e: Cloud189Error, fallback_code: u16, fallback_msg: &str) -> Value {
    match e.status_code() {
        401 => {
            let mut value = error(401, "天翼下载认证被拒绝，请检查后台登录状态");
            value["error_code"] = json!("cloud189_auth_required");
            value["need_relogin"] = json!(true);
            value
        }
        404 => error(404, "天翼分享不可用，请检查分享链接"),
        403 if e.kind == "access code required" => error(403, "天翼分享需要提取码，请填写提取码"),
        403 => error(403, "提取码错误或已过期，请检查提取码是否正确"),
        413 => error(413, "天翼请求超过资源上限"),
        _ => error(fallback_code, fallback_msg),
    }
}
fn download_error(e: Cloud189Error) -> Value {
    upstream_error(e, 502, "天翼下载链接请求失败")
}
fn field(v: &Value, key: &str, default: Value) -> Value {
    first_truthy(v, &[key], default)
}
fn string(v: &Value, key: &str) -> String {
    js_string(&field(v, key, json!("")))
}
struct Prepared {
    info: Value,
    sid: String,
}

// Own the client through share/password requests. Only clone AFTER capture has finished.
async fn prepare<C, G, GF, P, PF, E>(
    client: C,
    share_url: &str,
    pwd: &str,
    fast: bool,
    get: G,
    check: P,
) -> Result<(C, Prepared), Value>
where
    E: Into<Cloud189Error>,
    G: Fn(C, String) -> GF,
    GF: Future<Output = Result<(C, Option<Value>), E>>,
    P: FnOnce(C, String, String) -> PF,
    PF: Future<Output = Result<(C, Value), E>>,
{
    let share = cloud189::extract_share_info(share_url).ok_or_else(|| {
        if fast {
            error(502, "无效分享链接")
        } else {
            error(400, "无效的天翼云盘分享链接")
        }
    })?;
    let (mut client, mut info) = get(client, share.share_code.clone())
        .await
        .map_err(|e| upstream_error(e.into(), 500, "天翼分享信息请求失败"))?;
    let mut checked_sid = String::new();
    if !pwd.trim().is_empty()
        && info
            .as_ref()
            .is_none_or(|info| info.get("needAccessCode") != Some(&json!(false)))
    {
        let (updated, checked) = check(client, share.share_code.clone(), pwd.to_owned())
            .await
            .map_err(|e| upstream_error(e.into(), 500, "天翼提取码请求失败"))?;
        client = updated;
        if checked["success"] != true {
            return Err(error(403, "提取码错误或已过期，请检查提取码是否正确"));
        }
        checked_sid = string(&checked, "shareId");
        // Password-protected shares can withhold metadata until the cookie is captured.
        if info.is_none() {
            let (updated, refreshed) = get(client, share.share_code)
                .await
                .map_err(|e| upstream_error(e.into(), 500, "天翼分享信息请求失败"))?;
            client = updated;
            info = refreshed;
        }
    }
    let info = match info {
        Some(info) => info,
        None if fast => Value::Null,
        None => return Err(error(400, "获取分享信息失败，链接可能已失效")),
    };
    if !fast && crate::tree::entry_id(&info).is_none() {
        return Err(error(400, "无法获取文件ID"));
    }
    let sid = if checked_sid.is_empty() {
        string(&info, "shareId")
    } else {
        checked_sid
    };
    Ok((client, Prepared { info, sid }))
}
async fn prepare_client(
    client: Cloud189Client,
    url: &str,
    pwd: &str,
    fast: bool,
) -> Result<(Cloud189Client, Prepared), Value> {
    prepare(
        client,
        url,
        pwd,
        fast,
        |mut c, code| async move {
            let info = c.get_share_info(&code).await?;
            Ok::<_, crate::providers::cloud189_client::Cloud189Error>((c, info))
        },
        |mut c, code, pwd| async move {
            let checked = c.check_access_code(&code, &pwd).await?;
            Ok::<_, crate::providers::cloud189_client::Cloud189Error>((c, checked))
        },
    )
    .await
}
async fn download_client(
    c: Cloud189Client,
    method: DownloadMethod,
    id: String,
    sid: String,
) -> Result<Option<String>, crate::providers::cloud189_client::Cloud189Error> {
    match method {
        DownloadMethod::Open => c.download_open(&id, &sid).await,
        DownloadMethod::AppShared => c.download_app(&id, &sid, true).await,
    }
}
/// Complete, checked traversal using the client's concurrency snapshot.
/// Each directory gets an instance-local snapshot of captured share/password cookies.
pub async fn parse(
    client: Cloud189Client,
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> Value {
    parse_indexed(client, share_url, pwd, fid_chain, file_id)
        .await
        .0
}

/// Export only IDs from the original walk; the caller owns a bounded index cache.
pub async fn parse_indexed(
    client: Cloud189Client,
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> (Value, Option<(String, Vec<String>)>) {
    let (client, prepared) = match prepare_client(client, share_url, pwd, false).await {
        Ok(p) => p,
        Err(e) => return (e, None),
    };
    let concurrency = client.concurrency();
    let list = |sid: String, id: String, mode: String| {
        list_client(client.clone(), sid, id, pwd.to_owned(), mode)
    };
    let download = |method, id, sid| download_client(client.clone(), method, id, sid);
    let mut index = None;
    let value = parse_core(
        prepared,
        fid_chain,
        file_id,
        concurrency,
        list,
        download,
        |sid, tree| {
            index = Some((sid.to_owned(), directory_ids(tree)));
        },
    )
    .await;
    (value, index)
}

fn directory_ids(tree: &crate::tree::Walk) -> Vec<String> {
    std::iter::once(tree.root_id.clone())
        .chain(tree.all_folders.iter().map(|f| string(f, "fileId")))
        .collect()
}

#[doc(hidden)]
pub async fn parse_with_callbacks<G, GF, P, PF, L, LF, D, DF, E>(
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
    callbacks: (G, P, L, D),
) -> Value
where
    E: Into<Cloud189Error>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Option<Value>, E>>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    parse_indexed_with_callbacks(share_url, pwd, fid_chain, file_id, callbacks)
        .await
        .0
}
/// Synthetic callback seam; no endpoint override, transport trait or alternate walker.
#[doc(hidden)]
pub async fn parse_indexed_with_callbacks<G, GF, P, PF, L, LF, D, DF, E>(
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
    callbacks: (G, P, L, D),
) -> (Value, Option<(String, Vec<String>)>)
where
    E: Into<Cloud189Error>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Option<Value>, E>>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let (get, check, list, download) = callbacks;
    let prepared = prepare(
        (),
        share_url,
        pwd,
        false,
        |(), code| {
            let future = get(code);
            async move { future.await.map(|v| ((), v)) }
        },
        |(), code, pwd| async move { check(code, pwd).await.map(|v| ((), v)) },
    )
    .await;
    let mut index = None;
    let value = match prepared {
        Ok(((), p)) => {
            parse_core(
                p,
                fid_chain,
                file_id,
                crate::settings::DEFAULT_CONCURRENCY,
                list,
                download,
                |sid, tree| {
                    index = Some((sid.to_owned(), directory_ids(tree)));
                },
            )
            .await
        }
        Err(e) => e,
    };
    (value, index)
}
async fn try_download<D, DF, E>(
    id: &str,
    sid: &str,
    fast: bool,
    download: &D,
) -> Result<Option<String>, Cloud189Error>
where
    E: Into<Cloud189Error>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    // Public share parsing must never fall back to account-owned dt=3 requests:
    // they omit shareId and could expose an unrelated private file by its ID.
    let methods = if fast {
        [DownloadMethod::AppShared, DownloadMethod::Open]
    } else {
        [DownloadMethod::Open, DownloadMethod::AppShared]
    };
    let mut failure: Option<Cloud189Error> = None;
    for method in methods {
        match download(method, id.to_owned(), sid.to_owned()).await {
            Ok(Some(url)) if !url.is_empty() => return Ok(Some(url)),
            Err(e) => {
                let e = e.into();
                // A working public alternative can still succeed. If none does,
                // retain authentication rejection ahead of other useful errors.
                let priority = |e: &Cloud189Error| match e.status_code() {
                    401 => 2,
                    502 => 0,
                    _ => 1,
                };
                if failure
                    .as_ref()
                    .is_none_or(|old| priority(&e) > priority(old))
                {
                    failure = Some(e);
                }
            }
            _ => {}
        }
    }
    match failure {
        Some(e) => Err(e),
        None => Ok(None),
    }
}
#[allow(clippy::too_many_arguments)]
async fn parse_core<L, LF, D, DF, E, O>(
    p: Prepared,
    chain: &[String],
    target: &str,
    concurrency: usize,
    list: L,
    download: D,
    observe: O,
) -> Value
where
    E: Into<Cloud189Error>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
    O: FnOnce(&str, &Walk),
{
    let root_entry = match normalize_entry(&p.info) {
        Ok(entry) => entry,
        Err(e) => return tree_error(e),
    };
    let root = string(&root_entry, "fileId");
    let name = string(&root_entry, "fileName");
    if !is_folder(&root_entry) {
        if !chain.is_empty() {
            return error(404, "未找到指定文件夹");
        }
        if !target.is_empty() && target != root {
            return error(404, "未找到指定文件");
        }
        let url = try_download(&root, &p.sid, false, &download).await;
        return single(&root_entry, url);
    }
    if p.sid.is_empty() {
        return error(
            400,
            "无法获取shareId，文件夹解析失败。请确保分享链接有效或已正确验证提取码。",
        );
    }
    let mode = js_string(&field(&p.info, "shareMode", json!("1")));
    let tree = match walk_complete(
        |id| list(p.sid.clone(), id, mode.clone()),
        &root,
        &name,
        concurrency,
    )
    .await
    {
        Ok(tree) => tree,
        Err(e) => return tree_error(e),
    };
    observe(&p.sid, &tree);
    if !target.is_empty() {
        if tree.nodes.contains_key(target) {
            return error(400, "指定的ID是文件夹，请使用fid参数进入文件夹");
        }
        let Some(file) = tree.file(target) else {
            return error(404, "未找到指定文件");
        };
        return single(file, try_download(target, &p.sid, false, &download).await);
    }
    let want = chain
        .last()
        .filter(|s| !s.is_empty())
        .map_or(root.as_str(), String::as_str);
    folder_response(&tree, want, &p.sid)
}
fn tree_error(e: TreeError) -> Value {
    match e {
        TreeError::Upstream => error(502, "天翼目录列表获取失败"),
        TreeError::InvalidEntry => error(502, "天翼目录元数据无效"),
        TreeError::Limit => error(413, "天翼目录超过资源上限，未返回不完整结果"),
        TreeError::Cycle => error(502, "天翼目录存在循环或重复目录"),
    }
}
fn folder_response(tree: &Walk, id: &str, sid: &str) -> Value {
    let Some(node) = tree.nodes.get(id) else {
        return error(404, "未找到指定文件夹");
    };
    let name = node["currentPath"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("root");
    json!({"code":200,"msg":"文件夹列表获取成功","success":true,
        "shareKey":format!("c189:{sid}"),"data":{
            "tree":nested_tree_complete(tree,id,name),"sid":sid}})
}
fn single(file: &Value, url: Result<Option<String>, Cloud189Error>) -> Value {
    let url = match url {
        Ok(Some(url)) => url,
        Ok(None) => return error(500, "获取下载链接失败（只读模式未启用转存回退）"),
        Err(e) => return download_error(e),
    };
    let name = &file["fileName"];
    let size = &file["fileSize"];
    json!({"code":200,"msg":"解析成功","success":true,"data":{
        "file_name":name,"file_size":size,"download_url":url,"is_folder":false,"file_count":1,
        "files":[{"name":name,"size":size,"url":url}]}})
}

/// App shared dt=1 -> Open. A sid is not a password-verification session.
/// No Range hop / download-body request / cache is performed; main owns those policies.
/// With sid supplied, main must reject known directory IDs using its shared tree cache.
pub async fn fast_download(
    client: Cloud189Client,
    share_url: &str,
    pwd: &str,
    file_id: &str,
    sid: &str,
) -> Result<String, Value> {
    let (client, p) = if sid.is_empty() || !pwd.trim().is_empty() {
        prepare_client(client, share_url, pwd, true).await?
    } else {
        (
            client,
            Prepared {
                info: Value::Null,
                sid: sid.to_owned(),
            },
        )
    };
    fast_core(p, file_id, |method, id, sid| {
        download_client(client.clone(), method, id, sid)
    })
    .await
}
#[doc(hidden)]
pub async fn fast_with_callbacks<G, GF, P, PF, D, DF, E>(
    share_url: &str,
    pwd: &str,
    file_id: &str,
    sid: &str,
    get: G,
    check: P,
    download: D,
) -> Result<String, Value>
where
    E: Into<Cloud189Error>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Option<Value>, E>>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let p = if sid.is_empty() || !pwd.trim().is_empty() {
        prepare(
            (),
            share_url,
            pwd,
            true,
            |(), code| {
                let future = get(code);
                async move { future.await.map(|v| ((), v)) }
            },
            |(), code, pwd| async move { check(code, pwd).await.map(|v| ((), v)) },
        )
        .await?
        .1
    } else {
        Prepared {
            info: Value::Null,
            sid: sid.to_owned(),
        }
    };
    fast_core(p, file_id, download).await
}
async fn fast_core<D, DF, E>(p: Prepared, target: &str, download: D) -> Result<String, Value>
where
    E: Into<Cloud189Error>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    if target.is_empty() {
        return Err(error(400, "无效文件ID"));
    }
    if string(&p.info, "fileId") == target && is_folder(&p.info) {
        return Err(error(400, "目录无法下载"));
    }
    if p.sid.is_empty() {
        return Err(error(502, "无法获取shareId"));
    }
    let url = try_download(target, &p.sid, true, &download)
        .await
        .map_err(download_error)?
        .ok_or_else(|| error(502, "无法获取直链"))?;
    // download.cloud.189.cn/file/downloadFile.action is also the supplier's
    // normal signed file trampoline, not evidence that the target is a folder.
    // Only authoritative metadata / the HTTP layer's directory index can reject it.
    Ok(url)
}

/// File action with authoritative upstream metadata (not parse's snake_case response).
/// Caller-provided name/size remain in the HTTP signature but are not trusted.
#[allow(clippy::too_many_arguments)]
pub async fn parse_file(
    client: Cloud189Client,
    share_id: &str,
    file_id: &str,
    _file_name: &str,
    _file_size: &str,
    pwd: &str,
    share_code: &str,
) -> Value {
    if share_id.is_empty() || file_id.is_empty() {
        return error(400, "缺少必要参数 (share_id 或 file_id)");
    }
    let (client, info, sid) = if share_code.is_empty() {
        (client, None, share_id.to_owned())
    } else {
        let url = format!("https://cloud.189.cn/t/{share_code}");
        match prepare_client(client, &url, pwd, false).await {
            Ok((client, p)) => {
                let sid = if p.sid.is_empty() {
                    share_id.to_owned()
                } else {
                    p.sid
                };
                (client, Some(p.info), sid)
            }
            Err(e) => return e,
        }
    };
    let concurrency = client.concurrency();
    file_metadata_core(
        info,
        &sid,
        file_id,
        concurrency,
        |sid, id, mode| list_client(client.clone(), sid, id, pwd.to_owned(), mode),
        |method, id, sid| download_client(client.clone(), method, id, sid),
    )
    .await
}

/// Legacy callback seam lacks metadata/listing. It validates decimal sizes without
/// floating-point conversion; invalid or unknown sizes are null, not invented zero.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn file_action_with_callbacks<P, PF, D, DF, E>(
    share_id: &str,
    file_id: &str,
    file_name: &str,
    file_size: &str,
    pwd: &str,
    share_code: &str,
    check: P,
    download: D,
) -> Value
where
    E: Into<Cloud189Error>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    if share_id.is_empty() || file_id.is_empty() {
        return error(400, "缺少必要参数 (share_id 或 file_id)");
    }
    let sid = match action_password(share_id, pwd, share_code, check).await {
        Ok(sid) => sid,
        Err(e) => return e,
    };
    let file = json!({"fileName":file_name,"fileSize":decimal_size(file_size)});
    file_action_core(&sid, file_id, &file, download).await
}

/// Explicit metadata seam for the same read-only production file resolution path.
#[doc(hidden)]
pub async fn file_action_with_metadata_callbacks<G, GF, P, PF, L, LF, D, DF, E>(
    share_id: &str,
    file_id: &str,
    pwd: &str,
    share_code: &str,
    callbacks: (G, P, L, D),
) -> Value
where
    E: Into<Cloud189Error>,
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Option<Value>, E>>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    if share_id.is_empty() || file_id.is_empty() {
        return error(400, "缺少必要参数 (share_id 或 file_id)");
    }
    let (get, check, list, download) = callbacks;
    let (info, sid) = if share_code.is_empty() {
        (None, share_id.to_owned())
    } else {
        let url = format!("https://cloud.189.cn/t/{share_code}");
        let prepared = prepare(
            (),
            &url,
            pwd,
            false,
            |(), code| {
                let future = get(code);
                async move { future.await.map(|v| ((), v)) }
            },
            |(), code, pwd| async move { check(code, pwd).await.map(|v| ((), v)) },
        )
        .await;
        match prepared {
            Ok(((), p)) => (
                Some(p.info),
                if p.sid.is_empty() {
                    share_id.to_owned()
                } else {
                    p.sid
                },
            ),
            Err(e) => return e,
        }
    };
    file_metadata_core(
        info,
        &sid,
        file_id,
        crate::settings::DEFAULT_CONCURRENCY,
        list,
        download,
    )
    .await
}

async fn file_metadata_core<L, LF, D, DF, E>(
    info: Option<Value>,
    sid: &str,
    target: &str,
    concurrency: usize,
    list: L,
    download: D,
) -> Value
where
    E: Into<Cloud189Error>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let entry = match info.as_ref().map(normalize_entry).transpose() {
        Ok(entry) => entry,
        Err(e) => return tree_error(e),
    };
    if let Some(file) = entry.as_ref().filter(|entry| !is_folder(entry)) {
        if string(file, "fileId") != target {
            return error(404, "未找到指定文件");
        }
        return file_action_core(sid, target, file, download).await;
    }
    let root = entry
        .as_ref()
        .map(|entry| string(entry, "fileId"))
        .unwrap_or_default();
    let name = entry
        .as_ref()
        .map(|entry| string(entry, "fileName"))
        .unwrap_or_default();
    let mode = info
        .as_ref()
        .map(|info| js_string(&field(info, "shareMode", json!("1"))))
        .unwrap_or("1".into());
    let tree = match walk_complete(
        |id| list(sid.to_owned(), id, mode.clone()),
        &root,
        &name,
        concurrency,
    )
    .await
    {
        Ok(tree) => tree,
        Err(e) => return tree_error(e),
    };
    if tree.nodes.contains_key(target) {
        return error(400, "指定的ID是文件夹");
    }
    let Some(file) = tree.file(target) else {
        return error(404, "未找到指定文件");
    };
    file_action_core(sid, target, file, download).await
}

async fn file_action_core<D, DF, E>(sid: &str, id: &str, file: &Value, download: D) -> Value
where
    E: Into<Cloud189Error>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    // Signed URLs are returned verbatim: adding filename can invalidate signatures.
    let url = match try_download(id, sid, false, &download).await {
        Ok(url) => url,
        Err(e) => return download_error(e),
    };
    json!({"code":if url.is_some() {200} else {500},
        "msg":if url.is_some() {"解析成功"} else {"下载链接获取失败（只读模式未启用转存回退）"},
        "success":url.is_some(),"data":{"type":"file","fileName":file["fileName"],
        "fileSize":file["fileSize"],"fileId":id,"downloadUrl":url}})
}

fn decimal_size(input: &str) -> Value {
    let input = input.trim();
    if input.is_empty() || !input.bytes().all(|c| c.is_ascii_digit()) {
        return Value::Null;
    }
    input
        .parse::<u64>()
        .ok()
        .and_then(|n| crate::tree::byte_size(&json!(n)).ok())
        .unwrap_or(Value::Null)
}
async fn action_password<P, PF, E>(
    sid: &str,
    pwd: &str,
    code: &str,
    check: P,
) -> Result<String, Value>
where
    E: Into<Cloud189Error>,
    P: FnOnce(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
{
    if code.is_empty() || pwd.trim().is_empty() {
        return Ok(sid.to_owned());
    }
    let checked = check(code.to_owned(), pwd.to_owned())
        .await
        .map_err(|e| upstream_error(e.into(), 500, "天翼提取码请求失败"))?;
    if checked["success"] != true {
        return Err(error(403, "提取码错误或已过期，请检查提取码是否正确"));
    }
    let updated_sid = string(&checked, "shareId");
    Ok(if updated_sid.is_empty() {
        sid.to_owned()
    } else {
        updated_sid
    })
}
async fn list_client(
    mut client: Cloud189Client,
    sid: String,
    id: String,
    pwd: String,
    mode: String,
) -> Result<Vec<Value>, crate::providers::cloud189_client::Cloud189Error> {
    let list = client.list_share_dir(&sid, &id, &pwd, &mode).await?;
    list.and_then(|value| value.as_array().cloned()).ok_or(
        crate::providers::cloud189_client::Cloud189Error {
            operation: "list_share_dir",
            kind: "missing complete file list",
        },
    )
}

/// Resolve the target inside a completely loaded requested subtree. Files use only
/// public shared downloads; directories retain the tree/sid response shape.
#[allow(clippy::too_many_arguments)]
pub async fn browse_folder_item(
    mut client: Cloud189Client,
    share_id: &str,
    folder_id: &str,
    target_file_id: &str,
    pwd: &str,
    share_mode: &str,
    share_code: &str,
) -> Value {
    if share_id.is_empty() || target_file_id.is_empty() {
        return error(400, "缺少必要参数 (share_id 或 target_file_id)");
    }
    let captured = &mut client;
    let sid = match action_password(share_id, pwd, share_code, move |code, pwd| async move {
        captured.check_access_code(&code, &pwd).await
    })
    .await
    {
        Ok(sid) => sid,
        Err(e) => return e,
    };
    let concurrency = client.concurrency();
    folder_action_core(
        &sid,
        folder_id,
        target_file_id,
        share_mode,
        concurrency,
        |sid, id, mode| list_client(client.clone(), sid, id, pwd.to_owned(), mode),
        |method, id, sid| download_client(client.clone(), method, id, sid),
    )
    .await
}

/// List-only compatibility seam. Directory tests work; file targets cannot claim
/// successful downloads without an explicit download callback.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn folder_action_with_callbacks<P, PF, L, LF, E>(
    share_id: &str,
    folder_id: &str,
    target_file_id: &str,
    pwd: &str,
    share_mode: &str,
    share_code: &str,
    check: P,
    list: L,
) -> Value
where
    E: Into<Cloud189Error>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
{
    folder_action_with_download_callbacks(
        share_id,
        folder_id,
        target_file_id,
        pwd,
        share_mode,
        share_code,
        check,
        list,
        |_, _, _| async { Ok::<Option<String>, E>(None) },
    )
    .await
}
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn folder_action_with_download_callbacks<P, PF, L, LF, D, DF, E>(
    share_id: &str,
    folder_id: &str,
    target_file_id: &str,
    pwd: &str,
    share_mode: &str,
    share_code: &str,
    check: P,
    list: L,
    download: D,
) -> Value
where
    E: Into<Cloud189Error>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    if share_id.is_empty() || target_file_id.is_empty() {
        return error(400, "缺少必要参数 (share_id 或 target_file_id)");
    }
    let sid = match action_password(share_id, pwd, share_code, check).await {
        Ok(sid) => sid,
        Err(e) => return e,
    };
    folder_action_core(
        &sid,
        folder_id,
        target_file_id,
        share_mode,
        crate::settings::DEFAULT_CONCURRENCY,
        list,
        download,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn folder_action_core<L, LF, D, DF, E>(
    sid: &str,
    folder: &str,
    target: &str,
    mode: &str,
    concurrency: usize,
    list: L,
    download: D,
) -> Value
where
    E: Into<Cloud189Error>,
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let root = if folder.is_empty() { target } else { folder };
    let mode = if mode.is_empty() { "1" } else { mode };
    let tree = match walk_complete(
        |id| list(sid.to_owned(), id, mode.to_owned()),
        root,
        "",
        concurrency,
    )
    .await
    {
        Ok(tree) => tree,
        Err(e) => return tree_error(e),
    };
    if tree.nodes.contains_key(target) {
        return folder_response(&tree, target, sid);
    }
    let Some(file) = tree.file(target) else {
        return error(404, &format!("未找到文件/文件夹ID: {target}"));
    };
    file_action_core(sid, target, file, download).await
}
