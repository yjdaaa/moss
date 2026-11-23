import React, { useEffect, useRef, useState } from "react";
import { Play, Square, Check, FlaskConical, Trash2, Download, Copy, RefreshCw, Volume2 } from "lucide-react";
import { showAlert, showConfirm } from "../../appDialog.js";
import { voiceApi } from "../../voiceApi.js";
import { SaveBar } from "../VoiceLayout.jsx";
import {
  colabBridgeDefaultOpen,
  colabSavePayload,
  colabSettingsError,
  colabSettingsForm,
  parseColabPaste,
  savedNoteText,
  voiceErrorText,
} from "../../voiceState.js";
import { restartEffect } from "./shared.js";

/**
 * n80 Colab T4 专属联动（多模态 Qwen3-VL-8B + 电影 MOSS 550W 原声克隆 Qwen3-TTS-0.6B）：
 * - 默认关闭（`enabled = false`），未开启时完全不改变用户现有的大模型、视觉理解、语音合成与其余设置；
 * - 自动检测本机 `colab` CLI 登录状态（未登录显示「未登录 Colab CLI」并支持一键拉起 Google OAuth 授权码登录）；
 * - 已登录 `colab` CLI 时，打开开关会自动调用 `colab` CLI 在后台分配云端 T4 GPU、拉起 Qwen3-VL-8B + Qwen3-TTS-0.6B + 隧道并自动回填地址与密钥；
 * - 开启且就绪后，运行时只临时接管「大模型 + 摄像头看图 + 550W 克隆语音合成」三项，关闭即刻恢复原三项配置。
 */
function ColabBridgeSection({ builtin }) {
  const { busy } = builtin;
  const colab = builtin.status?.local?.colab || {};
  const cli = colab.cli || {};
  const cliInstalled = Boolean(cli.installed);
  const cliLoggedIn = Boolean(cli.logged_in);
  const runnerState = cli.runner_state || "idle";
  const runnerBusy = runnerState === "starting";
  const keyConfigured = Boolean(colab.key_configured);
  const active = Boolean(colab.active);
  const baseline = colabSettingsForm(colab);
  const baselineSig = JSON.stringify({ ...baseline, keyConfigured });
  const [form, setForm] = useState(baseline);
  const [testing, setTesting] = useState(false);
  const [testResult, setTestResult] = useState(null);
  const [saveResult, setSaveResult] = useState(null);
  const [exporting, setExporting] = useState(false);
  const [cliBusy, setCliBusy] = useState("");
  const [authCode, setAuthCode] = useState("");
  const [showAuthInput, setShowAuthInput] = useState(false);
  // 默认收起；已开启、接管中、正在拉起 / 运行或有拉起报错时自动展开，用户手动点开 / 收起后以用户为准
  const [openOverride, setOpenOverride] = useState(null);
  const open = openOverride ?? colabBridgeDefaultOpen(colab);
  const refAudioInputRef = useRef(null);
  const refAudioConfigured = Boolean(cli.ref_audio_configured);
  const refAudioName = cli.ref_audio_name || "";

  // 已保存的设置内容（签名）变了才同步表单；运行时读最新的设置
  const latestBaselineRef = useRef(baseline);
  useEffect(() => {
    latestBaselineRef.current = baseline;
  });
  useEffect(() => {
    setForm((prev) => ({
      ...latestBaselineRef.current,
      api_key: prev.api_key || "",
    }));
  }, [baselineSig]);

  const dirty =
    form.enabled !== baseline.enabled ||
    form.url.trim() !== baseline.url.trim() ||
    form.llm_model.trim() !== baseline.llm_model.trim() ||
    form.tts_model.trim() !== baseline.tts_model.trim() ||
    form.tts_voice.trim() !== baseline.tts_voice.trim() ||
    String(form.tts_speed).trim() !== String(baseline.tts_speed).trim() ||
    Boolean(form.api_key.trim());
  const validationError = colabSettingsError(form, keyConfigured, cliLoggedIn);

  const updateField = (key, value) => {
    setForm((current) => ({ ...current, [key]: value }));
    setSaveResult(null);
  };

  const handleUrlChange = (raw) => {
    const parsed = parseColabPaste(raw);
    setForm((current) => ({
      ...current,
      url: parsed.url,
      api_key: parsed.api_key || current.api_key,
    }));
    setSaveResult(null);
    setTestResult(null);
  };

  const runCliAction = async (action, extra = {}, successNote = "") => {
    setCliBusy(action);
    setSaveResult(null);
    try {
      const next = await voiceApi.localSave({ colab_cli: { action, ...extra } });
      builtin.setStatus(next);
      setForm(colabSettingsForm(next?.local?.colab));
      if (action === "login_start") {
        setShowAuthInput(true);
      } else if (action === "login_complete") {
        setShowAuthInput(false);
        setAuthCode("");
      }
      if (successNote) {
        setSaveResult({ ok: true, text: successNote });
      }
      return next;
    } catch (error) {
      setSaveResult({ ok: false, text: voiceErrorText(error) });
      return null;
    } finally {
      setCliBusy("");
    }
  };

  const commitColab = async (nextForm, noteTarget = "Colab T4 专属联动") => {
    const err = colabSettingsError(nextForm, keyConfigured, cliLoggedIn);
    if (err) {
      setSaveResult({ ok: false, text: err });
      return false;
    }
    builtin.setBusy("local-save-colab");
    setSaveResult(null);
    try {
      const next = await voiceApi.localSave({ colab: colabSavePayload(nextForm) });
      builtin.setStatus(next);
      setForm(colabSettingsForm(next?.local?.colab));
      const nextCli = next?.local?.colab?.cli || {};
      const autoStarting = Boolean(nextForm.enabled && nextCli.runner_state === "starting");
      setSaveResult({
        ok: true,
        text: savedNoteText(
          noteTarget,
          autoStarting
            ? "已通过 Colab CLI 在后台自动分配云端 T4 并启动模型，就绪后自动接管"
            : `${nextForm.enabled ? "已接管大模型、看图与克隆合成" : "已关闭联动并恢复原配置"} · ${restartEffect(next)}`,
        ),
      });
      return true;
    } catch (error) {
      setSaveResult({ ok: false, text: `保存失败：${voiceErrorText(error)}` });
      return false;
    } finally {
      builtin.setBusy("");
    }
  };

  const handleToggleEnabled = async (checked) => {
    const nextForm = { ...form, enabled: checked };
    setSaveResult(null);
    if (!checked) {
      setForm(nextForm);
      if (baseline.enabled) {
        await commitColab(nextForm, "关闭 Colab T4 联动");
      }
      return;
    }
    const err = colabSettingsError(nextForm, keyConfigured, cliLoggedIn);
    if (err) {
      // 未登录 Colab CLI 且未填写手动隧道地址/密钥时，保持开关关闭并直接提示用户先登录或填地址
      setForm((prev) => ({ ...prev, enabled: false }));
      setSaveResult({ ok: false, text: err });
      return;
    }
    setForm(nextForm);
    await commitColab(nextForm, "开启 Colab T4 联动");
  };

  const handlePickRefAudio = (e) => {
    const file = e.target.files?.[0];
    e.target.value = "";
    if (!file) return;
    if (file.size > 15 * 1024 * 1024) {
      setSaveResult({ ok: false, text: "参考音频文件不能超过 15 MB（建议裁剪为 3~8 秒干净 WAV 人声）" });
      return;
    }
    const reader = new FileReader();
    reader.onload = async () => {
      const dataUrl = String(reader.result || "");
      await runCliAction(
        "set_ref_audio",
        { file_name: file.name, wav_base64: dataUrl },
        `已保存 550W 参考音频（${file.name}），云端 T4 启动时将自动上传克隆`,
      );
    };
    reader.onerror = () => {
      setSaveResult({ ok: false, text: "读取参考音频文件失败" });
    };
    reader.readAsDataURL(file);
  };

  const handleTest = async () => {
    const checkForm = { ...form, enabled: true };
    const err = colabSettingsError(checkForm, keyConfigured, false);
    if (err) {
      setTestResult({ ok: false, text: err, audio: null });
      return;
    }
    setTesting(true);
    setTestResult(null);
    try {
      const res = await voiceApi.builtinModelsTest("colab", {
        target: "all",
        ...colabSavePayload(form),
      });
      setTestResult({
        ok: Boolean(res?.ok),
        text: res?.detail || (res?.ok ? "联动测试通过" : "联动测试未通过"),
        audio: res?.audio_data_url || null,
      });
    } catch (error) {
      setTestResult({ ok: false, text: `测试失败：${voiceErrorText(error)}`, audio: null });
    } finally {
      setTesting(false);
    }
  };

  const handleExportNotebook = async () => {
    setExporting(true);
    try {
      const res = await voiceApi.colabExportNotebook();
      if (res?.saved) {
        setSaveResult({
          ok: true,
          text: `已导出 ${res.file_name || "moss-colab-t4.ipynb"}，上传到 Google Colab 选 T4 GPU 按顺序运行 3 个单元格即可。`,
        });
      }
    } catch (error) {
      await showAlert(voiceErrorText(error), { title: "导出 Colab 笔记本失败", tone: "error" });
    } finally {
      setExporting(false);
    }
  };

  const statePillText = active
    ? "已开启 · 接管中"
    : runnerBusy
      ? "正在自动拉起 Colab T4…"
      : form.enabled
        ? cliLoggedIn
          ? "待自动连接云端 T4"
          : "待登录 Colab CLI 或填地址"
        : "未开启（保持原配置）";

  return (
    <details
      className="voice-fold voice-colab-fold"
      id="voice-local-colab-bridge"
      open={open}
      onToggle={(e) => setOpenOverride(e.currentTarget.open)}
    >
      <summary>
        <span className="voice-model-name">
          <b>Colab T4 专属联动（多模态大脑 + 550W 克隆声）</b>
          <span
            className={`ob-pill ${active ? "ob-pill--ok" : form.enabled || runnerBusy ? "ob-pill--warn" : "ob-pill--off"}`}
            id="voice-colab-state-pill"
          >
            {statePillText}
          </span>
          {dirty && <span className="ob-pill ob-pill--warn voice-dirty">未保存</span>}
        </span>
      </summary>
      <div className="voice-local-group">
        <div className="voice-field-head voice-colab-lead-row">
          <p className="voice-muted">
            借助免费 Google Colab T4 临时接管「大模型 + 摄像头看图 + 550W 克隆声」，不改动现有配置，关闭即恢复。
          </p>
          <button
            type="button"
            className="btn btn-ghost"
            onClick={handleExportNotebook}
            disabled={Boolean(busy || exporting || cliBusy)}
            id="voice-colab-export-ipynb"
          >
            <Download size={13} /> {exporting ? "导出中…" : "导出 Colab 笔记本（.ipynb）"}
          </button>
        </div>
      <div
        className={`voice-hint voice-hint--${!cliInstalled ? "warn" : cliLoggedIn ? "ok" : "warn"} voice-colab-cli-bar`}
        id="voice-colab-cli-bar"
      >
        <div className="voice-colab-cli-head">
          <span id="voice-colab-cli-status">
            <b>Colab CLI：</b>
            {!cliInstalled
              ? "未安装（点右侧一键安装，或点右上角导出笔记本到浏览器手动运行）"
              : cliLoggedIn
                ? `已登录${cli.account_hint ? `（${cli.account_hint}）` : ""} · 打开下方开关自动拉起云端 T4`
                : "未登录（点右侧完成 Google 授权，或下方手动填隧道地址）"}
          </span>
          <div className="voice-colab-cli-actions">
            {!cliInstalled ? (
              <button
                type="button"
                className="btn btn-primary"
                disabled={Boolean(busy || cliBusy)}
                onClick={async () => {
                  const ok = await showConfirm(
                    "将调用本机 uv 或 Python pip 从 PyPI 联网下载并安装外部命令行工具包 google-colab-cli（安装到当前用户目录，不修改系统 Python）。\n\n如果不想在本机安装外部 CLI，也可以点取消，改用右上角「导出 Colab 笔记本（.ipynb）」在浏览器手动运行。",
                    {
                      title: "确认安装外部 Colab CLI 工具",
                      tone: "warning",
                      confirmText: "确认联网安装",
                      cancelText: "取消",
                    },
                  );
                  if (!ok) return;
                  await runCliAction(
                    "install",
                    {},
                    "已安装 Colab CLI！请接着点击「登录 Colab CLI」完成 Google 授权。",
                  );
                }}
                id="voice-colab-cli-install"
              >
                <Download size={13} /> {cliBusy === "install" ? "正在安装 Colab CLI…" : "一键安装 Colab CLI"}
              </button>
            ) : !cliLoggedIn ? (
              <button
                type="button"
                className="btn btn-primary"
                disabled={Boolean(busy || cliBusy)}
                onClick={() => runCliAction("login_start", {}, "已在浏览器打开 Google 授权页，请复制授权码粘贴到下方完成登录")}
                id="voice-colab-cli-login"
              >
                {cliBusy === "login_start" ? "正在打开授权页…" : "登录 Colab CLI"}
              </button>
            ) : (
              <>
                <button
                  type="button"
                  className="btn btn-ghost"
                  disabled={Boolean(busy || cliBusy || runnerBusy)}
                  onClick={() => runCliAction("start_session", {}, "已在后台启动 Colab T4 极速并发部署，请稍候…")}
                  id="voice-colab-cli-start"
                >
                  <Play size={13} /> {runnerBusy ? "云端 T4 启动中…" : active ? "重新拉起云端 T4" : "一键拉起云端 T4"}
                </button>
                {(runnerBusy || active || runnerState === "running") && (
                  <button
                    type="button"
                    className="btn btn-ghost"
                    disabled={Boolean(busy || cliBusy)}
                    onClick={() => runCliAction("stop_session", {}, "已释放云端 Colab T4 会话")}
                    id="voice-colab-cli-stop"
                  >
                    <Square size={13} /> 释放云端 T4
                  </button>
                )}
                <button
                  type="button"
                  className="btn btn-ghost"
                  disabled={Boolean(busy || cliBusy || runnerBusy)}
                  onClick={() => runCliAction("logout", {}, "已退出 Colab CLI 登录")}
                  id="voice-colab-cli-logout"
                >
                  退出登录
                </button>
              </>
            )}
            <button
              type="button"
              className="btn btn-ghost"
              disabled={Boolean(busy || cliBusy)}
              onClick={() => runCliAction("refresh")}
              id="voice-colab-cli-refresh"
              title="刷新 Colab CLI 状态"
            >
              <RefreshCw size={13} />
            </button>
          </div>
        </div>
        {cliInstalled && !cliLoggedIn && (showAuthInput || cli.auth_url) && (
          <div className="voice-colab-auth-box" id="voice-colab-cli-auth-box">
            <input
              className="text-input mono voice-colab-auth-input"
              value={authCode}
              disabled={Boolean(cliBusy)}
              placeholder="粘贴浏览器 Google 授权后显示的 4/0A... 授权码"
              onChange={(e) => setAuthCode(e.target.value)}
              spellCheck="false"
              autoComplete="off"
              id="voice-colab-cli-auth-code"
            />
            {cli.auth_url && (
              <button
                type="button"
                className="btn btn-ghost"
                disabled={Boolean(cliBusy)}
                onClick={async () => {
                  try {
                    await navigator.clipboard?.writeText(cli.auth_url);
                    setSaveResult({ ok: true, text: "已复制 Google 授权链接，可粘贴到任意已登录 Google 的浏览器打开" });
                  } catch {
                    // 忽略剪贴板权限错误
                  }
                }}
                id="voice-colab-cli-copy-url"
              >
                <Copy size={13} /> 复制授权链接
              </button>
            )}
            <button
              type="button"
              className="btn btn-primary"
              disabled={Boolean(cliBusy || !authCode.trim())}
              onClick={() => runCliAction("login_complete", { code: authCode.trim() }, "Colab CLI 登录成功！现在打开开关即可自动启动云端 T4。")}
              id="voice-colab-cli-auth-submit"
            >
              <Check size={13} /> {cliBusy === "login_complete" ? "正在验证…" : "完成登录"}
            </button>
          </div>
        )}
        <div className="voice-colab-ref-row" id="voice-colab-ref-audio-row">
          <span className="voice-colab-ref-note">
            <b>550W 原声克隆样本（本机保存）：</b>
            {refAudioConfigured
              ? `已选「${refAudioName}」（启动云端 T4 时自动上传）`
              : "未选（默认内置低沉男声，建议 3~8 秒干净 .wav）"}
          </span>
          <div className="voice-colab-cli-actions">
            <input
              ref={refAudioInputRef}
              type="file"
              accept=".wav,audio/wav"
              className="sr-only-file"
              onChange={handlePickRefAudio}
              id="voice-colab-ref-audio-input"
            />
            <button
              type="button"
              className="btn btn-ghost"
              disabled={Boolean(busy || cliBusy)}
              onClick={() => refAudioInputRef.current?.click()}
              id="voice-colab-ref-audio-pick"
            >
              <Volume2 size={13} /> {refAudioConfigured ? "更换 550W 参考音频（.wav）" : "选择 550W 参考音频（.wav）"}
            </button>
            {refAudioConfigured && (
              <button
                type="button"
                className="btn btn-ghost"
                disabled={Boolean(busy || cliBusy)}
                onClick={() => runCliAction("clear_ref_audio", {}, "已清除自定义参考音频，恢复内置低沉男声基准")}
                id="voice-colab-ref-audio-clear"
              >
                <Trash2 size={13} /> 恢复默认
              </button>
            )}
          </div>
        </div>
        {cli.runner_step && (
          <div className="mono voice-colab-runner-step" id="voice-colab-cli-runner-step">
            {runnerBusy ? "⏳ " : runnerState === "running" ? "✅ " : ""}
            {cli.runner_step}
          </div>
        )}
        {cli.runner_error && (
          <div className="voice-colab-runner-error" role="alert" id="voice-colab-cli-runner-error">
            {cli.runner_error}
          </div>
        )}
      </div>
      <div className="voice-form-grid">
        <label className="voice-check voice-wide">
          <input
            type="checkbox"
            checked={form.enabled}
            disabled={Boolean(busy || testing || cliBusy)}
            onChange={(e) => handleToggleEnabled(e.target.checked)}
            id="voice-colab-enabled"
          />
          <span>
            启用 Colab T4 专属联动（{cliLoggedIn ? "开启后自动拉起云端 T4 并接管三项" : "开启后接管大模型、看图与克隆声"}）
          </span>
        </label>
        <label className="voice-wide">
          <span>Colab 隧道地址（支持整段粘贴配置块）</span>
          <input
            className="text-input mono"
            value={form.url}
            disabled={Boolean(busy || testing)}
            placeholder={cliLoggedIn ? "开启开关后自动回填，也可手动粘贴 https://xxx.trycloudflare.com/v1" : "https://xxx.trycloudflare.com/v1"}
            onChange={(e) => handleUrlChange(e.target.value)}
            spellCheck="false"
            autoComplete="off"
            id="voice-colab-url"
          />
        </label>
        <label>
          <span>API 密钥（{keyConfigured ? "已保存，留空沿用" : "未设置"}）</span>
          <input
            className="text-input mono"
            type="password"
            autoComplete="new-password"
            value={form.api_key}
            disabled={Boolean(busy || testing)}
            placeholder={
              keyConfigured
                ? "已保存（换新密钥时再填）"
                : cliLoggedIn
                  ? "开启后自动生成并保存"
                  : "粘贴 Colab 打印的 moss-... 密钥"
            }
            onChange={(e) => updateField("api_key", e.target.value)}
            id="voice-colab-key"
          />
        </label>
        <label>
          <span>克隆合成语速（0.25 ~ 4.0）</span>
          <input
            className="text-input"
            inputMode="decimal"
            value={form.tts_speed}
            disabled={Boolean(busy || testing)}
            onChange={(e) => updateField("tts_speed", e.target.value)}
            id="voice-colab-speed"
          />
        </label>
      </div>
      <details className="voice-fold" id="voice-colab-advanced">
        <summary>高级：自定义模型名与音色标识（一般无需修改）</summary>
        <div className="voice-form-grid">
          <label>
            <span>多模态大模型名</span>
            <input
              className="text-input mono"
              value={form.llm_model}
              disabled={Boolean(busy || testing)}
              onChange={(e) => updateField("llm_model", e.target.value)}
              id="voice-colab-llm-model"
            />
          </label>
          <label>
            <span>克隆合成模型名</span>
            <input
              className="text-input mono"
              value={form.tts_model}
              disabled={Boolean(busy || testing)}
              onChange={(e) => updateField("tts_model", e.target.value)}
              id="voice-colab-tts-model"
            />
          </label>
          <label>
            <span>克隆音色标识</span>
            <input
              className="text-input mono"
              value={form.tts_voice}
              disabled={Boolean(busy || testing)}
              onChange={(e) => updateField("tts_voice", e.target.value)}
              id="voice-colab-tts-voice"
            />
          </label>
        </div>
      </details>
      {dirty && validationError && (
        <div className="voice-hint voice-hint--bad" role="alert" id="voice-colab-error">
          {validationError}
        </div>
      )}
      {testResult && (
        <div className={`voice-hint voice-hint--${testResult.ok ? "ok" : "bad"}`} role={testResult.ok ? "status" : "alert"} id="voice-colab-test-result">
          <div>{testResult.text}</div>
          {testResult.audio && (
            <audio controls src={testResult.audio} style={{ marginTop: 6, height: 32, maxWidth: "100%" }} id="voice-colab-test-audio" />
          )}
        </div>
      )}
      <div className="voice-actions" style={{ marginTop: 6, flexWrap: "wrap", gap: 8 }}>
        <button
          type="button"
          className="btn btn-ghost"
          onClick={handleTest}
          disabled={Boolean(busy || testing || runnerBusy)}
          id="voice-colab-test"
        >
          <FlaskConical size={13} /> {testing ? "正在测试三项…" : "测试联动（对话 + 看图 + 克隆声）"}
        </button>
        <SaveBar
          id="voice-colab-save"
          target="Colab T4 专属联动"
          dirty={dirty}
          busy={Boolean(busy || testing || cliBusy)}
          saving={busy === "local-save-colab"}
          blocked={validationError ? "请先登录 Colab CLI 或补齐地址与密钥" : ""}
          result={saveResult}
          onSave={() => commitColab(form)}
        />
      </div>
      </div>
    </details>
  );
}

export { ColabBridgeSection };
