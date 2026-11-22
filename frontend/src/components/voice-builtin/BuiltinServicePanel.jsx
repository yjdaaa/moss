import React from "react";
import { Play, Square, Copy, Usb } from "lucide-react";
import { showAlert } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { VoiceGroup, gotoVoiceGroup } from "../VoiceLayout.jsx";
import {
  builtinModeView,
} from "../../voiceState.js";
import { TONE_CLASS } from "./shared.js";

/** 服务：运行状态、启停、设备要填的连接地址（n69 起设备表移到“设备 › 已连接设备”，副屏预览移到“设备 › 副屏预览”）。 */
export function BuiltinServicePanel({ builtin }) {
  const { status, busy, act } = builtin;
  if (!status) return <p className="voice-muted">正在读取内置服务状态…</p>;
  if (status.error) return <div className="voice-hint voice-hint--bad">{status.error}</div>;

  const running = Boolean(status.running);
  const sessions = Array.isArray(status.sessions) ? status.sessions : [];
  const modeView = builtinModeView(status);
  const copyOta = async (url = status.ota_url, label = "设备 OTA 地址") => {
    if (!url) return;
    try {
      await navigator.clipboard.writeText(url);
      await showAlert(`${label}已复制，粘贴到设备配网页面即可`, { title: "已复制", tone: "success" });
    } catch {
      await showAlert("无法访问剪贴板，请手动选中地址复制", { title: "复制失败", tone: "error" });
    }
  };
  return (
    <VoiceGroup
      id="builtin-service"
      title="内置语音服务"
      summary={`${running ? "运行中" : "未运行"} · ${modeView.text}模式 · ${sessions.length} 台在线`}
      tone={running ? "ok" : undefined}
      defaultOpen
    >
      <div className="voice-block" id="voice-builtin-card">
        <div className="voice-block-h">
          <h3>运行状态</h3>
          <span className={`ob-pill ${TONE_CLASS[modeView.tone]}`} id="voice-builtin-mode" title={modeView.hint}>
            {modeView.text}
          </span>
          <span className={`ob-pill ${running ? TONE_CLASS.ok : TONE_CLASS.off}`} id="voice-builtin-state">
            {running ? "运行中" : "未运行"}
          </span>
          <span className="voice-actions voice-push">
            {running ? (
              <button type="button" className="btn btn-danger" disabled={Boolean(busy)} onClick={() => act("停止内置服务", () => voiceApi.builtinStop())} id="voice-builtin-stop">
                <Square size={13} /> 停止
              </button>
            ) : (
              <button type="button" className="btn btn-primary" disabled={Boolean(busy)} onClick={() => act("启动内置服务", () => voiceApi.builtinStart())} id="voice-builtin-start">
                <Play size={13} /> 启动
              </button>
            )}
          </span>
        </div>
        <p className="voice-muted">
          {modeView.text === "对话" ? "识别、大模型、合成已配齐。" : "模型没配齐，先以回声测试运行；在“模型”标签配齐后自动切换为对话。"}
          {modeView.autostart}
        </p>
        {status.poll_error && <div className="voice-hint voice-hint--warn">状态刷新失败（显示的是上次读取的结果）：{status.poll_error}</div>}
        {status.last_error && !running && (
          <div className="voice-hint voice-hint--bad" id="voice-builtin-error">
            自动启动失败：{status.last_error}
          </div>
        )}
        <div className="voice-kv">
          <span className="voice-inline">
            设备 OTA 地址（固定不变）：<b className="mono voice-break" id="voice-builtin-ota">{status.ota_url || "未能获取本机局域网地址"}</b>
            {status.ota_url && (
              <button type="button" className="btn btn-ghost voice-inline-btn" onClick={() => copyOta(status.ota_url, "设备 OTA 地址")} id="voice-builtin-ota-copy" title="复制设备 OTA 地址">
                <Copy size={13} /> 复制
              </button>
            )}
            {status.ota_ip_url && status.ota_ip_url !== status.ota_url && (
              <>
                <span className="voice-muted">· 本机局域网 IP：</span>
                <span className="mono voice-break" id="voice-builtin-ota-ip">{status.ota_ip_url}</span>
                <button type="button" className="btn btn-ghost voice-inline-btn" onClick={() => copyOta(status.ota_ip_url, "本机局域网 OTA 地址")} id="voice-builtin-ota-ip-copy" title="复制本机局域网 IP 地址">
                  <Copy size={13} /> 复制 IP
                </button>
              </>
            )}
            <button
              type="button"
              className="btn btn-ghost voice-inline-btn"
              onClick={() => gotoVoiceGroup("devices", "device-usb-provision", "xiaozhi-usb-port-select")}
              id="voice-builtin-usb-provision"
              title="跳转到“设备 › USB 一键配网写地址”面板（插 USB 线直写小智 Wi-Fi、OTA 地址与设备密钥）"
            >
              <Usb size={13} /> USB 一键配网写地址
            </button>
          </span>
          <span className="voice-muted">
            在设备配网页面填入上方 <code>moss.local</code> 地址后，换网无需重改；若局域网广播解析不稳定，可改用右侧本机局域网 IP 地址。端口 {status.ws_port} / {status.http_port}，在下面“内置服务端口”组修改。
          </span>
        </div>
      </div>
    </VoiceGroup>
  );
}
