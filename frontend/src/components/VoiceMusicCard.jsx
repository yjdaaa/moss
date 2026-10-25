import React, { useEffect, useState } from "react";
import { FolderOpen } from "lucide-react";
import { voiceApi } from "../voiceApi.js";
import { formDirty, savedNoteText } from "../voiceState.js";
import { SaveBar, VoiceGroup } from "./VoiceLayout.jsx";
import { musicDirPlaceholder } from "../platform.js";

const EMPTY = { enabled: false, directory: "" };

export default function VoiceMusicCard() {
  const [saved, setSaved] = useState(EMPTY);
  const [config, setConfig] = useState(EMPTY);
  const [warning, setWarning] = useState("");
  const [picking, setPicking] = useState(false);
  const [saving, setSaving] = useState(false);
  const [loadError, setLoadError] = useState("");
  const [result, setResult] = useState(null);
  const [loaded, setLoaded] = useState(false);
  const busy = picking || saving;
  const dirty = loaded && formDirty(saved, config);

  useEffect(() => {
    let alive = true;
    voiceApi.musicStatus().then((state) => {
      if (!alive) return;
      const next = { ...EMPTY, ...(state.settings || {}) };
      setSaved(next);
      setConfig(next);
      setWarning(typeof state?.warning === "string" ? state.warning : "");
      setLoaded(true);
    }).catch((e) => { if (alive) setLoadError(e?.message || "本地音乐状态读取失败"); });
    return () => { alive = false; };
  }, []);

  const change = (key, value) => {
    setConfig((current) => ({ ...current, [key]: value }));
    setResult(null);
  };

  const pick = async () => {
    setPicking(true);
    try {
      const selected = await voiceApi.musicPickDirectory();
      if (selected?.directory) change("directory", selected.directory);
    } catch (e) {
      setResult({ ok: false, text: e?.message || "选择音乐目录失败" });
    } finally {
      setPicking(false);
    }
  };

  const save = async () => {
    setSaving(true);
    try {
      const response = await voiceApi.musicSave({ settings: config });
      const next = { ...EMPTY, ...(response.settings || {}) };
      setSaved(next);
      setConfig(next);
      setWarning(typeof response?.warning === "string" ? response.warning : "");
      const effect = config.enabled
        ? `索引 ${response.track_count || 0} 首 · ${response.restarted ? "内置服务已重启" : "启动内置服务后生效"}`
        : `音乐已关闭${response.restarted ? " · 内置服务已重启" : ""}`;
      setResult({ ok: true, text: savedNoteText("音乐设置", effect) });
    } catch (e) {
      setResult({ ok: false, text: `保存失败：${e?.message || "保存音乐配置失败"}` });
    } finally {
      setSaving(false);
    }
  };

  const summary = loadError
    ? "读取失败"
    : !loaded
      ? "读取中…"
      : saved.enabled
        ? (warning ? "目录不可用" : `已开启 · ${saved.directory}`)
        : "未开启";
  return (
    <VoiceGroup id="music" title="本地音乐" label="本地音乐" summary={summary} tone={saved.enabled && !warning ? "ok" : undefined} dirty={dirty}>
      <p className="voice-muted">默认关闭。只扫描你指定的本机目录，歌曲不上传给模型。歌名重复时先询问，不随机猜。</p>
      {loadError && <div className="voice-hint voice-hint--bad" role="alert">{loadError}</div>}
      {warning && <div className="voice-hint voice-hint--warn" role="status">{warning}</div>}
      <div className="voice-form-grid">
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={!!config.enabled} disabled={!loaded || busy} onChange={(event) => change("enabled", event.target.checked)} id="voice-music-enabled" />
          <span>启用本地音乐播放（需要内置服务已配好对话模型）</span>
        </label>
        <label className="voice-wide">
          <span>音乐目录（本机文件夹的完整路径，保存时才验证并建立索引）</span>
          <span className="voice-inline">
            <input className="text-input mono" value={config.directory} disabled={!loaded || busy} placeholder={musicDirPlaceholder()} autoComplete="off" onChange={(event) => change("directory", event.target.value)} id="voice-music-dir" />
            <button type="button" className="btn btn-ghost" disabled={!loaded || busy} onClick={pick}>
              <FolderOpen size={13} /> 选择…
            </button>
          </span>
        </label>
      </div>
      <SaveBar
        id="voice-music-save"
        target="音乐设置"
        dirty={dirty}
        busy={busy}
        saving={saving}
        blocked={config.enabled && !config.directory.trim() ? "开启时需要填写音乐目录" : ""}
        result={result}
        onSave={save}
      />
    </VoiceGroup>
  );
}
