//! Partial, read-only Cloud189 orchestration; not the deployment's complete login/transfer flow.
//! Returned direct links are data, never fetched here. Credential/config gates belong to the caller.
use crate::compat::{first_truthy, js_string, truthy};
use crate::providers::{cloud189, cloud189_client::Cloud189Client};
use crate::tree::{nested_tree, walk};
use serde_json::{Value, json};
use std::future::Future;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadMethod {
    Open,
    AppShared,
    AppOwned,
}
fn error(code: u16, msg: &str) -> Value {
    json!({"code":code,"msg":msg,"success":false,"data":null})
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
    G: FnOnce(C, String) -> GF,
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
    let (mut client, info) = get(client, share.share_code.clone())
        .await
        .map_err(|_| error(500, "天翼分享信息请求失败"))?;
    let info = match info {
        Some(info) => info,
        None if fast => Value::Null,
        None => return Err(error(400, "获取分享信息失败，链接可能已失效")),
    };
    if !fast && !truthy(&info["fileId"]) {
        return Err(error(400, "无法获取文件ID"));
    }
    let mut sid = string(&info, "shareId");
    if if fast {
        !pwd.is_empty()
    } else {
        !pwd.trim().is_empty()
    } {
        let (updated, checked) = check(client, share.share_code, pwd.to_owned())
            .await
            .map_err(|_| error(500, "天翼提取码请求失败"))?;
        client = updated;
        // Fast deployment only consumes shareId, even on a failed password check.
        if !fast && !truthy(&checked["success"]) {
            return Err(error(403, "提取码错误或已过期，请检查提取码是否正确"));
        }
        if truthy(&checked["shareId"]) {
            sid = string(&checked, "shareId");
        }
    }
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
        DownloadMethod::AppOwned => c.download_app(&id, &sid, false).await,
    }
}
/// Six-concurrent shared BFS walker (400 admitted nodes), no mutex held over await.
/// Each directory gets an instance-local snapshot of captured share/password cookies.
pub async fn parse(
    client: Cloud189Client,
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> Value {
    let (client, prepared) = match prepare_client(client, share_url, pwd, false).await {
        Ok(p) => p,
        Err(e) => return e,
    };
    let list = |sid: String, id: String, mode: String| {
        let mut c = client.clone();
        let pwd = pwd.to_owned();
        async move {
            c.list_share_dir(&sid, &id, &pwd, &mode)
                .await
                .map(|v| v.and_then(|v| v.as_array().cloned()).unwrap_or_default())
        }
    };
    let download = |method, id, sid| download_client(client.clone(), method, id, sid);
    parse_core(prepared, fid_chain, file_id, list, download).await
}

/// Synthetic callback seam; no endpoint override, transport trait or alternate walker.
#[doc(hidden)]
pub async fn parse_with_callbacks<G, GF, P, PF, L, LF, D, DF, E>(
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
    callbacks: (G, P, L, D),
) -> Value
where
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
        |(), code| async move { get(code).await.map(|v| ((), v)) },
        |(), code, pwd| async move { check(code, pwd).await.map(|v| ((), v)) },
    )
    .await;
    match prepared {
        Ok(((), p)) => parse_core(p, fid_chain, file_id, list, download).await,
        Err(e) => e,
    }
}
async fn try_download<D, DF, E>(id: &str, sid: &str, fast: bool, download: &D) -> Option<String>
where
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let methods = if fast {
        [
            DownloadMethod::AppShared,
            DownloadMethod::AppOwned,
            DownloadMethod::Open,
        ]
    } else {
        [
            DownloadMethod::Open,
            DownloadMethod::AppShared,
            DownloadMethod::AppOwned,
        ]
    };
    for method in methods {
        if let Ok(Some(url)) = download(method, id.to_owned(), sid.to_owned()).await
            && !url.is_empty()
        {
            return Some(url);
        }
    }
    None
}
async fn parse_core<L, LF, D, DF, E>(
    p: Prepared,
    chain: &[String],
    target: &str,
    list: L,
    download: D,
) -> Value
where
    L: Fn(String, String, String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let root = string(&p.info, "fileId");
    let name = string(&p.info, "fileName");
    let folder = truthy(&p.info["isFolder"]) || !truthy(&p.info["fileSize"]);
    if !folder {
        if !target.is_empty() && target != root {
            return error(404, "未找到指定文件");
        }
        let url = try_download(&root, &p.sid, false, &download).await;
        return single(&p.info, url);
    }
    if p.sid.is_empty() {
        return error(
            400,
            "无法获取shareId，文件夹解析失败。请确保分享链接有效或已正确验证提取码。",
        );
    }
    let mode = js_string(&field(&p.info, "shareMode", json!("1")));
    let tree = walk(
        |id| list(p.sid.clone(), id, mode.clone()),
        &root,
        &name,
        6,
        400,
    )
    .await;
    if !target.is_empty() {
        if target == root
            || tree
                .all_folders
                .iter()
                .any(|f| f["fileId"].as_str() == Some(target))
        {
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
    let Some(node) = tree.nodes.get(want).or_else(|| tree.nodes.get(&root)) else {
        return error(400, "文件夹为空或获取失败");
    };
    let start_name = js_string(&first_truthy(
        node,
        &["currentPath"],
        if name.is_empty() {
            json!("root")
        } else {
            json!(name)
        },
    ));
    json!({"code":200,"msg":"文件夹列表获取成功","success":true,
        "shareKey":format!("c189:{}",p.sid),"data":{
            "tree":nested_tree(&tree,want,&start_name),"sid":p.sid}})
}
fn single(file: &Value, url: Option<String>) -> Value {
    let Some(url) = url else {
        return error(500, "获取下载链接失败（只读模式未启用转存回退）");
    };
    let name = field(file, "fileName", json!(""));
    let size = field(file, "fileSize", json!(0));
    json!({"code":200,"msg":"解析成功","success":true,"data":{
        "file_name":name,"file_size":size,"download_url":url,"is_folder":false,"file_count":1,
        "files":[{"name":name,"size":size,"url":url}]}})
}

/// App dt=1 -> App dt=3 -> Open. An existing sid bypasses share/password calls.
/// No Range hop / download-body request / cache is performed; main owns those policies.
/// With sid supplied, main must reject known directory IDs using its shared tree cache.
pub async fn fast_download(
    client: Cloud189Client,
    share_url: &str,
    pwd: &str,
    file_id: &str,
    sid: &str,
) -> Result<String, Value> {
    let (client, p) = if sid.is_empty() {
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
    G: Fn(String) -> GF,
    GF: Future<Output = Result<Option<Value>, E>>,
    P: Fn(String, String) -> PF,
    PF: Future<Output = Result<Value, E>>,
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let p = if sid.is_empty() {
        prepare(
            (),
            share_url,
            pwd,
            true,
            |(), code| async move { get(code).await.map(|v| ((), v)) },
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
    D: Fn(DownloadMethod, String, String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    if target.is_empty() {
        return Err(error(400, "无效文件ID"));
    }
    if string(&p.info, "fileId") == target
        && (truthy(&p.info["isFolder"]) || !truthy(&p.info["fileSize"]))
    {
        return Err(error(400, "目录无法下载"));
    }
    if p.sid.is_empty() {
        return Err(error(502, "无法获取shareId"));
    }
    let url = try_download(target, &p.sid, true, &download)
        .await
        .ok_or_else(|| error(502, "无法获取直链"))?;
    if url
        .to_ascii_lowercase()
        .contains("download.cloud.189.cn/file/downloadfile.action")
    {
        return Err(error(400, "目录无法下载"));
    }
    Ok(url)
}
