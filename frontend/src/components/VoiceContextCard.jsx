import React, { useCallback, useEffect, useState } from "react";
import { voiceApi } from "../voiceApi.js";
import { showConfirm } from "../appDialog.js";
import { SOURCE_LABEL, scopeLabel, scopeOptions } from "../voiceContextState.js";

// 无感合一版语音记忆面板（技术书 n50）：
// 唯一总开关「让小智记住我的习惯」+ 随手补充输入框 + 单列可改可删列表。
// 口头说“记住…”实时 0ms 入库、口头纠错自动覆盖、满 8 轮后台自动浓缩均在服务端无感完成。

const call = (action, extra = {}) => voiceApi.contextManage({ action, ...extra });
const ask = (message, opts = {}) => showConfirm(message, { title: "请确认", tone: "warning", ...opts });
const key = (scope) => (scope ? `${scope.kind}:${scope.id}` : "");
const parseKey = (value) => {
  const at = value.indexOf(":");
  return at > 0 ? { kind: value.slice(0, at), id: value.slice(at + 1) } : null;
};

function formatShortTime(seconds) {
  if (!seconds) return "";
  return new Date(seconds * 1000).toLocaleString("zh-CN", {
    hour12: false,
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function MemoryItemRow({ row, multiScope, busy, onEdit, onDelete }) {
  const [draft, setDraft] = useState(null);
  return (
    <article className="voice-context-row" data-id={row.id}>
      {draft === null ? (
        <p className="voice-context-content">{row.content}</p>
      ) : (
        <textarea
          rows={2}
          maxLength={1000}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          aria-label="编辑记忆内容"
        />
      )}
      <div className="voice-memory-meta-row">
        <small className="voice-muted">
          {multiScope && row.scope_label ? `${row.scope_label} · ` : ""}
          {SOURCE_LABEL[row.source_kind] || "已记录"}
          {row.updated_at ? ` · ${formatShortTime(row.updated_at)}` : ""}
        </small>
        <span>
          {draft === null ? (
            <button type="button" disabled={busy} onClick={() => setDraft(row.content)}>
              编辑
            </button>
          ) : (
            <>
              <button
                type="button"
                disabled={busy || !draft.trim()}
                onClick={() => onEdit(row, draft).then((ok) => ok && setDraft(null))}
              >
                保存修改
              </button>
              <button type="button" disabled={busy} onClick={() => setDraft(null)}>
                取消
              </button>
            </>
          )}
          <button type="button" disabled={busy} onClick={() => onDelete(row)}>
            删除
          </button>
        </span>
      </div>
    </article>
  );
}

export default function VoiceContextCard() {
  const [status, setStatus] = useState(null);
  const [rows, setRows] = useState([]);
  const [scopeKey, setScopeKey] = useState("");
  const [newContent, setNewContent] = useState("");
  const [query, setQuery] = useState("");
  const [appliedQuery, setAppliedQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const [result, setResult] = useState(null);

  const loadAll = useCallback(async (searchText = "") => {
    const [nextStatus, listed] = await Promise.all([
      call("status"),
      call("list_all", searchText ? { query: searchText } : {}),
    ]);
    setStatus(nextStatus);
    setRows(Array.isArray(listed?.rows) ? listed.rows : []);
    setError("");
  }, []);

  useEffect(() => {
    loadAll("").catch((e) => setError(e?.message || "无法读取记忆状态"));
  }, [loadAll]);

  const act = async (work, doneText, { reloadQuery = appliedQuery } = {}) => {
    setBusy(true);
    setResult(null);
    setError("");
    try {
      await work();
      await loadAll(reloadQuery);
      if (doneText) setResult({ ok: true, text: doneText });
      return true;
    } catch (e) {
      setResult({ ok: false, text: e?.message || "操作失败" });
      return false;
    } finally {
      setBusy(false);
    }
  };

  const memoryEnabled = Boolean(status?.retrieval || status?.extraction);
  const writableOptions = scopeOptions(status).filter((o) => o.kind === "device" || o.kind === "person");
  const selectedScope = parseKey(scopeKey) || writableOptions[0] || null;
  const multiScope = writableOptions.length > 1;

  const toggleMaster = async (next) => {
    if (
      next &&
      !(await ask(
        "开启后，小智会在本机自动记住你的偏好与常用习惯；使用云端大模型时，过滤后的对话文字会发给该模型服务商（不上传录音，自动拦截密码、密钥、网址与 G-code）。确定开启？",
      ))
    ) {
      return;
    }
    await act(
      () =>
        call("set_options", {
          retrieval: next,
          extraction: next,
          confirmed: next,
          sync_legacy_memory: true,
        }),
      next ? "已开启自动记忆" : "已关闭自动记忆",
    );
  };

  const handleAdd = async () => {
    if (!newContent.trim()) return;
    const ok = await act(
      () =>
        call("quick_create", {
          content: newContent,
          ...(selectedScope ? { scope: selectedScope } : {}),
        }),
      "已记住",
    );
    if (ok) setNewContent("");
  };

  const handleEdit = (row, text) =>
    act(() => call("edit", { id: row.id, revision: row.revision, content: text }), "已保存修改");

  const handleDelete = async (row) => {
    if (!(await ask("删除这条记忆？", { danger: true, confirmText: "删除" }))) return;
    await act(() => call("delete", { id: row.id, revision: row.revision }), "已删除");
  };

  const handleClearAll = async () => {
    if (!(await ask("清空当前全部已记住的内容？清空后不可恢复。", { danger: true, confirmText: "全部清空" }))) return;
    await act(() => call("clear_all", { confirmed: true }), "已清空全部记忆", { reloadQuery: "" });
    setQuery("");
    setAppliedQuery("");
  };

  return (
    <section className="voice-card" aria-label="对话记忆">
      <h3>对话记忆</h3>
      <p>
        对小智说“记住我常切 3mm 椴木板”“以后回答简短点”即可实时记住，改口时会自动替换旧习惯。内容只保存在本机，自动拦截密码、密钥、网址、审批与 G-code，不能授权任何设备操作。
      </p>
      {error && <p role="alert">{error}</p>}
      {status && (
        <>
          <label>
            <input
              type="checkbox"
              id="context-memory-switch"
              checked={memoryEnabled}
              disabled={busy}
              onChange={(e) => toggleMaster(e.target.checked)}
            />
            让小智记住我的习惯（默认关闭）
          </label>

          {writableOptions.length === 0 ? (
            <p>暂无已允许的小智设备；在“服务”标签允许设备接入后，对话习惯会自动按设备记录，也可在此手动补充。</p>
          ) : (
            <span>
              {multiScope && (
                <select
                  value={key(selectedScope)}
                  onChange={(e) => setScopeKey(e.target.value)}
                  disabled={busy}
                  aria-label="记忆范围"
                >
                  {writableOptions.map((o) => (
                    <option key={key(o)} value={key(o)}>
                      {scopeLabel(o, status)}
                    </option>
                  ))}
                </select>
              )}
              <input
                value={newContent}
                maxLength={1000}
                disabled={busy}
                placeholder="随手补一条习惯，例如：我常切 3mm 椴木板 / 回答尽量简短"
                onChange={(e) => setNewContent(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter" && newContent.trim() && !busy) {
                    e.preventDefault();
                    void handleAdd();
                  }
                }}
                aria-label="新记忆内容"
              />
              <button
                type="button"
                className="btn btn-primary"
                disabled={busy || !newContent.trim()}
                onClick={handleAdd}
              >
                保存新记忆
              </button>
            </span>
          )}

          {result && (
            <p role={result.ok ? "status" : "alert"}>
              {result.ok ? "✓ " : ""}
              {result.text}
            </p>
          )}

          <div className="voice-form-group" id="context-memories">
            <div className="voice-memory-list-head">
              <h4>已记住的内容（{rows.length}）</h4>
              {(rows.length > 3 || Boolean(appliedQuery)) && (
                <span>
                  <input
                    value={query}
                    placeholder="按词筛选"
                    disabled={busy}
                    onChange={(e) => setQuery(e.target.value)}
                    aria-label="查找记忆"
                  />
                  <button
                    type="button"
                    disabled={busy}
                    onClick={() => {
                      setAppliedQuery(query);
                      void loadAll(query);
                    }}
                  >
                    查找
                  </button>
                  {(query || appliedQuery) && (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => {
                        setQuery("");
                        setAppliedQuery("");
                        void loadAll("");
                      }}
                    >
                      重置
                    </button>
                  )}
                </span>
              )}
              {rows.length > 0 && (
                <button type="button" disabled={busy} onClick={handleClearAll}>
                  清空全部
                </button>
              )}
            </div>

            {rows.length === 0 ? (
              <p>{appliedQuery ? "没有匹配的记忆条目。" : "暂未记录任何习惯。"}</p>
            ) : (
              rows.map((row) => (
                <MemoryItemRow
                  key={row.id}
                  row={row}
                  multiScope={multiScope}
                  busy={busy}
                  onEdit={handleEdit}
                  onDelete={handleDelete}
                />
              ))
            )}
          </div>
        </>
      )}
    </section>
  );
}
