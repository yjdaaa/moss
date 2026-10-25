import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
import { showConfirm } from "../appDialog.js";

const ask = (message) => showConfirm(message, { title: "请确认", tone: "warning", danger: true });

export default function VoiceMemoryCard({ status }) {
  const [enabled, setEnabled] = useState(false);
  const [items, setItems] = useState([]);
  const [drafts, setDrafts] = useState({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const refresh = async () => {
    const list = await voiceApi.memoryList();
    setItems(Array.isArray(list) ? list : []);
    setDrafts(Object.fromEntries((Array.isArray(list) ? list : []).map((entry) => [entry.device_id, entry.summary])));
  };
  useEffect(() => {
    setEnabled(Boolean(status?.memory_enabled));
    refresh().catch((e) => setError(e.message));
  }, [status?.memory_enabled]);
  const act = async (operation) => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      await operation();
      await refresh();
      setMessage("记忆设置已保存");
    } catch (e) {
      setError(e.code === "memory_conflict" ? "记忆已在其他会话修改，请先复制未保存文字再刷新" : e.message);
    } finally {
      setBusy(false);
    }
  };
  const devices = status?.devices || [];
  return (
    <section className="voice-card" aria-label="设备记忆">
      <h3>对话记忆</h3>
      <p>默认关闭。开启后会用对话文字生成本机摘要；使用云模型时，筛选后的文字会发送给服务商。不保存录音，记忆不能授权设备动作。</p>
      <label>
        <input type="checkbox" checked={enabled} disabled={busy} onChange={(e) => {
          const next = e.target.checked;
          act(async () => {
            await voiceApi.memorySetEnabled(next);
            setEnabled(next);
          });
        }} />
        启用对话记忆
      </label>
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}
      <details className="voice-private-more">
        <summary>查看或编辑已保存的设备摘要</summary>
        <button type="button" disabled={busy} onClick={() => act(async () => {})}>刷新记忆</button>
      {devices.filter((d) => d.status === "allowed").map((device) => {
        const record = items.find((m) => m.device_id === device.device_id);
        return <div key={device.device_id} className="voice-form-group">
          <label htmlFor={`memory-${device.device_id}`}>{device.name || device.device_id}</label>
          <textarea id={`memory-${device.device_id}`} rows={3} maxLength={1500}
            value={drafts[device.device_id] ?? ""} disabled={busy}
            onChange={(e) => setDrafts((old) => ({ ...old, [device.device_id]: e.target.value }))} />
          <span>{(drafts[device.device_id] || "").length}/1500 字</span>
          <button type="button" disabled={busy} onClick={() => act(async () => {
            await voiceApi.memorySave(device.device_id, record?.revision ?? 0, drafts[device.device_id] || "");
          })}>保存</button>
          <button type="button" disabled={busy} onClick={async () => {
            if (!(await ask("确定要清空此设备的长期记忆？"))) return;
            act(async () => { await voiceApi.memoryClear(device.device_id, record?.revision ?? 0); });
          }}>清空</button>
        </div>;
      })}
      </details>
    </section>
  );
}
