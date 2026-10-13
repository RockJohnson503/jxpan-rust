mod support;

use jxpan_rust::http::params::Params;
use serde_json::json;

#[test]
fn frozen_deployment_get_post_alias_and_directory_parameters_match() {
    let corpus = support::oracle("params", include_str!("fixtures/params.json"));
    let vectors = corpus["vectors"].as_array().unwrap();
    assert_eq!(vectors.len(), 96);
    for vector in vectors {
        let query = vector["query"].as_str().unwrap();
        let mut params = Params::query(Some(query));
        if vector["method"] == "POST" {
            let mut body = url::form_urlencoded::Serializer::new(String::new());
            for entry in vector["form"].as_array().unwrap() {
                body.append_pair(entry[0].as_str().unwrap(), entry[1].as_str().unwrap());
            }
            params.apply_form(body.finish().as_bytes());
        }
        let expected = &vector["expected"];
        for name in ["url", "pwd", "type", "sid", "auth", "action"] {
            assert_eq!(
                params.get(name),
                expected[name].as_str().unwrap_or(""),
                "{name}: {vector}"
            );
        }
        assert_eq!(json!(params.folders()), expected["folders"]);
        assert_eq!(params.file_id(), expected["id"].as_str().unwrap());
    }
}
