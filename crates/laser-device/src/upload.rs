use std::io::Write;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// 将 G-code 文件内容通过 FluidNC 的 HTTP /upload 接口上传至 SD 卡
pub fn upload_gcode_to_fluidnc_sd(
    host: &str,
    port: u16,
    remote_filename: &str,
    file_bytes: &[u8],
    timeout: Duration,
) -> Result<(), String> {
    use std::io::Read;

    let boundary = "---------------------------MOSSBoundary7d93b28b";
    let header_part = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{remote_filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
    );
    let footer_part = format!("\r\n--{boundary}--\r\n");
    let content_length = header_part.len() + file_bytes.len() + footer_part.len();

    let request_header = format!(
        "POST /upload HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         User-Agent: MOSS-Laser\r\n\
         Content-Type: multipart/form-data; boundary={boundary}\r\n\
         Content-Length: {content_length}\r\n\
         Connection: close\r\n\r\n"
    );

    let addrs = (host, port)
        .to_socket_addrs()
        .map_err(|e| format!("解析地址 {host}:{port} 失败: {e}"))?;

    let mut last_err = format!("连接 {host}:{port} 失败");
    let mut stream_opt = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(10)));
                stream_opt = Some(s);
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }

    let mut stream = stream_opt.ok_or(last_err)?;
    stream
        .write_all(request_header.as_bytes())
        .map_err(|e| e.to_string())?;
    stream
        .write_all(header_part.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.write_all(file_bytes).map_err(|e| e.to_string())?;
    stream
        .write_all(footer_part.as_bytes())
        .map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|e| e.to_string())?;
    let resp_str = String::from_utf8_lossy(&response);
    let status_line = resp_str.lines().next().unwrap_or("");
    if status_line.contains(" 200 ")
        || status_line.contains(" 204 ")
        || status_line.contains(" 201 ")
    {
        Ok(())
    } else {
        Err(format!("FluidNC HTTP 上传失败，响应: {status_line}"))
    }
}
