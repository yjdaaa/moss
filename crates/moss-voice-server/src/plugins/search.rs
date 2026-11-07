//! Tavily search: request only textual top results; never follow result URLs.
use super::http::{self, FetchError};
use reqwest::Method;
use reqwest::blocking::Client;
use serde_json::{Value, json};

pub fn search(client: &Client, base: &str, token: &str, query: &str) -> Result<Value, FetchError> {
    let request = build_request(client, base, token, query)?;
    parse(&http::send_json(client, request)?)
}
fn build_request(
    client: &Client,
    base: &str,
    token: &str,
    query: &str,
) -> Result<reqwest::blocking::Request, FetchError> {
    if query.trim().is_empty()
        || !(1..=500).contains(&query.chars().count())
        || token.trim().is_empty()
    {
        return Err(FetchError::InvalidResponse);
    }
    let mut url = http::validate_endpoint(base, false)?;
    url.set_path("/search");
    url.set_query(None);
    let request =
        json!({"query":query,"search_depth":"basic","max_results":5,"include_raw_content":false});
    let bearer = format!("Bearer {token}");
    http::build_json_request(
        client,
        Method::POST,
        &url,
        Some(("Authorization", &bearer)),
        Some(request),
    )
}
pub fn parse(value: &Value) -> Result<Value, FetchError> {
    let rows = value["results"]
        .as_array()
        .ok_or(FetchError::InvalidResponse)?;
    let rows=rows.iter().take(5).map(|v| {
        let title=v["title"].as_str().filter(|s| !s.trim().is_empty()).ok_or(FetchError::InvalidResponse)?;
        let target=v["url"].as_str().ok_or(FetchError::InvalidResponse)?;
        let url=reqwest::Url::parse(target).map_err(|_|FetchError::InvalidResponse)?;
        if !matches!(url.scheme(),"http"|"https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
            return Err(FetchError::InvalidResponse);
        }
        let content=v["content"].as_str().ok_or(FetchError::InvalidResponse)?;
        Ok(json!({"title":title,"url":target,"content":content.chars().take(500).collect::<String>()}))
    }).collect::<Result<Vec<_>,FetchError>>()?;
    Ok(json!({"results":rows}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parse_results_does_not_infer_missing_sources() {
        let v=parse(&json!({"results":[{"title":"标题","url":"https://site.example/a","content":"测试内容"}]})).unwrap();
        assert_eq!(v["results"][0]["url"], "https://site.example/a");
        assert!(parse(&json!({"answer":"foo"})).is_err());
    }
    #[test]
    fn request_contract_and_malformed_rows_fail_closed() {
        let request = build_request(
            &http::client().unwrap(),
            "https://api.tavily.com/ignored?x=1",
            "fixture-token",
            "问题",
        )
        .unwrap();
        assert_eq!(request.method(), Method::POST);
        assert_eq!(request.url().as_str(), "https://api.tavily.com/search");
        assert_eq!(request.headers()["Authorization"], "Bearer fixture-token");
        let body: Value =
            serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(
            body,
            json!({"query":"问题","search_depth":"basic","max_results":5,"include_raw_content":false})
        );
        for row in [
            json!({"title":"t","content":"c"}),
            json!({"title":"t","url":"file:///private","content":"c"}),
            json!({"title":"t","url":"https://user:key@example.org","content":"c"}),
            json!({"title":"t","url":"https://example.org","content":{}}),
        ] {
            assert_eq!(
                parse(&json!({"results":[row]})).unwrap_err(),
                FetchError::InvalidResponse
            );
        }
        assert_eq!(
            parse(&json!({"results":[]})).unwrap(),
            json!({"results":[]})
        );
    }
}
