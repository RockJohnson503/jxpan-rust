//! Shared compatibility layer for the deployed jxpanWalkDirTree / jxpanBuildNestedTree.
//! Provider authentication, pagination, HTTP clients and file classification stay in
//! the caller. This walker intentionally does NOT use MobileCloudParser's classifier.
use crate::compat::{first_truthy, js_string, truthy};
use futures_util::future::join_all;
use serde_json::{Map, Value, json};
use std::collections::{HashSet, VecDeque};
use std::future::Future;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_CONCURRENCY: usize = 6;
pub const DEFAULT_MAX_NODES: usize = 400;
/// Avoid recursive JSON consumers overflowing on hostile, unusually deep graphs.
pub const MAX_NESTED_DEPTH: usize = 400;

/// Fields returned by the deployment, without provider-specific state.
#[derive(Debug, Clone)]
pub struct Walk {
    pub root_id: String,
    pub nodes: Map<String, Value>,
    pub all_files: Vec<Value>,
    pub all_folders: Vec<Value>,
    pub visited: usize,
}

impl Walk {
    pub fn to_json(&self) -> Value {
        json!({"rootId": self.root_id, "nodes": self.nodes,
            "allFiles": self.all_files, "allFolders": self.all_folders,
            "visited": self.visited})
    }

    /// First discovery wins, like the deployment's allFiles.find(...).
    pub fn file(&self, id: &str) -> Option<&Value> {
        self.all_files
            .iter()
            .find(|f| f["fileId"].as_str() == Some(id))
    }
}

fn id(row: &Value, keys: &[&str]) -> String {
    js_string(&first_truthy(row, keys, Value::String(String::new())))
}

/// JS own-key enumeration: array indices first, then other strings in insertion order.
fn js_key_order(map: &mut Map<String, Value>) {
    fn index(key: &str) -> Option<u32> {
        key.parse::<u32>()
            .ok()
            .filter(|n| *n != u32::MAX && n.to_string() == key)
    }
    let mut entries: Vec<_> = std::mem::take(map).into_iter().collect();
    entries.sort_by_key(|(key, _)| index(key).map_or((1, 0), |n| (0, n)));
    map.extend(entries);
}

struct Pending {
    id: String,
    path: Value,
}

/// Breadth-first, bounded *batch* concurrency; no spawned tasks and no locks.
///
/// The callback owns its ID and may capture a cloneable reqwest client. Errors
/// become empty preloaded nodes, as in the JS try/catch. Return Vec<Value> after
/// provider pagination/response normalization. Zero options select JS || defaults.
/// Duplicate queued IDs are retained for admission accounting but requested once.
pub async fn walk<F, Fut, E>(
    list: F,
    root_id: &str,
    root_name: &str,
    concurrency: usize,
    max_nodes: usize,
) -> Walk
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<Vec<Value>, E>>,
{
    let max_nodes = if max_nodes == 0 {
        DEFAULT_MAX_NODES
    } else {
        max_nodes
    };
    let concurrency = if concurrency == 0 {
        DEFAULT_CONCURRENCY
    } else {
        concurrency
    }
    .min(max_nodes);
    let mut result = Walk {
        root_id: root_id.to_owned(),
        nodes: Map::new(),
        all_files: Vec::new(),
        all_folders: Vec::new(),
        visited: 0,
    };
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([Pending {
        id: root_id.to_owned(),
        path: json!(root_name),
    }]);
    while !queue.is_empty() && seen.len() < max_nodes {
        // Mark the entire batch before calling/awaiting list, matching Promise.all.
        let mut batch = Vec::new();
        for _ in 0..concurrency.min(queue.len()) {
            let item = queue.pop_front().expect("nonempty batch queue");
            if seen.insert(item.id.clone()) {
                batch.push(item);
            }
        }
        let futures = batch.into_iter().map(|item| {
            let response = list(item.id.clone());
            async move { (item, response.await.unwrap_or_default()) }
        });
        // join_all returns input order even when responses finish out of order.
        for (item, entries) in join_all(futures).await {
            let mut folders = Vec::new();
            let mut files = Vec::new();
            for entry in entries {
                let fid = id(&entry, &["fileId", "id", "coID", "contentID"]);
                if fid.is_empty() {
                    continue;
                }
                let name = first_truthy(
                    &entry,
                    &["fileName", "name", "coName", "contentName"],
                    json!(""),
                );
                let is_dir = entry.get("isFolder").is_some_and(truthy)
                    || entry.get("isDir").is_some_and(truthy)
                    || entry
                        .get("coType")
                        .is_some_and(|v| v.is_number() && v.as_f64() == Some(1.0));
                let path = if truthy(&item.path) {
                    json!(format!("{}/{}", js_string(&item.path), js_string(&name)))
                } else {
                    name.clone()
                };
                let mut row = Map::new();
                row.insert("fileId".into(), json!(fid));
                row.insert("fileName".into(), name);
                if !is_dir {
                    row.insert(
                        "fileSize".into(),
                        first_truthy(
                            &entry,
                            &["fileSize", "size", "coSize", "contentSize"],
                            json!(0),
                        ),
                    );
                }
                row.insert("isFolder".into(), json!(is_dir));
                let mut full = row.clone();
                full.insert("parentId".into(), json!(item.id));
                full.insert("path".into(), json!(path));
                if is_dir {
                    folders.push(Value::Object(row));
                    result.all_folders.push(Value::Object(full));
                    if !seen.contains(&fid) && seen.len() + queue.len() < max_nodes {
                        queue.push_back(Pending { id: fid, path });
                    }
                } else {
                    files.push(Value::Object(row));
                    result.all_files.push(Value::Object(full));
                }
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            result.nodes.insert(
                item.id.clone(),
                json!({
                    "type": "folder_list", "currentPath": item.path, "currentId": item.id,
                    "folders": folders, "files": files, "_preloaded": true,
                    "_preloadedAt": u64::try_from(now).unwrap_or(u64::MAX)
                }),
            );
        }
    }
    result.visited = seen.len();
    js_key_order(&mut result.nodes);
    result
}

struct Frame<'a> {
    id: String,
    attach_key: Option<String>,
    folders: &'a [Value],
    files: &'a [Value],
    position: usize,
    out: Map<String, Value>,
}

impl<'a> Frame<'a> {
    fn new(walk: &'a Walk, id: String, attach_key: Option<String>) -> Self {
        let node = walk.nodes.get(&id);
        Self {
            id,
            attach_key,
            position: 0,
            out: Map::new(),
            folders: node
                .and_then(|n| n["folders"].as_array())
                .map_or(&[], |a| a.as_slice()),
            files: node
                .and_then(|n| n["files"].as_array())
                .map_or(&[], |a| a.as_slice()),
        }
    }
}

/// Nested name-keyed tree: folders first, then {id,size} files; one #ID suffix
/// on collisions (including overwrites of an already suffixed name), as deployed.
/// Ancestor cycles and depth >400 become {}, rather than infinite recursion.
/// Literal __proto__ is a safe data key, not a JS prototype mutation.
pub fn nested_tree(walk: &Walk, start_id: &str, start_name: &str) -> Value {
    let start_id = if start_id.is_empty() {
        &walk.root_id
    } else {
        start_id
    };
    let mut ancestors = HashSet::from([start_id.to_owned()]);
    let mut stack = vec![Frame::new(walk, start_id.to_owned(), None)];
    let body = loop {
        let frame = stack.last_mut().expect("root frame");
        if frame.position == frame.folders.len() + frame.files.len() {
            let mut finished = stack.pop().expect("completed frame");
            ancestors.remove(&finished.id);
            js_key_order(&mut finished.out);
            let value = Value::Object(finished.out);
            if let Some(parent) = stack.last_mut() {
                parent
                    .out
                    .insert(finished.attach_key.expect("child key"), value);
                continue;
            }
            break value;
        }
        let is_folder = frame.position < frame.folders.len();
        let row = if is_folder {
            &frame.folders[frame.position]
        } else {
            &frame.files[frame.position - frame.folders.len()]
        };
        frame.position += 1;
        let fid = id(row, &["fileId", "id"]);
        let name = first_truthy(row, &["fileName", "name"], json!(fid));
        if !truthy(&name) || (!is_folder && fid.is_empty()) {
            continue;
        }
        let mut key = js_string(&name);
        if frame.out.contains_key(&key) {
            key = format!("{key}#{fid}");
        }
        if !is_folder {
            let size = first_truthy(row, &["fileSize", "size"], json!(0));
            frame.out.insert(key, json!({"id": fid, "size": size}));
        } else if fid.is_empty() || ancestors.contains(&fid) || stack.len() >= MAX_NESTED_DEPTH {
            stack
                .last_mut()
                .expect("parent frame")
                .out
                .insert(key, json!({}));
        } else {
            ancestors.insert(fid.clone());
            stack.push(Frame::new(walk, fid, Some(key)));
        }
    };
    let mut root = Map::new();
    root.insert(
        if start_name.is_empty() {
            "root"
        } else {
            start_name
        }
        .into(),
        body,
    );
    Value::Object(root)
}
