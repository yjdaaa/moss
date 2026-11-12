//! 本地语音合成（TTS）模型包的语言包说明、全量音色清单与本机试听 WAV 缓存管理。
//!
//! 设计目标：
//! - 点击任意本地 TTS 合成模型名称即可查看其支持的语言包、全部音色编号/代号/性别及代表性试听句（即使模型尚未下载）；
//! - 模型下载完成（或启动发现已安装但缺试听缓存）后，由桌面端在后台静默把各音色试听语音合成并原子缓存到
//!   `<models_root>/.tts-previews/<model_id>/v1/sid_<speaker>.wav`，点击试听直接秒读本机磁盘；
//! - 删除本地合成模型时同步清理对应的 `.tts-previews/<model_id>` 缓存目录。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::local_models::{ModelKind, spec_of};

/// 试听缓存协议版本：若将来调整默认试听句，递增目录号即可无感失效旧缓存。
pub const PREVIEW_CACHE_VERSION: &str = "v1";

pub const SAMPLE_ZH: &str = "你好，我是 MOSS 语音助手，激光雕刻工作台已就绪。";
pub const SAMPLE_EN_US: &str = "Hello, I am MOSS. Your laser engraving studio is ready.";
pub const SAMPLE_EN_GB: &str = "Good day, I am MOSS, standing by for your next job.";
pub const SAMPLE_ACCENT: &str = "Hello, MOSS studio is ready. 你好，工作台已就绪。";

const KOKORO_V10_CODES: [&str; 54] = [
    "af_alloy",
    "af_aoede",
    "af_bella",
    "af_heart",
    "af_jessica",
    "af_kore",
    "af_nicole",
    "af_nova",
    "af_river",
    "af_sarah",
    "af_sky",
    "am_adam",
    "am_echo",
    "am_eric",
    "am_fenrir",
    "am_liam",
    "am_michael",
    "am_onyx",
    "am_puck",
    "am_santa",
    "bf_alice",
    "bf_emma",
    "bf_isabella",
    "bf_lily",
    "bm_daniel",
    "bm_fable",
    "bm_george",
    "bm_lewis",
    "ef_dora",
    "em_alex",
    "ff_siwis",
    "hf_alpha",
    "hf_beta",
    "hm_omega",
    "hm_psi",
    "if_sara",
    "im_nicola",
    "jf_alpha",
    "jf_gongitsune",
    "jf_nezumi",
    "jf_tebukuro",
    "jm_kumo",
    "pf_dora",
    "pm_alex",
    "pm_santa",
    "zf_xiaobei",
    "zf_xiaoni",
    "zf_xiaoxiao",
    "zf_xiaoyi",
    "zm_yunjian",
    "zm_yunxi",
    "zm_yunxia",
    "zm_yunyang",
    "em_santa",
];

const KOKORO_V11_CODES: [&str; 103] = [
    "af_maple", "af_sol", "bf_vale", "zf_001", "zf_002", "zf_003", "zf_004", "zf_005", "zf_006",
    "zf_007", "zf_008", "zf_017", "zf_018", "zf_019", "zf_021", "zf_022", "zf_023", "zf_024",
    "zf_026", "zf_027", "zf_028", "zf_032", "zf_036", "zf_038", "zf_039", "zf_040", "zf_042",
    "zf_043", "zf_044", "zf_046", "zf_047", "zf_048", "zf_049", "zf_051", "zf_059", "zf_060",
    "zf_067", "zf_070", "zf_071", "zf_072", "zf_073", "zf_074", "zf_075", "zf_076", "zf_077",
    "zf_078", "zf_079", "zf_083", "zf_084", "zf_085", "zf_086", "zf_087", "zf_088", "zf_090",
    "zf_092", "zf_093", "zf_094", "zf_099", "zm_009", "zm_010", "zm_011", "zm_012", "zm_013",
    "zm_014", "zm_015", "zm_016", "zm_020", "zm_025", "zm_029", "zm_030", "zm_031", "zm_033",
    "zm_034", "zm_035", "zm_037", "zm_041", "zm_045", "zm_050", "zm_052", "zm_053", "zm_054",
    "zm_055", "zm_056", "zm_057", "zm_058", "zm_061", "zm_062", "zm_063", "zm_064", "zm_065",
    "zm_066", "zm_068", "zm_069", "zm_080", "zm_081", "zm_082", "zm_089", "zm_091", "zm_095",
    "zm_096", "zm_097", "zm_098", "zm_100",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TtsLanguagePack {
    pub code: &'static str,
    pub label: &'static str,
    pub count: usize,
    pub sample_text: &'static str,
    pub recommended: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TtsSpeakerMeta {
    pub id: i32,
    pub code: &'static str,
    pub name: String,
    pub gender: &'static str,
    pub gender_label: &'static str,
    pub lang: &'static str,
    pub lang_label: &'static str,
    pub sample_text: &'static str,
    pub recommended_for_zh: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TtsModelVoiceCatalog {
    pub model_id: &'static str,
    pub label: &'static str,
    pub note: &'static str,
    pub default_speaker: i32,
    pub languages: Vec<TtsLanguagePack>,
    /// 按日常实用优先级排序（中文普通话在前，其余语种在后；同语种内按音色编号升序）
    pub speakers: Vec<TtsSpeakerMeta>,
}

fn title_case_ascii(raw: &str) -> String {
    let mut chars = raw.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

fn parse_kokoro_code(code: &'static str, id: i32) -> TtsSpeakerMeta {
    let (prefix, stem) = code.split_once('_').unwrap_or(("", code));
    let gender = if prefix.ends_with('f') {
        "female"
    } else {
        "male"
    };
    let gender_label = if gender == "female" {
        "女声"
    } else {
        "男声"
    };

    match prefix {
        "zf" | "zm" => {
            let name = match code {
                "zf_xiaobei" => "晓北".to_owned(),
                "zf_xiaoni" => "晓妮".to_owned(),
                "zf_xiaoxiao" => "晓晓".to_owned(),
                "zf_xiaoyi" => "晓伊".to_owned(),
                "zm_yunjian" => "云健".to_owned(),
                "zm_yunxi" => "云希".to_owned(),
                "zm_yunxia" => "云夏".to_owned(),
                "zm_yunyang" => "云扬".to_owned(),
                _ if stem.chars().all(|c| c.is_ascii_digit()) => {
                    format!("中文{gender_label} {stem}")
                }
                _ => title_case_ascii(stem),
            };
            TtsSpeakerMeta {
                id,
                code,
                name,
                gender,
                gender_label,
                lang: "zh",
                lang_label: "中文普通话（中英混读）",
                sample_text: SAMPLE_ZH,
                recommended_for_zh: true,
            }
        }
        "af" | "am" => TtsSpeakerMeta {
            id,
            code,
            name: title_case_ascii(stem),
            gender,
            gender_label,
            lang: "en-US",
            lang_label: "美式英语 (en-US)",
            sample_text: SAMPLE_EN_US,
            recommended_for_zh: false,
        },
        "bf" | "bm" => TtsSpeakerMeta {
            id,
            code,
            name: title_case_ascii(stem),
            gender,
            gender_label,
            lang: "en-GB",
            lang_label: "英式英语 (en-GB)",
            sample_text: SAMPLE_EN_GB,
            recommended_for_zh: false,
        },
        _ => {
            let origin = match prefix.chars().next().unwrap_or(' ') {
                'e' => "西语声线",
                'f' => "法语声线",
                'h' => "印地语声线",
                'i' => "意语声线",
                'j' => "日语声线",
                'p' => "葡语声线",
                _ => "扩展声线",
            };
            TtsSpeakerMeta {
                id,
                code,
                name: format!("{}（{origin}）", title_case_ascii(stem)),
                gender,
                gender_label,
                lang: "accent",
                lang_label: "多语种扩展声线",
                sample_text: SAMPLE_ACCENT,
                recommended_for_zh: false,
            }
        }
    }
}

fn lang_sort_rank(lang: &str) -> u8 {
    match lang {
        "zh" => 0,
        "en-US" => 1,
        "en-GB" => 2,
        _ => 3,
    }
}

/// 查询指定本地合成模型（`kokoro` / `kokoro_v11` / `matcha_zh_en`）的语言包与全量音色目录。
pub fn tts_voice_catalog(model_id: &str) -> Option<TtsModelVoiceCatalog> {
    let spec = spec_of(model_id, ModelKind::Tts)?;
    let default_speaker = spec.voices.map(|v| v.default).unwrap_or(0);
    let mut speakers: Vec<TtsSpeakerMeta> = match spec.id {
        "kokoro" => KOKORO_V10_CODES
            .iter()
            .enumerate()
            .map(|(idx, code)| parse_kokoro_code(code, idx as i32))
            .collect(),
        "kokoro_v11" => KOKORO_V11_CODES
            .iter()
            .enumerate()
            .map(|(idx, code)| parse_kokoro_code(code, idx as i32))
            .collect(),
        "matcha_zh_en" => vec![TtsSpeakerMeta {
            id: 0,
            code: "matcha_zh_en",
            name: "标准中英女声".to_owned(),
            gender: "female",
            gender_label: "女声",
            lang: "zh",
            lang_label: "中文普通话（中英混读）",
            sample_text: SAMPLE_ZH,
            recommended_for_zh: true,
        }],
        _ => return None,
    };

    speakers.sort_by_key(|s| (lang_sort_rank(s.lang), s.id));

    let count_lang = |code: &str| speakers.iter().filter(|s| s.lang == code).count();
    let mut languages = Vec::new();
    let zh_count = count_lang("zh");
    if zh_count > 0 {
        languages.push(TtsLanguagePack {
            code: "zh",
            label: "中文普通话（中英混读）",
            count: zh_count,
            sample_text: SAMPLE_ZH,
            recommended: true,
            note: Some("内置中文词典与数字/日期读法规则，日常中文与英文术语混读最自然"),
        });
    }
    let us_count = count_lang("en-US");
    if us_count > 0 {
        languages.push(TtsLanguagePack {
            code: "en-US",
            label: "美式英语 (en-US)",
            count: us_count,
            sample_text: SAMPLE_EN_US,
            recommended: false,
            note: Some("内置美式英语词典，适合纯英文播报；直接读中文会带外语口音"),
        });
    }
    let gb_count = count_lang("en-GB");
    if gb_count > 0 {
        languages.push(TtsLanguagePack {
            code: "en-GB",
            label: "英式英语 (en-GB)",
            count: gb_count,
            sample_text: SAMPLE_EN_GB,
            recommended: false,
            note: Some("英式发音声线，适合英文播报；直接读中文会带外语口音"),
        });
    }
    let accent_count = count_lang("accent");
    if accent_count > 0 {
        languages.push(TtsLanguagePack {
            code: "accent",
            label: "多语种扩展声线（西/法/印/意/日/葡）",
            count: accent_count,
            sample_text: SAMPLE_ACCENT,
            recommended: false,
            note: Some(
                "权重自带的多国发音人声线；当前离线引擎加载中英双语词库，读中文会带明显外语腔调",
            ),
        });
    }

    Some(TtsModelVoiceCatalog {
        model_id: spec.id,
        label: spec.label,
        note: spec.note,
        default_speaker,
        languages,
        speakers,
    })
}

/// 查找单个音色的定义与代表性试听句。
pub fn tts_speaker_meta(model_id: &str, speaker: i32) -> Option<TtsSpeakerMeta> {
    let catalog = tts_voice_catalog(model_id)?;
    catalog.speakers.into_iter().find(|s| s.id == speaker)
}

/// 后台自动预缓存顺序：默认音色第一 → 其余中文音色 → 英语及多语种音色。
pub fn tts_warmup_plan(model_id: &str) -> Vec<TtsSpeakerMeta> {
    let Some(mut catalog) = tts_voice_catalog(model_id) else {
        return Vec::new();
    };
    let default_id = catalog.default_speaker;
    catalog.speakers.sort_by_key(|s| {
        let is_default = if s.id == default_id { 0u8 } else { 1u8 };
        (is_default, lang_sort_rank(s.lang), s.id)
    });
    catalog.speakers
}

/// 指定模型试听缓存根目录：`<models_root>/.tts-previews/<model_id>`。
pub fn model_preview_root(models_root: &Path, model_id: &str) -> Option<PathBuf> {
    let spec = spec_of(model_id, ModelKind::Tts)?;
    Some(models_root.join(".tts-previews").join(spec.id))
}

/// 单个音色试听 WAV 文件路径：`<models_root>/.tts-previews/<model_id>/v1/sid_<speaker>.wav`。
pub fn preview_wav_path(models_root: &Path, model_id: &str, speaker: i32) -> Option<PathBuf> {
    let root = model_preview_root(models_root, model_id)?;
    tts_speaker_meta(model_id, speaker)?;
    Some(
        root.join(PREVIEW_CACHE_VERSION)
            .join(format!("sid_{speaker}.wav")),
    )
}

fn looks_like_valid_wav(bytes: &[u8]) -> bool {
    bytes.len() > 128 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WAVE"
}

pub fn has_cached_preview(models_root: &Path, model_id: &str, speaker: i32) -> bool {
    let Some(path) = preview_wav_path(models_root, model_id, speaker) else {
        return false;
    };
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let Ok(meta) = file.metadata() else {
        return false;
    };
    if meta.len() <= 128 {
        return false;
    }
    use std::io::Read as _;
    let mut header = [0u8; 12];
    file.read_exact(&mut header).is_ok() && &header[0..4] == b"RIFF" && &header[8..12] == b"WAVE"
}

pub fn read_cached_preview(models_root: &Path, model_id: &str, speaker: i32) -> Option<Vec<u8>> {
    let path = preview_wav_path(models_root, model_id, speaker)?;
    let bytes = fs::read(path).ok()?;
    looks_like_valid_wav(&bytes).then_some(bytes)
}

pub fn write_cached_preview_atomic(
    models_root: &Path,
    model_id: &str,
    speaker: i32,
    wav_bytes: &[u8],
) -> Result<PathBuf, String> {
    if !looks_like_valid_wav(wav_bytes) {
        return Err("试听 WAV 数据无效或过短".into());
    }
    let dst = preview_wav_path(models_root, model_id, speaker)
        .ok_or_else(|| "未知的合成模型或音色编号".to_owned())?;
    let parent = dst
        .parent()
        .ok_or_else(|| "试听缓存目录路径无效".to_owned())?;
    fs::create_dir_all(parent).map_err(|e| format!("创建试听缓存目录失败：{e}"))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = parent.join(format!(
        "sid_{speaker}.wav.tmp.{}_{nanos}",
        std::process::id()
    ));
    fs::write(&tmp, wav_bytes).map_err(|e| format!("写入试听缓存临时文件失败：{e}"))?;
    if let Err(e) = fs::rename(&tmp, &dst) {
        let _ = fs::remove_file(&tmp);
        return Err(format!("落盘试听缓存文件失败：{e}"));
    }
    Ok(dst)
}

pub fn remove_model_previews(models_root: &Path, model_id: &str) {
    if let Some(dir) = model_preview_root(models_root, model_id) {
        let _ = fs::remove_dir_all(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_models::{CATALOG, ModelKind};

    #[test]
    fn every_tts_model_in_catalog_has_complete_voice_and_language_metadata() {
        for spec in CATALOG.iter().filter(|s| s.kind == ModelKind::Tts) {
            let cat = tts_voice_catalog(spec.id).expect(spec.id);
            assert_eq!(cat.model_id, spec.id);
            assert!(!cat.languages.is_empty());
            let expected_total = spec.voices.map(|v| (v.max + 1) as usize).unwrap_or(1);
            assert_eq!(
                cat.speakers.len(),
                expected_total,
                "{} 音色总数须与 ModelSpec::voices 对齐",
                spec.id
            );
            let sum_by_lang: usize = cat.languages.iter().map(|l| l.count).sum();
            assert_eq!(sum_by_lang, expected_total);
            // 中文普通话始终排在首位，预热计划把默认音色放在第 0 位
            assert_eq!(cat.languages[0].code, "zh");
            assert_eq!(cat.speakers[0].lang, "zh");
            let warmup = tts_warmup_plan(spec.id);
            assert_eq!(warmup.len(), expected_total);
            assert_eq!(warmup[0].id, cat.default_speaker);
        }
        assert!(tts_voice_catalog("silero_vad").is_none());
        assert!(tts_voice_catalog("../evil").is_none());
    }

    #[test]
    fn preview_cache_roundtrip_validates_wav_and_cleans_up_on_remove() {
        let dir = tempfile::tempdir().unwrap();
        let mut fake_wav = Vec::with_capacity(200);
        fake_wav.extend_from_slice(b"RIFF");
        fake_wav.extend_from_slice(&(192u32).to_le_bytes());
        fake_wav.extend_from_slice(b"WAVE");
        fake_wav.resize(200, 0u8);

        assert!(!has_cached_preview(dir.path(), "kokoro", 45));
        write_cached_preview_atomic(dir.path(), "kokoro", 45, &fake_wav).unwrap();
        assert!(has_cached_preview(dir.path(), "kokoro", 45));
        assert_eq!(
            read_cached_preview(dir.path(), "kokoro", 45).unwrap().len(),
            200
        );

        // 拒绝越界音色与损坏 WAV
        assert!(write_cached_preview_atomic(dir.path(), "kokoro", 999, &fake_wav).is_err());
        assert!(write_cached_preview_atomic(dir.path(), "kokoro", 46, b"not-a-wav").is_err());

        remove_model_previews(dir.path(), "kokoro");
        assert!(!has_cached_preview(dir.path(), "kokoro", 45));
    }
}
