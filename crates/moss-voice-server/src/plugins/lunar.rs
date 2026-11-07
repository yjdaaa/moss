//! Local Chinese lunisolar calendar. This is cultural/date information, never a device safety rule.
use super::http::FetchError;
use chrono::{Datelike, NaiveDate};
use lunar_rust::{
    lunar::LunarRefHelper,
    solar::{self, SolarRefHelper},
};
use serde_json::{Value, json};
pub fn calendar(date: &str) -> Result<Value, FetchError> {
    let parsed =
        NaiveDate::parse_from_str(date, "%Y-%m-%d").map_err(|_| FetchError::InvalidResponse)?;
    if parsed.year() < 1900 || parsed.year() > 2100 {
        return Err(FetchError::InvalidResponse);
    }
    let lunar = std::panic::catch_unwind(|| {
        solar::from_ymdhms(
            parsed.year() as i64,
            parsed.month() as i64,
            parsed.day() as i64,
            12,
            0,
            0,
        )
        .get_lunar()
    })
    .map_err(|_| FetchError::InvalidResponse)?;
    let month = lunar.get_month();
    let jie = lunar.get_jie_qi();
    Ok(
        json!({"solar_date":date,"lunar_date":format!("{}年{}月{}",lunar.get_year_in_chinese(),lunar.get_month_in_chinese(),lunar.get_day_in_chinese()),
        "leap_month":month<0,"solar_term":if jie.is_empty(){None}else{Some(jie)},
        "yi":lunar.get_day_yi(None).into_iter().take(8).collect::<Vec<_>>(),
        "ji":lunar.get_day_ji(None).into_iter().take(8).collect::<Vec<_>>() }),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spring_festival_and_range() {
        let d = calendar("2024-02-10").unwrap();
        assert!(d["lunar_date"].as_str().unwrap().contains("正月初一"));
        let leap = calendar("2023-03-22").unwrap();
        assert_eq!(leap["leap_month"], true);
        let new_year = calendar("2025-01-29").unwrap();
        assert!(
            new_year["lunar_date"]
                .as_str()
                .unwrap()
                .contains("正月初一")
        );
        assert!(calendar("2200-01-01").is_err());
        assert!(calendar("2025-02-30").is_err());
    }
}
