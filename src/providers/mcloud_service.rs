//! Read-only MobileCloudParser orchestration. No cache, login, persistence or URL fetches.
pub use crate::compat::format_file_size;
use crate::compat::{first_truthy as choose, js_string, truthy};
use crate::providers::{mcloud, mcloud_client::MobileCloudClient};
use crate::tree::{Walk, nested_tree, walk};
use serde_json::{Value, json};
use std::{collections::HashSet, future::Future};

/// Configuration gates are supplied by the caller, not inferred from HTTP failures.
/// `authorization_configured` means the original authorization string is nonempty.
#[derive(Clone, Copy)]
pub struct ServiceConfig {
    pub enabled: bool,
    pub authorization_configured: bool,
}

fn error(code: u16, msg: impl Into<String>) -> Value {
    json!({"code":code,"msg":msg.into(),"success":false,"data":null})
}
fn success(link: &str, data: Value) -> Value {
    json!({"code":200,"msg":"解析成功","success":true,"shareKey":format!("mc:{link}"),"data":data})
}
fn file_id(v: &Value) -> Value {
    choose(v, &["coID", "contentID", "id", "fileId"], json!(""))
}
fn file_name(v: &Value, fallback: &str) -> Value {
    choose(
        v,
        &["coName", "contentName", "name", "fileName"],
        json!(fallback),
    )
}
fn file_size(v: &Value) -> Value {
    choose(v, &["coSize", "contentSize", "size", "fileSize"], json!(0))
}

fn row(file: &Value, fallback: &str) -> Value {
    json!({"file_id":file_id(file),"file_name":file_name(file,fallback),
        "file_size":format_file_size(&file_size(file)),"download_url":null})
}
fn malformed(files: &[Value]) -> bool {
    files.iter().any(Value::is_null)
}
fn malformed_error() -> Value {
    // Static text only: never include HTTP bodies, authorization, password or share URL.
    error(
        500,
        "解析失败: Cannot read properties of null (reading 'coType')",
    )
}
fn gate(config: ServiceConfig, share_url: &str) -> Result<String, Value> {
    if !config.enabled {
        return Err(error(503, "移动云盘解析已禁用"));
    }
    if !config.authorization_configured {
        return Err(error(
            401,
            "移动云盘 Authorization 未配置，请检查 MCLOUD_AUTHORIZATION 环境变量",
        ));
    }
    mcloud::extract_share_info(share_url)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error(400, "无效的移动云盘分享链接"))
}

/// Production adapter. The HTTP client must disable redirects, as required by MobileCloudClient.
/// All requests use that client's pinned host; returned download URLs are data only.
pub async fn parse(
    client: &MobileCloudClient,
    config: ServiceConfig,
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> Value {
    let link = match gate(config, share_url) {
        Ok(link) => link,
        Err(e) => return e,
    };
    let list = |id: String| {
        let client = client.clone();
        let link = link.clone();
        let pwd = pwd.to_owned();
        async move { client.list_files(&link, &pwd, &id).await }
    };
    let download = |path: String| {
        let client = client.clone();
        let link = link.clone();
        async move { client.download_url(&link, &path).await }
    };
    parse_core(&link, fid_chain, file_id, &list, &download).await
}

/// Offline injection seam for integration tests; no endpoint setter or transport abstraction.
/// Callbacks receive only folder IDs / file paths; the production adapter supplies fixed-host calls.
#[doc(hidden)]
pub async fn parse_with_callbacks<L, LF, D, DF, E>(
    config: ServiceConfig,
    share_url: &str,
    fid_chain: &[String],
    file_id: &str,
    list: L,
    download: D,
) -> Value
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let link = match gate(config, share_url) {
        Ok(link) => link,
        Err(e) => return e,
    };
    parse_core(&link, fid_chain, file_id, &list, &download).await
}

async fn parse_core<L, LF, D, DF, E>(
    link: &str,
    fid_chain: &[String],
    target: &str,
    list: &L,
    download: &D,
) -> Value
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    if !target.is_empty() {
        return file_by_id(link, target, list, download).await;
    }
    if let Some(want) = fid_chain.last() {
        return tree_result(link, want, None, list).await;
    }
    let files = list("root".into()).await.unwrap_or_default();
    if files.is_empty() {
        return error(404, "分享中没有文件，可能是分享链接失效或Authorization无效");
    }
    if malformed(&files) {
        return malformed_error();
    }
    if files.len() == 1 && !mcloud::is_folder(&files[0]) {
        let file = &files[0];
        return success(
            link,
            json!({"name":file_name(file,""),"desc":"","folder":[],"list":[row(file,"")],
            "have_page":false,"is_mcloud_folder":true,"tree_complete":true,"share_id":link,
            "current_folder_id":"root","parent_folder_id":null}),
        );
    }
    tree_result(link, "root", Some(files), list).await
}

async fn tree_result<L, LF, E>(
    link: &str,
    want: &str,
    root_files: Option<Vec<Value>>,
    list: &L,
) -> Value
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
{
    // Seed the already-read root, saving exactly one list request. No lock across await.
    let tree: Walk = walk(
        |id: String| {
            let seed = if id == "root" {
                root_files.clone()
            } else {
                None
            };
            async move {
                if let Some(files) = seed {
                    Ok(files)
                } else {
                    list(id).await
                }
            }
        },
        "root",
        "",
        4,
        400,
    )
    .await;
    let want = if want.is_empty() { "root" } else { want };
    let node = tree
        .nodes
        .get(want)
        .or_else(|| tree.nodes.get("root"))
        .or_else(|| tree.nodes.get(&tree.root_id));
    if node.is_none() {
        return error(404, "文件夹为空或获取失败");
    }
    // Preserve original quirk: fallback node does NOT change the nested tree's requested ID.
    let name = tree
        .nodes
        .get(want)
        .and_then(|n| n.get("currentPath"))
        .filter(|v| truthy(v))
        .map(js_string)
        .unwrap_or_else(|| "移动云盘分享".into());
    success(
        link,
        json!({"tree":nested_tree(&tree,want,&name),"sid":link}),
    )
}

async fn single_file<D, DF, E>(link: &str, file: &Value, download: &D) -> Value
where
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let mut result = row(file, "未知文件");
    let path = choose(file, &["path", "coPath"], json!(""));
    if truthy(&path) {
        result["download_url"] = download(js_string(&path))
            .await
            .ok()
            .flatten()
            .map(Value::String)
            .unwrap_or(Value::Null);
    }
    success(link, result)
}
async fn file_by_id<L, LF, D, DF, E>(link: &str, target: &str, list: &L, download: &D) -> Value
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let mut visited = HashSet::new();
    let mut stack = vec![("root".to_owned(), 0usize)];
    while let Some((id, depth)) = stack.pop() {
        if depth > 8 || !visited.insert(id.clone()) {
            continue;
        }
        let files = list(id).await.unwrap_or_default();
        if let Some(file) = files.iter().find(|f| js_string(&file_id(f)) == target) {
            if file.is_null() {
                return malformed_error();
            }
            if mcloud::is_folder(file) {
                return error(400, "指定的ID是文件夹，请使用fid参数进入文件夹");
            }
            return single_file(link, file, download).await;
        }
        if malformed(&files) {
            if depth == 0 {
                return malformed_error();
            } else {
                continue;
            }
        }
        // Reverse stack insertion preserves recursive DFS order and first-match precedence.
        for folder in files.iter().rev().filter(|f| mcloud::is_folder(f)) {
            let id = file_id(folder);
            if truthy(&id) {
                stack.push((js_string(&id), depth + 1));
            }
        }
    }
    let mut result = error(404, format!("未找到指定文件(id={target})"));
    result["available_files"] = json!([]);
    result
}

/// Dedicated subdirectory action, matching parseWithFidChain (not parse's tree/fid dispatch).
/// This accepts an already-extracted link ID just like the original method.
pub async fn parse_with_fid_chain(
    client: &MobileCloudClient,
    link: &str,
    pwd: &str,
    fid_chain: &[String],
    target: &str,
) -> Value {
    let list = |id: String| {
        let client = client.clone();
        let link = link.to_owned();
        let pwd = pwd.to_owned();
        async move { client.list_files(&link, &pwd, &id).await }
    };
    let download = |path: String| {
        let client = client.clone();
        let link = link.to_owned();
        async move { client.download_url(&link, &path).await }
    };
    fid_chain_core(link, fid_chain, target, &list, &download).await
}
#[doc(hidden)]
pub async fn fid_chain_with_callbacks<L, LF, D, DF, E>(
    link: &str,
    fid_chain: &[String],
    target: &str,
    list: L,
    download: D,
) -> Value
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    fid_chain_core(link, fid_chain, target, &list, &download).await
}
async fn fid_chain_core<L, LF, D, DF, E>(
    link: &str,
    chain: &[String],
    target: &str,
    list: &L,
    download: &D,
) -> Value
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let mut parent = "root";
    let mut last = None;
    for (i, id) in chain.iter().enumerate() {
        let files = list(id.clone()).await.unwrap_or_default();
        if files.is_empty() {
            return error(
                404,
                format!(
                    "无法加载第{}层子文件夹（fid={id}），请检查文件夹ID是否正确",
                    i + 1
                ),
            );
        }
        parent = id;
        if i == chain.len() - 1 && !target.is_empty() {
            if let Some(file) = files.iter().find(|f| js_string(&file_id(f)) == target) {
                if mcloud::is_folder(file) {
                    return error(400, "指定的ID是文件夹而非文件，请使用fid参数进入");
                }
                return single_file(link, file, download).await;
            }
            if malformed(&files) {
                return malformed_error();
            }
            let available: Vec<Value> = files
                .iter()
                .filter(|f| !mcloud::is_folder(f))
                .map(|f| {
                    // JS omits undefined properties, including in available_files.
                    let mut row = serde_json::Map::new();
                    if let Some(name) = f
                        .get("coName")
                        .filter(|v| truthy(v))
                        .or_else(|| f.get("name"))
                    {
                        row.insert("name".into(), name.clone());
                    }
                    if let Some(id) = f.get("coID").filter(|v| truthy(v)).or_else(|| f.get("id")) {
                        row.insert("id".into(), id.clone());
                    }
                    Value::Object(row)
                })
                .collect();
            let names = available
                .iter()
                .map(|f| {
                    f.get("name")
                        .filter(|v| !v.is_null())
                        .map(js_string)
                        .unwrap_or_default()
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut result = error(
                404,
                format!("未找到指定文件(id={target})，可用文件: {names}"),
            );
            result["available_files"] = json!(available);
            return result;
        }
        last = Some(files);
    }
    if !target.is_empty() {
        return error(404, format!("未找到指定文件(id={target})"));
    }
    let files = match last {
        Some(files) => files,
        None => list(parent.into()).await.unwrap_or_default(),
    };
    if files.is_empty() {
        return error(404, "子文件夹为空");
    }
    if malformed(&files) {
        return malformed_error();
    }
    let folders: Vec<_> = files
        .iter()
        .filter(|f| mcloud::is_folder(f))
        .map(|f| json!({"id":file_id(f),"name":file_name(f,"")}))
        .collect();
    let rows: Vec<_> = files
        .iter()
        .filter(|f| !mcloud::is_folder(f))
        .map(|f| row(f, ""))
        .collect();
    success(
        link,
        json!({"name":"移动云盘子文件夹","desc":"","folder":folders,"list":rows,
        "have_page":false,"is_mcloud_folder":true,"share_id":link,"current_folder_id":parent,
        "parent_folder_id":if chain.len()>1 { &chain[chain.len()-2] } else { "root" }}),
    )
}
