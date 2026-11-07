//! P4-06 / A-67 离线校验的生成端：用语音中心真实的 `config_yaml::edits_for` + `apply`
//! 把三分区（LLM / ASR / TTS）与自建接入点写进给定的 `data/.config.yaml` 副本。
//!
//! 只读输入文件、只写输出文件；使用假地址与假密钥，不访问网络。
//! 用法：`cargo run -p moss-voice --example p4_06_config_yaml -- <输入.yaml> <输出.yaml>`
//! 输入为全模块部署（含 `manager-api.url`）时按语音中心规则拒绝写入，退出码 2。
//! 校验端见 `scripts/verify-p4-06-upstream-config.py`。

use moss_voice::config_yaml::{apply, edits_for, is_full_module};
use moss_voice::models::ResolvedSection;
use moss_voice::{ProviderKind, Section};

/// 校验端据此确认写入值，也确认密钥没有被改写成别的值。
pub const FAKE_KEY: &str = "sk-moss-p4-06-fake-key";

fn section(section: Section, url: &str, model: &str, voice: &str) -> ResolvedSection {
    ResolvedSection {
        section,
        kind: ProviderKind::Openai,
        url: url.to_owned(),
        api_key: FAKE_KEY.to_owned(),
        model: model.to_owned(),
        voice: voice.to_owned(),
        speed: 1.25,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("用法：p4_06_config_yaml <输入.yaml> <输出.yaml>");
        std::process::exit(64);
    };
    let original = std::fs::read_to_string(input).unwrap_or_else(|error| {
        eprintln!("读取 {input} 失败：{error}");
        std::process::exit(66);
    });
    if is_full_module(&original) {
        println!("refused: full_module");
        std::process::exit(2);
    }
    let sections = [
        section(
            Section::Llm,
            "https://llm.example.invalid/v1",
            "moss-llm-model",
            "",
        ),
        section(
            Section::Asr,
            "https://asr.example.invalid/v1/audio/transcriptions",
            "moss-asr-model",
            "",
        ),
        section(
            Section::Tts,
            "https://tts.example.invalid/v1/audio/speech",
            "moss-tts-model",
            "moss-voice",
        ),
    ];
    let edits = edits_for(
        &sections,
        Some("ws://127.0.0.1:8004/mcp_endpoint/mcp/?token=placeholder"),
    );
    let updated = apply(&original, &edits).unwrap_or_else(|error| {
        eprintln!("写入失败：{error}");
        std::process::exit(1);
    });
    std::fs::write(output, updated).unwrap_or_else(|error| {
        eprintln!("写出 {output} 失败：{error}");
        std::process::exit(73);
    });
    println!("written: {output}");
}
