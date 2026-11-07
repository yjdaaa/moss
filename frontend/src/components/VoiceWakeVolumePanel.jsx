import React, { useState } from "react";
import { Volume2, VolumeX, Mic, ChevronRight, Lock } from "lucide-react";
import { showAlert } from "../appDialog.js";
import { voiceApi } from "../voiceApi.js";
import { volumeDevicesView, volumeLevelText, voiceErrorText } from "../voiceState.js";

// 2026-11-07 用户决定：唤醒词从“设备维护”里单独拿出来，与 MOSS 说话音量放在同一个标签。
// 音量走主窗口已有的设备即时请求（set_volume，重连自动保持）；唤醒词会重启设备并写闪存，
// 仍只在打包私密窗口里修改，这里只放入口。
export default function VoiceWakeVolumePanel({ builtin, mode }) {
  if (mode !== "builtin") {
    return (
      <div className="voice-hint" id="voice-wake-volume-mode-hint">
        唤醒词和音量要通过 MOSS 内置语音服务下发到小智设备。请先在“服务”标签把语音链路切换为“MOSS 内置服务”。
      </div>
    );
  }
  return (
    <div className="wake-volume" id="voice-wake-volume-panel">
      <VolumeCard status={builtin?.status} />
      <WakeWordEntry />
    </div>
  );
}

function VolumeCard({ status }) {
  const [drafts, setDrafts] = useState({});
  const [sending, setSending] = useState("");
  const [notice, setNotice] = useState({});
  if (!status) return <section className="wake-volume-card"><p className="voice-muted">正在读取内置服务状态…</p></section>;
  const rows = volumeDevicesView(status);

  const apply = async (row, volume) => {
    const value = Math.max(0, Math.min(100, Math.round(volume)));
    setDrafts((old) => ({ ...old, [row.device_id]: value }));
    setSending(row.device_id);
    setNotice((old) => ({ ...old, [row.device_id]: "" }));
    try {
      await voiceApi.deviceRequest(row.device_id, "set_volume", { volume: value });
      setNotice((old) => ({ ...old, [row.device_id]: value === 0 ? "已静音" : `已设为 ${value}%` }));
    } catch (error) {
      setDrafts((old) => { const next = { ...old }; delete next[row.device_id]; return next; });
      await showAlert(voiceErrorText(error), { title: "调节音量失败", tone: "error" });
    } finally {
      setSending("");
    }
  };

  return (
    <section className="wake-volume-card" aria-labelledby="voice-volume-title">
      <header className="wake-volume-card-head">
        <span className="wake-volume-icon" aria-hidden="true"><Volume2 size={18} /></span>
        <div>
          <h3 id="voice-volume-title">MOSS 说话音量</h3>
          <p>MOSS 的声音从小智设备的扬声器播出。拖动后松手即生效，设备断电重连后自动保持。</p>
        </div>
      </header>
      {!status.running && <div className="voice-hint">内置语音服务未运行，设备上线后才能调节。</div>}
      {rows.length === 0 ? (
        <p className="voice-muted wake-volume-empty">还没有已允许的设备。在“服务”标签允许设备连接后，这里会列出它们。</p>
      ) : (
        <ul className="wake-volume-list" aria-label="设备音量">
          {rows.map((row) => {
            const value = drafts[row.device_id] ?? row.volume;
            const disabled = !row.adjustable || sending === row.device_id;
            return (
              <li key={row.device_id} className="wake-volume-row" data-online={row.online ? "true" : "false"}>
                <div className="wake-volume-row-head">
                  <span className="wake-volume-dot" aria-hidden="true" />
                  <strong>{row.name}</strong>
                  <small>{row.online ? "在线" : "离线"}</small>
                  <span className="wake-volume-value mono" aria-live="polite">
                    {value}%<em>{volumeLevelText(value)}</em>
                  </span>
                </div>
                <div className="wake-volume-row-control">
                  <button
                    type="button"
                    className="wake-volume-mute"
                    disabled={disabled}
                    onClick={() => void apply(row, value === 0 ? 60 : 0)}
                    aria-label={value === 0 ? `取消静音 ${row.name}` : `静音 ${row.name}`}
                    title={value === 0 ? "取消静音（恢复到 60%）" : "静音"}
                  >
                    {value === 0 ? <VolumeX size={16} /> : <Volume2 size={16} />}
                  </button>
                  <input
                    type="range"
                    className="wake-volume-slider"
                    id={`voice-volume-${row.device_id}`}
                    min="0"
                    max="100"
                    step="5"
                    value={value}
                    disabled={disabled}
                    style={{ "--fill": `${value}%` }}
                    aria-label={`${row.name} 音量`}
                    onChange={(event) => setDrafts((old) => ({ ...old, [row.device_id]: Number(event.target.value) }))}
                    onPointerUp={(event) => void apply(row, Number(event.currentTarget.value))}
                    onKeyUp={(event) => {
                      if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End", "PageUp", "PageDown"].includes(event.key)) {
                        void apply(row, Number(event.currentTarget.value));
                      }
                    }}
                  />
                </div>
                {(row.hint || notice[row.device_id]) && (
                  <p className="wake-volume-note" role="status">{row.hint || notice[row.device_id]}</p>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

function WakeWordEntry() {
  const open = async () => {
    try {
      await voiceApi.openPrivateSettings("wakeword");
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "无法打开唤醒词设置", tone: "error" });
    }
  };
  return (
    <section className="wake-volume-card" aria-labelledby="voice-wakeword-entry-title">
      <header className="wake-volume-card-head">
        <span className="wake-volume-icon" aria-hidden="true"><Mic size={18} /></span>
        <div>
          <h3 id="voice-wakeword-entry-title">唤醒词</h3>
          <p>默认“你好小智”，可以改成“你好 MOSS”等。唤醒词由设备自己识别，改好后推送给设备，设备会重启并写入新的唤醒模型。</p>
        </div>
      </header>
      <button type="button" className="wake-volume-entry" id="voice-wakeword-open" onClick={() => void open()}>
        <span>
          <strong>修改唤醒词</strong>
          <small><Lock size={12} aria-hidden="true" /> 会写入设备闪存，在本机安全窗口里逐台确认</small>
        </span>
        <ChevronRight size={18} aria-hidden="true" />
      </button>
    </section>
  );
}
