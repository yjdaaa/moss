use super::*;

/// 激光侧诊断事件级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaserEventLevel {
    Info,
    Warn,
    Error,
}

/// 激光雕刻机相关的诊断事件（2026-11-09 用户要求）：桌面端接到语音中心“记录与诊断”，
/// 标为“激光雕刻”，和小智、MOSS 软件的事件放在一起，方便判断是哪里出的错。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaserEvent {
    pub level: LaserEventLevel,
    /// 大白话说明（含停在第几行、简短原因）
    pub text: String,
    /// 下一步建议
    pub hint: Option<String>,
    /// 原始报错（固件回复与 MOSS 的说明）
    pub detail: Option<String>,
}

/// 事件出口：没有设置时（独立 moss-host、Android）静默丢弃。
pub type LaserEventSink = std::sync::Arc<dyn Fn(LaserEvent) + Send + Sync>;
pub(crate) type SharedLaserEventSink = std::sync::Arc<std::sync::RwLock<Option<LaserEventSink>>>;

/// 取 HTTP 响应正文里的 JSON（取不到时为 Null），用于把失败原因写进诊断事件。
pub(crate) fn http_response_json(response: &[u8]) -> serde_json::Value {
    response
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .and_then(|pos| serde_json::from_slice(&response[pos + 4..]).ok())
        .unwrap_or(serde_json::Value::Null)
}

pub(crate) fn emit_laser_event(sink: &SharedLaserEventSink, event: LaserEvent) {
    let sink = sink.read().ok().and_then(|guard| guard.clone());
    if let Some(sink) = sink {
        sink(event);
    }
}

/// 加工结束的诊断事件：完成（信息）、取消（警告）、失败（错误，带简短原因与停在第几行）。
pub(crate) fn job_terminal_event(
    workflow_id: &str,
    status: &str,
    send_result: &serde_json::Value,
) -> LaserEvent {
    let sent = send_result["lines_sent"].as_u64().unwrap_or(0);
    let total = send_result["total_lines"].as_u64().unwrap_or(0);
    let target = send_result["target"].as_str().unwrap_or("");
    let at = if target.is_empty() {
        String::new()
    } else {
        format!("（{target}）")
    };
    match status {
        "completed" => LaserEvent {
            level: LaserEventLevel::Info,
            text: format!("加工完成：{workflow_id}，共 {total} 行{at}"),
            hint: None,
            detail: None,
        },
        "cancelled" => LaserEvent {
            level: LaserEventLevel::Warn,
            text: format!("加工已停止：{workflow_id}，停在第 {sent}/{total} 行{at}"),
            hint: Some(if send_result["can_resume"].as_bool() == Some(true) {
                "到设备旁确认机器已停；需要时可在工作台从断点继续".to_owned()
            } else {
                "到设备旁确认机器已停、激光已灭".to_owned()
            }),
            detail: send_result["error"].as_str().map(str::to_owned),
        },
        _ => {
            let error = send_result["error"].as_str().unwrap_or("加工失败");
            let short: String = error.chars().take(120).collect();
            LaserEvent {
                level: LaserEventLevel::Error,
                text: format!("加工失败：{workflow_id}，停在第 {sent}/{total} 行{at}：{short}"),
                hint: Some(
                    laser_failure_hint(error)
                        .unwrap_or("看技术细节里固件给的原因，排除后再发送；到设备旁确认机器已停")
                        .to_owned(),
                ),
                detail: Some(error.to_owned()),
            }
        }
    }
}

/// 按报错内容给出下一步建议（只覆盖常见原因；其余看技术细节里固件自己的说明）。
pub fn laser_failure_hint(error: &str) -> Option<&'static str> {
    let has = |needle: &str| error.contains(needle);
    if has("ALARM:2") || has("软限位") {
        Some("图形超出了机器行程：检查图形尺寸、工作原点和板材起点，必要时缩小图形")
    } else if has("ALARM:1") || has("硬限位") {
        Some("碰到了限位开关：确认图形没有超出行程，解锁（$X）后重新回零再加工")
    } else if has("ALARM:14") || has("未回零") {
        Some("机器要求先回零：在工作台点“回零”（$H）后再开始")
    } else if has("急停") && (has("Pn:") || has("引脚") || has("开关")) {
        Some("急停开关或故障输入被触发：松开急停、排除故障后再试")
    } else if has("Door") || has("安全门") || has("门开") {
        Some("安全门没关好：关门后机器会继续，或重新开始")
    } else if has("error:120") {
        Some("别的通道（WebUI 或 SD 卡任务）正在加工：等它结束或先停止它")
    } else if has("SHA-256") || has("哈希") {
        Some("机器上的文件和确认过的刀路不一致：重新生成并发送")
    } else if has("失联") || has("断开") || has("连接") || has("超时") {
        Some("检查雕刻机电源、网线 / Wi-Fi 或 USB 线，确认 IP / 串口正确后点“握手探测”")
    } else {
        None
    }
}

impl ApiService {
    /// 替换系统串口枚举（测试用；生产环境使用默认的真实枚举）。
    /// 设置激光侧诊断事件出口（桌面端在内置语音初始化后调用；运行中可替换）。
    pub fn set_event_sink(&self, sink: LaserEventSink) {
        if let Ok(mut slot) = self.event_sink.write() {
            *slot = Some(sink);
        }
    }

    pub(crate) fn emit_event(&self, event: LaserEvent) {
        emit_laser_event(&self.event_sink, event);
    }
}
