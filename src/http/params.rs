use std::collections::HashMap;

#[derive(Default)]
pub struct Params {
    query: HashMap<String, String>,
    form: HashMap<String, String>,
}

impl Params {
    pub fn query(query: Option<&str>) -> Self {
        let mut values = HashMap::new();
        for (key, value) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
            // URLSearchParams.get uses the first occurrence.
            values
                .entry(key.into_owned())
                .or_insert_with(|| value.into_owned());
        }
        Self {
            query: values,
            form: HashMap::new(),
        }
    }

    pub fn apply_form(&mut self, body: &[u8]) {
        for (key, value) in url::form_urlencoded::parse(body) {
            // Preserve the last field, including empty: an empty final form value
            // falls back to the query, not to an earlier duplicate form value.
            self.form.insert(key.into_owned(), value.into_owned());
        }
    }

    pub fn get(&self, name: &str) -> &str {
        self.form
            .get(name)
            .filter(|value| !value.is_empty())
            .or_else(|| self.query.get(name))
            .map_or("", String::as_str)
    }

    pub fn file_id(&self) -> &str {
        let value = if self.get("id").is_empty() {
            self.get("d")
        } else {
            self.get("id")
        };
        value.split("?webpage=").next().unwrap_or("")
    }

    pub fn folders(&self) -> Vec<String> {
        let mut folders = Vec::new();
        for index in 0..=20 {
            let key = if index == 0 {
                "fid".into()
            } else {
                format!("fid{index}")
            };
            let value = self.get(&key);
            if !value.is_empty() && !folders.iter().any(|item| item == value) {
                folders.push(value.to_owned());
            }
        }
        folders
    }
}

#[cfg(test)]
mod tests {
    use super::Params;

    #[test]
    fn preserves_query_first_occurrence_and_form_nonempty_override() {
        let mut params = Params::query(Some("url=first&url=second&pwd=query&id=old&d=alias"));
        params.apply_form(b"pwd=&id=&url=post&url=last");
        assert_eq!(params.get("url"), "last");
        assert_eq!(params.get("pwd"), "query");
        assert_eq!(params.file_id(), "old");
    }

    #[test]
    fn aliases_and_deduplicates_all_twenty_directory_levels() {
        let params = Params::query(Some(
            "d=file%3Fwebpage%3Dtoken&fid=root&fid1=one&fid2=root&fid20=end&fid21=ignored",
        ));
        assert_eq!(params.file_id(), "file");
        assert_eq!(params.folders(), ["root", "one", "end"]);
    }
}
