import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
import { showConfirm } from "../appDialog.js";
import { WAKE_WORD_PRESETS, wakeWordDraftError } from "../voiceState.js";

// n61 设备唤醒词（2026-11-06 用户决定：唤醒词改为 MOSS，并能在设置里修改）。
// 唤醒词由小智设备自己识别；MOSS 生成资源包，逐台确认后让设备重启、自己下载写入资源分区。
export default function VoiceWakeWordCard() {
  const [data, setData] = useState(null);
  const [draft, setDraft] = useState(null);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
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
  const push = async (device, target) => {
    const name = device.name || device.device_id;
    const what = target === "custom" ? `唤醒词“${saved.text}”（拼音 ${saved.command}）` : "官方唤醒词“你好小智”";
    const ok = await showConfirm(
      `把${what}推送给设备 ${name}？\n` +
      "设备会立即重启，开机后自己下载资源包并写入闪存的资源分区，屏幕会显示下载进度，期间不要断电。" +
      (data?.base ? "会保留基础资源包里的字体、表情和背景。" : "没有导入基础资源包：设备屏幕的字体和表情会回到固件自带的。") +
      "\n只支持 ESP32-S3、16 MB 闪存、小智固件 2.0.2 以上（v2 分区表）的设备；不满足时设备会拒收，原有内容不变。" +
      "下载中途断电会让资源分区失效（字体和唤醒词暂时缺失），设备连上后重新推送一次即可恢复。下载地址 30 分钟内有效。",
      { title: "推送唤醒词？", tone: "warning", danger: true, confirmText: "确认推送这一台" },
    );
    if (!ok) return;
    await run(() => voiceApi.wakewordManage({ action: "push", device_id: device.device_id, target, confirmed: true }),
      (next) => `已发送：${name} 正在重启并下载资源包（${((next?.pushed?.size || 0) / 1048576).toFixed(1)} MB）。装好后对它说“${target === "custom" ? saved.text : "你好小智"}”试试。`);
  };
  const devices = Array.isArray(data?.devices) ? data.devices : [];
  const base = data?.base;
  return (
    <section className="voice-card voice-speaker-card" id="voice-wakeword-card">
      <h2>设备唤醒词</h2>
      <p>唤醒词由小智设备自己识别。保存后选一台在线设备确认推送：设备会重启，开机后自己下载新的唤醒模型并写入闪存。要求 ESP32-S3 板子（如实战派）、16 MB 闪存、小智固件 2.0.2 以上。自定义唤醒词的准确度一般不如原装“你好小智”，误唤醒多就把灵敏度阈值调高一点；随时可以恢复“你好小智”。</p>
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}

      <h3>唤醒词</h3>
      <div role="group" aria-label="唤醒词预设">
        {WAKE_WORD_PRESETS.map((preset) => (
          <button key={preset.command} className="btn btn-ghost" type="button" disabled={busy || !draft}
            onClick={() => setDraft((old) => ({ ...old, text: preset.text, command: preset.command }))}>{preset.text}</button>
        ))}
      </div>
      {draft && (
        <div style={{ display: "grid", gap: 8, maxWidth: 420 }}>
          <label>显示文字（设备屏幕上显示）
            <input className="text-input" id="voice-wakeword-text" maxLength={16} value={draft.text}
              onChange={(event) => setDraft((old) => ({ ...old, text: event.target.value }))} />
          </label>
          <label>拼音（设备按这个发音识别，音节用空格隔开，不带声调）
            <input className="text-input" id="voice-wakeword-command" value={draft.command} placeholder="ni hao mo si"
              onChange={(event) => setDraft((old) => ({ ...old, command: event.target.value }))} />
          </label>
          <label>灵敏度阈值 {Number(draft.threshold).toFixed(2)}（越小越容易唤醒，也越容易误唤醒；默认 0.20）
            <input type="range" id="voice-wakeword-threshold" min="0.05" max="0.95" step="0.01" value={draft.threshold}
              onChange={(event) => setDraft((old) => ({ ...old, threshold: Number(event.target.value) }))} />
          </label>
          <p className="voice-muted">MOSS 按中文读作“莫斯”（mo si）。乐鑫建议唤醒词 3～6 个音节，“你好 MOSS”比单独“MOSS”更不容易误唤醒。</p>
          {draftError && <p role="alert">{draftError}</p>}
          <div>
            <button className="btn" type="button" id="voice-wakeword-save" disabled={busy || !dirty || Boolean(draftError)} onClick={save}>保存唤醒词</button>
            {dirty && <span className="voice-muted"> 有未保存的修改，保存后才能推送</span>}
          </div>
        </div>
      )}

      <h3>基础资源包（可选）</h3>
      {base ? (
        <p>已导入：{base.files} 个文件，{(base.size / 1048576).toFixed(1)} MB{base.text_font ? ` · 字体 ${base.text_font}` : ""}{base.emoji_count ? ` · ${base.emoji_count} 个表情` : ""}{base.wake_word ? ` · 原唤醒词 ${base.wake_word}` : ""}。推送时保留这些，只换唤醒词。</p>
      ) : (
        <p>未导入。推送的资源包只含唤醒词，设备屏幕的字体和表情会回到固件自带的。想保留自定义字体、表情或背景，可以先用小智官方网页生成器（xiaozhi-assets-generator）导出一个 assets.bin 再导入。</p>
      )}
      <button className="btn btn-ghost" type="button" disabled={busy} onClick={importBase}>导入资源包…</button>
      {base && <button className="btn btn-ghost" type="button" disabled={busy} onClick={clearBase}>移除</button>}

      <h3>推送到设备（逐台确认）</h3>
      {!data?.running ? <p>内置语音服务未运行。</p> : devices.length === 0 ? <p>没有在线的已允许设备。</p> : (
        <ul aria-label="可推送唤醒词的设备">
          {devices.map((device) => (
            <li key={device.device_id}>
              {device.name || device.device_id}
              {device.supported ? (
                <>
                  <button className="btn btn-ghost" type="button" disabled={busy || dirty || !saved} onClick={() => push(device, "custom")}>推送“{saved?.text}”</button>
                  <button className="btn btn-ghost" type="button" disabled={busy} onClick={() => push(device, "default")}>恢复“你好小智”</button>
                </>
              ) : <span className="voice-muted"> · 固件不支持网络更新资源包（需要小智固件 2.0.2 以上、v2 分区表）</span>}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
