use crate::compat::truthy;
use axum::{
    body::{Body, Bytes},
    http::{StatusCode, header},
    response::Response,
};
use serde_json::{Value, json};

#[derive(Clone)]
pub enum ReplyBody {
    Json { bytes: Bytes, retained: usize },
    Redirect(String),
}

/// Serialize a JSON response once; cache hits only clone the immutable buffer.
#[derive(Clone)]
pub struct Reply {
    pub status: StatusCode,
    pub body: ReplyBody,
    pub cacheable: bool,
    pub request_success: bool,
}

impl Reply {
    pub fn json(value: Value) -> Self {
        let cacheable = value["code"] == 200 && value["success"] == true;
        Self::with_status(StatusCode::OK, value, cacheable)
    }

    pub fn fast_error(mut value: Value) -> Self {
        let status = match value["code"].as_u64() {
            Some(400) => StatusCode::BAD_REQUEST,
            Some(401) => StatusCode::UNAUTHORIZED,
            Some(403) => StatusCode::FORBIDDEN,
            Some(404) => StatusCode::NOT_FOUND,
            Some(413) => StatusCode::PAYLOAD_TOO_LARGE,
            _ => StatusCode::BAD_GATEWAY,
        };
        value["code"] = json!(status.as_u16());
        Self::with_status(status, value, false)
    }

    fn with_status(status: StatusCode, value: Value, cacheable: bool) -> Self {
        let bytes = serde_json::to_vec(&value).expect("JSON Value is serializable");
        let retained = bytes.capacity();
        Self {
            request_success: truthy(&value["success"]),
            status,
            body: ReplyBody::Json {
                bytes: bytes.into(),
                retained,
            },
            cacheable,
        }
    }

    pub fn error(status: StatusCode, message: &str) -> Self {
        Self::with_status(
            status,
            json!({"code":status.as_u16(),"msg":message,"success":false,"data":null}),
            false,
        )
    }

    pub fn redirect(url: String) -> Self {
        let valid = url::Url::parse(&url).is_ok_and(|parsed| {
            matches!(parsed.scheme(), "https" | "http")
                && parsed.host_str().is_some()
                && parsed.username().is_empty()
                && parsed.password().is_none()
        }) && header::HeaderValue::from_str(&url).is_ok();
        if !valid {
            return Self::error(StatusCode::BAD_GATEWAY, "上游下载地址无效");
        }
        Self {
            request_success: true,
            status: StatusCode::FOUND,
            body: ReplyBody::Redirect(url),
            cacheable: true,
        }
    }

    /// Budget the retained backing allocation, not only the visible JSON length.
    pub fn retained_bytes(&self) -> Option<usize> {
        self.cacheable.then(|| {
            let bytes = match &self.body {
                ReplyBody::Json { retained, .. } => *retained,
                ReplyBody::Redirect(url) => url.capacity(),
            };
            bytes.saturating_add(std::mem::size_of::<Self>() + 128)
        })
    }

    pub fn response(&self) -> Response {
        let mut response = match &self.body {
            ReplyBody::Json { bytes, .. } => {
                let mut response = Response::new(Body::from(bytes.clone()));
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json; charset=utf-8"),
                );
                response
            }
            ReplyBody::Redirect(url) => {
                let mut response = Response::new(Body::empty());
                response.headers_mut().insert(
                    header::LOCATION,
                    header::HeaderValue::from_str(url).expect("validated redirect header"),
                );
                response.headers_mut().insert(
                    header::CACHE_CONTROL,
                    header::HeaderValue::from_static("no-cache"),
                );
                response
            }
        };
        *response.status_mut() = self.status;
        cors(response)
    }
}

pub fn cors(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        header::HeaderValue::from_static("*"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_failures_keep_http_and_json_codes_in_sync_and_are_not_cached() {
        for (code, expected) in [
            (400, 400),
            (401, 401),
            (403, 403),
            (404, 404),
            (413, 413),
            (502, 502),
            (500, 502),
        ] {
            let reply = Reply::fast_error(
                json!({"code":code,"msg":"synthetic failure","success":false,"data":null}),
            );
            assert_eq!(reply.status.as_u16(), expected);
            assert!(reply.retained_bytes().is_none());
            let ReplyBody::Json { bytes, .. } = &reply.body else {
                panic!("JSON required")
            };
            let value: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(value["code"], expected);
            assert_eq!(reply.response().status().as_u16(), expected);
        }
    }

    #[test]
    fn json_cache_hits_share_the_backing_buffer() {
        let reply = Reply::json(json!({"code":200,"success":true,"data":{"synthetic":"tree"}}));
        let cloned = reply.clone();
        let (ReplyBody::Json { bytes: first, .. }, ReplyBody::Json { bytes: second, .. }) =
            (&reply.body, &cloned.body)
        else {
            panic!("JSON required")
        };
        assert_eq!(first.as_ptr(), second.as_ptr());
        assert!(reply.retained_bytes().unwrap() >= first.len());
    }

    #[test]
    fn unsafe_redirects_are_never_retained_or_returned_as_success() {
        for url in [
            "javascript:alert(1)",
            "/relative",
            "https://user:secret@offline.invalid/",
            "https://offline.invalid/\r\nX-Injected:yes",
        ] {
            let reply = Reply::redirect(url.into());
            assert_eq!(reply.status, StatusCode::BAD_GATEWAY);
            assert!(reply.retained_bytes().is_none());
        }
        let reply = Reply::redirect("https://offline.invalid/synthetic".into());
        assert_eq!(reply.status, StatusCode::FOUND);
        assert_eq!(
            reply.response().headers()[header::LOCATION],
            "https://offline.invalid/synthetic"
        );
    }
}
