use jxpan_rust::providers::cloud189_transfer as transfer;
use jxpan_rust::tree;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use transfer::{
    Cleanup, Credentials, Options, Request, Runtime, ShareItem, TransferError, TransferKernel,
    Transport,
};

const EPOCH: u64 = 1_700_000_000_123;
struct Fixed {
    sleeps: Vec<u64>,
    n: u64,
}
impl Runtime for Fixed {
    fn now_ms(&self) -> u64 {
        EPOCH
    }
    fn random(&mut self) -> f64 {
        0.25
    }
    fn uuid(&mut self) -> String {
        self.n += 1;
        format!("{:08x}-4444-4444-8444-444444444444", self.n)
    }
    async fn sleep(&mut self, delay: Duration) {
        self.sleeps.push(delay.as_millis() as u64);
    }
}
struct Memory {
    steps: VecDeque<Value>,
    trace: Vec<Value>,
}
impl Transport for Memory {
    async fn fetch(&mut self, request: Request) -> Result<Value, TransferError> {
        let headers: serde_json::Map<String, Value> = request
            .headers
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect();
        self.trace.push(json!({"url":request.url,"method":request.method,"headers":headers,"body":request.body}));
        let response = self.steps.pop_front().expect("unexpected memory request");
        if response == json!({"transport_failure":true}) {
            Err(TransferError::Transport)
        } else {
            Ok(response)
        }
    }
}
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/cloud189-transfer.json")).unwrap()
}
fn kernel(steps: Vec<Value>) -> TransferKernel<Memory, Fixed> {
    TransferKernel::new(
        Memory {
            steps: steps.into(),
            trace: vec![],
        },
        Fixed {
            sleeps: vec![],
            n: 0,
        },
        Credentials::from_account(&fixture()["credentials"]),
        Options {
            enable_transfers: true,
            ..Options::default()
        },
    )
}
fn item(id: &str, name: &str, folder: bool) -> ShareItem {
    ShareItem {
        file_id: id.into(),
        file_name: name.into(),
        is_folder: folder,
    }
}
async fn scope(k: &mut TransferKernel<Memory, Fixed>) -> transfer::IsolatedScope {
    k.provision_scope("authorized-test-root", |root, name| async move {
        assert_eq!(root, "authorized-test-root");
        assert_eq!(name, "jxpan-transfer-00000002-4444-4444-8444-444444444444");
        Ok("isolated".into())
    })
    .await
    .unwrap()
}
async fn single_tree(id: String) -> Result<tree::Walk, TransferError> {
    Ok(tree::walk(|_| async { Ok::<_, TransferError>(vec![json!({"fileId":"saved-new-id","fileName":"video.mkv","fileSize":42,"isFolder":false})]) },
        &id, "isolated", 1, 400).await)
}
async fn batch_tree(id: String) -> Result<tree::Walk, TransferError> {
    Ok(tree::walk(
        |folder| async move {
            Ok::<_, TransferError>(match folder.as_str() {
                "isolated" => vec![
                    json!({"fileId":"saved-dir","fileName":"season","isFolder":true}),
                    json!({"fileId":"saved-video","fileName":"video.mkv","fileSize":42}),
                ],
                "saved-dir" => vec![
                    json!({"fileId":"saved-nested","fileName":"nested","isFolder":true}),
                    json!({"fileId":"saved-episode","fileName":"episode.mkv","fileSize":42}),
                ],
                "saved-nested" => {
                    vec![json!({"fileId":"saved-subtitle","fileName":"subtitle.srt","fileSize":42})]
                }
                _ => panic!("unexpected own directory"),
            })
        },
        &id,
        "isolated",
        1,
        400,
    )
    .await)
}
fn prefix(status: Value) -> Vec<Value> {
    vec![
        json!({"sessionKey":"web-key"}),
        json!({"res_code":0,"taskId":"task"}),
        status,
    ]
}
fn done() -> Value {
    json!({"res_code":0,"taskStatus":3})
}
fn deleted() -> Value {
    json!({"res_code":0})
}
fn forms(trace: &[Value], action: &str) -> Vec<String> {
    trace
        .iter()
        .filter(|v| v["url"].as_str().unwrap().contains(action))
        .map(|v| v["body"].as_str().unwrap().into())
        .collect()
}

#[tokio::test]
async fn disabled_and_default_roots_have_zero_effects() {
    let mut k = TransferKernel::new(
        Memory {
            steps: VecDeque::new(),
            trace: vec![],
        },
        Fixed {
            sleeps: vec![],
            n: 0,
        },
        Credentials::from_account(&fixture()["credentials"]),
        Options::default(),
    );
    let result = k
        .provision_scope("test", |_, _| async { panic!("disabled callback") })
        .await;
    assert!(matches!(result, Err(TransferError::Disabled)));
    let mut k = kernel(vec![]);
    for root in ["-11", "-12", "0", "", "bad,ids"] {
        assert!(matches!(
            k.provision_scope(root, |_, _| async { panic!("forbidden callback") })
                .await,
            Err(TransferError::UnauthorizedScope)
        ));
    }
    assert!(k.transport.trace.is_empty());
    assert!(matches!(
        k.provision_scope("test", |_, _| async { Ok("test".into()) })
            .await,
        Err(TransferError::UnauthorizedScope)
    ));
}
#[tokio::test]
async fn success_uses_saved_id_and_exact_original_signatures() {
    let mut steps = prefix(done());
    steps.push(deleted());
    let mut k = kernel(steps);
    let s = scope(&mut k).await;
    let mut outcome = k
        .save_and_download(
            s,
            "share",
            item("source", "video.mkv", false),
            single_tree,
            |id| async move {
                assert_eq!(id, "saved-new-id");
                Ok(Some("https://offline.invalid/video".into()))
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome.cleanup, Cleanup::FilesRemovedDirectoriesRetained);
    assert_eq!(outcome.failure, None);
    assert!(outcome.receipt.files_cleaned());
    assert_eq!(outcome.receipt.resource_ids(), ["saved-new-id"]);
    let save = &k.transport.trace[1];
    assert_eq!(save["method"], "POST");
    let form: std::collections::HashMap<_, _> =
        url::form_urlencoded::parse(save["body"].as_str().unwrap().as_bytes())
            .into_owned()
            .collect();
    assert_eq!(form["targetFolderId"], "isolated");
    assert_eq!(form["shareId"], "share");
    assert_eq!(
        serde_json::from_str::<Value>(&form["taskInfos"]).unwrap(),
        json!([{"fileId":"source","fileName":"video.mkv","isFolder":0}])
    );
    assert_eq!(
        save["headers"]["signature"],
        fixture()["wire"]["web_signature"]
    );
    assert_eq!(
        save["headers"]["cookie"],
        "COOKIE_LOGIN_USER=offline-user; SSON=offline-sson"
    );
    assert_eq!(
        forms(&k.transport.trace, "batchDeleteFile"),
        ["fileIdList=saved-new-id"]
    );
    let delete = &k.transport.trace[3];
    assert_eq!(delete["headers"]["date"], "Tue, 14 Nov 2023 22:13:20 GMT");
    assert_eq!(
        delete["headers"]["signature"],
        fixture()["wire"]["delete_signature"]
    );
    assert!(delete["url"].as_str().unwrap().ends_with(
        "clientType=TELEPC&version=6.2&channelId=web_cloud.189.cn&rand=25000_2500000000"
    ));
    assert_eq!(k.runtime.sleeps, [2000]);
    let before = k.transport.trace.len();
    k.cleanup(&mut outcome.receipt).await.unwrap();
    assert_eq!(before, k.transport.trace.len());
}
#[tokio::test]
async fn download_none_and_error_both_cleanup_unlike_original() {
    for error in [false, true] {
        let mut steps = prefix(done());
        steps.push(deleted());
        let mut k = kernel(steps);
        let s = scope(&mut k).await;
        let out = k
            .save_and_download(
                s,
                "share",
                item("source", "video.mkv", false),
                single_tree,
                move |_| async move {
                    if error {
                        Err(TransferError::Transport)
                    } else {
                        Ok(None)
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(out.cleanup, Cleanup::FilesRemovedDirectoriesRetained);
        assert!(out.receipt.files_cleaned());
        assert!(out.failure.is_some());
        assert!(out.links.is_empty());
        assert_eq!(
            forms(&k.transport.trace, "batchDeleteFile"),
            ["fileIdList=saved-new-id"]
        );
    }
}
#[tokio::test]
async fn failed_cleanup_is_manual_even_with_download_url() {
    let mut steps = prefix(done());
    steps.push(json!({"res_code":1}));
    let mut k = kernel(steps);
    let s = scope(&mut k).await;
    let out = k
        .save_and_download(
            s,
            "share",
            item("source", "video.mkv", false),
            single_tree,
            |_| async { Ok(Some("https://offline.invalid/video".into())) },
        )
        .await
        .unwrap();
    assert_eq!(out.cleanup, Cleanup::ManualInterventionRequired);
    assert!(!out.receipt.files_cleaned());
    assert_eq!(out.receipt.remaining_file_ids(), ["saved-new-id"]);
    assert_eq!(out.links.len(), 1);
}
#[tokio::test]
async fn polling_is_bounded_and_strict_numeric_status() {
    let mut steps = prefix(json!({"res_code":0,"taskStatus":"3"}));
    steps.extend((0..14).map(|_| json!({"res_code":0,"taskStatus":1})));
    let mut k = TransferKernel::new(
        Memory {
            steps: steps.into(),
            trace: vec![],
        },
        Fixed {
            sleeps: vec![],
            n: 0,
        },
        Credentials::from_account(&fixture()["credentials"]),
        Options {
            enable_transfers: true,
            max_polls: usize::MAX,
            poll_delay: Duration::from_secs(999),
        },
    );
    let s = scope(&mut k).await;
    let out = k
        .save_and_download(
            s,
            "share",
            item("source", "video.mkv", false),
            |_| async { panic!("pending tree") },
            |_| async { panic!("pending download") },
        )
        .await
        .unwrap();
    assert_eq!(out.failure, Some(TransferError::PollLimit));
    assert_eq!(out.cleanup, Cleanup::ManualInterventionRequired);
    assert_eq!(out.receipt.task_id(), Some("task"));
    assert_eq!(k.runtime.sleeps, vec![2000; 15]);
    assert_eq!(forms(&k.transport.trace, "checkBatchTask").len(), 15);
    assert!(forms(&k.transport.trace, "batchDeleteFile").is_empty());
}
#[tokio::test]
async fn task_errors_stop_immediately_with_recovery_receipt() {
    for status in [
        json!({"res_code":0,"taskStatus":3,"errorCode":"QuotaExceeded"}),
        json!({"res_code":"0","taskStatus":3}),
        json!({"transport_failure":true}),
    ] {
        let mut k = kernel(prefix(status));
        let s = scope(&mut k).await;
        let out = k
            .save_and_download(
                s,
                "share",
                item("source", "video.mkv", false),
                |_| async { panic!("failed tree") },
                |_| async { panic!("failed download") },
            )
            .await
            .unwrap();
        assert!(out.failure.is_some());
        assert_eq!(out.cleanup, Cleanup::ManualInterventionRequired);
        assert_eq!(k.runtime.sleeps, [2000]);
        assert!(forms(&k.transport.trace, "batchDeleteFile").is_empty());
    }
}
#[tokio::test]
async fn full_batch_tree_downloads_every_leaf_and_never_deletes_directories() {
    let mut steps = prefix(done());
    steps.extend([deleted(), deleted(), deleted()]);
    let mut k = kernel(steps);
    let s = scope(&mut k).await;
    let seen = Arc::new(Mutex::new(vec![]));
    let log = seen.clone();
    let out = k
        .batch_save_share(
            s,
            "share",
            vec![
                item("source-dir", "season", true),
                item("source-file", "video.mkv", false),
            ],
            batch_tree,
            move |id| {
                log.lock().unwrap().push(id.clone());
                async move { Ok(Some(format!("https://offline.invalid/{id}"))) }
            },
        )
        .await
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        ["saved-video", "saved-episode", "saved-subtitle"]
    );
    assert_eq!(out.links.len(), 3);
    assert_eq!(out.receipt.resource_ids().len(), 5);
    assert_eq!(
        forms(&k.transport.trace, "batchDeleteFile"),
        [
            "fileIdList=saved-video",
            "fileIdList=saved-episode",
            "fileIdList=saved-subtitle"
        ]
    );
    assert_eq!(out.cleanup, Cleanup::FilesRemovedDirectoriesRetained);
}
#[tokio::test]
async fn partial_cleanup_retry_does_not_redelete_acknowledged_files() {
    let mut steps = prefix(done());
    steps.extend([deleted(), json!({"res_code":1})]);
    let mut k = kernel(steps);
    let s = scope(&mut k).await;
    let mut out = k
        .batch_save_share(
            s,
            "share",
            vec![
                item("source-dir", "season", true),
                item("source-file", "video.mkv", false),
            ],
            batch_tree,
            |_| async { Ok(None) },
        )
        .await
        .unwrap();
    assert_eq!(out.cleanup, Cleanup::ManualInterventionRequired);
    assert_eq!(
        out.receipt.remaining_file_ids(),
        ["saved-episode", "saved-subtitle"]
    );
    k.transport.steps.extend([deleted(), deleted()]);
    assert_eq!(
        k.cleanup(&mut out.receipt).await.unwrap(),
        Cleanup::FilesRemovedDirectoriesRetained
    );
    assert_eq!(
        forms(&k.transport.trace, "batchDeleteFile"),
        [
            "fileIdList=saved-video",
            "fileIdList=saved-episode",
            "fileIdList=saved-episode",
            "fileIdList=saved-subtitle"
        ]
    );
}
#[tokio::test]
async fn wrong_root_outside_parent_extra_file_and_truncated_tree_fail_closed() {
    for variant in 0..6 {
        let mut k = kernel(prefix(done()));
        let s = scope(&mut k).await;
        let out=k.save_and_download(s,"share",item("source","video.mkv",false),move |id|async move {
            let mut w=single_tree(id).await?;
            match variant {
                0=>w.root_id="share-root".into(),
                1=>w.all_files[0]["parentId"]=json!("account-root"),
                2=>{let extra=w.all_files[0].clone();w.all_files.push(extra);},
                3=>w.all_files[0]["fileId"]=json!("-11"),
                4=>w.all_files[0]["fileName"]=json!("unexpected.mkv"),
                _=>w.all_folders.push(json!({"fileId":"unvisited","parentId":"isolated","fileName":"nested","isFolder":true})),
            }Ok(w)
        },|_|async {panic!("invalid mapping download")}).await.unwrap();
        assert_eq!(out.failure, Some(TransferError::UnresolvedResources));
        assert!(out.receipt.completed());
        assert_eq!(out.cleanup, Cleanup::ManualInterventionRequired);
        assert!(forms(&k.transport.trace, "batchDeleteFile").is_empty());
    }
}
#[tokio::test]
async fn root_protocol_and_save_string_code_rejection() {
    let mut k = kernel(vec![json!({"res_code":0,"fileId":"root"})]);
    assert_eq!(
        k.get_root_directory("web-key").await.unwrap()["fileId"],
        "root"
    );
    assert_eq!(k.transport.trace[0]["headers"]["sessionkey"], "web-key");
    let mut k = kernel(vec![
        json!({"sessionKey":"web-key"}),
        json!({"res_code":"0","taskId":"task"}),
    ]);
    let s = scope(&mut k).await;
    let out = k
        .save_and_download(
            s,
            "share",
            item("source", "video.mkv", false),
            |_| async { panic!("rejected save tree") },
            |_| async { panic!("rejected save download") },
        )
        .await
        .unwrap();
    assert_eq!(out.failure, Some(TransferError::Rejected));
    assert_eq!(out.receipt.task_id(), None);
    assert_eq!(k.transport.trace.len(), 2);
}
#[tokio::test]
async fn utf8_form_encoding_matches_frozen_save() {
    let mut k = kernel(vec![json!({"sessionKey":"web-key"}), json!({"res_code":1})]);
    let s = scope(&mut k).await;
    k.save_and_download(
        s,
        "share",
        item("source", "视频 & =.mkv", false),
        |_| async { panic!() },
        |_| async { panic!() },
    )
    .await
    .unwrap();
    let f = &k.transport.trace[1]["body"];
    assert_eq!(*f, fixture()["wire"]["utf8_save_body"]);
}
#[test]
fn exact_original_vm_is_explicit_opt_in_and_never_network() {
    let f = fixture();
    assert_eq!(f["vectors"].as_array().unwrap().len(), 15);
    if std::env::var_os("JXPAN_LIVE_ORACLE").is_some() {
        let output = std::process::Command::new("node")
            .arg("tools/cloud189-transfer-vectors.mjs")
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "original VM failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let oracle: Value = serde_json::from_slice(&output.stdout).unwrap();
        for (actual, expected) in oracle["vectors"]
            .as_array()
            .unwrap()
            .iter()
            .zip(f["vectors"].as_array().unwrap())
        {
            assert_eq!(actual["result"], expected["expected"]);
            assert!(
                actual["trace"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|r| r["url"].as_str().unwrap().starts_with("https://"))
            );
        }
    }
}
