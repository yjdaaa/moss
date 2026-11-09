import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
import { showConfirm } from "../appDialog.js";
import { WAKE_WORD_PRESETS, screenPackDeviceView, wakeWordDraftError } from "../voiceState.js";

// n61 设备唤醒词（2026-11-06 用户决定：唤醒词改为 MOSS，并能在设置里修改）。
// 唤醒词由小智设备自己识别；MOSS 生成资源包，逐台确认后让设备重启、自己下载写入资源分区。
export default function VoiceWakeWordCard() {
  const [data, setData] = useState(null);
  const [draft, setDraft] = useState(null);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  // n70 推送时默认一并刷入 MOSS 全屏画风（只对报了彩屏分辨率的设备生效）
  const [withScreen, setWithScreen] = useState(true);
  const refresh = async (resetDraft = false) => {
    try {
      const next = await voiceApi.wakewordStatus();
      setData(next);
      setDraft((old) => (resetDraft || !old ? { ...next.word } : old));
      setError("");
    } catch (e) { setError(e.message); }
  };
  useEffect(() => {
    void refresh(true);
    const timer = setInterval(() => refresh(false), 5000);
    return () => clearInterval(timer);
  }, []);
  const run = async (work, done) => {
    setBusy(true); setError(""); setMessage("");
    try { const next = await work(); if (next) setData(next); if (done) setMessage(typeof done === "function" ? done(next) : done); return next; }
    catch (e) { setError(e.message); return null; }
    finally { setBusy(false); }
  };
  const saved = data?.word;
  const dirty = Boolean(draft && saved) && (draft.text !== saved.text || draft.command.trim().toLowerCase().split(/\s+/).join(" ") !== saved.command || Number(draft.threshold) !== saved.threshold);
  const draftError = draft ? wakeWordDraftError(draft) : "";
  const save = async () => {
    const next = await run(() => voiceApi.wakewordManage({ action: "save", text: draft.text, command: draft.command, threshold: Number(draft.threshold) }), "已保存唤醒词；还需要推送到设备才会生效");
    if (next) setDraft({ ...next.word });
  };
  const importBase = async () => {
    setBusy(true); setError(""); setMessage("");
    try {
      const summary = await voiceApi.wakewordImportBase();
      if (summary) setMessage(`已导入基础资源包：${summary.files} 个文件${summary.text_font ? `，字体 ${summary.text_font}` : ""}${summary.emoji_count ? `，${summary.emoji_count} 个表情` : ""}`);
      await refresh(false);
    } catch (e) { setError(e.message); } finally { setBusy(false); }
  };
  const clearBase = async () => {
    if (!(await showConfirm("移除基础资源包？之后推送的资源包只含唤醒词，设备屏幕的字体和表情会回到固件自带的。", { title: "移除基础资源包", danger: true, confirmText: "移除" }))) return;
    await run(() => voiceApi.wakewordManage({ action: "clear_base", confirmed: true }), "已移除基础资源包");
  };
  const push = async (device, target, forceScreen = false) => {
    const name = device.name || device.device_id;
    const screen = (forceScreen || withScreen) && Array.isArray(device.screen_pack_size);
    const what = (target === "custom" ? `唤醒词“${saved.text}”（拼音 ${saved.command}）` : "官方唤醒词“你好小智”") + (screen ? "和 MOSS 全屏画风" : "");
    const ok = await showConfirm(
      `把${what}推送给设备 ${name}？\n` +
      (screen ? `全屏画风按这台的屏幕 ${device.screen_pack_size[0]}×${device.screen_pack_size[1]} 和当前画风生成：对话、雕刻、审批、听歌画面铺满整屏一直显示；顶部“聆听中”等状态字由固件显示，改不了。\n` : (device.screen_pack ? "这次不带全屏画风：装好后屏幕回到官方表情。\n" : "")) +
      "设备会立即重启，开机后自己下载资源包并写入闪存的资源分区，屏幕会显示下载进度，期间不要断电。" +
      (data?.base ? "会保留基础资源包里的字体、表情和背景。" : "没有导入基础资源包：设备屏幕的字体和表情会回到固件自带的。") +
      "\n只支持 ESP32-S3、16 MB 闪存、小智固件 2.0.2 以上（v2 分区表）的设备；不满足时设备会拒收，原有内容不变。" +
      "下载中途断电会让资源分区失效（字体和唤醒词暂时缺失），设备连上后重新推送一次即可恢复。下载地址 30 分钟内有效。",
      { title: "推送唤醒词？", tone: "warning", danger: true, confirmText: "确认推送这一台" },
    );
    if (!ok) return;
    await run(() => voiceApi.wakewordManage({ action: "push", device_id: device.device_id, target, confirmed: true, screen_pack: screen }),
      (next) => `已发送：${name} 正在重启并下载资源包（${((next?.pushed?.size || 0) / 1048576).toFixed(1)} MB${screen ? "，含全屏画风" : ""}）。下载完这里会显示“已刷全屏画风”，之后上电就是全屏画面；装好后对它说“${target === "custom" ? saved.text : "你好小智"}”试试。`);
  };
  // 2026-11-09 一键刷入全屏画风：沿用这台上次推送的唤醒词（没推过按“你好小智”），仍逐台确认
  const quickPack = (device) => {
    const target = device.wake_target === "custom" && saved && !dirty ? "custom" : "default";
    return push(device, target, true);
  };
  const devices = Array.isArray(data?.devices) ? data.devices : [];
  const base = data?.base;
  const presetActive = (preset) => draft && draft.command.trim().toLowerCase().split(/\s+/).join(" ") === preset.command;
  return (
    <section className="voice-card wakeword-card" id="voice-wakeword-card">
      <p className="wakeword-intro">
        唤醒词由小智设备自己识别。先选好唤醒词并保存，再逐台推送：设备会重启，开机后自己下载新的唤醒模型写入闪存。随时可以恢复“你好小智”。
      </p>
      {error && <p className="wakeword-banner wakeword-banner--bad" role="alert">{error}</p>}
      {message && <p className="wakeword-banner" role="status">{message}</p>}

      <div className="wakeword-step">
        <div className="wakeword-step-head">
          <span className="wakeword-step-no" aria-hidden="true">1</span>
          <div>
            <h3>选择唤醒词</h3>
            <p>选一个预设，或自己填写显示文字和拼音。</p>
          </div>
          {saved && <span className="wakeword-current">已保存：<b>{saved.text}</b></span>}
        </div>
        <div className="wakeword-presets" role="group" aria-label="唤醒词预设">
          {WAKE_WORD_PRESETS.map((preset) => (
            <button key={preset.command} className="wakeword-preset" type="button" disabled={busy || !draft}
              aria-pressed={presetActive(preset) ? "true" : "false"}
              onClick={() => setDraft((old) => ({ ...old, text: preset.text, command: preset.command }))}>
              <strong>{preset.text}</strong>
              <small className="mono">{preset.command}</small>
            </button>
          ))}
        </div>
        {draft && (
          <>
            <div className="wakeword-fields">
              <label className="wakeword-field">
                <span>显示文字</span>
                <input className="text-input" id="voice-wakeword-text" maxLength={16} value={draft.text}
                  onChange={(event) => setDraft((old) => ({ ...old, text: event.target.value }))} />
                <small>设备屏幕上显示的文字</small>
              </label>
              <label className="wakeword-field">
                <span>拼音</span>
                <input className="text-input mono" id="voice-wakeword-command" value={draft.command} placeholder="ni hao mo si"
                  onChange={(event) => setDraft((old) => ({ ...old, command: event.target.value }))} />
                <small>设备按这个发音识别；音节用空格隔开，不带声调</small>
              </label>
            </div>
            <label className="wakeword-field wakeword-threshold">
              <span>灵敏度阈值 <b className="mono">{Number(draft.threshold).toFixed(2)}</b></span>
              <input type="range" id="voice-wakeword-threshold" min="0.05" max="0.95" step="0.01" value={draft.threshold}
                onChange={(event) => setDraft((old) => ({ ...old, threshold: Number(event.target.value) }))} />
              <span className="wakeword-threshold-ends" aria-hidden="true"><small>更容易唤醒</small><small>默认 0.20</small><small>更少误唤醒</small></span>
            </label>
            <p className="wakeword-tip">MOSS 按中文读作“莫斯”（mo si）。唤醒词 3～6 个音节最好，“你好 MOSS”比单独“MOSS”更不容易误唤醒；误唤醒多就把阈值调高一点。</p>
            {draftError && <p className="wakeword-banner wakeword-banner--bad" role="alert">{draftError}</p>}
            <div className="wakeword-actions">
              <button className="btn btn-hud-primary" type="button" id="voice-wakeword-save" disabled={busy || !dirty || Boolean(draftError)} onClick={save}>保存唤醒词</button>
              {dirty ? <span className="voice-muted">有未保存的修改，保存后才能推送</span> : saved && <span className="voice-muted">已保存，可以推送到设备</span>}
            </div>
          </>
        )}
      </div>

      <div className="wakeword-step">
        <div className="wakeword-step-head">
          <span className="wakeword-step-no" aria-hidden="true">2</span>
          <div>
            <h3>推送到设备（逐台确认）</h3>
            <p>需要 ESP32-S3、16 MB 闪存、小智固件 2.0.2 以上；推送期间不要断电。</p>
          </div>
        </div>
        <label className="wakeword-screen-pack" id="voice-wakeword-screen-pack">
          <input type="checkbox" checked={withScreen} disabled={busy} onChange={(event) => setWithScreen(event.target.checked)} />
          <span>
            同时刷入 MOSS 全屏画风
            <small>彩屏小智的对话、雕刻、审批、听歌画面铺满整屏、一直显示，按“副屏预览”里当前选的画风；单色屏或没报分辨率的设备自动只推唤醒词。不勾就是官方表情。</small>
          </span>
        </label>
        {!data?.running ? <p className="wakeword-empty">内置语音服务未运行。</p> : devices.length === 0 ? <p className="wakeword-empty">没有在线的已允许设备。</p> : (
          <ul className="wakeword-devices" aria-label="可推送唤醒词的设备">
            {devices.map((device) => (
              <li key={device.device_id} className="wakeword-device">
                <span className="wakeword-device-name">
                  <strong>{device.name || device.device_id}</strong>
                  <small>
                    {device.supported ? "支持在线更新唤醒词" : "固件不支持在线更新资源包（需要小智固件 2.0.2 以上、v2 分区表）"}
                    {device.supported && ` · ${screenPackDeviceView(device).text}`}
                  </small>
                </span>
                {device.supported && (
                  <span className="wakeword-device-actions">
                    {screenPackDeviceView(device).canQuickPack && (
                      <button className="btn btn-hud-primary" type="button" disabled={busy} data-quick-pack={device.device_id} onClick={() => quickPack(device)}>
                        {screenPackDeviceView(device).quickLabel}
                      </button>
                    )}
                    <button className="btn btn-hud-primary" type="button" disabled={busy || dirty || !saved} onClick={() => push(device, "custom")}>推送“{saved?.text}”</button>
                    <button className="btn btn-ghost" type="button" disabled={busy} onClick={() => push(device, "default")}>恢复“你好小智”</button>
                  </span>
                )}
              </li>
            ))}
          </ul>
        )}
      </div>

      <details className="wakeword-advanced">
        <summary>高级：基础资源包（可选）{base ? " · 已导入" : ""}</summary>
        {base ? (
          <p>已导入：{base.files} 个文件，{(base.size / 1048576).toFixed(1)} MB{base.text_font ? ` · 字体 ${base.text_font}` : ""}{base.emoji_count ? ` · ${base.emoji_count} 个表情` : ""}{base.wake_word ? ` · 原唤醒词 ${base.wake_word}` : ""}。推送时保留这些，只换唤醒词。</p>
        ) : (
          <p>未导入时，推送的资源包只含唤醒词，设备屏幕的字体和表情会回到固件自带的。想保留自定义字体、表情或背景，可以先用小智官方网页生成器（xiaozhi-assets-generator）导出 assets.bin 再导入。</p>
        )}
        <div className="wakeword-actions">
          <button className="btn btn-ghost" type="button" disabled={busy} onClick={importBase}>导入资源包…</button>
          {base && <button className="btn btn-ghost" type="button" disabled={busy} onClick={clearBase}>移除</button>}
        </div>
      </details>
    </section>
  );
}
