use super::*;

mod golden;
mod material_routes;
mod preview;
mod send_gate;
mod stop_and_resume;
mod strict_mode;
mod system_routes;
mod web;

fn parse_json_response(response: &[u8]) -> serde_json::Value {
    let start = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    serde_json::from_slice(&response[start..]).unwrap()
}

fn test_loopback_settings() -> laser_runtime::LaserSettings {
    laser_runtime::LaserSettings {
        default_connection_mode: "network".into(),
        engraving_mode: "raster".into(),
        web_host: "127.0.0.1".into(),
        web_port: 8766,
        material_params_file: None,
    }
}

fn fake_transport_factory(ok_lines: usize) -> TransportFactory {
    fake_transport_factory_with_settings(ok_lines, &[])
}

/// 同 `fake_transport_factory`，`$$` 额外回报 `extra_settings`（如 `$130=100`）。
fn fake_transport_factory_with_settings(
    ok_lines: usize,
    extra_settings: &[&str],
) -> TransportFactory {
    struct ResponsiveGrbl {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        remaining: std::sync::atomic::AtomicUsize,
        extra_settings: Vec<String>,
    }
    impl laser_device::DeviceTransport for ResponsiveGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            let mut responses = self.responses.lock().unwrap();
            if line == "$$" {
                responses.extend(self.extra_settings.iter().cloned());
                responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else if line == "$CD" || line.starts_with("$RI=") {
                // 模拟 FluidNC：配置读取与自动上报设置都只回 ok，不占用 G-code 行的 ack 配额
                responses.push_back("ok".into());
            } else if line == "G4 P0"
                || self
                    .remaining
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
                    > 0
            {
                responses.push_back("ok".into());
            }
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }
    let extra: Vec<String> = extra_settings
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
        Ok(Box::new(ResponsiveGrbl {
            responses: Default::default(),
            remaining: std::sync::atomic::AtomicUsize::new(ok_lines),
            extra_settings: extra.clone(),
        }) as Box<dyn laser_device::DeviceTransport>)
    })
}

fn recording_transport_factory(
    ok_lines: usize,
    written_lines: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) -> TransportFactory {
    struct RecordingGrbl {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        remaining: std::sync::atomic::AtomicUsize,
        written: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }
    impl laser_device::DeviceTransport for RecordingGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            self.written.lock().unwrap().push(line.to_string());
            let mut responses = self.responses.lock().unwrap();
            if line == "$$" {
                responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
            } else if line == "$CD"
                || line.starts_with("$RI=")
                || line == "$X"
                || line == "G4 P0"
                || self
                    .remaining
                    .fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
                    > 0
            {
                responses.push_back("ok".into());
            }
            Ok(())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }
    std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
        Ok(Box::new(RecordingGrbl {
            responses: Default::default(),
            remaining: std::sync::atomic::AtomicUsize::new(ok_lines),
            written: written_lines.clone(),
        }) as Box<dyn laser_device::DeviceTransport>)
    })
}

fn wait_for_status(service: &ApiService, workflow_id: &str, wanted: &str) -> serde_json::Value {
    let status_body = format!(r#"{{"action":"status","workflow_id":"{workflow_id}"}}"#);
    let status_request = format!(
        "POST /api/workflow HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        status_body.len(),
        status_body
    );
    let start = std::time::Instant::now();
    loop {
        let value = parse_json_response(&service.handle_http(status_request.as_bytes()));
        // 成功即返回；20 秒上限只为全工作区并行测试负载下不误报（2026-11-05）。
        if value["result"]["status"] == wanted
            || start.elapsed() > std::time::Duration::from_secs(20)
        {
            return value;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn post_json(service: &ApiService, route: &str, body: &serde_json::Value) -> serde_json::Value {
    let body = body.to_string();
    let request = format!(
        "POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    parse_json_response(&service.handle_http(request.as_bytes()))
}

fn cors_test_service() -> ApiService {
    ApiService::new(
        laser_runtime::LaserSettings {
            default_connection_mode: "network".into(),
            engraving_mode: "raster".into(),
            web_host: "127.0.0.1".into(),
            web_port: 9666,
            material_params_file: None,
        },
        laser_contracts::builtin_material_library(),
    )
}

fn post_with_ctx(
    service: &ApiService,
    route: &str,
    body: &serde_json::Value,
    ctx: laser_contracts::CallerContext,
) -> (u16, serde_json::Value) {
    let body = body.to_string();
    let request = format!(
        "POST {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let raw = service.handle_http_with_ctx(request.as_bytes(), ctx);
    let status = String::from_utf8_lossy(&raw[9..12]).parse::<u16>().unwrap();
    (status, parse_json_response(&raw))
}

fn desktop_request(
    service: &ApiService,
    method: &str,
    path: &str,
    body: Option<&serde_json::Value>,
    session: Option<&str>,
) -> (u16, serde_json::Value) {
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let header = session
        .map(|value| format!("X-Moss-Desktop-Session: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{header}Content-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    let raw = service.handle_http(request.as_bytes());
    let status = String::from_utf8_lossy(&raw[9..12]).parse::<u16>().unwrap();
    (status, parse_json_response(&raw))
}

/// 握手与预检正常，第一条加工行写入时一直阻塞到测试放行：作业确定停在“发送中”，
/// 模拟进程崩溃时的在途作业（不依赖心跳或超时的时序）。
fn gated_transport_factory(
    gate: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> TransportFactory {
    struct GatedGrbl {
        responses: std::sync::Mutex<std::collections::VecDeque<String>>,
        gate: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    impl laser_device::DeviceTransport for GatedGrbl {
        fn write_line(&self, line: &str) -> Result<(), String> {
            if line.starts_with('$') {
                let mut responses = self.responses.lock().unwrap();
                if line == "$$" {
                    responses.extend(["$30=1000", "$32=1", "ok"].map(str::to_owned));
                } else {
                    responses.push_back("ok".into());
                }
                return Ok(());
            }
            while !self.gate.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err("测试结束：模拟连接断开".into())
        }
        fn write_realtime(&self, command: laser_device::RealtimeCommand) -> Result<(), String> {
            if command == laser_device::RealtimeCommand::StatusQuery {
                self.responses
                    .lock()
                    .unwrap()
                    .push_back("<Idle|MPos:0,0,0|FS:0,0>".into());
            }
            Ok(())
        }
        fn read_line(&self) -> Result<Option<String>, String> {
            Ok(self.responses.lock().unwrap().pop_front())
        }
    }
    std::sync::Arc::new(move |_spec: &laser_device::ConnectionSpec| {
        Ok(Box::new(GatedGrbl {
            responses: Default::default(),
            gate: gate.clone(),
        }) as Box<dyn laser_device::DeviceTransport>)
    })
}
