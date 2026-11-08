use super::*;
use crate::wake_assets::{
    ASSETS_MAX_BYTES, WakeTarget, build_package_with_screen, pack_assets, parse_assets,
};
use serde_json::Value;

fn index_of(package: &[u8]) -> (Vec<AssetEntry>, Value) {
    let entries = parse_assets(package).expect("资源包能解析");
    let index = entries
        .iter()
        .find(|e| e.name == "index.json")
        .map(|e| serde_json::from_slice::<Value>(&e.data).expect("index.json 是 JSON"))
        .expect("有 index.json");
    (entries, index)
}

#[test]
fn every_scene_renders_a_full_screen_looping_gif() {
    for style in [ScreenCardStyle::MossHud, ScreenCardStyle::GoggleBot] {
        for scene in SCENES {
            let gif = scene_gif(style, scene, logical_size((320, 240)), true).expect("能编码");
            assert_eq!(&gif[..3], b"GIF", "{style:?} {scene:?}");
            let decoded = image::load_from_memory(&gif).expect("能解码").to_rgb8();
            assert_eq!(
                decoded.dimensions(),
                (320, 240),
                "全屏尺寸 {style:?} {scene:?}"
            );
        }
    }
}

#[test]
fn frames_actually_animate() {
    for style in [ScreenCardStyle::MossHud, ScreenCardStyle::GoggleBot] {
        for scene in SCENES {
            let a = render_scene(style, scene, 0, 160, 120);
            let b = render_scene(style, scene, 1, 160, 120);
            let c = render_scene(style, scene, 2, 160, 120);
            assert!(a != b || b != c, "{style:?} {scene:?} 应该有动画");
        }
    }
}

#[test]
fn extras_cover_both_styles_and_alias_standard_emotions() {
    let extras = build_screen_extras(ScreenCardStyle::GoggleBot, (320, 240)).expect("能生成");
    let names = emoji_names(&extras);
    for scene in SCENES {
        for style in [ScreenCardStyle::MossHud, ScreenCardStyle::GoggleBot] {
            assert!(
                names.contains(&emotion_name(style, scene)),
                "{style:?} {scene:?}"
            );
        }
    }
    let file_of = |name: &str| {
        extras
            .emoji_collection
            .iter()
            .find(|e| e["name"] == name)
            .and_then(|e| e["file"].as_str())
            .map(str::to_owned)
    };
    // 固件待命/聆听固定显示 neutral：指向刷包画风的待命画面
    assert_eq!(file_of("neutral").as_deref(), Some("bot_idle.gif"));
    assert_eq!(file_of("thinking").as_deref(), Some("bot_think.gif"));
    assert_eq!(file_of("crying").as_deref(), Some("bot_sad.gif"));
    for e in &extras.emoji_collection {
        let file = e["file"].as_str().unwrap();
        assert!(
            extras.entries.iter().any(|x| x.name == file),
            "{file} 要在包里"
        );
        assert!(file.len() < 32, "文件名不能超过资源表 32 字节");
    }
    let backdrop = extras
        .entries
        .iter()
        .find(|e| e.name == BACKDROP_FILE)
        .expect("有整屏背景");
    assert_eq!(backdrop.data.len(), 28 + 320 * 240 * 2);
    assert_eq!(backdrop.data[0], 0x19, "LVGL 图片魔数");
    assert_eq!(backdrop.data[1], 0x12, "RGB565");
    assert_eq!(
        u16::from_le_bytes([backdrop.data[4], backdrop.data[5]]),
        320
    );
    assert_eq!(
        u16::from_le_bytes([backdrop.data[6], backdrop.data[7]]),
        240
    );
    let skin = extras.skin.as_ref().expect("有皮肤");
    assert_eq!(skin["dark"]["background_image"], BACKDROP_FILE);
    assert_eq!(skin["light"]["background_image"], BACKDROP_FILE);
    let total: usize = extras.entries.iter().map(|e| e.data.len()).sum();
    eprintln!("全屏画风资源合计 {:.2} MB", total as f64 / 1048576.0);
    assert!(total < 3 * 1024 * 1024, "画风资源 {total} 字节，过大");
}

#[test]
fn package_keeps_base_font_and_other_emoji_but_overrides_same_names() {
    let base_index = serde_json::json!({
        "version": 1,
        "text_font": "font.bin",
        "emoji_collection": [
            {"name": "neutral", "file": "old_neutral.png"},
            {"name": "custom_wave", "file": "wave.png"}
        ],
        "skin": {"dark": {"text_color": "#000000"}}
    });
    let base = pack_assets(&[
        AssetEntry {
            name: "font.bin".into(),
            data: vec![1, 2, 3],
            width: 0,
            height: 0,
        },
        AssetEntry {
            name: "old_neutral.png".into(),
            data: vec![4],
            width: 0,
            height: 0,
        },
        AssetEntry {
            name: "wave.png".into(),
            data: vec![5],
            width: 0,
            height: 0,
        },
        AssetEntry {
            name: "index.json".into(),
            data: serde_json::to_vec(&base_index).unwrap(),
            width: 0,
            height: 0,
        },
    ])
    .unwrap();
    let extras = build_screen_extras(ScreenCardStyle::MossHud, (320, 240)).unwrap();
    let package = build_package_with_screen(Some(&base), &WakeTarget::Default, Some(&extras))
        .expect("能打包");
    assert!(package.len() <= ASSETS_MAX_BYTES);
    let (entries, index) = index_of(&package);
    assert_eq!(index["text_font"], "font.bin", "保留基础包字体");
    assert!(entries.iter().any(|e| e.name == "font.bin"));
    let collection = index["emoji_collection"].as_array().unwrap();
    let neutral: Vec<_> = collection
        .iter()
        .filter(|e| e["name"] == "neutral")
        .collect();
    assert_eq!(neutral.len(), 1, "同名表情只留一个");
    assert_eq!(neutral[0]["file"], "hud_idle.gif");
    assert!(
        collection.iter().any(|e| e["name"] == "custom_wave"),
        "保留其余表情"
    );
    assert_eq!(index["skin"]["dark"]["background_image"], BACKDROP_FILE);
    assert!(index.get("srmodels").is_some(), "唤醒模型照旧");
}

#[test]
fn package_without_screen_has_no_emoji() {
    let package = build_package_with_screen(None, &WakeTarget::Default, None).unwrap();
    let (_, index) = index_of(&package);
    assert!(index.get("emoji_collection").is_none());
    assert!(index.get("skin").is_none());
}

#[test]
fn speaking_emotions_map_to_scenes_and_markers_parse() {
    assert_eq!(speaking_scene("thinking"), BotPose::Thinking);
    assert_eq!(speaking_scene("laughing"), BotPose::Happy);
    assert_eq!(speaking_scene("crying"), BotPose::Sad);
    assert_eq!(speaking_scene("shocked"), BotPose::Surprised);
    assert_eq!(speaking_scene("neutral"), BotPose::Talking);
    assert_eq!(
        emotion_name(ScreenCardStyle::MossHud, BotPose::Listening),
        "hud_listen"
    );
    assert!(marker_installed(&pack_marker(ScreenCardStyle::GoggleBot)));
    assert!(!marker_installed("moss-screen-0:moss_hud"));
    assert!(!marker_installed(""));
}

#[test]
fn other_screen_sizes_scale_with_the_screen() {
    let extras = build_screen_extras(ScreenCardStyle::MossHud, (240, 240)).unwrap();
    let gif = &extras
        .entries
        .iter()
        .find(|e| e.name == "hud_listen.gif")
        .unwrap()
        .data;
    let decoded = image::load_from_memory(gif).unwrap().to_rgb8();
    assert_eq!(decoded.dimensions(), (240, 240));
}
