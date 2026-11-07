//! Read a bounded RSS feed from a desktop-configured source.
use super::http::{self, FetchError};
use quick_xml::{Reader, events::Event};
use reqwest::Method;
use reqwest::blocking::Client;
use serde_json::{Value, json};

pub fn fetch_rss(client: &Client, url: &str, limit: usize) -> Result<Value, FetchError> {
    if !(1..=5).contains(&limit) {
        return Err(FetchError::InvalidResponse);
    }
    let base = http::validate_endpoint(url, false)?;
    let response = client.request(Method::GET, base).send().map_err(|e| {
        if e.is_timeout() {
            FetchError::Timeout
        } else {
            FetchError::Network
        }
    })?;
    parse_rss(&http::read_bounded(response)?, limit)
}

fn clean_html(text: &str) -> String {
    let mut result = String::new();
    let mut tag = false;
    for c in text.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            _ if !tag && !c.is_control() => result.push(c),
            _ => (),
        }
    }
    result.chars().take(500).collect()
}
pub fn parse_rss(raw: &[u8], limit: usize) -> Result<Value, FetchError> {
    if raw.len() > http::MAX_BODY_BYTES
        || !(1..=5).contains(&limit)
        || raw
            .windows(9)
            .any(|part| part.eq_ignore_ascii_case(b"<!DOCTYPE"))
        || raw
            .windows(8)
            .any(|part| part.eq_ignore_ascii_case(b"<!ENTITY"))
    {
        return Err(FetchError::InvalidResponse);
    }
    let mut reader = Reader::from_reader(raw);
    // GeneralRef events split text at &amp;/numeric references. Preserve the
    // intervening spaces instead of trimming each fragment independently.
    reader.config_mut().trim_text(false);
    let mut rss = false;
    let mut channel = false;
    let mut item = false;
    let mut field = String::new();
    let mut title = String::new();
    let mut link = String::new();
    let mut date = String::new();
    let mut desc = String::new();
    let mut items = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut closed_root = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => {
                let name = tag.name();
                let name = name.as_ref();
                if stack.is_empty() {
                    if rss || closed_root || name != "rss" {
                        return Err(FetchError::InvalidResponse);
                    }
                    rss = true;
                } else if stack.len() == 1 && name == "channel" {
                    channel = true;
                } else if stack.len() == 2 && stack[1] == "channel" && name == "item" {
                    item = true;
                    title.clear();
                    link.clear();
                    date.clear();
                    desc.clear();
                } else if item && stack.len() == 3 {
                    field = name.to_string();
                }
                stack.push(name.to_string());
            }
            Ok(Event::Text(text)) if item => {
                let text = quick_xml::escape::unescape(text.as_ref())
                    .map_err(|_| FetchError::InvalidResponse)?;
                match field.as_str() {
                    "title" => title.push_str(&text),
                    "link" => link.push_str(&text),
                    "pubDate" => date.push_str(&text),
                    "description" => desc.push_str(&text),
                    _ => {}
                }
            }
            Ok(Event::CData(text)) if item => match field.as_str() {
                "title" => title.push_str(text.as_ref()),
                "link" => link.push_str(text.as_ref()),
                "pubDate" => date.push_str(text.as_ref()),
                "description" => desc.push_str(text.as_ref()),
                _ => {}
            },
            Ok(Event::GeneralRef(text)) if item => {
                let reference = format!("&{};", text.as_ref());
                let decoded = quick_xml::escape::unescape(&reference)
                    .map_err(|_| FetchError::InvalidResponse)?;
                match field.as_str() {
                    "title" => title.push_str(&decoded),
                    "link" => link.push_str(&decoded),
                    "pubDate" => date.push_str(&decoded),
                    "description" => desc.push_str(&decoded),
                    _ => {}
                }
            }
            Ok(Event::End(tag)) => {
                let name = tag.name();
                if stack
                    .last()
                    .is_none_or(|last| last.as_str() != name.as_ref())
                {
                    return Err(FetchError::InvalidResponse);
                }
                if item && stack.len() == 3 && name.as_ref() == "item" {
                    item = false;
                    field.clear();
                    if title.trim().is_empty() {
                        return Err(FetchError::InvalidResponse);
                    }
                    {
                        let url = reqwest::Url::parse(link.trim())
                            .map_err(|_| FetchError::InvalidResponse)?;
                        if !matches!(url.scheme(), "http" | "https")
                            || url.host_str().is_none()
                            || !url.username().is_empty()
                            || url.password().is_some()
                        {
                            return Err(FetchError::InvalidResponse);
                        }
                        if items.len() < limit {
                            items.push(json!({"title":clean_html(&title),"url":link.trim(),"published":date,"description":clean_html(&desc)}));
                        }
                    }
                } else if stack.len() == 4 {
                    field.clear();
                }
                stack.pop();
                if stack.is_empty() {
                    closed_root = true;
                }
            }
            Ok(Event::Empty(tag)) if stack.len() == 1 && tag.name().as_ref() == "channel" => {
                channel = true;
            }
            Ok(Event::Empty(_)) if stack.is_empty() => return Err(FetchError::InvalidResponse),
            Ok(Event::GeneralRef(_)) => return Err(FetchError::InvalidResponse),
            Ok(Event::CData(_)) if stack.is_empty() => return Err(FetchError::InvalidResponse),
            Ok(Event::Decl(_)) if rss => return Err(FetchError::InvalidResponse),
            Ok(Event::Text(text))
                if !item && stack.is_empty() && !text.as_ref().trim().is_empty() =>
            {
                return Err(FetchError::InvalidResponse);
            }
            Ok(Event::DocType(_)) => return Err(FetchError::InvalidResponse),
            Ok(Event::Eof) => break,
            Err(_) => return Err(FetchError::InvalidResponse),
            _ => {}
        }
    }
    if !rss || !channel || !closed_root || !stack.is_empty() {
        return Err(FetchError::InvalidResponse);
    }
    Ok(json!({"items":items}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rss_parses_known_items_and_rejects_entities() {
        let xml=br#"<rss><channel><item><title>Title</title><link>https://example.test</link><pubDate>Tue</pubDate><description><![CDATA[<b>news</b>]]></description></item></channel></rss>"#;
        let v = parse_rss(xml, 5).unwrap();
        assert_eq!(v["items"][0]["title"], "Title");
        assert_eq!(v["items"][0]["description"], "news");
        assert!(
            parse_rss(
                b"<!DOCTYPE rss [<!ENTITY x SYSTEM 'file:///etc/hosts'>]><rss/>",
                5
            )
            .is_err()
        );
        assert!(parse_rss(b"broken text", 5).is_err());
        assert!(parse_rss(b"<not-rss></not-rss>", 5).is_err());
    }
}
