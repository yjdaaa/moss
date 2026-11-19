//! 小智彩屏预览的临时令牌缓存与 Axum HTTP 路由。
//!
//! 纯图像、HUD 与动图渲染逻辑位于 `moss_xiaozhi_display::render`，此处原样重导出并补充
//! `PreviewStore` 与 `/mcp/preview/*` 路由。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::goggle_bot::{BotPose, ScreenCardStyle};
use crate::{Shared, unix_now};
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;

pub use moss_xiaozhi_display::render::*;

const MAX_PREVIEW_ENTRIES: usize = 12;
const PREVIEW_TTL_SECS: u64 = 300;

#[derive(Clone)]
struct PreviewItem {
    token: String,
    png_bytes: Arc<Vec<u8>>,
    expires_at: u64,
}

#[derive(Default)]
pub struct PreviewStore {
    items: Mutex<VecDeque<PreviewItem>>,
}

fn random_token_hex() -> Result<String, String> {
    let mut raw = [0u8; 16];
    getrandom::fill(&mut raw).map_err(|_| "无法生成预览图随机令牌".to_owned())?;
    Ok(raw.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn token_ok(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

impl PreviewStore {
    pub fn stage_thumbnail(&self, source_bytes: &[u8]) -> Result<String, String> {
        self.stage_thumbnail_for_style(
            source_bytes,
            DEFAULT_CANVAS_W,
            DEFAULT_CANVAS_H,
            None,
            ScreenCardStyle::MossHud,
        )
    }

    pub fn stage_thumbnail_for_canvas(
        &self,
        source_bytes: &[u8],
        canvas_w: u32,
        canvas_h: u32,
        ruler_ascii: Option<&str>,
    ) -> Result<String, String> {
        self.stage_thumbnail_for_style(
            source_bytes,
            canvas_w,
            canvas_h,
            ruler_ascii,
            ScreenCardStyle::MossHud,
        )
    }

    pub fn stage_thumbnail_for_style(
        &self,
        source_bytes: &[u8],
        canvas_w: u32,
        canvas_h: u32,
        ruler_ascii: Option<&str>,
        style: ScreenCardStyle,
    ) -> Result<String, String> {
        let png = make_device_thumbnail_png_for_style(
            source_bytes,
            canvas_w,
            canvas_h,
            ruler_ascii,
            style,
        )?;
        self.stage_prebuilt_png(png)
    }
    pub fn stage_prebuilt_png(&self, png: Vec<u8>) -> Result<String, String> {
        if png.is_empty() || png.len() > MAX_THUMBNAIL_PNG_BYTES {
            return Err("屏幕图像体积超出范围".into());
        }
        let token = random_token_hex()?;
        self.insert_with_now(token.clone(), png, unix_now());
        Ok(token)
    }

    fn insert_with_now(&self, token: String, png: Vec<u8>, now: u64) {
        if let Ok(mut guard) = self.items.lock() {
            guard.retain(|item| item.expires_at > now);
            while guard.len() >= MAX_PREVIEW_ENTRIES {
                guard.pop_front();
            }
            guard.push_back(PreviewItem {
                token,
                png_bytes: Arc::new(png),
                expires_at: now.saturating_add(PREVIEW_TTL_SECS),
            });
        }
    }

    pub fn get(&self, token: &str) -> Option<Arc<Vec<u8>>> {
        self.get_at(token, unix_now())
    }

    fn get_at(&self, token: &str, now: u64) -> Option<Arc<Vec<u8>>> {
        if !token_ok(token) {
            return None;
        }
        let mut guard = self.items.lock().ok()?;
        guard.retain(|item| item.expires_at > now);
        guard
            .iter()
            .find(|item| item.token.eq_ignore_ascii_case(token))
            .map(|item| item.png_bytes.clone())
    }
}

pub(crate) fn router(shared: Arc<Shared>) -> Router {
    Router::new()
        .route("/mcp/preview/{file}", get(serve_preview))
        .route("/mcp/preview/goggle-bot/{file}", get(serve_goggle_bot_gif))
        .with_state(shared)
}

/// 免鉴权的局域网路由：2026-11-05 起取进程级缓存（每个姿势只编码一次），首次编码放到阻塞线程，
/// 反复请求不再在 tokio 工作线程上逐次重编码 GIF。
async fn serve_goggle_bot_gif(Path(file): Path<String>) -> Response {
    let slug = file.strip_suffix(".gif").unwrap_or(&file);
    let Some(pose) = BotPose::from_slug(slug) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let cached = match crate::goggle_bot::goggle_bot_gif_if_encoded(pose) {
        Some(cached) => cached,
        None => tokio::task::spawn_blocking(move || crate::goggle_bot::cached_goggle_bot_gif(pose))
            .await
            .ok()
            .flatten(),
    };
    let Some(bytes) = cached else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let len = bytes.len().to_string();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/gif".to_owned()),
            (header::CONTENT_LENGTH, len),
            (header::CACHE_CONTROL, "public, max-age=300".to_owned()),
        ],
        bytes,
    )
        .into_response()
}
async fn serve_preview(State(shared): State<Arc<Shared>>, Path(file): Path<String>) -> Response {
    let token = file.strip_suffix(".png").unwrap_or(&file);
    let Some(bytes) = shared.preview_store.get(token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let len = bytes.len().to_string();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "image/png".to_owned()),
            (header::CONTENT_LENGTH, len),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        bytes.as_ref().clone(),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_store_enforces_capacity_and_ttl() {
        let hud = make_job_hud_png(
            &JobHudSpec {
                percent: 68,
                lines_sent: 680,
                total_lines: 1000,
                power: Some(600),
                feed_rate: Some(1200),
                eta_seconds: Some(42),
            },
            160,
            120,
        )
        .unwrap();
        let store = PreviewStore::default();
        for i in 0..14u8 {
            let tok = format!("{:032x}", i as u128 + 1);
            store.insert_with_now(tok, hud.clone(), 100);
        }
        assert!(store.get_at(&format!("{:032x}", 1u128), 110).is_none());
        assert!(store.get_at(&format!("{:032x}", 14u128), 110).is_some());
        assert!(store.get_at(&format!("{:032x}", 14u128), 500).is_none());
    }
}
