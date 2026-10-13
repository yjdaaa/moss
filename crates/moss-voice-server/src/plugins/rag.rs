//! RAGFlow retrieval: dataset IDs belong to the desktop configuration, not model arguments.
use reqwest::{Method};
use reqwest::blocking::Client;
use serde_json::{json,Value};
use super::http::{self,FetchError};
pub fn retrieve(client:&Client,base:&str,token:&str,datasets:&[String],question:&str)->Result<Value,FetchError>{
    let request=build_request(client,base,token,datasets,question)?;
    let parsed = parse(&http::send_json(client,request)?)?;
    // The configured datasets are also the response provenance boundary.
    if parsed["chunks"].as_array().unwrap().iter().any(|row| {
        !datasets.iter().any(|id| row["dataset_id"].as_str() == Some(id.as_str()))
    }) { return Err(FetchError::InvalidResponse); }
    Ok(parsed)
}
fn build_request(client:&Client,base:&str,token:&str,datasets:&[String],question:&str)->Result<reqwest::blocking::Request,FetchError>{
    if question.trim().is_empty() || !(1..=500).contains(&question.chars().count()) || datasets.is_empty() || datasets.len()>20 || datasets.iter().any(|s|s.trim().is_empty()||s.len()>128) || token.trim().is_empty() {
        return Err(FetchError::InvalidResponse);
    }
    let mut url=http::validate_endpoint(base,true)?;
    url.set_path("/api/v1/retrieval");
    url.set_query(None);
    let bearer=format!("Bearer {token}");
    http::build_json_request(client,Method::POST,&url,Some(("Authorization",&bearer)),Some(json!({"question":question,"dataset_ids":datasets,"page":1,"page_size":5})))
}
pub fn parse(value:&Value)->Result<Value,FetchError>{
    if value["code"].as_i64()!=Some(0){return Err(FetchError::InvalidResponse)}
    let rows=value["data"]["chunks"].as_array().ok_or(FetchError::InvalidResponse)?;
    let rows=rows.iter().take(5).map(|v|{
        let content=v["content"].as_str().ok_or(FetchError::InvalidResponse)?;
        for key in ["document_id","dataset_id"] {
            if v[key].as_str().is_none_or(|s|s.trim().is_empty()) {return Err(FetchError::InvalidResponse);}
        }
        Ok(json!({"content":content.chars().take(500).collect::<String>(),"document_id":v["document_id"],"dataset_id":v["dataset_id"]}))
    }).collect::<Result<Vec<_>,FetchError>>()?;
    Ok(json!({"chunks":rows}))
}
#[cfg(test)] mod tests {use super::*;#[test]fn error_code_is_not_empty_retrieval(){
    assert!(parse(&json!({"code":102,"data":{"chunks":[]}})).is_err());
    assert_eq!(parse(&json!({"code":0,"data":{"chunks":[]}})).unwrap()["chunks"].as_array().unwrap().len(),0);
}
#[test]fn request_uses_configured_datasets_and_rejects_malformed_chunks(){
    let request=build_request(&http::client().unwrap(),"http://127.0.0.1:8080/ignored","fixture-token",&["allowed".into()],"问题").unwrap();
    assert_eq!(request.url().path(),"/api/v1/retrieval");
    assert_eq!(request.headers()["Authorization"],"Bearer fixture-token");
    let body:Value=serde_json::from_slice(request.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(body,json!({"question":"问题","dataset_ids":["allowed"],"page":1,"page_size":5}));
    for chunk in [json!({"document_id":"a"}),json!({"content":"c","dataset_id":{}})] {
        assert_eq!(parse(&json!({"code":0,"data":{"chunks":[chunk]}})).unwrap_err(),FetchError::InvalidResponse);
    }
}}
