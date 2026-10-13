//! Weather adapter for QWeather current conditions API.
use reqwest::Method;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use super::http::{self, FetchError};

pub fn current(client: &Client, base: &str, key: &str, auth_kind: &str, lat: f64, lon: f64) -> Result<Value, FetchError> {
    let request=build_request(client,base,key,auth_kind,lat,lon)?;
    parse(&http::send_json(client,request)?)
}
fn build_request(client: &Client, base: &str, key: &str, auth_kind: &str, lat: f64, lon: f64) -> Result<reqwest::blocking::Request, FetchError> {
    if !lat.is_finite() || !lon.is_finite() || !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) || key.is_empty() {
        return Err(FetchError::InvalidResponse);
    }
    let mut url = http::validate_endpoint(base, false)?;
    url.set_path(&format!("/weather/v1/current/{lat}/{lon}"));
    url.set_query(None);
    let bearer = format!("Bearer {key}");
    let auth = match auth_kind {"bearer"=>("Authorization", bearer.as_str()),"api_key"=>("X-QW-Api-Key", key),_=>return Err(FetchError::InvalidResponse)};
    http::build_json_request(client, Method::GET, &url, Some(auth), None)
}
pub fn parse(value: &Value) -> Result<Value, FetchError> {
    let temperature = &value["temperature"];
    let condition = &value["condition"];
    let degrees = temperature["value"].as_f64().ok_or(FetchError::InvalidResponse)?;
    let state = condition["text"].as_str().ok_or(FetchError::InvalidResponse)?;
    let code = condition["code"].as_str().filter(|s|!s.trim().is_empty()).ok_or(FetchError::InvalidResponse)?;
    if !degrees.is_finite() || state.trim().is_empty() {return Err(FetchError::InvalidResponse);}
    let unit=temperature["unit"].as_str().filter(|s|!s.is_empty()).ok_or(FetchError::InvalidResponse)?;
    let humidity=value["humidity"].as_f64().filter(|n|n.is_finite() && (0.0..=1.0).contains(n)).ok_or(FetchError::InvalidResponse)?;
    let wind=&value["wind"]["speed"];
    let speed=wind["value"].as_f64().filter(|n|n.is_finite() && *n>=0.0).ok_or(FetchError::InvalidResponse)?;
    let wind_unit=wind["unit"].as_str().filter(|s|!s.is_empty()).ok_or(FetchError::InvalidResponse)?;
    let attributions=value["metadata"]["attributions"].as_array().ok_or(FetchError::InvalidResponse)?;
    if attributions.is_empty() || attributions.iter().any(|v|v.as_str().is_none_or(|s|s.trim().is_empty())){return Err(FetchError::InvalidResponse);}
    Ok(json!({
        "temperature": {"value":degrees,"unit":unit},
        "condition": {"text":state,"code":code},
        "humidity":humidity,
        "wind":{"speed":{"value":speed,"unit":wind_unit}},
        "attributions":attributions
    }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_weather_schema_keeps_provider_fields() {
        let fixture=json!({"temperature":{"value":19.5,"unit":"°C"},"condition":{"text":"晴朗","code":"100"},"humidity":0.5,"wind":{"speed":{"value":9,"unit":"km/h"}},"metadata":{"attributions":["和风天气"]}});
        let result=parse(&fixture).unwrap();
        assert_eq!(result["temperature"]["value"],19.5);
        assert_eq!(result["attributions"][0],"和风天气");
        assert!(parse(&json!({"now":{"temp":"19"}})).is_err());
    }
    #[test]fn current_request_and_invalid_units_are_checked(){
        let client=http::client().unwrap();
        for (kind,header,value) in [("api_key","X-QW-Api-Key","fixture-token"),("bearer","Authorization","Bearer fixture-token")] {
            let request=build_request(&client,"https://weather.example.org/old?secret=no","fixture-token",kind,39.9,116.4).unwrap();
            assert_eq!(request.method(),Method::GET);
            assert_eq!(request.url().path(),"/weather/v1/current/39.9/116.4");
            assert!(request.url().query().is_none());
            assert_eq!(request.headers()[header],value);
            assert!(request.body().is_none());
        }
        assert!(build_request(&client,"https://weather.example.org","key","unknown",39.9,116.4).is_err());
        let fixture=json!({"temperature":{"value":19.5,"unit":"°C"},"condition":{"text":"晴","code":"100"},"humidity":0.5,"wind":{"speed":{"value":9,"unit":"km/h"}},"metadata":{"attributions":["和风天气"]}});
        for (pointer,invalid) in [("/humidity",json!(2)),("/temperature/unit",json!(null)),("/wind/speed/value",json!(-1)),("/metadata/attributions",json!([]))]{
            let mut row=fixture.clone();*row.pointer_mut(pointer).unwrap()=invalid;
            assert_eq!(parse(&row).unwrap_err(),FetchError::InvalidResponse);
        }
    }
}
