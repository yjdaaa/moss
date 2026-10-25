import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
import { showConfirm } from "../appDialog.js";
import { firmwareDeliveryView } from "../voiceState.js";

// NV17 固件 OTA（2026-10-15 用户决定：逐台确认推送）。导入自己编译的 ESP32 应用固件，
// 选一台已允许的设备确认推送；设备下次检查更新时自动下载刷写，上报新版本后才算升级完成。
export default function VoiceFirmwareCard() {
  const [data, setData] = useState(null);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);
  const [choice, setChoice] = useState({});
  const refresh = async () => {
    try { setData(await voiceApi.firmwareStatus()); setError(""); } catch (e) { setError(e.message); }
  };
  useEffect(() => {
    void refresh();
    const timer = setInterval(refresh, 5000);
    return () => clearInterval(timer);
  }, []);
  const run = async (work, done) => {
    setBusy(true); setError(""); setMessage("");
    try { const next = await work(); if (next) setData(next); if (done) setMessage(done); return true; }
    catch (e) { setError(e.message); return false; }
    finally { setBusy(false); }
  };
  const importFirmware = async () => {
    setBusy(true); setError(""); setMessage("");
    try {
      const result = await voiceApi.firmwareImport();
      if (result) setMessage(`已导入固件 ${result.version}${result.project ? `（${result.project}）` : ""} · SHA-256 ${result.sha256.slice(0, 12)}…`);
      await refresh();
    } catch (e) { setError(e.message); } finally { setBusy(false); }
  };
  const artifacts = Array.isArray(data?.artifacts) ? data.artifacts : [];
  const devices = Array.isArray(data?.devices) ? data.devices : [];
  const records = Array.isArray(data?.records) ? data.records : [];
  const offer = async (device) => {
    const artifact = artifacts.find((item) => item.sha256 === (choice[device.device_id] || artifacts[0]?.sha256));
    if (!artifact) return;
    const ok = await showConfirm(
      `把固件 ${artifact.version}${artifact.project ? `（${artifact.project}）` : ""} 推送给设备 ${device.name || device.device_id}（型号 ${device.model || "未知"}，当前 ${device.firmware_version || "未知"}）？\n` +
      `SHA-256：${artifact.sha256}\n设备下次开机检查更新时会自动下载并刷写，期间不要断电。固件与板子型号不符可能导致设备无法启动，需要串口重刷。推送 ${data?.offer_hours ?? 24} 小时内有效。`,
      { title: "推送固件？", tone: "warning", danger: true, confirmText: "确认推送这一台" },
    );
    if (!ok) return;
    await run(() => voiceApi.firmwareManage({ action: "offer", device_id: device.device_id, sha256: artifact.sha256, confirmed: true }), "已推送：请重启这台设备，它开机检查更新时会自动升级");
  };
  const remove = async (artifact) => {
    if (!(await showConfirm(`删除固件 ${artifact.version}？`, { title: "删除固件", danger: true, confirmText: "删除" }))) return;
    await run(() => voiceApi.firmwareManage({ action: "delete", sha256: artifact.sha256, confirmed: true }), "已删除固件");
  };
  return (
    <section className="voice-card voice-speaker-card" id="voice-firmware-card">
      <h2>固件升级（OTA）</h2>
      <p>导入自己编译的 ESP32 应用固件（编译输出里的应用 .bin，不是整片烧录包），选一台已允许的设备确认推送。设备下次开机检查更新时会自动下载并刷写；只有设备之后上报了新版本才算升级完成。一次只推一台，先确认没问题再推下一台。</p>
      {error && <p role="alert">{error}</p>}
      {message && <p role="status">{message}</p>}
      <button className="btn btn-ghost" type="button" disabled={busy} onClick={importFirmware}>导入固件…</button>
      <button className="btn btn-ghost" type="button" disabled={busy} onClick={refresh}>刷新</button>

      <h3>已导入的固件</h3>
      {artifacts.length === 0 ? <p>还没有导入固件。</p> : (
        <ul aria-label="已导入的固件">
          {artifacts.map((item) => (
            <li key={item.sha256}>
              <b>{item.version}</b>{item.project ? ` · ${item.project}` : ""} · {(item.size / 1024).toFixed(0)} KB · SHA-256 <code>{item.sha256.slice(0, 16)}…</code>
              <button className="btn btn-ghost" type="button" disabled={busy} onClick={() => remove(item)}>删除</button>
            </li>
          ))}
        </ul>
      )}

      <h3>推送到设备（逐台确认）</h3>
      {devices.length === 0 ? <p>没有已允许的设备。</p> : (
        <ul aria-label="可推送的设备">
          {devices.map((device) => (
            <li key={device.device_id}>
              {device.name || device.device_id} · 型号 {device.model || "未知"} · 当前 {device.firmware_version || "未知"}
              <select className="text-input" aria-label={`${device.device_id} 要推送的固件`} disabled={busy || artifacts.length === 0}
                value={choice[device.device_id] || artifacts[0]?.sha256 || ""}
                onChange={(event) => setChoice((old) => ({ ...old, [device.device_id]: event.target.value }))}>
                {artifacts.map((item) => <option key={item.sha256} value={item.sha256}>{item.version}{item.project ? ` · ${item.project}` : ""}</option>)}
              </select>
              <button className="btn btn-ghost" type="button" disabled={busy || artifacts.length === 0} onClick={() => offer(device)}>推送</button>
            </li>
          ))}
        </ul>
      )}

      <h3>推送记录</h3>
      {records.length === 0 ? <p>暂无推送记录。</p> : (
        <ul aria-label="固件推送记录">
          {records.map((row) => {
            const view = firmwareDeliveryView(row, data?.now);
            return (
              <li key={row.delivery_id}>
                {row.device_id} · {row.firmware_version || "旧记录"} · {view.text} · SHA-256 <code>{row.artifact_sha256.slice(0, 12)}…</code>
                {view.cancellable && <button className="btn btn-ghost" type="button" disabled={busy}
                  onClick={() => run(() => voiceApi.firmwareManage({ action: "cancel", delivery_id: row.delivery_id }), "已取消推送")}>取消推送</button>}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}
