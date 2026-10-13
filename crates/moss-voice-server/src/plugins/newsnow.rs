//! NewsNow, verified /api/s?id=source-id contract. No arbitrary model URLs.
use reqwest::blocking::Client;
use reqwest::Method;
use serde_json::{json,Value};
use super::http::{self,FetchError};

pub fn fetch(client:&Client,base:&str,source_id:&str,limit:usize)->Result<Value,FetchError>{
    if !(1..=5).contains(&limit) || source_id.is_empty() || source_id.len()>80
       || !source_id.chars().all(|c|c.is_ascii_alphanumeric()||c=='-'||c=='_'){
        return Err(FetchError::InvalidResponse);
    }
    let mut url=http::validate_endpoint(base,true)?;
    if url.query().is_some(){return Err(FetchError::InvalidUrl);}
    url.set_path("/api/s");
    url.query_pairs_mut().append_pair("id",source_id);
    let response=http::request_json(client,Method::GET,&url,None,None)?;
    parse(&response,source_id,limit)
}

pub fn parse(value:&Value,source_id:&str,limit:usize)->Result<Value,FetchError>{
    let timestamp = &value["updatedTime"];
    // SourceResponse explicitly permits number|string. Preserve the provider's
    // value; do not relabel it as our fetch time or invent a unit conversion.
    if !(1..=5).contains(&limit) || source_id.is_empty()
        || !(timestamp.as_u64().is_some() || timestamp.as_str().is_some_and(|s|!s.trim().is_empty() && s.len()<=128)) {
        return Err(FetchError::InvalidResponse);
    }
    if !matches!(value["status"].as_str(),Some("success"|"cache")) || value["id"].as_str()!=Some(source_id){
        return Err(FetchError::InvalidResponse);
    }
    let items=value["items"].as_array().ok_or(FetchError::InvalidResponse)?;
    let mut rows=Vec::new();
    for item in items.iter().take(limit){
        let title=item["title"].as_str().ok_or(FetchError::InvalidResponse)?;
        let url=item["url"].as_str().ok_or(FetchError::InvalidResponse)?;
        if title.trim().is_empty() || http::validate_endpoint(url,false).is_err(){return Err(FetchError::InvalidResponse);}
        if !item["pubDate"].is_null() && !item["pubDate"].is_string() && !item["pubDate"].is_number() {
            return Err(FetchError::InvalidResponse);
        }
        rows.push(json!({"title":title.chars().take(160).collect::<String>(),"url":url,
          "published":item["pubDate"],"description":Value::Null}));
    }
    Ok(json!({"items":rows,"updated_time":value["updatedTime"],"status":value["status"]}))
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn response_schema_and_errors(){
        let valid=json!({"status":"cache","id":"weibo","updatedTime":1720000000000_i64,
            "items":[{"id":"123","title":"标题","url":"https://news.example.org/one"}]});
        let response=parse(&valid,"weibo",3).unwrap();
        assert_eq!(response["items"][0]["title"],"标题");
        assert_eq!(response["status"],"cache");
        assert!(parse(&json!({"status":"error","id":"weibo","items":[]}),"weibo",3).is_err());
        assert!(parse(&json!({"status":"success","id":"other","items":[]}),"weibo",3).is_err());
        assert!(parse(&json!({"status":"success","id":"weibo","items":[{"title":"x"}]}),"weibo",3).is_err());
    }
}
