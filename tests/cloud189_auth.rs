mod support;
use jxpan_rust::providers::cloud189_auth::{
    self as auth, AuthError, AuthKernel, Request, Response, Runtime, Transport,
};
use serde_json::{Value, json};
use std::collections::VecDeque;
const EPOCH: u64 = 1_700_000_000_123;
struct Fixed;
impl Runtime for Fixed {
    fn now_ms(&self) -> u64 {
        EPOCH
    }
    fn random(&mut self) -> f64 {
        0.25
    }
    fn uuid(&mut self) -> String {
        "44444444-4444-4444-8444-444444444444".into()
    }
}
struct Offline {
    steps: VecDeque<Value>,
    trace: Vec<Value>,
}
impl Transport for Offline {
    async fn fetch(&mut self, r: Request) -> Result<Response, AuthError> {
        let headers: serde_json::Map<String, Value> =
            r.headers.into_iter().map(|(k, v)| (k, json!(v))).collect();
        self.trace.push(json!({"url":r.url,"method":r.method,"headers":headers,"body":r.body,"redirect":r.redirect}));
        let s = self.steps.pop_front().expect("unexpected offline fetch");
        if s["fail"] == true {
            return Err(AuthError::Transport);
        }
        let mut h = s["cookies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| ("set-cookie".into(), v.as_str().unwrap().into()))
            .collect::<Vec<_>>();
        if let Some(location) = s["location"].as_str() {
            h.push(("location".into(), location.into()));
        }
        Ok(Response {
            status: s["status"].as_u64().unwrap() as u16,
            headers: h,
            text: s["text"].as_str().unwrap().into(),
        })
    }
}
fn kernel(steps: Vec<Value>) -> AuthKernel<Offline, Fixed> {
    AuthKernel {
        transport: Offline {
            steps: steps.into(),
            trace: vec![],
        },
        runtime: Fixed,
    }
}
#[tokio::test]
async fn frozen_login_and_renewal_offline_differential() {
    let corpus = support::oracle("cloud189-auth", include_str!("fixtures/cloud189-auth.json"));
    assert_eq!(
        corpus["source_sha256"],
        "47e2a84566ffa338a420113818a4bb629d3a43f4290dd65547c52b4cfe23f118"
    );
    for v in corpus["vectors"].as_array().unwrap() {
        let name = v["name"].as_str().unwrap();
        let input = &v["input"];
        let mut k = kernel(v["steps"].as_array().unwrap().clone());
        let result = match v["op"].as_str().unwrap() {
            "init" => k.init_qr_login().await,
            "complete" => k.check_and_login(input).await,
            "poll" => k.check_qr_status(input).await,
            "exchange" => k.exchange_session(input.as_str().unwrap()).await,
            "open" => {
                k.get_open_access_token(
                    input["key"].as_str().unwrap(),
                    input["cookies"].as_str().unwrap(),
                )
                .await
            }
            "cookie" => k.get_cookie_login_user(input.as_str().unwrap()).await,
            "refresh" => k
                .refresh_on_demand(input)
                .await
                .map(|v| v.unwrap_or(Value::Null)),
            "status" => Ok(auth::login_status(
                if input.is_null() { None } else { Some(input) },
                EPOCH,
            )),
            "state" => Ok(auth::poll_state(input)),
            _ => panic!("unexpected operation"),
        };
        if let Some(error) = v["error"].as_str() {
            assert_eq!(format!("{:?}", result.unwrap_err()), error, "{name}");
        } else {
            assert_eq!(
                result.unwrap_or_else(|e| panic!("{name}: {e}")),
                v["expected"],
                "result: {name}"
            );
        }
        assert_eq!(
            json!(k.transport.trace),
            v["trace"],
            "request trace: {name}"
        );
        assert!(k.transport.steps.is_empty(), "unconsumed responses: {name}");
    }
}
#[tokio::test]
async fn unsafe_redirects_never_emit_credentials() {
    for url in [
        "http://cloud.189.cn/callback",
        "https://cloud.189.cn.evil.invalid/",
        "https://cloud.189.cn@evil.invalid/",
        "https://127.0.0.1/",
        "https://cloud.189.cn:444/",
    ] {
        let mut k = kernel(vec![]);
        assert_eq!(
            k.exchange_session(url).await.unwrap_err(),
            AuthError::UnsafeUrl
        );
        assert!(k.transport.trace.is_empty());
    }
    let mut k = kernel(vec![
        json!({"text":"","status":302,"cookies":["SECRET=synthetic"],"location":"https://evil.invalid/callback"}),
    ]);
    assert_eq!(k.init_qr_login().await.unwrap_err(), AuthError::UnsafeUrl);
    assert_eq!(k.transport.trace.len(), 1);
}
#[tokio::test]
async fn cookie_redirect_scope_and_renewal_fail_closed() {
    let mut k = kernel(vec![
        json!({"text":"","status":302,"cookies":["CLOUD=synthetic"],"location":"https://open.e.189.cn/next"}),
        json!({"text":"","status":200,"cookies":["COOKIE_LOGIN_USER=user=="],"location":null}),
    ]);
    let result = k.get_cookie_login_user("").await.unwrap();
    assert_eq!(result["cookieLoginUser"], "user==");
    assert!(k.transport.trace[1]["headers"].get("cookie").is_none());
    let input = json!({"app_access_token":"synthetic","session_key":"old","session_secret":"old","custom":1});
    let mut k = kernel(vec![
        json!({"text":"not-json","status":200,"cookies":[],"location":null}),
    ]);
    assert_eq!(
        k.refresh_on_demand(&input).await.unwrap_err(),
        AuthError::InvalidJson
    );
    assert_eq!(input["session_key"], "old");
}
#[test]
fn strict_login_validation_and_local_status_boundary() {
    let valid = json!({"session_key":"key","session_secret":"secret","app_access_token":"token","login_time":1,"expires_at":EPOCH/1000});
    auth::validate_login_info(&valid).unwrap();
    assert_eq!(
        auth::login_status(Some(&valid), EPOCH)["data"]["logged_in"],
        true
    );
    assert_eq!(
        auth::login_status(Some(&valid), EPOCH + 1000)["data"]["logged_in"],
        false
    );
    for input in [
        json!(null),
        json!({}),
        json!({"app_access_token":"bad\r\nheader","login_time":1,"expires_at":2}),
        json!({"app_access_token":"token","session_key":"key","login_time":1,"expires_at":2}),
        json!({"app_access_token":123,"login_time":1,"expires_at":2}),
    ] {
        assert_eq!(
            auth::validate_login_info(&input),
            Err(AuthError::InvalidLoginInfo)
        );
    }
}
