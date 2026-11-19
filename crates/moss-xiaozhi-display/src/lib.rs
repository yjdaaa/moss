//! 小智设备屏幕渲染、动图画风、中文字库与 `assets.bin` 资源包纯构造库。
//!
//! 本 crate 独立于 `moss-voice-server` 的 Tokio/Axum/Opus 运行时，只做纯数据与纯像素处理，
//! 承载内置 4.8 MB 中文字库与 3.9 MB ESP-SR 唤醒词资源，便于独立快速编译与测试。

pub mod glyph_font;
pub mod goggle_bot;
pub mod render;
pub mod screen_pack;
pub mod wake_pack;

pub use goggle_bot::{
    BotPose, GoggleBotGifItem, ScreenCardStyle, cached_goggle_bot_gif, cached_goggle_bot_gifs,
    encode_goggle_bot_gif, goggle_bot_gif_if_encoded,
};
