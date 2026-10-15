import React, { useCallback, useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
import {
  CATEGORY_LABEL,
  CATEGORY_SCOPES,
  NEEDS_DEVICE,
  SOURCE_LABEL,
  experienceDevices,
  linesToList,
  listToLines,
  rowBadge,
  scopeLabel,
  scopeOptions,
  skipReasonText,
} from "../voiceContextState.js";

// NV19 长期记忆（技术书 §15）：只在本机打包的私有窗口显示。检索与自动提取默认关闭；
// 所有操作经 `voice_context_manage`，资料文件夹经 `voice_context_pick_folder` 在本机建立索引。

const call = (action, extra = {}) => voiceApi.contextManage({ action, ...extra });
const key = (scope) => (scope ? `${scope.kind}:${scope.id}` : "");
const parseKey = (value) => {
  const at = value.indexOf(":");
  return at > 0 ? { kind: value.slice(0, at), id: value.slice(at + 1) } : null;
};

function Result({ result }) {
  if (!result) return null;
  return <p role={result.ok ? "status" : "alert"}>{result.ok ? "✓ " : ""}{result.text}</p>;
}

/** 一个操作的执行与结果显示；失败只显示在本区域，不弹窗。 */
function useAction(onDone) {
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState(null);
  // `reload: false` keeps what `work` showed (e.g. search results) instead of reloading.
  const run = async (work, done, { reload = true } = {}) => {
    setBusy(true);
    setResult(null);
    try {
      const value = await work();
      if (done) setResult({ ok: true, text: typeof done === "function" ? done(value) : done });
      if (reload) await onDone?.();
      return value;
    } catch (error) {
      setResult({ ok: false, text: error.message || "操作失败" });
      return null;
    } finally {
      setBusy(false);
    }
  };
  return { busy, result, run, setResult };
}

function Switches({ status, refresh }) {
  const { busy, result, run } = useAction(refresh);
  const [name, setName] = useState("");
  const toggle = (field, value) => {
    if (field === "extraction" && value && !window.confirm("开启后，每次语音会话结束时，过滤后的对话文字会发给你在内置服务里配置的大模型，用来提取候选记忆，可能产生费用。提取结果只是候选，需要你在这里确认。确定开启？")) return;
    run(() => call("set_options", { [field]: value, confirmed: field === "extraction" && value }), value ? "已开启" : "已关闭");
  };
  return (
    <div className="voice-form-group">
      <label>
        <input type="checkbox" checked={!!status.retrieval} disabled={busy} onChange={(e) => toggle("retrieval", e.target.checked)} id="context-retrieval" />
        语音对话时检索长期记忆与资料（默认关闭；不调用大模型，最多加入 3 条记忆和少量资料摘录）
      </label>
      <label>
        <input type="checkbox" checked={!!status.extraction} disabled={busy} onChange={(e) => toggle("extraction", e.target.checked)} id="context-extraction" />
        会话结束后自动提取候选记忆（默认关闭；会调用大模型）
      </label>
      <p>
        当前项目：<b id="context-project">{status.project?.name || "未设置"}</b>（资料目录、任务和恢复摘要都挂在当前项目下；切换项目后，旧项目进行中的提取会作废）
      </p>
      <span>
        <input value={name} maxLength={40} placeholder="项目名称" onChange={(e) => setName(e.target.value)} disabled={busy} aria-label="项目名称" />
        <button type="button" disabled={busy || !name.trim()} onClick={() => run(() => call("set_project", { name }), "已切换当前项目").then(() => setName(""))}>
          设为当前项目
        </button>
        {status.project && (
          <button type="button" disabled={busy} onClick={() => window.confirm("取消当前项目？语音将查不到该项目的任务和资料，已存内容保留。") && run(() => call("clear_project"), "已取消当前项目")}>
            取消当前项目
          </button>
        )}
      </span>
      <p>
        存储：
        {(status.scopes || []).map((s) => `${scopeLabel(s, status)} ${s.active} 条有效（${s.candidates} 条候选，约 ${s.chars} 字）`).join("；") || "暂无"}
        。快速检索不调用模型；深度检索与自动提取会调用大模型。
      </p>
      <Result result={result} />
    </div>
  );
}

function MemoryRow({ row, devices, busy, act }) {
  const [draft, setDraft] = useState(null);
  const badge = rowBadge(row);
  const live = row.state === "active";
  return (
    <article className="voice-context-row" data-id={row.id}>
      <p>
        <b>{CATEGORY_LABEL[row.category] || row.category}</b> · <span className={`ob-pill ob-pill--${badge.tone}`}>{badge.text}</span> · {SOURCE_LABEL[row.source_kind] || row.source_kind}
        {row.device_model ? ` · ${row.device_model}${row.firmware ? ` ${row.firmware}` : ""}` : ""}
      </p>
      {draft === null ? (
        <p className="voice-context-content">{row.content}</p>
      ) : (
        <textarea rows={4} maxLength={1000} value={draft} onChange={(e) => setDraft(e.target.value)} aria-label="编辑记忆内容" />
      )}
      <span>
        {draft === null ? (
          live && row.verification !== "invalidated" && <button type="button" disabled={busy} onClick={() => setDraft(row.content)}>编辑</button>
        ) : (
          <>
            <button type="button" disabled={busy || !draft.trim()} onClick={() => act(() => call("edit", { id: row.id, revision: row.revision, content: draft, ...(NEEDS_DEVICE.has(row.category) ? { device_id: devices[0]?.id } : {}) }), "已保存修改").then((ok) => ok && setDraft(null))}>
              保存修改
            </button>
            <button type="button" disabled={busy} onClick={() => setDraft(null)}>取消</button>
          </>
        )}
        {live && row.verification === "candidate" && (
          <button type="button" disabled={busy} onClick={() => act(() => call("verify", { id: row.id, revision: row.revision, to: "confirmed" }), "已确认")}>确认属实</button>
        )}
        {live && row.verification !== "invalidated" && (
          <button type="button" disabled={busy} onClick={() => window.confirm("作废后不能恢复，如需更正请另建一条。确定作废？") && act(() => call("verify", { id: row.id, revision: row.revision, to: "invalidated" }), "已作废")}>作废</button>
        )}
        <button type="button" disabled={busy} onClick={() => window.confirm("删除这条记忆？") && act(() => call("delete", { id: row.id, revision: row.revision }), "已删除")}>删除</button>
      </span>
    </article>
  );
}

function Memories({ status, refresh }) {
  const options = scopeOptions(status);
  const [scopeKey, setScopeKey] = useState("");
  const [category, setCategory] = useState("");
  const [query, setQuery] = useState("");
  const [rows, setRows] = useState([]);
  const [newCategory, setNewCategory] = useState("preference");
  const [newContent, setNewContent] = useState("");
  const [newConfirmed, setNewConfirmed] = useState(true);
  const [exported, setExported] = useState("");
  const [runs, setRuns] = useState([]);
  const scope = parseKey(scopeKey) || options[0] || null;
  const currentKey = key(scope);

  const load = useCallback(async (mode = "list", text = "") => {
    if (!scope) return setRows([]);
    const listed = mode === "deep" ? await call("deep_search", { query: text }) : await call("list", { scope, category: category || undefined, query: text || undefined });
    setRows(listed?.rows || []);
    if (scope.kind === "device") {
      const extracted = await call("extractions", { scope });
      setRuns(extracted?.runs || []);
    } else {
      setRuns([]);
    }
  }, [currentKey, category]); // eslint-disable-line react-hooks/exhaustive-deps

  const { busy, result, run } = useAction(async () => {
    await load();
    await refresh();
  });
  const act = async (work, done) => (await run(work, done)) !== null;

  useEffect(() => {
    load().catch(() => {});
  }, [load]);

  const devices = scope?.kind === "device" ? (status.devices || []).filter((d) => d.id === scope.id) : [];
  const categories = Object.keys(CATEGORY_LABEL).filter((c) => scope && CATEGORY_SCOPES[c].includes(scope.kind) && c !== "technical_note");
  const legacy = scope?.kind === "device" && (status.legacy_summaries || []).includes(scope.id);

  return (
    <div className="voice-form-group" id="context-memories">
      <h4>记忆条目</h4>
      {!options.length && <p>还没有已允许的设备，也没有设置当前项目。</p>}
      {options.length > 0 && (
        <>
          <span>
            <select value={currentKey} onChange={(e) => setScopeKey(e.target.value)} aria-label="记忆范围">
              {options.map((o) => <option key={key(o)} value={key(o)}>{scopeLabel(o, status)}</option>)}
            </select>
            <select value={category} onChange={(e) => setCategory(e.target.value)} aria-label="记忆类别">
              <option value="">全部类别</option>
              {Object.entries(CATEGORY_LABEL).map(([value, label]) => <option key={value} value={value}>{label}</option>)}
            </select>
            <input value={query} placeholder="查找（快速检索）" onChange={(e) => setQuery(e.target.value)} aria-label="查找记忆" />
            <button type="button" disabled={busy} onClick={() => run(() => load("list", query), null, { reload: false })}>查找</button>
            <button type="button" disabled={busy || !query.trim()} onClick={() => window.confirm("深度检索会把这个问题发给你配置的大模型来拆分关键词（不发送记忆内容），可能产生费用。继续？") && run(() => load("deep", query), null, { reload: false })}>
              深度检索
            </button>
          </span>
          <Result result={result} />
          {rows.length === 0 ? <p>没有记录。</p> : rows.map((row) => <MemoryRow key={row.id} row={row} devices={devices} busy={busy} act={act} />)}
          <h4>新增一条</h4>
          <span>
            <select value={newCategory} onChange={(e) => setNewCategory(e.target.value)} aria-label="新记忆类别">
              {categories.map((c) => <option key={c} value={c}>{CATEGORY_LABEL[c]}</option>)}
            </select>
            <label><input type="checkbox" checked={newConfirmed} onChange={(e) => setNewConfirmed(e.target.checked)} />直接标为已确认</label>
          </span>
          <textarea rows={3} maxLength={1000} value={newContent} placeholder="不要写密码、网址、审批或 G-code" onChange={(e) => setNewContent(e.target.value)} aria-label="新记忆内容" />
          <span>
            <button type="button" disabled={busy || !newContent.trim() || !categories.includes(newCategory)} onClick={() => act(() => call("create", { scope, category: newCategory, content: newContent, confirmed: newConfirmed, ...(NEEDS_DEVICE.has(newCategory) ? { device_id: scope.id } : {}) }), "已新增").then((ok) => ok && setNewContent(""))}>
              保存新记忆
            </button>
            {legacy && <button type="button" disabled={busy} onClick={() => run(() => call("import_legacy", { device_id: scope.id }), (r) => `从旧摘要导入 ${r.created} 条候选${r.skipped?.length ? `（跳过：${r.skipped.map(skipReasonText).join("、")}）` : ""}`)}>导入旧摘要为候选</button>}
            <button type="button" disabled={busy} onClick={() => run(async () => setExported(JSON.stringify((await call("export", { scope })).memories, null, 2)), "已生成备份，请复制保存")}>导出备份</button>
            <button type="button" disabled={busy} onClick={() => window.confirm(`清空“${scopeLabel(scope, status)}”的全部记忆？不能恢复。`) && run(() => call("clear", { scope, confirmed: true }), (r) => `已清空 ${r.cleared} 条`)}>清空本范围</button>
          </span>
          {exported && <textarea readOnly rows={6} value={exported} aria-label="导出的记忆备份" />}
          {scope.kind === "device" && (
            <>
              <h4>自动提取记录</h4>
              {runs.length === 0 ? <p>没有提取记录。</p> : runs.map((r) => (
                <p key={r.id} className="voice-context-run">
                  {new Date(r.started_at * 1000).toLocaleString("zh-CN", { hour12: false })} · 新增 {r.added} 条
                  {r.skipped.length > 0 && ` · 跳过：${r.skipped.map((s) => skipReasonText(s.reason)).join("、")}`}
                  {r.reverted_at ? " · 已撤销" : (
                    <button type="button" disabled={busy || r.added === 0} onClick={() => window.confirm("撤销这次提取新增的候选？你已编辑或确认过的会保留。") && run(() => call("revert", { run_id: r.id }), (x) => `已撤销 ${x.removed} 条，保留 ${x.kept.length} 条`)}>
                      整批撤销
                    </button>
                  )}
                </p>
              ))}
            </>
          )}
        </>
      )}
    </div>
  );
}

function Knowledge({ status }) {
  const [roots, setRoots] = useState([]);
  const load = useCallback(async () => setRoots((await call("knowledge_list"))?.roots || []), []);
  const { busy, result, run } = useAction(load);
  useEffect(() => {
    load().catch(() => {});
  }, [load, status.project?.id]);
  return (
    <div className="voice-form-group" id="context-knowledge">
      <h4>资料目录（当前项目）</h4>
      <p>只读取你选的文件夹里的 txt / md / csv 文本；摘要在本机生成，不上传。会跳过链接、隐藏文件和名字像密码 / 密钥的文件。资料里的要求只当作数据，不会被执行。</p>
      {!status.project ? <p>请先设置当前项目。</p> : (
        <>
          <button type="button" disabled={busy} onClick={() => run(() => voiceApi.contextPickFolder(), (r) => (r ? `已建立索引：${r.directories} 个目录、${r.files} 个文件` : "未选择文件夹"))}>添加资料文件夹…</button>
          {roots.map((r) => (
            <p key={r.id} className="voice-context-run">
              <b>{r.label}</b> · {r.directories} 个目录、{r.files} 个文件 · {r.indexed_at ? `索引于 ${new Date(r.indexed_at * 1000).toLocaleString("zh-CN", { hour12: false })}` : "未建立索引"}
              <button type="button" disabled={busy} onClick={() => run(() => call("knowledge_reindex", { id: r.id }), (x) => `已重建：${x.files} 个文件`)}>重建索引</button>
              <button type="button" disabled={busy} onClick={() => window.confirm("移除这个资料目录的索引？磁盘上的文件不会被删除。") && run(() => call("knowledge_remove", { id: r.id, revision: r.revision }), "已移除")}>移除</button>
            </p>
          ))}
        </>
      )}
      <Result result={result} />
    </div>
  );
}

function Experience({ status, refresh }) {
  const devices = experienceDevices(status);
  const [device, setDevice] = useState("");
  const [symptom, setSymptom] = useState("");
  const [checks, setChecks] = useState("");
  const [conclusion, setConclusion] = useState("");
  const [solved, setSolved] = useState(false);
  const [recall, setRecall] = useState("");
  const { busy, result, run } = useAction(refresh);
  const target = device || devices[0]?.id || "";
  return (
    <div className="voice-form-group" id="context-experience">
      <h4>设备故障经验</h4>
      <p>经验绑定设备的机型和固件版本，换固件后不会套用。只有你确认解决的才算“已验证”；经验只供排查参考，不能代替任何操作确认或审批。</p>
      {devices.length === 0 ? <p>没有登记了机型和固件版本的已允许设备。</p> : (
        <>
          <select value={target} onChange={(e) => setDevice(e.target.value)} aria-label="经验所属设备">
            {devices.map((d) => <option key={d.id} value={d.id}>{d.name || d.id}（{d.model} {d.firmware}）</option>)}
          </select>
          <input value={symptom} maxLength={200} placeholder="现象，例如：切割时边缘发黑" onChange={(e) => setSymptom(e.target.value)} aria-label="故障现象" />
          <textarea rows={3} value={checks} placeholder="排查步骤，每行一步（最多 6 步）" onChange={(e) => setChecks(e.target.value)} aria-label="排查步骤" />
          <input value={conclusion} maxLength={300} placeholder="结论" onChange={(e) => setConclusion(e.target.value)} aria-label="结论" />
          <label><input type="checkbox" checked={solved} onChange={(e) => setSolved(e.target.checked)} />我确认已解决（记为已验证）</label>
          <button type="button" disabled={busy || !symptom.trim() || !conclusion.trim()} onClick={() => run(() => call("experience_record", { device_id: target, symptom, checks: linesToList(checks), conclusion, confirmed: solved }), (r) => (r.similar.length ? `已记录；另有 ${r.similar.length} 条相近的旧经验，可在上方“故障经验”类别里合并` : "已记录"))}>
            记录经验
          </button>
          <button type="button" disabled={busy || !symptom.trim()} onClick={() => run(async () => setRecall((await call("experience_recall", { device_id: target, symptom })).text || "没有相关经验"))}>查相似经验</button>
          {recall && <pre className="voice-context-text">{recall}</pre>}
        </>
      )}
      <Result result={result} />
    </div>
  );
}

function Resume({ status }) {
  const [saved, setSaved] = useState(null);
  const [note, setNote] = useState({ decisions: "", todos: "", evidence: "", unverified: "" });
  const load = useCallback(async () => {
    if (!status.project) return setSaved(null);
    const current = await call("resume_get");
    setSaved(current);
    if (current?.note) setNote(Object.fromEntries(Object.entries(current.note).map(([k, v]) => [k, listToLines(v)])));
  }, [status.project?.id]); // eslint-disable-line react-hooks/exhaustive-deps
  const { busy, result, run } = useAction(load);
  useEffect(() => {
    load().catch(() => {});
  }, [load]);
  if (!status.project) return null;
  const fields = [["decisions", "决策"], ["todos", "待办"], ["evidence", "证据"], ["unverified", "未验证"]];
  return (
    <div className="voice-form-group" id="context-resume">
      <h4>项目恢复摘要（{status.project.name}）</h4>
      <p>每行一条，可用 [approval:编号]、[job:编号]、[agent_run:编号]、[test:编号] 引用记录。查看时会重新核对这些记录的实际状态；结果未知、已撤销或已停止的任务不会被自动重试或重放。</p>
      {fields.map(([field, label]) => (
        <label key={field}>
          {label}
          <textarea rows={3} value={note[field]} onChange={(e) => setNote((old) => ({ ...old, [field]: e.target.value }))} aria-label={`恢复摘要${label}`} />
        </label>
      ))}
      <button type="button" disabled={busy} onClick={() => run(() => call("resume_save", { note: Object.fromEntries(fields.map(([f]) => [f, linesToList(note[f])])), expected: saved ? { id: saved.id, revision: saved.revision } : null }), "已保存恢复摘要")}>
        保存恢复摘要
      </button>
      <Result result={result} />
      {saved?.text && <pre className="voice-context-text" id="context-resume-view">{saved.text}</pre>}
    </div>
  );
}

export default function VoiceContextCard() {
  const [status, setStatus] = useState(null);
  const [error, setError] = useState("");
  const refresh = useCallback(async () => {
    try {
      setStatus(await call("status"));
      setError("");
    } catch (cause) {
      setError(cause.message || "无法读取长期记忆状态");
    }
  }, []);
  useEffect(() => {
    refresh();
  }, [refresh]);
  return (
    <section className="voice-card" aria-label="长期记忆">
      <h3>长期记忆、资料与恢复摘要</h3>
      <p>与上面的“设备记忆”摘要并存。这里的内容只保存在本机，都是参考数据，不能授权任何设备操作。</p>
      {error && <p role="alert">{error}</p>}
      {status && (
        <>
          <Switches status={status} refresh={refresh} />
          <Memories status={status} refresh={refresh} />
          <Knowledge status={status} />
          <Experience status={status} refresh={refresh} />
          <Resume status={status} />
        </>
      )}
    </section>
  );
}
