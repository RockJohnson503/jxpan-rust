use capability::{
    Error, OAuthCredentials, Options, OwnedClient, Request, Response, Result, Transport,
};
use jxpan_rust::providers::{self, cloud189_transfer_client as capability};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

const NAME: &str = "jxpan-transfer-12345678-1234-4234-8234-123456789abc";
// All credentials, IDs, bodies and URLs in this file are synthetic.
struct Memory {
    replies: VecDeque<Result<Response>>,
    trace: Arc<Mutex<Vec<Request>>>,
}
impl Transport for Memory {
    async fn fetch(&mut self, request: Request) -> Result<Response> {
        self.trace.lock().unwrap().push(request);
        self.replies
            .pop_front()
            .expect("unexpected synthetic request")
    }
}
fn reply(body: impl Into<String>) -> Result<Response> {
    Ok(Response {
        status: 200,
        body: body.into().into_bytes(),
    })
}
fn page(count: usize, rows: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><listFiles><lastRev>42</lastRev><fileList><count>{count}</count>{rows}</fileList></listFiles>"
    )
}
fn dir(id: usize, name: &str) -> String {
    format!("<folder><id>{id}</id><name>{name}</name><rev>42</rev></folder>")
}
fn file(id: usize) -> String {
    format!("<file><id>{id}</id><name>synthetic-{id}.mkv</name><size>64</size></file>")
}
fn new(
    replies: Vec<Result<Response>>,
    options: Options,
) -> (OwnedClient<Memory>, Arc<Mutex<Vec<Request>>>) {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let client = OwnedClient::new(
        Memory {
            replies: replies.into(),
            trace: trace.clone(),
        },
        OAuthCredentials::new(
            "synthetic-oauth-token".into(),
            "synthetic-app-secret".into(),
        ),
        options,
        "1234".into(),
    )
    .unwrap();
    (client, trace)
}
fn on() -> Options {
    Options {
        enable_owned_read: true,
        enable_provision: true,
    }
}
fn provision_prefix() -> Vec<Result<Response>> {
    vec![
        reply(page(0, "")),
        reply(dir(5678, NAME)),
        reply(page(1, &dir(5678, NAME))),
        reply(page(0, "")),
    ]
}

#[tokio::test]
async fn creates_once_then_attests_owned_id_parent_name_and_empty() {
    let (mut client, trace) = new(provision_prefix(), on());
    assert_eq!(
        client.provision_new_empty("1234", NAME).await,
        Ok("5678".into())
    );
    let trace = trace.lock().unwrap();
    assert_eq!(trace.len(), 4);
    assert!(trace[0].url.contains("folderId=1234"));
    assert_eq!(trace[1].url, "https://api.cloud.189.cn/createFolder.action");
    assert_eq!(trace[1].method, "POST");
    let form: Vec<_> = url::form_urlencoded::parse(trace[1].body.as_ref().unwrap().as_bytes())
        .into_owned()
        .collect();
    assert_eq!(
        form,
        vec![
            ("parentFolderId".into(), "1234".into()),
            ("folderName".into(), NAME.into())
        ]
    );
    assert!(trace[3].url.contains("folderId=5678"));
    for request in trace.iter() {
        let header = |name: &str| {
            request
                .headers
                .iter()
                .find(|(k, _)| k == name)
                .unwrap()
                .1
                .as_str()
        };
        let path = url::Url::parse(&request.url).unwrap().path().to_owned();
        let expected_text = format!(
            "AccessToken=synthetic-oauth-token&Operate={}&RequestURI={path}&Date={}",
            request.method,
            header("Date")
        );
        assert_eq!(
            capability::oauth_signing_text(
                header("AccessToken"),
                request.method,
                &path,
                header("Date")
            ),
            expected_text
        );
        assert_eq!(
            header("Signature"),
            providers::cloud189::hmac_sha1("synthetic-app-secret", &expected_text).unwrap()
        );
        assert!(
            request
                .headers
                .iter()
                .all(|(k, _)| !matches!(k.to_ascii_lowercase().as_str(), "cookie" | "sessionkey"))
        );
    }
}

#[tokio::test]
async fn default_off_invalid_roots_wrong_root_and_bad_nonce_never_call_transport() {
    let (mut client, trace) = new(vec![], Options::default());
    assert_eq!(
        client.list_authorized_root().await.unwrap_err(),
        Error::Disabled
    );
    assert_eq!(
        client.provision_new_empty("1234", NAME).await.unwrap_err(),
        Error::Disabled
    );
    for root in ["-11", "-12", "0", "", "01234", "share-id", "1/2"] {
        let transport = Memory {
            replies: VecDeque::new(),
            trace: trace.clone(),
        };
        assert!(matches!(
            OwnedClient::new(
                transport,
                OAuthCredentials::new("x".into(), "y".into()),
                on(),
                root.into()
            ),
            Err(Error::UnauthorizedScope)
        ));
    }
    let (mut client, trace2) = new(vec![], on());
    assert_eq!(
        client.provision_new_empty("9999", NAME).await.unwrap_err(),
        Error::UnauthorizedScope
    );
    assert_eq!(
        client
            .provision_new_empty("1234", "not-a-uuid")
            .await
            .unwrap_err(),
        Error::UnauthorizedScope
    );
    assert_eq!(
        client.walk_owned("5678").await.unwrap_err(),
        Error::UnauthorizedScope
    );
    assert!(trace.lock().unwrap().is_empty() && trace2.lock().unwrap().is_empty());
}

#[tokio::test]
async fn collision_is_not_find_or_reuse() {
    let (mut client, trace) = new(vec![reply(page(1, &dir(5678, NAME)))], on());
    assert_eq!(
        client.provision_new_empty("1234", NAME).await.unwrap_err(),
        Error::ExistingName
    );
    assert_eq!(
        client.provision_new_empty("1234", NAME).await.unwrap_err(),
        Error::ExistingName
    );
    assert_eq!(trace.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn ambiguous_post_is_not_retried_and_no_cleanup() {
    for response in [
        Err(Error::Transport),
        reply("<error><code>failed</code></error>"),
        Ok(Response {
            status: 302,
            body: vec![],
        }),
    ] {
        let (mut client, trace) = new(vec![reply(page(0, "")), response], on());
        assert!(client.provision_new_empty("1234", NAME).await.is_err());
        assert_eq!(
            client.provision_new_empty("1234", NAME).await.unwrap_err(),
            Error::ExistingName
        );
        let trace = trace.lock().unwrap();
        assert_eq!(trace.len(), 2);
        assert_eq!(trace.iter().filter(|r| r.method == "POST").count(), 1);
        assert!(trace.iter().all(|r| !r.url.contains("Delete")));
    }
}

#[tokio::test]
async fn parent_mismatch_wrong_name_wrong_id_nonempty_fail() {
    for (after, child) in [
        (page(1, &dir(9999, NAME)), None),
        (page(1, &dir(5678, "different")), None),
        (
            page(
                1,
                &format!(
                    "<folder><id>5678</id><name>{NAME}</name><parentFolderId>9999</parentFolderId></folder>"
                ),
            ),
            None,
        ),
        (page(1, &dir(5678, NAME)), Some(page(1, &file(7000)))),
    ] {
        let mut replies = vec![reply(page(0, "")), reply(dir(5678, NAME)), reply(after)];
        if let Some(body) = child {
            replies.push(reply(body));
        }
        let (mut client, _) = new(replies, on());
        assert!(client.provision_new_empty("1234", NAME).await.is_err());
        assert_eq!(
            client.walk_owned("5678").await.unwrap_err(),
            Error::UnauthorizedScope
        );
    }
}

#[tokio::test]
async fn complete_pagination_uses_official_total_and_pages_start_at_one() {
    let first = (2000..2100).map(file).collect::<String>();
    let (mut client, trace) = new(
        vec![reply(page(101, &first)), reply(page(101, &file(2100)))],
        on(),
    );
    let rows = client.list_authorized_root().await.unwrap();
    assert_eq!(rows.len(), 101);
    assert_eq!(rows[100]["fileId"], "2100");
    let trace = trace.lock().unwrap();
    assert_eq!(trace.len(), 2);
    assert!(trace[0].url.contains("pageNum=1&pageSize=100"));
    assert!(trace[1].url.contains("pageNum=2&pageSize=100"));
    assert!(trace[0].url.contains("fileType=0&mediaType=0"));
}

#[tokio::test]
async fn changed_total_revision_repeat_short_page_and_limits_fail_closed() {
    let first = (2000..2100).map(file).collect::<String>();
    for second in [
        page(102, &file(2100)),
        page(101, &file(2000)),
        page(101, ""),
        page(101, &file(2100)).replace("<lastRev>42", "<lastRev>43"),
    ] {
        let (mut client, trace) = new(vec![reply(page(101, &first)), reply(second)], on());
        assert!(client.list_authorized_root().await.is_err());
        assert_eq!(trace.lock().unwrap().len(), 2);
    }
    let (mut client, _) = new(vec![reply(page(1001, ""))], on());
    assert_eq!(
        client.list_authorized_root().await.unwrap_err(),
        Error::Limit
    );
    let (mut client, _) = new(vec![reply(page(2, &file(2000)))], on());
    assert_eq!(
        client.list_authorized_root().await.unwrap_err(),
        Error::InvalidResponse
    );
}

#[tokio::test]
async fn http_body_xml_errors_and_unknown_schema_are_not_empty_success() {
    let fixtures = [
        "<listFiles><lastRev>42</lastRev><fileList><count>0</count><error>bad</error></fileList></listFiles>",
        "<listFiles><lastRev>42</lastRev><fileList><count>0</count><count>0</count></fileList></listFiles>",
        "<!DOCTYPE listFiles [<!ENTITY evil SYSTEM 'file:///synthetic'>]><listFiles/>",
        "<listFiles xmlns='synthetic'><fileList/></listFiles>",
        "<listFiles><lastRev>42</lastRev><fileList><count>0</count></listFiles>",
        "<listFiles/ ><listFiles/>",
        "{\"res_code\":0}",
    ];
    for body in fixtures {
        let (mut client, _) = new(vec![reply(body)], on());
        assert!(client.list_authorized_root().await.is_err());
    }
    let (mut client, _) = new(
        vec![Ok(Response {
            status: 503,
            body: page(0, "").into_bytes(),
        })],
        on(),
    );
    assert_eq!(
        client.list_authorized_root().await.unwrap_err(),
        Error::Http
    );
    let (mut client, _) = new(
        vec![Ok(Response {
            status: 200,
            body: vec![b' '; capability::MAX_BODY + 1],
        })],
        on(),
    );
    assert_eq!(
        client.list_authorized_root().await.unwrap_err(),
        Error::Oversized
    );
}

#[tokio::test]
async fn walk_propagates_listing_error_instead_of_swallowing_it() {
    let mut replies = provision_prefix();
    replies.extend([reply(page(1, &dir(7000, "season"))), Err(Error::Transport)]);
    let (mut client, trace) = new(replies, on());
    client.provision_new_empty("1234", NAME).await.unwrap();
    assert_eq!(
        client.walk_owned("5678").await.unwrap_err(),
        Error::Transport
    );
    assert_eq!(trace.lock().unwrap().len(), 6);
}

#[tokio::test]
async fn walk_complete_normalizes_owned_parent_and_rejects_cycles() {
    let mut replies = provision_prefix();
    replies.extend([
        reply(page(2, &(dir(7000, "season") + &file(7001)))),
        reply(page(1, &file(7002))),
    ]);
    let (mut client, _) = new(replies, on());
    client.provision_new_empty("1234", NAME).await.unwrap();
    let walk = client.walk_owned("5678").await.unwrap();
    assert_eq!(walk.nodes.len(), 2);
    assert_eq!(walk.all_files.len(), 2);
    assert_eq!(walk.all_files[1]["parentId"], "7000");
    let mut replies = provision_prefix();
    replies.extend([
        reply(page(1, &dir(7000, "season"))),
        reply(page(1, &dir(5678, NAME))),
    ]);
    let (mut client, _) = new(replies, on());
    client.provision_new_empty("1234", NAME).await.unwrap();
    assert_eq!(
        client.walk_owned("5678").await.unwrap_err(),
        Error::RepeatedResource
    );
}

#[tokio::test]
async fn node_admission_and_global_resource_budgets_do_not_truncate_success() {
    // Root + 400 directories exceeds max_nodes=400 before walker can truncate.
    let mut replies = provision_prefix();
    for start in [2000, 2100, 2200, 2300] {
        let rows = (start..start + 100)
            .map(|id| dir(id, "synthetic-dir"))
            .collect::<String>();
        replies.push(reply(page(400, &rows)));
    }
    let (mut client, trace) = new(replies, on());
    client.provision_new_empty("1234", NAME).await.unwrap();
    assert_eq!(client.walk_owned("5678").await.unwrap_err(), Error::Limit);
    assert_eq!(trace.lock().unwrap().len(), 8);
    // Root contains 1000 resources; child's extra file exceeds global budget.
    let mut replies = provision_prefix();
    for start in (2000..3000).step_by(100) {
        let rows = (start..start + 100)
            .map(|id| {
                if id == 2000 {
                    dir(id, "child")
                } else {
                    file(id)
                }
            })
            .collect::<String>();
        replies.push(reply(page(1000, &rows)));
    }
    replies.push(reply(page(1, &file(4000))));
    let (mut client, _) = new(replies, on());
    client.provision_new_empty("1234", NAME).await.unwrap();
    assert_eq!(client.walk_owned("5678").await.unwrap_err(), Error::Limit);
}

#[test]
fn pinned_target_has_no_redirect_or_nonofficial_escape() {
    for (url, method) in [
        ("http://api.cloud.189.cn/listFiles.action", "GET"),
        ("https://cloud.189.cn/listFiles.action", "GET"),
        ("https://api.cloud.189.cn.evil.test/listFiles.action", "GET"),
        (
            "https://synthetic:synthetic@api.cloud.189.cn/listFiles.action",
            "GET",
        ),
        ("https://api.cloud.189.cn:8443/listFiles.action", "GET"),
        ("https://api.cloud.189.cn/listFiles.action#escape", "GET"),
        ("https://api.cloud.189.cn/deleteFolder.action", "POST"),
        ("https://api.cloud.189.cn/createFolder.action", "GET"),
    ] {
        let request = Request {
            url: url.into(),
            method,
            headers: vec![],
            body: None,
        };
        assert_eq!(
            capability::validate_target(&request),
            Err(Error::UnauthorizedScope)
        );
    }
    assert!(capability::ReqwestTransport::new().is_ok()); // builds only; no HTTP.
}

#[tokio::test]
async fn independent_write_switch_and_missing_credentials_are_fail_closed() {
    let (mut client, trace) = new(
        vec![reply(page(0, ""))],
        Options {
            enable_owned_read: true,
            enable_provision: false,
        },
    );
    assert_eq!(
        client.provision_new_empty("1234", NAME).await.unwrap_err(),
        Error::Disabled
    );
    assert_eq!(client.list_authorized_root().await.unwrap().len(), 0);
    assert_eq!(trace.lock().unwrap().len(), 1);
    let transport = Memory {
        replies: VecDeque::new(),
        trace: trace.clone(),
    };
    let mut client = OwnedClient::new(
        transport,
        OAuthCredentials::new(String::new(), "synthetic".into()),
        on(),
        "1234".into(),
    )
    .unwrap();
    assert_eq!(
        client.list_authorized_root().await.unwrap_err(),
        Error::MissingCredentials
    );
    assert_eq!(trace.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn xml_escaping_preserves_names_and_rejects_custom_entities() {
    let escaped =
        "<file><id>2000</id><name>电影&amp;&#x4E2D;&#25991;.mkv</name><size>64</size></file>";
    let (mut client, _) = new(vec![reply(page(1, escaped))], on());
    let rows = client.list_authorized_root().await.unwrap();
    assert_eq!(rows[0]["fileName"], "电影&中文.mkv");
    let (mut client, _) = new(
        vec![reply(page(1, &escaped.replace("&amp;", "&unknown;")))],
        on(),
    );
    assert_eq!(
        client.list_authorized_root().await.unwrap_err(),
        Error::UnsupportedXml
    );
}

#[tokio::test]
async fn exact_node_budget_traverses_all_nodes_without_false_truncation() {
    let mut replies = provision_prefix();
    for start in [2000, 2100, 2200, 2300] {
        let rows = (start..(start + 100).min(2399))
            .map(|id| dir(id, "synthetic-dir"))
            .collect::<String>();
        replies.push(reply(page(399, &rows)));
    }
    replies.extend((0..399).map(|_| reply(page(0, ""))));
    let (mut client, trace) = new(replies, on());
    client.provision_new_empty("1234", NAME).await.unwrap();
    let walk = client.walk_owned("5678").await.unwrap();
    assert_eq!(walk.nodes.len(), 400);
    assert_eq!(walk.visited, 400);
    assert_eq!(walk.all_folders.len(), 399);
    assert_eq!(trace.lock().unwrap().len(), 407);
}
