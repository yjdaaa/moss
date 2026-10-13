//! Metaso official SDK search/v2 contract; non-streaming, no result URL fetches.
//! Kept separate from Tavily so credentials never cross provider namespaces.
use reqwest::{Method,blocking::Client};
use serde_json::{Value,json};
use super::http::{self,FetchError};

pub fn search(client:&Client,base:&str,token:&str,query:&str)->Result<Value,FetchError>{
    let request=build_request(client,base,token,query)?;
    parse(&http::send_json(client,request)?)
}
fn build_request(client:&Client,base:&str,token:&str,query:&str)->Result<reqwest::blocking::Request,FetchError>{
    if query.trim().is_empty() || !(1..=500).contains(&query.chars().count()) || token.trim().is_empty(){return Err(FetchError::InvalidResponse);}
    let mut url=http::validate_endpoint(base,false)?;
    url.set_path("/api/open/search/v2"); url.set_query(None);
    http::build_json_request(client,Method::POST,&url,Some(("Authorization",&format!("Bearer {token}"))),
        Some(json!({"question":query,"lang":"zh","stream":false,"enableMix":false,"newEngine":false})))
}
pub fn parse(value:&Value)->Result<Value,FetchError>{
    // Metaso can return application errors in an HTTP200 JSON envelope.
    // Never expose errMsg or mistake exhausted credits for an empty search.
    match value["errCode"].as_i64() {
        Some(0)=>{},
        Some(401|403)=>return Err(FetchError::Unauthorized),
        Some(4002)=>return Err(FetchError::QuotaExceeded),
        Some(400|4009)=>return Err(FetchError::ProviderRejected),
        Some(500|-500)=>return Err(FetchError::Network),
        _=>return Err(FetchError::InvalidResponse),
    }
    let text=value["data"]["text"].as_str().filter(|s|!s.trim().is_empty()).ok_or(FetchError::InvalidResponse)?;
    let references=value["data"]["references"].as_array().ok_or(FetchError::InvalidResponse)?;
    // The SDK may include uploaded-file metadata and relative preview links.
    // Only expose public web citations; never emit arbitrary nested metadata.
    let sources=references.iter().filter_map(|row|{
        let title=row["title"].as_str()?;
        let target=row["url"].as_str()?;
        let url=reqwest::Url::parse(target).ok()?;
        if !matches!(url.scheme(),"http"|"https") || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some(){return None;}
        Some(json!({"title":title.chars().take(200).collect::<String>(),"url":target}))
    }).take(5).collect::<Vec<_>>();
    Ok(json!({"answer":text.chars().take(1200).collect::<String>(),"sources":sources,"untrusted_external_text":true}))
}
#[cfg(test)] mod tests{
    use super::*;
    #[test]fn pinned_sdk_request_contract(){
        let request=build_request(&http::client().unwrap(),"https://metaso.cn/ignored?x=1","fixture-token","查询").unwrap();
        assert_eq!(request.method(),Method::POST);
        assert_eq!(request.url().as_str(),"https://metaso.cn/api/open/search/v2");
        assert_eq!(request.headers()["Authorization"],"Bearer fixture-token");
        let body:Value=serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(body,json!({"question":"查询","lang":"zh","stream":false,"enableMix":false,"newEngine":false}));
    }
    #[test]fn response_rejects_errors_and_does_not_expose_file_metadata(){
        let parsed=parse(&json!({"errCode":0,"data":{"text":"回答","references":[
            {"title":"来源","url":"https://example.org/a"},
            {"title":"私有文件","url":"/preview/a","file_meta":{"token":"must-not-leak"}},
            {"title":"注入","url":"javascript:alert(1)"}
        ],"balance":99,"sessionId":"private"}})).unwrap();
        assert_eq!(parsed["sources"].as_array().unwrap().len(),1);
        assert!(!parsed.to_string().contains("must-not-leak"));
        assert!(!parsed.to_string().contains("sessionId"));
        for invalid in [json!({"errCode":1,"data":{"text":"error","references":[]}}),json!({"errCode":0,"data":{"text":"","references":[]}}),json!({"errCode":0,"data":{"text":"answer"}})] {
            assert_eq!(parse(&invalid).unwrap_err(),FetchError::InvalidResponse);
        }
    }
}
