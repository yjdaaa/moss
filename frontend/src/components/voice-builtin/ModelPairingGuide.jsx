// 本地 / 云端语音模型搭配推荐（静态说明，不改任何设置）。
// 数字来自 2026-11-14 本机实测（12 逻辑核 Mac；识别用经过小智上行编解码的同一批音频，见技术书 n95），
// 换电脑后速度会不同。
import React from "react";

const PROFILES = [
  {
    id: "balanced",
    title: "普通电脑：推荐的默认搭配（说话最顺）",
    picks: [
      ["识别", "SenseVoice 2024 版：实测最准也最快（字错率约 5.6%，7 秒的话约 0.15 秒认完，带标点）"],
      ["合成", "Kokoro v1.0：第一声约 0.15–0.25 秒，整段回答不停顿"],
      ["大模型", "云端 OpenAI 兼容模型（首字快、工具调用稳）"],
    ],
    note: "嫌 v1.0 的中文音色不够好听时，换下面“音色优先”的搭配。",
  },
  {
    id: "voice",
    title: "音色优先：声音更自然",
    picks: [
      ["识别", "SenseVoice 2024 版"],
      ["合成", "Kokoro v1.1 int8：中文音色多（3–57 女声、58–102 男声），先在“语言包与试听”里挑一个，语速 0.9–0.95"],
      ["大模型", "云端模型"],
    ],
    note: "实测比 v1.0 慢：第一声约 0.3–0.6 秒，“好的，”这类很短的开场白之后可能停 0.2–1 秒，句子中间不停。",
  },
  {
    id: "fast",
    title: "低配电脑：回应最快",
    picks: [
      ["识别", "SenseVoice 2024 版"],
      ["合成", "Matcha（1 秒语音约 0.04 秒算完）"],
      ["大模型", "云端模型"],
    ],
    note: "代价是 Matcha 只有一个 16 kHz 女声，音色偏平。",
  },
  {
    id: "cloud",
    title: "有云端密钥：音质与速度都优先",
    picks: [
      ["识别", "豆包流式识别（本地 SenseVoice 作断网兜底）"],
      ["合成", "豆包双向流式合成；想用自己的声音可开 Colab 联动（云端克隆）"],
      ["大模型", "云端模型"],
    ],
    note: "依赖网络和账号费用，说的话会发到云端。",
  },
];

const TIPS = [
  "Qwen3-ASR、FireRedASR2 在公开榜单上更准，但本机实测都不如 SenseVoice 2024：Qwen3-ASR 字错率 8.4%（填了热词 7.0%）、FireRedASR2 9.8% 且不带标点，识别都慢 5–7 倍，模型约 0.5–0.9 GB。口音或方言重、SenseVoice 总认错时再试 Qwen3-ASR，并在热词里填上常用材料名。",
  "SenseVoice 2025 版是粤语增强版、不带标点，说普通话请选 2024 版。",
  "听起来“像机器在念”时：先换 Kokoro v1.1 并挑音色；说话角色不要选“MOSS”，那个角色本来就是平稳宣读的语气。",
  "风扇、气泵噪声大时开“本地前端降噪”；说话中间常停顿思考时开“本地语义判停”。",
];

export function ModelPairingGuide() {
  return (
    <details className="voice-fold" id="voice-model-pairing-guide">
      <summary>怎么搭配模型：按电脑配置推荐（本机实测）</summary>
      <div className="voice-stack">
        {PROFILES.map((profile) => (
          <div className="voice-local-group" key={profile.id} id={`voice-pairing-${profile.id}`}>
            <div className="voice-field-head">
              <b>{profile.title}</b>
            </div>
            <ul className="voice-muted">
              {profile.picks.map(([part, pick]) => (
                <li key={part}>
                  {part}：{pick}
                </li>
              ))}
            </ul>
            <p className="voice-muted">{profile.note}</p>
          </div>
        ))}
        <ul className="voice-muted">
          {TIPS.map((tip) => (
            <li key={tip}>{tip}</li>
          ))}
        </ul>
      </div>
    </details>
  );
}
