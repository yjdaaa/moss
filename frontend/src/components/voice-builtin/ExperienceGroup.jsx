import React from "react";
import { SaveBar, VoiceGroup } from "../VoiceLayout.jsx";
import {
  DEFAULT_JEV_MODEL,
  EXPERIENCE_KEYS,
  BUILTIN_ASR_CORRECTION_PREVIEW,
  MAX_CUSTOM_PERSONAS,
  newCustomPersonaId,
  parseCustomPersonas,
  personaOptions,
  canonicalLocalJevModel,
  experienceSettingsError,
  isSpecificLocalJevInstalled,
} from "../../voiceState.js";
import { JEV_GROUP_TITLE } from "./shared.js";
import { useLocalGroup } from "./useLocalGroup.js";

/** n59 自建角色：名字 + 人设，最多 6 个；表单里存 JSON 文字。 */
function CustomPersonaEditor({ value, onChange, onRemove }) {
  const roles = parseCustomPersonas(value);
  const commit = (next) => onChange(JSON.stringify(next));
  const edit = (index, key, text) => commit(roles.map((role, i) => (i === index ? { ...role, [key]: text } : role)));
  return (
    <div className="voice-wide voice-persona-editor" id="voice-custom-personas" style={{ display: "grid", gap: 8 }}>
      <span>自建角色（可选，最多 {MAX_CUSTOM_PERSONAS} 个；名字最多 12 字，人设最多 300 字，写性格、说话方式和口头禅；不能写 G-code）</span>
      {roles.map((role, index) => (
        <div className="voice-persona-row" key={role.id} style={{ display: "flex", flexWrap: "wrap", gap: 8, alignItems: "flex-start" }}>
          <input
            className="text-input"
            type="text"
            maxLength={12}
            placeholder="角色名，如：老师傅"
            style={{ flex: "0 1 10em", minWidth: "7em" }}
            value={role.name}
            onChange={(e) => edit(index, "name", e.target.value)}
            aria-label={`自建角色 ${index + 1} 名字`}
          />
          <textarea
            className="text-input"
            rows={2}
            maxLength={300}
            placeholder="例如：说话像干了二十年的老师傅，话少，先说结论，偶尔提醒一句安全。"
            style={{ flex: "1 1 14em", minWidth: 0 }}
            value={role.prompt}
            onChange={(e) => edit(index, "prompt", e.target.value)}
            aria-label={`自建角色 ${index + 1} 人设`}
          />
          <button
            type="button"
            className="btn btn-ghost"
            onClick={() => {
              commit(roles.filter((_, i) => i !== index));
              onRemove?.(role.id);
            }}
          >
            删除
          </button>
        </div>
      ))}
      {roles.length < MAX_CUSTOM_PERSONAS && (
        <button
          type="button"
          className="btn btn-ghost"
          id="voice-custom-persona-add"
          style={{ justifySelf: "start" }}
          onClick={() => commit([...roles, { id: newCustomPersonaId(roles), name: "", prompt: "" }])}
        >
          新增角色
        </button>
      )}
    </div>
  );
}

/**
 * 对话体验：说话角色、快捷查询、热词与纠偏、Jev 细项、插话。
 * n69：小智彩屏画风只在“设备 › 副屏预览”切换、Jev 开关只在顶部第 3 栏切换，这里各显示一行状态和跳转按钮。
 */
export function ExperienceGroup({ builtin }) {
  const group = useLocalGroup(builtin, EXPERIENCE_KEYS, experienceSettingsError);
  const { baseline, draft: form, update, dirty, error, result } = group;
  const busy = builtin.busy;
  const selectedJevModel = form.jev_model || baseline.jev_model || DEFAULT_JEV_MODEL;
  const activeLocalJevId = canonicalLocalJevModel(selectedJevModel);
  const isLocalJev = Boolean(activeLocalJevId);
  const localJevInstalled = activeLocalJevId
    ? isSpecificLocalJevInstalled(builtin.status, activeLocalJevId)
    : false;
  const jevKeyConfigured = isLocalJev
    ? localJevInstalled
    : Boolean(builtin.status?.jev?.key_set || builtin.status?.jev?.openrouter_llm_key_set || builtin.status?.local?.jev_key_configured);
  const jevOn = Boolean(baseline.jev_filter_enabled);
  const styleLabel = personaOptions(baseline.custom_personas).find((opt) => opt.id === (baseline.persona_style || "workshop"))?.label?.replace(/（.*）$/, "") || "简明工坊助手";
  const corrCount = String(baseline.custom_asr_corrections || "").split(/\r?\n/).filter((w) => w.trim()).length;
  // 热词：本地 SenseVoice 与 FireRedASR2-CTC 不支持；X-ASR 热词识别只对本地 X-ASR 有意义（2026-11-08 n73 只在用得上时显示）
  const localAsrModel = baseline.asr ? String(baseline.asr_model || "") : "";
  const hotwordsSupported = !localAsrModel.startsWith("sense_voice") && localAsrModel !== "firered_asr2_ctc";
  const xAsrInUse = localAsrModel.startsWith("x_asr");
  const summary = [
    styleLabel,
    baseline.llm_fast_reply !== false ? "快速回答开" : "快速回答关",
    baseline.barge_in ? "插话开" : "插话关",
    baseline.asr_homophone_fix !== false ? `同音纠偏开${corrCount ? `(+${corrCount})` : ""}` : "同音纠偏关",
  ].join(" · ");
  return (
    <VoiceGroup id="experience" title="对话体验" summary={summary} dirty={dirty}>
      <p className="voice-muted">停止词整句命中时立即请求急停，不能关闭；设备是否停下仍需确认。</p>
      <div className="voice-form-grid">
        <label className="voice-wide">
          <span>说话角色</span>
          <select
            className="select-input"
            value={form.persona_style || "workshop"}
            onChange={(e) => update("persona_style", e.target.value)}
            id="voice-persona-style"
          >
            {personaOptions(form.custom_personas).map((opt) => (
              <option key={opt.id} value={opt.id}>
                {opt.label}
              </option>
            ))}
          </select>
          <small className="voice-muted" id="voice-persona-hint">
            {personaOptions(form.custom_personas).find((opt) => opt.id === (form.persona_style || "workshop"))?.hint || ""}
            {" "}只改语气，复述确认与急停对所有角色锁定；声音在“语音合成”里换；也可以对小智说“换成冷面幽默”。
          </small>
        </label>
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.persona_risk_notes === true}
            onChange={(e) => update("persona_risk_notes", e.target.checked)}
            id="voice-persona-risk-notes"
          />
          <span title="所有角色都能用：建议时只说把握较大 / 一般 / 不大，不报百分比，点出最主要的风险；没把握建议先在边角料上试。">
            给加工建议时附上把握高低和主要风险
          </span>
        </label>
        <CustomPersonaEditor
          value={form.custom_personas}
          onChange={(next) => update("custom_personas", next)}
          onRemove={(id) => {
            // Deleting the selected role falls back to the default one, as the backend does.
            if ((form.persona_style || "workshop") === id) update("persona_style", "workshop");
          }}
        />
        <label className="voice-wide">
          <span>工坊背景说明（可选，最多 300 字）</span>
          <input
            className="text-input"
            type="text"
            maxLength={300}
            placeholder="例如：工坊主力机是翼宿 5W 蓝光雕刻机，平时主要做木质书签与亚克力铭牌"
            value={form.persona_prompt || ""}
            onChange={(e) => update("persona_prompt", e.target.value)}
            id="voice-persona-prompt"
          />
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.llm_fast_reply !== false} onChange={(e) => update("llm_fast_reply", e.target.checked)} id="voice-llm-fast-reply" />
          <span title="对阿里云百炼、硅基流动、火山方舟、智谱、OpenRouter、OpenAI GPT-5 等请求关闭大模型思考，每轮少等几秒；服务不支持时自动按原样调用。复述确认、电脑审批与急停由程序把关，不受影响。推理专用模型（如 DeepSeek-R1）关不掉。">
            快速回答（让大模型不先思考，回答更快；默认开）
          </span>
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.job_done_announce} onChange={(e) => update("job_done_announce", e.target.checked)} id="voice-job-announce" />
          <span title="只在设备在线且当前不在说话时播报；播报中被打断按插话机制处理。">加工结束后主动播报结果</span>
        </label>
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.screen_preview_sync !== false}
            onChange={(e) => update("screen_preview_sync", e.target.checked)}
            id="voice-screen-preview-sync"
          />
          <span title="彩屏显示这次的刀路与实时进度，等电脑批准、完工、急停、听歌也有对应画面；单色 OLED 显示滚动参数卡；只生成预览时不推。画风在“设备 › 副屏预览”里选。">
            开始雕刻后同步到小智屏幕
          </span>
        </label>
        <label className="voice-check voice-wide">
          <input type="checkbox" checked={form.barge_in} onChange={(e) => update("barge_in", e.target.checked)} id="voice-barge-in" />
          <span title="须先在设备固件开启设备端回声消除并使用 realtime 模式；未消除的回声可能误打断。MOSS 不修改固件。">
            播报时可直接插话（需设备开启回声消除）
          </span>
        </label>
        {form.barge_in && (
          <label>
            <span>连续插话时长（300 ~ 600 毫秒）</span>
            <input className="text-input" type="number" min="300" max="600" step="20" value={form.barge_in_ms} onChange={(e) => update("barge_in_ms", e.target.value)} id="voice-barge-in-ms" />
          </label>
        )}
      </div>

      <details className="voice-more" id="voice-experience-accuracy">
        <summary>识别准确度：热词与同音纠偏</summary>
        <div className="voice-form-grid">
          <label className="voice-check voice-wide">
            <input
              type="checkbox"
              checked={form.asr_homophone_fix !== false}
              onChange={(e) => update("asr_homophone_fix", e.target.checked)}
              id="voice-asr-homophone-fix"
            />
            <span title={`云端和本地识别出的文字都会先过一遍内置纠偏表：${BUILTIN_ASR_CORRECTION_PREVIEW}`}>激光行业同音错词自动纠偏</span>
          </label>
          <label className="voice-wide">
            <span title="不管识别模型是否支持热词都立即生效；急停等安全词受保护，不会被改写。">自己的同音纠偏（每行一条“错词=正词”，最多 64 条）</span>
            <textarea
              className="text-input"
              rows={3}
              placeholder={"例如：\n红松木=樟子松\n大鱼一宿=大鱼翼宿"}
              value={form.custom_asr_corrections || ""}
              onChange={(e) => update("custom_asr_corrections", e.target.value)}
              id="voice-asr-corrections"
            />
          </label>
          {hotwordsSupported ? (
            <label className="voice-wide">
              <span title="自动合并材料名称和行业词。X-ASR 暂不支持英文热词。">自定义热词（每行一条，最多 64 条）</span>
              <textarea className="text-input" rows={3} value={form.custom_hotwords} onChange={(e) => update("custom_hotwords", e.target.value)} id="voice-hotwords" />
            </label>
          ) : (
            <p className="voice-muted voice-wide" id="voice-hotwords-unsupported">
              当前本地识别模型 {localAsrModel === "firered_asr2_ctc" ? "FireRedASR2" : "SenseVoice"} 不支持热词，可用上面的同音纠偏修正常错的词。
            </p>
          )}
          {xAsrInUse && (
            <label className="voice-check voice-wide">
              <input type="checkbox" checked={form.asr_beam_search} onChange={(e) => update("asr_beam_search", e.target.checked)} id="voice-asr-beam" />
              <span>X-ASR 热词识别（识别较慢时可关闭）</span>
            </label>
          )}
        </div>
      </details>

      <details className="voice-more" id="voice-experience-advanced">
        <summary>高级：快捷查询与 Jev 细项</summary>
        <div className="voice-form-grid">
          <label className="voice-check voice-wide">
            <input type="checkbox" checked={form.read_only_shortcuts} onChange={(e) => update("read_only_shortcuts", e.target.checked)} id="voice-read-shortcuts" />
            <span title="问状态、连接、进度、剩余时间与材料时直接回答，不经大模型。开启 Jev 后口语化的问法也能走这条快路。">快捷查询（状态、进度等直接回答，不经大模型）</span>
          </label>
          {jevOn ? (
            <div className="voice-wide voice-stack" id="voice-jev-suboptions">
              <div className={`voice-hint ${jevKeyConfigured ? "" : "voice-hint--bad"}`}>
                {isLocalJev
                  ? jevKeyConfigured
                    ? `Jev 已开启：本地决策模型 ${activeLocalJevId}（识别文字不出本机）；异常时自动回退常规链路。`
                    : `本地决策模型（${activeLocalJevId}）尚未下载：请到“本地模型（断网可用）”下载，未就绪时自动回退常规链路。`
                  : jevKeyConfigured
                    ? `Jev 已开启：${selectedJevModel}；超时或限流时自动回退常规链路。`
                    : `尚未配置 OpenRouter API Key：请到本标签上方“${JEV_GROUP_TITLE}”保存密钥；未配置时自动回退常规链路。`}
              </div>
              <label className="voice-check voice-wide">
                <input type="checkbox" checked={form.jev_unfinished_hold} onChange={(e) => update("jev_unfinished_hold", e.target.checked)} id="voice-jev-unfinished" />
                <span title="停顿想词时单次最多多等 1.2 秒，并提前缓存本句路由。">半截话不抢话</span>
              </label>
              <label className="voice-check voice-wide">
                <input type="checkbox" checked={form.read_only_shortcuts && form.jev_fast_route} disabled={!form.read_only_shortcuts} onChange={(e) => update("jev_fast_route", e.target.checked)} id="voice-jev-fast-route" />
                <span>口语问状态也直接回答{form.read_only_shortcuts ? "" : "（需先开启快捷查询）"}</span>
              </label>
              <label className="voice-check voice-wide">
                <input type="checkbox" checked={form.jev_tool_prune} onChange={(e) => update("jev_tool_prune", e.target.checked)} id="voice-jev-tool-prune" />
                <span title="内置与扩展工具收拢为 6 个工具组按需挂载；日常闲聊不带工具进大模型，缩短首字延迟。">只给大模型带用得上的工具</span>
              </label>
              <label className="voice-check voice-wide">
                <input type="checkbox" checked={form.jev_memory_filter} onChange={(e) => update("jev_memory_filter", e.target.checked)} id="voice-jev-memory-filter" />
                <span>剔除字面相近但无关的长期记忆</span>
              </label>
            </div>
          ) : (
            <p className="voice-muted voice-wide" id="voice-jev-off-note">在本页顶部链路栏第 3 栏开启 Jev 后，这里会出现它的细项。</p>
          )}
        </div>
      </details>
      {dirty && error && <div className="voice-hint voice-hint--bad">{error}</div>}
      <SaveBar id="voice-experience-save" target="对话体验设置" dirty={dirty} busy={Boolean(busy)} saving={busy === "local-save-对话体验设置"} blocked={error ? "请先改正上面的错误" : ""} result={result} onSave={() => group.save("对话体验设置")} />
    </VoiceGroup>
  );
}
