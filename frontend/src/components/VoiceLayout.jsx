import React, { createContext, useContext, useEffect, useId, useState } from "react";
import { ChevronDown, Save } from "lucide-react";
import { readFoldOpen, writeFoldOpen } from "../foldState.js";

// 语音中心的分组与保存栏（UI-3，技术书 §9.5.1 r15）：
// 每个分组折叠时只显示一行摘要，展开后填写；每组一个保存按钮，按钮写明保存对象；
// 没改动时不能点；有改动时分组标题与所在标签显示“未保存”；保存结果显示在按钮旁。
// 收起只隐藏、不卸载，所以收起分组或切换标签都不会丢失已填写的内容。

/** 当前分组所在标签（由页面提供）与上报未保存改动的回调。 */
const VoiceTabContext = createContext("");
const VoiceDirtyContext = createContext(() => {});

export function VoiceDirtyProvider({ report, children }) {
  return <VoiceDirtyContext.Provider value={report}>{children}</VoiceDirtyContext.Provider>;
}

export function VoiceTabPanel({ tab, active, children }) {
  return (
    <VoiceTabContext.Provider value={tab}>
      <div className="voice-tabpanel" role="tabpanel" id={`voice-panel-${tab}`} aria-labelledby={`voice-page-tab-${tab}`} hidden={!active}>
        {children}
      </div>
    </VoiceTabContext.Provider>
  );
}

/** 向页面上报“本分组有未保存改动”；卸载时自动清除。 */
export function useVoiceDirty(dirty) {
  const report = useContext(VoiceDirtyContext);
  const tab = useContext(VoiceTabContext);
  const id = useId();
  useEffect(() => {
    report(id, dirty ? tab || "?" : "");
  }, [report, id, tab, dirty]);
  useEffect(() => () => report(id, ""), [report, id]);
}

/**
 * 可折叠分组。`summary` 为折叠时的一行摘要；`dirty` 时标题旁显示“未保存”。
 * 展开状态按 `id` 记在本机（界面偏好）。
 */
export function VoiceGroup({ id, title, summary, dirty = false, defaultOpen = false, tone, children, label }) {
  const [open, setOpen] = useState(() => readFoldOpen(`voice-${id}`, defaultOpen));
  useVoiceDirty(dirty);
  const toggle = () => {
    setOpen((current) => {
      writeFoldOpen(`voice-${id}`, !current);
      return !current;
    });
  };
  return (
    <section className={`voice-group${open ? " open" : ""}`} id={`voice-group-${id}`} aria-label={label || title}>
      <button type="button" className="voice-group-h" onClick={toggle} aria-expanded={open}>
        <ChevronDown size={15} className="voice-group-chevron" aria-hidden="true" />
        <span className="voice-group-title">{title}</span>
        {dirty && <span className="ob-pill ob-pill--warn voice-dirty">未保存</span>}
        {summary ? <span className={`voice-group-summary${tone ? ` voice-${tone}` : ""}`}>{summary}</span> : null}
      </button>
      <div className="voice-group-body" hidden={!open}>
        {children}
      </div>
    </section>
  );
}

/**
 * 保存栏：主按钮“保存{target}”；`blocked` 为不能保存的原因（显示在按钮旁）；
 * `result` = {ok, text, action?: {label, onClick}} 为最近一次保存结果。
 */
export function SaveBar({ id, target, dirty, busy = false, saving = false, blocked = "", result, onSave, extra }) {
  const disabled = busy || saving || !dirty || Boolean(blocked);
  const reason = blocked || (!dirty ? "没有需要保存的修改" : "");
  return (
    <div className="voice-savebar">
      <button type="button" className="btn btn-primary" onClick={onSave} disabled={disabled} id={id} title={reason || `保存${target}`}>
        <Save size={13} /> {saving ? "保存中…" : `保存${target}`}
      </button>
      {extra}
      {dirty && blocked ? <span className="voice-muted">{blocked}</span> : null}
      {result ? (
        <span className={result.ok ? "voice-ok" : "voice-bad"} role={result.ok ? "status" : "alert"}>
          {result.ok ? "✓ " : ""}
          {result.text}
          {result.action ? (
            <button type="button" className="btn btn-ghost voice-inline-btn" onClick={result.action.onClick} disabled={busy}>
              {result.action.label}
            </button>
          ) : null}
        </span>
      ) : null}
    </div>
  );
}
