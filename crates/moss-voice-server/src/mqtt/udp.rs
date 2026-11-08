//! 小智 MQTT 模式的 UDP 语音通道（核实自 `78/xiaozhi-esp32` `main/protocols/mqtt_protocol.cc`）。
//!
//! 每个包：`|type 1B=0x01|flags 1B|payload_len 2B|ssrc 4B|timestamp 4B|sequence 4B|` + 加密的 Opus。
//! 这 16 字节包头同时是 AES-128-CTR 的初始计数块（整块按大端递增，与 PSA `PSA_ALG_CTR` 相同）：
//! 发送方拿服务端在 hello 里给的 nonce，填上本包的长度、时间戳与序号。设备原样保留 nonce 的
//! 第 0、1、4~7 字节，所以 MOSS 用 nonce 里随机的 ssrc（第 4~7 字节）区分是哪台设备的通道。
//! 两个方向的序号都从 1 递增，收到不大于上一个序号的包丢弃（防重放）。

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aes::Aes128;
use ctr::Ctr128BE;
use ctr::cipher::{KeyIvInit, StreamCipher};
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;

use super::LinkEvent;

pub(crate) const HEADER: usize = 16;
/// 设备还没发来第一个包（不知道它的 UDP 端口）时最多先攒多少帧下行音频（60 ms 一帧，约 3 秒）。
const MAX_PENDING: usize = 50;
/// 单个 Opus 包上限（60 ms 帧远小于此）。
const MAX_PAYLOAD: usize = 4000;

/// AES-128-CTR 原地加解密（同一个函数）。
pub(crate) fn crypt(key: &[u8; 16], iv: &[u8; 16], data: &mut [u8]) {
    let mut cipher = Ctr128BE::<Aes128>::new(key.into(), iv.into());
    cipher.apply_keystream(data);
}

/// 按 nonce 填好本包的包头（也是这个包的 CTR 初始计数块）。
pub(crate) fn header(
    nonce: &[u8; 16],
    payload_len: usize,
    timestamp: u32,
    sequence: u32,
) -> [u8; 16] {
    let mut head = *nonce;
    head[2..4].copy_from_slice(&(payload_len as u16).to_be_bytes());
    head[8..12].copy_from_slice(&timestamp.to_be_bytes());
    head[12..16].copy_from_slice(&sequence.to_be_bytes());
    head
}

/// 加密一个 Opus 包，返回完整的 UDP 数据报。
pub(crate) fn seal(
    key: &[u8; 16],
    nonce: &[u8; 16],
    opus: &[u8],
    timestamp: u32,
    sequence: u32,
) -> Vec<u8> {
    let head = header(nonce, opus.len(), timestamp, sequence);
    let mut out = Vec::with_capacity(HEADER + opus.len());
    out.extend_from_slice(&head);
    out.extend_from_slice(opus);
    crypt(key, &head, &mut out[HEADER..]);
    out
}

/// 解析数据报的包头：返回 (ssrc, sequence)；格式不对返回 None。
pub(crate) fn parse_header(datagram: &[u8]) -> Option<([u8; 4], u32)> {
    if datagram.len() < HEADER || datagram[0] != 0x01 {
        return None;
    }
    let payload_len = usize::from(u16::from_be_bytes([datagram[2], datagram[3]]));
    if datagram.len() != HEADER + payload_len || payload_len > MAX_PAYLOAD {
        return None;
    }
    let ssrc = [datagram[4], datagram[5], datagram[6], datagram[7]];
    let sequence = u32::from_be_bytes([datagram[12], datagram[13], datagram[14], datagram[15]]);
    Some((ssrc, sequence))
}

struct ChannelState {
    peer: Option<SocketAddr>,
    remote_sequence: u32,
    local_sequence: u32,
    pending: VecDeque<Vec<u8>>,
}

/// 一次语音会话（设备每次唤醒发 hello 时新建）的 UDP 通道。
pub(crate) struct UdpChannel {
    pub key: [u8; 16],
    pub nonce: [u8; 16],
    /// 只接受来自这台设备 MQTT 连接同一 IP 的包
    device_ip: IpAddr,
    opened: Instant,
    state: Mutex<ChannelState>,
    incoming: UnboundedSender<LinkEvent>,
}

impl UdpChannel {
    fn ssrc(&self) -> [u8; 4] {
        [self.nonce[4], self.nonce[5], self.nonce[6], self.nonce[7]]
    }

    pub fn key_hex(&self) -> String {
        hex(&self.key)
    }

    pub fn nonce_hex(&self) -> String {
        hex(&self.nonce)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn random16() -> [u8; 16] {
    let mut bytes = [0u8; 16];
    let _ = getrandom::fill(&mut bytes);
    bytes
}

/// MOSS 唯一的 UDP 语音端口：按 ssrc 把设备上行分给各自的会话，并发送下行。
pub(crate) struct UdpHub {
    socket: Arc<UdpSocket>,
    channels: Mutex<HashMap<[u8; 4], Arc<UdpChannel>>>,
}

impl UdpHub {
    pub fn new(socket: UdpSocket) -> Arc<Self> {
        Arc::new(Self {
            socket: Arc::new(socket),
            channels: Mutex::new(HashMap::new()),
        })
    }

    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// 新建一个通道（随机密钥与 nonce，ssrc 不与现有通道重复）。
    pub fn open(&self, device_ip: IpAddr, incoming: UnboundedSender<LinkEvent>) -> Arc<UdpChannel> {
        let mut channels = self.channels.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            let mut nonce = random16();
            nonce[0] = 0x01;
            nonce[1] = 0x00;
            for byte in &mut nonce[2..4] {
                *byte = 0;
            }
            for byte in &mut nonce[8..16] {
                *byte = 0;
            }
            let ssrc = [nonce[4], nonce[5], nonce[6], nonce[7]];
            if channels.contains_key(&ssrc) {
                continue;
            }
            let channel = Arc::new(UdpChannel {
                key: random16(),
                nonce,
                device_ip,
                opened: Instant::now(),
                state: Mutex::new(ChannelState {
                    peer: None,
                    remote_sequence: 0,
                    local_sequence: 0,
                    pending: VecDeque::new(),
                }),
                incoming,
            });
            channels.insert(ssrc, channel.clone());
            return channel;
        }
    }

    pub fn close(&self, channel: &UdpChannel) {
        let mut channels = self.channels.lock().unwrap_or_else(|p| p.into_inner());
        if channels
            .get(&channel.ssrc())
            .is_some_and(|c| std::ptr::eq(Arc::as_ptr(c), channel))
        {
            channels.remove(&channel.ssrc());
        }
    }

    /// 发一帧下行 Opus；还不知道设备端口时先攒着，收到它的第一个包后补发。
    pub async fn send(&self, channel: &UdpChannel, opus: &[u8]) {
        if opus.len() > MAX_PAYLOAD {
            return;
        }
        let (datagram, peer, backlog) = {
            let mut state = channel.state.lock().unwrap_or_else(|p| p.into_inner());
            state.local_sequence = state.local_sequence.wrapping_add(1);
            let timestamp = channel.opened.elapsed().as_millis() as u32;
            let datagram = seal(
                &channel.key,
                &channel.nonce,
                opus,
                timestamp,
                state.local_sequence,
            );
            match state.peer {
                Some(peer) => (datagram, peer, std::mem::take(&mut state.pending)),
                None => {
                    if state.pending.len() >= MAX_PENDING {
                        state.pending.pop_front();
                    }
                    state.pending.push_back(datagram);
                    return;
                }
            }
        };
        for queued in backlog {
            let _ = self.socket.send_to(&queued, peer).await;
        }
        let _ = self.socket.send_to(&datagram, peer).await;
    }

    /// 接收循环：解密后按 ssrc 交给对应会话。
    pub async fn run(self: Arc<Self>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
        let mut buf = vec![0u8; HEADER + MAX_PAYLOAD + 64];
        loop {
            let received = tokio::select! {
                _ = shutdown.wait_for(|stop| *stop) => return,
                received = self.socket.recv_from(&mut buf) => received,
            };
            let Ok((len, from)) = received else {
                continue;
            };
            let datagram = &buf[..len];
            let Some((ssrc, sequence)) = parse_header(datagram) else {
                continue;
            };
            let channel = {
                let channels = self.channels.lock().unwrap_or_else(|p| p.into_inner());
                channels.get(&ssrc).cloned()
            };
            let Some(channel) = channel else {
                continue;
            };
            if from.ip() != channel.device_ip {
                continue;
            }
            let backlog = {
                let mut state = channel.state.lock().unwrap_or_else(|p| p.into_inner());
                if sequence <= state.remote_sequence {
                    continue;
                }
                state.remote_sequence = sequence;
                if state.peer != Some(from) {
                    state.peer = Some(from);
                    std::mem::take(&mut state.pending)
                } else {
                    VecDeque::new()
                }
            };
            let mut iv = [0u8; 16];
            iv.copy_from_slice(&datagram[..HEADER]);
            let mut opus = datagram[HEADER..].to_vec();
            crypt(&channel.key, &iv, &mut opus);
            let _ = channel.incoming.send(LinkEvent::Audio(opus));
            for queued in backlog {
                let _ = self.socket.send_to(&queued, from).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_ctr_matches_nist_sp800_38a_vector() {
        // NIST SP 800-38A F.5.1 CTR-AES128.Encrypt
        let key: [u8; 16] = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let iv: [u8; 16] = [
            0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa, 0xfb, 0xfc, 0xfd,
            0xfe, 0xff,
        ];
        let mut data: Vec<u8> = vec![
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a, 0xae, 0x2d, 0x8a, 0x57, 0x1e, 0x03, 0xac, 0x9c, 0x9e, 0xb7, 0x6f, 0xac,
            0x45, 0xaf, 0x8e, 0x51,
        ];
        crypt(&key, &iv, &mut data);
        assert_eq!(
            data,
            vec![
                0x87, 0x4d, 0x61, 0x91, 0xb6, 0x20, 0xe3, 0x26, 0x1b, 0xef, 0x68, 0x64, 0x99, 0x0d,
                0xb6, 0xce, 0x98, 0x06, 0xf6, 0x6b, 0x79, 0x70, 0xfd, 0xff, 0x86, 0x17, 0x18, 0x7b,
                0xb9, 0xff, 0xfd, 0xff,
            ],
            "第二块要用整块大端递增后的计数器（0xff → 下一块 0x...ff00）"
        );
    }

    /// 唤醒问候比设备第一个上行包先到：先攒着，收到设备的包（知道它的端口）后按顺序补发；
    /// 重放的旧序号丢弃。（只认设备 MQTT 连接同一 IP 的规则在回环地址上测不了。）
    #[tokio::test]
    async fn downlink_waits_for_device_port_then_flushes_in_order() {
        let server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let hub = UdpHub::new(server);
        let port = hub.local_port();
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        tokio::spawn(hub.clone().run(stop_rx));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let channel = hub.open("127.0.0.1".parse().unwrap(), tx);
        hub.send(&channel, b"greet-1").await;
        hub.send(&channel, b"greet-2").await;

        let device = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        device.connect(("127.0.0.1", port)).await.unwrap();
        let first = seal(&channel.key, &channel.nonce, b"mic", 0, 1);
        device.send(&first).await.unwrap();
        match rx.recv().await {
            Some(LinkEvent::Audio(opus)) => assert_eq!(opus, b"mic"),
            _ => panic!("上行音频要交给会话"),
        }
        let mut buf = [0u8; 256];
        for (expected_seq, expected) in [(1u32, &b"greet-1"[..]), (2, &b"greet-2"[..])] {
            let len =
                tokio::time::timeout(std::time::Duration::from_secs(2), device.recv(&mut buf))
                    .await
                    .unwrap()
                    .unwrap();
            let (_, sequence) = parse_header(&buf[..len]).unwrap();
            assert_eq!(sequence, expected_seq);
            let mut iv = [0u8; 16];
            iv.copy_from_slice(&buf[..HEADER]);
            let mut body = buf[HEADER..len].to_vec();
            crypt(&channel.key, &iv, &mut body);
            assert_eq!(body, expected);
        }
        // 重放旧序号：丢弃
        device.send(&first).await.unwrap();
        let replay = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
        assert!(replay.is_err(), "旧序号的包不交给会话");
        let _ = stop_tx.send(true);
    }

    #[test]
    fn sealed_packet_has_firmware_layout_and_decrypts() {
        let key = [7u8; 16];
        let mut nonce = [0u8; 16];
        nonce[0] = 1;
        nonce[4..8].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        let opus = b"opus-frame".to_vec();
        let packet = seal(&key, &nonce, &opus, 120, 3);
        assert_eq!(packet[0], 0x01);
        assert_eq!(
            u16::from_be_bytes([packet[2], packet[3]]),
            opus.len() as u16
        );
        assert_eq!(&packet[4..8], &[0xAA, 0xBB, 0xCC, 0xDD], "ssrc 来自 nonce");
        assert_eq!(u32::from_be_bytes(packet[8..12].try_into().unwrap()), 120);
        assert_eq!(parse_header(&packet), Some(([0xAA, 0xBB, 0xCC, 0xDD], 3)));
        assert_ne!(&packet[HEADER..], opus.as_slice(), "负载已加密");
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&packet[..HEADER]);
        let mut body = packet[HEADER..].to_vec();
        crypt(&key, &iv, &mut body);
        assert_eq!(body, opus);
        let mut short = packet.clone();
        short.pop();
        assert_eq!(parse_header(&short), None, "长度与包头不符");
        let mut wrong_type = packet;
        wrong_type[0] = 2;
        assert_eq!(parse_header(&wrong_type), None);
    }
}
