import React, { useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";

const EMPTY = { enabled: false, directory: "" };

export default function VoiceMusicCard() {
  const [config, setConfig] = useState(EMPTY);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [message, setMessage] = useState("");
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let alive = true;
    voiceApi.musicStatus().then((state) => {
      if (!alive) return;
      setConfig({ ...EMPTY, ...(state.settings || {}) });
      setLoaded(true);
    }).catch((e) => { if (alive) setError(e?.message || "本地音乐状态读取失败"); });
    return () => { alive = false; };
  }, []);

  const pick = async () => {
    setBusy(true); setError(""); setMessage("");
    try {
      const selected = await voiceApi.musicPickDirectory();
      if (selected?.directory) {
        setConfig((current) => ({ ...current, directory: selected.directory }));
        setMessage("目录已选择；点击保存后才会验证和索引歌曲。");
      }
    } catch (e) {
      setError(e?.message || "选择音乐目录失败");
    } finally {
      setBusy(false);
    }
  };

  const save = async () => {
    setBusy(true); setMessage(""); setError("");
    try {
      const result = await voiceApi.musicSave({ settings: config });
      setConfig({ ...EMPTY, ...(result.settings || {}) });
      setMessage(config.enabled
        ? "已保存，索引 " + (result.track_count || 0) + " 首歌曲。" +
          (result.restarted ? "内置语音服务已安全重启。" : "启动内置对话服务后生效。")
        : "音乐功能已关闭。" + (result.restarted ? "内置语音服务已重启。" : ""));
    } catch (e) {
      setError(e?.message || "保存音乐配置失败");
    } finally {
      setBusy(false);
    }
  };

  return <section className="voice-card" aria-label="本地音乐">
    <h3>本地音乐（NV14）</h3>
    <p>默认关闭。仅扫描你明确配置的<strong>本机音乐目录</strong>，不上传歌曲到模型，也不打开麦克风、摄像头或激光设备。需要先配置可用的内置语音对话模型。</p>
    <label className="voice-form-group">
      <input type="checkbox" checked={!!config.enabled}
        disabled={!loaded || busy}
        onChange={(event) => setConfig((current) => ({ ...current, enabled: event.target.checked }))} />
      启用本地音乐播放
    </label>
    <label className="voice-form-group">
      <span>音乐目录（现有本地文件夹的完整路径）</span>
      <input value={config.directory} disabled={!loaded || busy}
        placeholder="例如 D:\Music（不会自动读取）"
        autoComplete="off"
        onChange={(event) => setConfig((current) => ({ ...current, directory: event.target.value }))} />
    </label>
    {error && <p role="alert">{error}</p>}    <button type="button" disabled={!loaded || busy} onClick={pick}>
      {busy ? "正在处理…" : "选择音乐文件夹…"}
    </button>

    {message && <p role="status">{message}</p>}
    <button type="button" disabled={busy || !loaded || (config.enabled && !config.directory.trim())}
      onClick={save}>{busy ? "验证目录并保存中…" : "保存音乐配置"}</button>
    <p>歌名完全匹配优先；重名时先询问，不会随机猜一首。只在启用后建立索引；设备掉线、插话、切歌或撤销权限时，旧歌曲缓冲会被清除。实际音质仍需最后进行设备验收。</p>
  </section>;
}
