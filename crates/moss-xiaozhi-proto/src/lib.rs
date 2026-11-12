//! 小智设备协议（MOSS 内置语音服务，技术书 `docs/integration/moss-native-voice-spec.md` NV1）。
//!
//! - [`message`]：设备 ↔ 服务端的 JSON 文本消息；
//! - [`frame`]：二进制音频帧（协议版本 1 裸 Opus、2 带 16 字节头、3 带 4 字节头）；
//! - [`token`]：设备 token（HMAC-SHA256，与 xiaozhi-esp32-server 格式一致）；
//! - [`ota`]：OTA 请求解析、响应构造与版本比较。
//!
//! 协议依据：xiaozhi-esp32 固件 `main/protocols/websocket_protocol.cc`、`main/ota.cc`（提交 `0d576d3`），
//! xiaozhi-esp32-server（`7a260ee`）与海门 `crates/haimen-xiaozhi`（`513d9e6`）。来源与许可见仓库根目录
//! `THIRD_PARTY_NOTICES.md`。

pub mod frame;
pub mod message;
pub mod ota;
pub mod token;

pub use frame::{DecodedFrame, FrameError, decode_frame, encode_frame};
pub use message::{
    AudioParams, ClientMessage, Features, GlyphItem, GlyphPushPayload, ListenState, ServerMessage,
    TextFontCapability, TtsState,
};
pub use ota::{FirmwareOffer, OtaInfo, OtaReply, is_newer_version};
pub use token::TokenSigner;
