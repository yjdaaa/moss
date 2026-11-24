// 按使用场景推荐的整套本地模型：每行一句话，点“用这套”一次换好判停 / 降噪 / 识别 / 合成。
// 依据 2026-11-15 整条语音链路连起来的本机实测（技术书 n99）。
import React, { useState } from "react";
import { MODEL_COMBOS, comboActive, comboMissingModels, comboSummary } from "../../voiceState.js";

/**
 * `models`：本地模型清单（看这套还缺什么）；`onDownload(ids)`：下载缺的模型。
 * 2026-11-24 n111：新安装默认就选着第一套，但模型要下载后才生效——正在用的这套还缺模型时如实写“还没下载”，
 * 并给一键下载，不显示“✓ 正在用”。
 */
export function ModelPairingGuide({ settings, models, busy, onApply, onDownload }) {
  const [note, setNote] = useState(null);
  const apply = async (combo) => {
    setNote(null);
    setNote(await onApply(combo));
  };
  return (
    <details className="voice-fold" id="voice-model-pairing-guide">
      <summary>按场景选一套模型（整条链路实测）</summary>
      <ul className="voice-preset-list">
        {MODEL_COMBOS.map((combo) => {
          const active = comboActive(combo, settings);
          const missing = active ? comboMissingModels(combo, models) : [];
          const toDownload = missing.filter((model) => !model.downloading);
          return (
            <li key={combo.id} id={`voice-combo-${combo.id}`} className="voice-combo-row">
              <div>
                <b>{combo.title}</b>
                <span className="voice-muted">（{combo.fit}）</span>
                <div className="voice-muted">
                  {comboSummary(combo)} · {combo.result}
                </div>
              </div>
              {active && missing.length > 0 ? (
                <span className="voice-combo-missing" id={`voice-combo-missing-${combo.id}`}>
                  <span className="voice-muted">
                    已选这套，还没下载：{missing.map((model) => `${model.label}${model.downloading ? "（下载中）" : ""}`).join("、")}
                    ；下载完才用本地模型，之前判停用音量判断、识别与合成用云端
                  </span>
                  {toDownload.length > 0 && (
                    <button
                      type="button"
                      className="btn btn-secondary btn-sm"
                      id={`voice-combo-download-${combo.id}`}
                      disabled={Boolean(busy)}
                      onClick={() => onDownload?.(toDownload.map((model) => model.id))}
                    >
                      下载缺的模型
                    </button>
                  )}
                </span>
              ) : active ? (
                <span className="voice-muted voice-combo-active">✓ 正在用</span>
              ) : (
                <button
                  type="button"
                  className="btn btn-secondary btn-sm"
                  id={`voice-combo-apply-${combo.id}`}
                  disabled={Boolean(busy)}
                  onClick={() => apply(combo)}
                >
                  用这套
                </button>
              )}
            </li>
          );
        })}
      </ul>
      <p className="voice-muted">
        前端降噪（GTCRN / DPDFNet）实测不能让识别更准，这几套都不开；命令词抗噪在噪声大时能多救回少数听错的“停 / 退下”（只放进“车间噪声大”）。有云端密钥时，识别与合成用豆包声音最自然（需联网）；大模型都建议用云端。
      </p>
      {note && (
        <div className={note.ok ? "voice-hint" : "voice-hint voice-hint--warn"} role="status">
          {note.text}
        </div>
      )}
    </details>
  );
}
