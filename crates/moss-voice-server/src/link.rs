//! n71 会话底下的设备连接：WebSocket（唤醒时才连，文字与音频同一条连接）或
//! MQTT + UDP 常连模式（控制消息走 MQTT 一直连着，音频只在唤醒后走加密 UDP）。
//! 会话层只用 `recv` / `send` 和下面几个查询，不关心是哪一种。

use axum::extract::ws::{Message, WebSocket};

use crate::mqtt::MqttLink;

pub(crate) enum Link {
    Ws(WebSocket),
    Mqtt(MqttLink),
}

fn closed() -> axum::Error {
    axum::Error::new(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "设备连接已关闭",
    ))
}

impl Link {
    pub(crate) async fn recv(&mut self) -> Option<Result<Message, axum::Error>> {
        match self {
            Self::Ws(socket) => socket.recv().await,
            Self::Mqtt(link) => link.recv().await.map(Ok),
        }
    }

    pub(crate) async fn send(&mut self, message: Message) -> Result<(), axum::Error> {
        match self {
            Self::Ws(socket) => socket.send(message).await,
            Self::Mqtt(link) => match message {
                Message::Text(text) => {
                    if link.send_text(text.as_str()) {
                        Ok(())
                    } else {
                        Err(closed())
                    }
                }
                Message::Binary(opus) => {
                    link.send_audio(&opus).await;
                    Ok(())
                }
                // MQTT 连接由外层在会话结束后关闭
                _ => Ok(()),
            },
        }
    }

    /// 常连模式（MQTT）。
    pub(crate) fn is_mqtt(&self) -> bool {
        matches!(self, Self::Mqtt(_))
    }

    /// 语音通道是否开着：WebSocket 连着就算开着；常连模式只在设备唤醒（发了 hello）后才开。
    pub(crate) fn audio_open(&self) -> bool {
        match self {
            Self::Ws(_) => true,
            Self::Mqtt(link) => link.audio_open(),
        }
    }

    /// 常连模式结束这一轮对话：发 `goodbye` 让设备关掉语音通道，控制通道保持在线。
    /// `notify` 为假表示设备自己先说了 goodbye，不再回发。
    pub(crate) fn end_audio(&mut self, session_id: &str, notify: bool) {
        if let Self::Mqtt(link) = self {
            link.close_audio(session_id, notify);
        }
    }
}
