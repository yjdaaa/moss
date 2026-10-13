//! NV14 fake WebSocket device: no actual device, microphone, model or songs.
use super::*;
use crate::music::{MusicIndex, MusicTools};
use std::{fs::File, io::Write, path::Path};

fn fake_wav(dir: &Path, seconds: u32) {
    let rate = 24_000u32;
    let frames = rate * seconds;
    let mut file = File::create(dir.join("合成歌曲.wav")).unwrap();
    let mut header = Vec::new();
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&1u16.to_le_bytes());
    header.extend_from_slice(&rate.to_le_bytes());
    header.extend_from_slice(&(rate * 2).to_le_bytes());
    header.extend_from_slice(&2u16.to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&(frames * 2).to_le_bytes());
    file.write_all(&header).unwrap();
    let pcm = vec![2500i16; frames as usize];
    for sample in pcm {
        file.write_all(&sample.to_le_bytes()).unwrap();
    }
}
fn start_song(
    seconds: u32,
) -> (
    VoiceServer,
    Arc<FakeRegistry>,
    tempfile::TempDir,
    Arc<FakeTts>,
) {
    let dir = tempfile::tempdir().unwrap();
    fake_wav(dir.path(), seconds);
    let index = Arc::new(MusicIndex::scan(dir.path()).unwrap());
    let tools = Arc::new(MusicTools::new(
        Arc::new(FakeTools::default()),
        index.clone(),
    ));
    let llm = Arc::new(FakeLlm {
        rounds: Mutex::new(VecDeque::from([Round::Reply {
            deltas: vec![],
            calls: vec![ToolCall::new(
                "music-1",
                "play_music",
                r#"{"query":"合成歌曲"}"#,
            )],
            delay_ms: 0,
        }])),
        ..Default::default()
    });
    let asr = Arc::new(FakeAsr {
        replies: Mutex::new(VecDeque::from([Ok("播放合成歌曲".to_owned())])),
        ..Default::default()
    });
    let tts = Arc::new(FakeTts::default());
    let conversation = Conversation::new(
        asr,
        llm,
        tts.clone(),
        tools,
        ConversationSettings::default(),
    );
    let registry = Arc::new(FakeRegistry::default());
    let server = VoiceServer::start(
        ServerConfig {
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
            ws_port: 0,
            http_port: 0,
            mode: ServerMode::Chat,
            conversation: Some(Arc::new(conversation)),
            music: Some(index),
            playback_start_ms: 100,
            ..Default::default()
        },
        registry.clone(),
    )
    .unwrap();
    (server, registry, dir, tts)
}
#[test]
fn fake_xiaozhi_listen_choose_music_and_receive_opus_without_tts() {
    let (server, registry, _dir, tts) = start_song(1);
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 260);
    let messages = collect_items(&mut device);
    let states = texts(&messages);
    assert!(states.iter().any(|item| item == "tts:start"), "{states:?}");
    assert!(states.iter().any(|item| item == "tts:stop"), "{states:?}");
    let frame_count = messages
        .iter()
        .filter(|item| matches!(item, Item::Frame))
        .count();
    assert!(
        (16..=18).contains(&frame_count),
        "1s PCM -> 60ms Opus frame count {frame_count}"
    );
    assert!(
        tts.said.lock().unwrap().is_empty(),
        "music PCM must not be spoken through cloud TTS"
    );
}

#[test]
fn revoking_device_during_fake_song_closes_session_and_stops_frames() {
    let (server, registry, _dir, _tts) = start_song(5);
    let mut device = chat_device(&server, &registry, 1);
    device
        .get_mut()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    speak_manual(&mut device, 1, 260);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        assert!(Instant::now() < deadline, "no first music frame");
        match device.read() {
            Ok(Message::Binary(_)) => break,
            Ok(_) => {}
            Err(error) => panic!("failed before first frame: {error}"),
        }
    }
    registry.set(DEVICE, DeviceStatus::Revoked);
    let mut frames = 0;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        match device.read() {
            Ok(Message::Binary(_)) => frames += 1,
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
        assert!(frames < 40, "revoked device continued receiving full song");
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while !server.status().sessions.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        server.status().sessions.is_empty(),
        "revoked fake device must be disconnected"
    );
}
#[test]
fn abort_during_fake_xiaozhi_music_stops_before_full_song() {
    let (server, registry, _dir, _tts) = start_song(5);
    let mut device = chat_device(&server, &registry, 1);
    speak_manual(&mut device, 1, 260);
    let mut before = 0;
    loop {
        match device.read().unwrap() {
            Message::Binary(_) => {
                before += 1;
                break;
            }
            Message::Text(text) if text.contains(r#""state":"stop""#) => {
                panic!("song stopped before first audio frame")
            }
            _ => {}
        }
    }
    send_json(&mut device, r#"{"type":"abort","reason":"test-only"}"#);
    let after = collect_items(&mut device);
    let frames = after
        .iter()
        .filter(|item| matches!(item, Item::Frame))
        .count();
    assert!(
        before + frames < 40,
        "abort must not drain all ~84 frames, got {}",
        before + frames
    );
    assert!(texts(&after).iter().any(|item| item == "tts:stop"));
    assert!(
        server
            .status()
            .events
            .iter()
            .any(|item| item.text.contains("播放被打断"))
    );
}
