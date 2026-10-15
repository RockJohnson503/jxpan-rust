//! Read-only mobile orchestration: complete traversal, explicit failure, no transfer.
pub use crate::compat::format_file_size;
use crate::compat::js_string;
use crate::providers::{mcloud, mcloud_client::MobileCloudClient};
use crate::tree::{self, Walk, nested_tree_complete, walk_complete};
use serde_json::{Value, json};
use std::future::Future;

#[derive(Clone, Copy)]
pub struct ServiceConfig {
    pub enabled: bool,
    pub authorization_configured: bool,
}
fn error(code: u16, msg: &str) -> Value {
    json!({"code":code,"msg":msg,"success":false,"data":null})
}
fn success(link: &str, data: Value) -> Value {
    json!({"code":200,"msg":"解析成功","success":true,"shareKey":format!("mc:{link}"),"data":data})
}
fn gate(config: ServiceConfig, share_url: &str) -> Result<String, Value> {
    if !config.enabled {
        return Err(error(503, "移动云盘解析已禁用"));
    }
    if !config.authorization_configured {
        return Err(error(401, "移动云盘 Authorization 未配置"));
    }
    mcloud::extract_share_info(share_url)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error(400, "无效的移动云盘分享链接"))
}
fn tree_error(error_kind: tree::TreeError) -> Value {
    match error_kind {
        tree::TreeError::Limit => error(413, "分享目录超过安全资源上限，未返回不完整结果"),
        tree::TreeError::Cycle => error(502, "分享目录结构异常，未返回不完整结果"),
        _ => error(502, "移动云盘目录或文件信息获取失败，请重试"),
    }
}
fn directory_ids(walk: &Walk) -> Vec<String> {
    std::iter::once(walk.root_id.clone())
        .chain(walk.all_folders.iter().map(|f| js_string(&f["fileId"])))
        .collect()
}
fn row(file: &Value) -> Value {
    let size = file.get("fileSize").cloned().unwrap_or(Value::Null);
    json!({"file_id":file["fileId"],"file_name":file["fileName"],
        "file_size":if size.is_null() { "未知".to_owned() } else { format_file_size(&size) },
        "file_size_bytes":size,"is_folder":false,"download_url":null})
}

pub async fn parse(
    client: &MobileCloudClient,
    config: ServiceConfig,
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> Value {
    parse_indexed(client, config, share_url, pwd, fid_chain, file_id)
        .await
        .0
}
pub async fn parse_indexed(
    client: &MobileCloudClient,
    config: ServiceConfig,
    share_url: &str,
    pwd: &str,
    fid_chain: &[String],
    file_id: &str,
) -> (Value, Option<(String, Vec<String>)>) {
    let link = match gate(config, share_url) {
        Ok(link) => link,
        Err(e) => return (e, None),
    };
    let list = |id: String| {
        let client = client.clone();
        let link = link.clone();
        let pwd = pwd.to_owned();
        async move { client.list_files(&link, &pwd, &id).await }
    };
    let download = |id: String| {
        let client = client.clone();
        let link = link.clone();
        async move { client.download_url(&link, &id).await }
    };
    parse_core(
        &link,
        fid_chain,
        file_id,
        client.concurrency(),
        &list,
        &download,
    )
    .await
}
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
    parse_indexed_with_callbacks(config, share_url, fid_chain, file_id, list, download)
        .await
        .0
}
#[doc(hidden)]
pub async fn parse_indexed_with_callbacks<L, LF, D, DF, E>(
    config: ServiceConfig,
    share_url: &str,
    fid_chain: &[String],
    file_id: &str,
    list: L,
    download: D,
) -> (Value, Option<(String, Vec<String>)>)
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let link = match gate(config, share_url) {
        Ok(link) => link,
        Err(e) => return (e, None),
    };
    parse_core(
        &link,
        fid_chain,
        file_id,
        crate::settings::DEFAULT_CONCURRENCY,
        &list,
        &download,
    )
    .await
}
async fn parse_core<L, LF, D, DF, E>(
    link: &str,
    chain: &[String],
    target: &str,
    concurrency: usize,
    list: &L,
    download: &D,
) -> (Value, Option<(String, Vec<String>)>)
where
    L: Fn(String) -> LF,
    LF: Future<Output = Result<Vec<Value>, E>>,
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let root_files = match list("root".into()).await {
        Ok(files) => files,
        Err(_) => return (error(502, "移动云盘根目录获取失败，请重试"), None),
    };
    // Preserve the homepage's single-file list behavior, without an extra root call.
    if target.is_empty()
        && chain.is_empty()
        && root_files.len() == 1
        && !tree::is_folder(&root_files[0])
    {
        let file = match tree::normalize_entry(&root_files[0]) {
            Ok(file) => file,
            Err(e) => return (tree_error(e), None),
        };
        return (
            success(
                link,
                json!({"name":file["fileName"],"desc":"","folder":[],"list":[row(&file)],
            "have_page":false,"is_mcloud_folder":true,"tree_complete":true,"share_id":link,
            "current_folder_id":"root","parent_folder_id":null}),
            ),
            None,
        );
    }
    let walk = walk_complete(
        |id: String| {
            let seed = (id == "root").then(|| root_files.clone());
            async move {
                match seed {
                    Some(files) => Ok(files),
                    None => list(id).await,
                }
            }
        },
        "root",
        "",
        concurrency,
    )
    .await;
    let walk = match walk {
        Ok(walk) => walk,
        Err(e) => return (tree_error(e), None),
    };
    let index = Some((link.to_owned(), directory_ids(&walk)));
    if !target.is_empty() {
        if target == "root" || walk.nodes.contains_key(target) {
            return (
                error(400, "指定的 ID 是文件夹，请使用 fid 参数进入文件夹"),
                index,
            );
        }
        let Some(file) = walk.file(target) else {
            return (error(404, "未找到指定文件"), index);
        };
        return (single_file(link, file, download).await, index);
    }
    let want = chain.last().map_or("root", String::as_str);
    let Some(node) = walk.nodes.get(want) else {
        return (error(404, "未找到指定文件夹"), index);
    };
    let name = node["currentPath"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("移动云盘分享");
    (
        success(
            link,
            json!({"tree":nested_tree_complete(&walk,want,name),"sid":link,
        "tree_complete":true,"file_count":walk.all_files.len(),"folder_count":walk.visited}),
        ),
        index,
    )
}
async fn single_file<D, DF, E>(link: &str, file: &Value, download: &D) -> Value
where
    D: Fn(String) -> DF,
    DF: Future<Output = Result<Option<String>, E>>,
{
    let id = js_string(&file["fileId"]);
    let url = match download(id).await {
        Ok(Some(url)) if !url.is_empty() => url,
        _ => return error(502, "移动云盘获取下载链接失败，请重试"),
    };
    let mut data = row(file);
    data["download_url"] = json!(url);
    success(link, data)
}

/// Direct folder IDs already scope the supplier call; earlier chain components
/// are navigation metadata, not separate requests for every ancestor.
pub async fn parse_with_fid_chain(
    client: &MobileCloudClient,
    link: &str,
    pwd: &str,
    chain: &[String],
    target: &str,
) -> Value {
    let list = |id: String| {
        let client = client.clone();
        let link = link.to_owned();
        let pwd = pwd.to_owned();
        async move { client.list_files(&link, &pwd, &id).await }
    };
    let download = |id: String| {
        let client = client.clone();
        let link = link.to_owned();
        async move { client.download_url(&link, &id).await }
    };
    fid_chain_core(link, chain, target, &list, &download).await
}
#[doc(hidden)]
pub async fn fid_chain_with_callbacks<L, LF, D, DF, E>(
    link: &str,
    chain: &[String],
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
    fid_chain_core(link, chain, target, &list, &download).await
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
    let folder = chain.last().map_or("root", String::as_str);
    let files = match list(folder.into()).await {
        Ok(files) => files,
        Err(_) => return error(502, "移动云盘子目录获取失败"),
    };
    let files = match files
        .iter()
        .map(tree::normalize_entry)
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(files) => files,
        Err(e) => return tree_error(e),
    };
    if !target.is_empty() {
        let Some(file) = files.iter().find(|f| f["fileId"].as_str() == Some(target)) else {
            return error(404, "未找到指定文件");
        };
        if file["isFolder"] == true {
            return error(400, "指定的 ID 是文件夹而非文件");
        }
        return single_file(link, file, download).await;
    }
    let folders: Vec<_> = files
        .iter()
        .filter(|f| f["isFolder"] == true)
        .map(|f| json!({"id":f["fileId"],"name":f["fileName"]}))
        .collect();
    let rows: Vec<_> = files
        .iter()
        .filter(|f| f["isFolder"] == false)
        .map(row)
        .collect();
    success(
        link,
        json!({"name":"移动云盘子文件夹","desc":"","folder":folders,"list":rows,
        "have_page":false,"is_mcloud_folder":true,"share_id":link,"current_folder_id":folder,
        "parent_folder_id":if chain.len()>1 {chain[chain.len()-2].as_str()} else {"root"}}),
    )
}
