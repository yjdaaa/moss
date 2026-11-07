// P4-08 / UI-3 voice center browser test: real React app, isolated Vite loopback server,
// synthetic stateful Tauri bridge (no real xiaozhi-server, device, model or network).
// Covers link switching with confirmation, server start/stop, grouped saves that submit
// only their own fields, unsaved markers that survive tab switches, and narrow layout.
// Run: node tests/voice-center.browser.mjs [--baseline]   (from frontend/; needs Playwright + Edge)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import path from "node:path";
import { createServer } from "vite";

const require = createRequire(import.meta.url);
const { chromium } = require("playwright");
const root = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
const baselineDir = path.join(root, "tests", "visual-baseline");
const writeBaseline = process.argv.includes("--baseline");

const vite = await createServer({ root, server: { host: "127.0.0.1", port: 0, strictPort: false }, logLevel: "error" });
let browser;
try {
  await vite.listen();
  const local = vite.resolvedUrls?.local?.[0];
  if (!local) throw new Error("Vite loopback server URL unavailable");
  browser = await chromium.launch({ headless: true, channel: process.env.PLAYWRIGHT_CHANNEL || (process.platform === "win32" ? "msedge" : undefined) });
  const context = await browser.newContext({ serviceWorkers: "block", viewport: { width: 1440, height: 1000 } });
  const page = await context.newPage();
  const errors = [];
  const calls = [];
  page.on("pageerror", (error) => errors.push(error.message));

  // ---------- 有状态的假桌面端 ----------
  const masked = "wss://api.example.test/mcp/?token=***";
  const state = {
    mode: "official",
    server: { state: "stopped", owned: false },
    settings: { server_dir: "", python: "", args: ["app.py"], server_port: 8000, builtin_ws_port: 8000, builtin_http_port: 8003, official_mcp_endpoint: masked, official_mcp_endpoint_set: true },
    builtin: {
      running: false, mode: "echo", link_mode: "official", ws_port: 8000, http_port: 8003, ota_url: "http://192.0.2.10:8003/xiaozhi/ota/",
      models: {}, jev: { endpoint: "https://openrouter.ai/api/alpha/decisions", model: "typesafe/jev-1.14", key_set: false, openrouter_llm_key_set: false, voice_enabled: false, feedback_enabled: false }, local: { settings: {}, jev_key_configured: false, onejev_env: { ready: true, installed: true, binary_found: true, accelerator: "metal", accelerator_label: "Apple Metal GPU 加速", summary: "OneJev 原生推理引擎已就绪（Apple Metal GPU 加速 · 免装 Python）" }, jev_runtime: { model: "onejev_1_6_4b", state: "failed", accelerator: null, error: "OneJev 模型加载超时", notice: null }, laya_env: { ready: true, python_found: true, laya_serve_found: true, accelerator: "mps", cuda_found: false, mps_found: true, summary: "运行环境检测通过（Python + laya[serve] + Apple Metal MPS 加速已就绪），可直接下载并启用。" }, models: [{ id: "silero_vad", kind: "vad", label: "Silero VAD", installed: true }, { id: "kokoro", kind: "tts", label: "Kokoro v1.0", installed: true, voices: { default: 50, max: 53, hint: "45-48" } }, { id: "onejev_1_6_0_8b", kind: "jev", label: "OneJev 1.6 0.8B（轻量极速，推荐）", size: 1369128576, note: "Qwen3.5-0.8B 全参微调", installed: true }, { id: "onejev_1_6_4b", kind: "jev", label: "OneJev 1.6 4B（均衡主力）", size: 3328881152, note: "Qwen3.5-4B 全参微调", installed: true }, { id: "onejev_1_6_9b", kind: "jev", label: "OneJev 1.6 9B（高阶旗舰）", size: 6599787584, note: "Qwen3.5-9B 全参微调", installed: false }, { id: "laya_multilingual", kind: "jev", label: "Laya 决策模型（322M 多语种）", size: 1291563689, note: "mDeBERTa + RL", installed: true }], helper_found: true },
      debug_enabled: false,
      debug_traces: [],
      device_prefs: {},
      goggle_bot_gifs: ["idle", "engrave", "approval", "music", "celebrate"].map((id) => ({ id, label: id, data_url: `data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7#${id}-bot` })),
      screen_style_gifs: {
        moss_hud: Object.fromEntries(["idle", "engrave", "approval", "music", "celebrate"].map((id) => [id, `data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7#${id}-moss`])),
        goggle_bot: Object.fromEntries(["idle", "engrave", "approval", "music", "celebrate"].map((id) => [id, `data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7#${id}-bot`])),
      },
      sessions: [{ session_id: "s-dev1", device_id: "aa:bb:cc:11:22:33", ip: "192.0.2.55", protocol_version: 1, connected_at: 1_790_000_000, state: "idle", level: 0, screen_monochrome: false, screen_width: 320, screen_height: 240, device_tools: ["self.get_device_status", "self.audio_speaker.set_volume", "self.screen.set_brightness", "self.screen.set_theme", "self.screen.preview_image", "self.set_press_to_talk"] }],
      // n68：结构化事件（级别 / 分类 / 下一步 / 技术细节）、覆盖计数与降级提示
      events: [
        { seq: 3, at: 1_790_000_002, at_ms: 1_790_000_002_345, level: "info", category: "general", text: "识别完成：7 字" },
        { seq: 2, at: 1_790_000_001, at_ms: 1_790_000_001_120, level: "error", category: "auth", text: "对话出错：大模型返回 HTTP 401，密钥无效或没有权限", hint: "密钥填错、过期或被停用了：到服务方控制台复制新密钥重新填写", detail: "服务方原话：Incorrect API key provided" },
        { seq: 1, at: 1_790_000_000, at_ms: 1_790_000_000_010, level: "warn", category: "device", text: "设备 aa:bb:cc:11:22:33 已断开：连接突然中断（最可能是 Wi-Fi 掉线、设备重启或断电）" },
      ],
      events_dropped: { info: 12, problems: 0, timings: 0, max_info: 300, max_problems: 200, max_timings: 50 },
      warnings: ["本地音乐目录当前不可用（换电脑或移动盘拔出时请重新选择音乐文件夹），本次启动已跳过本地音乐"],
      timings: [{ session_id: "s-dev1", device_id: "aa:bb:cc:11:22:33", at_ms: 1_790_000_003_000, turn: 2, outcome: "ignored", models: ["fake-asr"], stages: { recording_start: 0, final_asr: 320 }, tools: [] }],
      devices: [{ device_id: "aa:bb:cc:11:22:33", name: "立创实战派小智", model: "lichuang-dev", firmware_version: "2.2.0", status: "allowed", last_seen: 1_790_000_000, status_json: JSON.stringify({ audio_speaker: { volume: 65 }, screen: { brightness: 80, theme: "dark" } }) }],
    },
    agent: {
      settings: { enabled: false, workspace: "", codex_executable: "", claude_executable: "" },
      strict_mode: false, providers: [], runs: [],
    },
    kbRoots: [],
    memoryState: { enabled: false, rows: [] },
  };
  const voiceStatus = (extra = {}) => ({
    mode: state.mode, settings: structuredClone(state.settings), server: structuredClone(state.server),
    server_config: { exists: true, full_module: false }, endpoint: { health: null }, device: { state: state.mode === "self_hosted" ? "waiting" : "unknown" },
    bridge: { connected: true }, strict: { enabled: state.mode === "self_hosted", locked: state.mode === "self_hosted", from_env: false }, gateway: { enabled: false },
    models: {}, mcp_endpoint: masked,
    mcp: { endpoints: [{ id: "primary", endpoint: masked, connected: true }], tools: [{ name: "get_status", description: "状态", enabled: true }], max_extra_endpoints: 4 },
    ...extra,
  });
  const handlers = {
    get_backend_origin: () => "http://127.0.0.1:9666",
    get_desktop_session: () => "a".repeat(64),
    voice_status: () => voiceStatus(),
    voice_server_log: () => ({ lines: ["[fake] started api_key=secret"] }),
    voice_builtin_status: () => structuredClone(state.builtin),
    voice_diagnostics_export: () => ({ saved: true, file_name: "moss-voice-diagnostics-1790000000.json", bytes: 4096, events: state.builtin.events.length }),
    voice_mcp_status: () => ({ servers: [], connections: [], audit: [], revision: "r1", max_servers: 8 }),
    voice_memory_list: () => [],
    voice_context_pick_folder: () => {
      const added = { id: "kr-1", label: "椴木工艺笔记", enabled: true, revision: 1, indexed_at: 1_790_000_000, directories: 1, files: 3 };
      state.kbRoots = [added];
      return added;
    },
    voice_context_manage: ({ input }) => {
      switch (input?.action) {
        case "knowledge_list": return { roots: structuredClone(state.kbRoots) };
        case "knowledge_toggle": {
          const r = state.kbRoots.find((x) => x.id === input.id);
          if (r) { r.enabled = Boolean(input.enabled); r.revision += 1; }
          return structuredClone(r || {});
        }
        case "knowledge_reindex": return { directories: 1, files: 3, skipped: {} };
        case "knowledge_remove": state.kbRoots = state.kbRoots.filter((x) => x.id !== input.id); return { removed: true };
        case "status": return { retrieval: state.memoryState.enabled, extraction: state.memoryState.enabled, project: null, devices: [{ id: "dev-1", name: "测试小智", model: "MOSS", firmware: "1.0" }], persons: [], scopes: [], legacy_summaries: [] };
        case "list_all": return { rows: structuredClone(state.memoryState.rows) };
        case "set_options": state.memoryState.enabled = Boolean(input.retrieval); return { retrieval: state.memoryState.enabled, extraction: state.memoryState.enabled };
        case "quick_create": {
          const row = { id: "ctx-1", scope: { kind: "device", id: "dev-1" }, scope_label: "测试小智", category: "preference", content: input.content, verification: "confirmed", source_kind: "user_edit", revision: 1, updated_at: 1_790_000_000, state: "active" };
          state.memoryState.rows.unshift(row);
          return row;
        }
        default: return {};
      }
    },
    voice_detect_python: () => ({ candidates: ["C:\\Python312\\python.exe"], suggested: "C:\\Python312\\python.exe" }),
    voice_strict_set: ({ enabled }) => ({ enabled, applied: true, needs_restart: false, env_file: "C:\\moss\\.env" }),
    voice_music_status: () => ({
      settings: { enabled: true, directory: "D:\\unplugged-music" },
      track_count: 0,
      warning: "本地音乐目录当前不可用（换电脑或移动盘拔出时请重新选择音乐文件夹），本次启动已跳过本地音乐",
    }),
    voice_vision_status: () => ({ settings: { enabled: false, url: "", model: "" }, key_configured: false }),
    voice_agent_status: () => ({ configured: state.agent.settings.enabled, strict_mode: state.agent.strict_mode, providers: state.agent.providers, runs: structuredClone(state.agent.runs), settings: structuredClone(state.agent.settings) }),
    // n67：唤醒词与固件卡挂在主窗口语音中心（回环假数据，不连设备）
    voice_wakeword_status: () => ({
      running: true,
      word: { text: "你好小智", command: "ni hao xiao zhi", threshold: 0.2 },
      base: null,
      devices: [],
    }),
    voice_firmware_status: () => ({ artifacts: [], devices: [], records: [], offer_hours: 24, now: 0 }),
    get_export_settings: () => ({ dir: "", effective_dir: "C:\\moss\\exports", available: true, ask_each_time: false }),
    voice_agent_save: ({ input }) => {
      state.agent.settings = structuredClone(input);
      state.agent.providers = input.codex_executable
        ? [{ provider: "codex", ready: true, version: "codex-cli 0.157.1" }, { provider: "claude_code", ready: false, error_code: "agent_capability_missing" }]
        : [];
      return handlers.voice_agent_status();
    },
    voice_agent_stop: ({ runId }) => {
      const run = state.agent.runs.find((item) => item.id === runId);
      const stopped = Boolean(run && run.status === "running");
      if (stopped) Object.assign(run, { status: "cancelled", stoppable: false, error_code: "agent_stopped" });
      return { ...handlers.voice_agent_status(), stopped };
    },
    voice_save_settings: ({ settings }) => {
      // 与 center.rs 一致：官方/内置链路下官方接入点变了才重连，并在返回里报告 reconnected
      let reconnected = false;
      for (const [key, value] of Object.entries(settings)) {
        if (key === "official_mcp_endpoint") {
          reconnected = state.mode !== "self_hosted" && state.officialRaw !== value;
          state.officialRaw = value;
          state.settings[key] = masked;
        } else state.settings[key] = value;
      }
      return voiceStatus({ reconnected });
    },
    voice_mode_changes: ({ mode }) => ({ changes: [`语音链路：${state.mode} → ${mode}`] }),
    voice_set_mode: ({ mode }) => {
      const stillRunning = mode !== "self_hosted" && state.server.state === "running";
      state.mode = mode;
      state.builtin.link_mode = mode;
      return voiceStatus({ server_still_running: stillRunning });
    },
    voice_server_start: () => {
      state.server = { state: "running", owned: true, pid: 4242, started_at: 1_790_000_000 };
      return voiceStatus();
    },
    voice_server_stop: () => {
      state.server = { state: "stopped", owned: false };
      return voiceStatus();
    },
    voice_builtin_models_test: ({ section }) => ({
      ok: true,
      elapsed_ms: 180,
      detail: "合成测试响应",
      ...(section === "tts" ? { audio_data_url: "data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=" } : {}),
    }),
    voice_builtin_models_list: () => ({
      ok: true,
      elapsed_ms: 85,
      models: ["Qwen/Qwen2.5-7B-Instruct", "FunAudioLLM/SenseVoiceSmall", "MOSS/TTS-1"],
      detail: "已读取 3 个模型",
    }),
    voice_builtin_models_save: ({ input, clear }) => {
      if (Array.isArray(clear) && clear.includes("jev")) {
        state.builtin.jev = { ...state.builtin.jev, key_set: false, voice_enabled: false, feedback_enabled: false, model: "typesafe/jev-1.14" };
        state.builtin.local.jev_key_configured = false;
        state.builtin.local.settings.jev_filter_enabled = false;
      }
      for (const [section, value] of Object.entries(input || {})) {
        if (section === "jev") {
          state.builtin.jev = {
            endpoint: "https://openrouter.ai/api/alpha/decisions",
            model: value.model,
            key_set: true,
            openrouter_llm_key_set: false,
            voice_enabled: Boolean(value.voice_enabled),
            feedback_enabled: Boolean(value.feedback_enabled),
          };
          state.builtin.local.jev_key_configured = true;
          state.builtin.local.settings.jev_model = value.model;
          state.builtin.local.settings.jev_filter_enabled = Boolean(value.voice_enabled);
        } else {
          state.builtin.models[section] = { configured: true, url: value.url, model: value.model, key_set: true };
        }
      }
      return { ...structuredClone(state.builtin), restarted: false };
    },
    voice_local_save: ({ settings }) => {
      state.builtin.local.settings = { ...state.builtin.local.settings, ...structuredClone(settings) };
      if (state.builtin.jev) {
        if (Object.prototype.hasOwnProperty.call(settings, "jev_filter_enabled")) {
          state.builtin.jev.voice_enabled = Boolean(settings.jev_filter_enabled);
        }
        if (settings.jev_model) state.builtin.jev.model = settings.jev_model;
      }
      if (Object.prototype.hasOwnProperty.call(settings, "debug_trace_enabled")) {
        state.builtin.debug_enabled = Boolean(settings.debug_trace_enabled);
        if (state.builtin.debug_enabled) {
          state.builtin.debug_traces = [
            {
              session_id: "s-dev1",
              turn: 1,
              at: 1_790_000_100,
              raw_asr: "帮我切段木书签",
              corrected_asr: "帮我切椴木书签",
              tools: [{ name: "generate_text_laser_task_tool", arguments: "{\"material\":\"椴木\"}", result: "预览已生成" }],
              reply: "已准备 3 毫米椴木预览，确认开始吗？",
              outcome: "done",
            },
          ];
        } else {
          state.builtin.debug_traces = [];
        }
      }
      if (settings.clear_debug_traces) state.builtin.debug_traces = [];
      return { ...structuredClone(state.builtin), restarted: false };
    },
    // 2026-11-07：下载请求故意慢一点，验证等待期间其他模型的按钮不被锁住
    voice_local_model_download: async ({ id }) => {
      await new Promise((resolve) => setTimeout(resolve, 600));
      const model = state.builtin.local.models.find((item) => item.id === id.replace(/:force$/, ""));
      if (model.id === "laya_multilingual") {
        model.progress = { state: "downloading", done: 0, total: model.size };
        state.builtin.local.laya_setup = { state: "running", stage: "安装 PyTorch（CUDA 12.4 · cu124，约 2.5 GB）", step: 4, steps: 6, detail: "Downloading torch-2.5.1+cu124", elapsed_secs: 65 };
      } else {
        model.progress = { state: "waiting_runtime" };
      }
      return structuredClone(state.builtin);
    },
    voice_local_laya_setup: ({ action }) => {
      if (action === "cancel") state.builtin.local.laya_setup = { state: "cancelled" };
      return structuredClone(state.builtin);
    },
    voice_device_request: ({ deviceId, request, value }) => {
      if (request === "set_volume" && value && typeof value.volume === "number") {
        state.builtin.device_prefs[deviceId] = { ...(state.builtin.device_prefs[deviceId] || {}), volume: value.volume };
      }
      if (request === "set_talk_mode" && value && typeof value.mode === "string") {
        state.builtin.device_prefs[deviceId] = { ...(state.builtin.device_prefs[deviceId] || {}), talk_mode: value.mode };
      }
      return { requested: request };
    },
  };
  await page.exposeFunction("__fakeInvoke", async (command, args = {}) => {
    calls.push({ command, args: structuredClone(args) });
    if (!handlers[command]) throw new Error(`Synthetic Tauri command unavailable: ${command}`);
    return handlers[command](args);
  });
  await context.addInitScript(() => {
    window.__TAURI__ = { core: { invoke: (command, args) => window.__fakeInvoke(command, args) }, event: { listen: async () => () => {} } };
  });
  await context.route("**/*", async (route) => {
    const target = new URL(route.request().url());
    if (target.origin === new URL(local).origin) return route.continue();
    if (target.hostname === "127.0.0.1" && target.port === "9666" && target.pathname === "/v1/approvals" && target.searchParams.get("status") === "all") {
      // n68：失败的审批显示原因与错误码；标题按后端 limit 写“最近 N 条”
      const approvals = [{ approval_id: "apr_fail", status: "failed", kind: "laser_job", summary: "椴木书签", source: "voice", created_at: 1_790_000_000, outcome: { success: false, message: "控制器忙，未开始加工", error_code: "controller_busy" } }];
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { approvals, limit: 50, server_time: 1_790_000_100, ttl_seconds: 120 } }) });
    }
    if (target.hostname === "127.0.0.1" && target.port === "9666") {
      return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ success: true, result: { mcp: {}, approvals: [] } }) });
    }
    return route.abort("blockedbyclient");
  });

  const last = (command) => [...calls].reverse().find((call) => call.command === command);
  const confirmDialog = async (button) => {
    const dialog = page.getByRole("alertdialog").filter({ has: page.getByRole("button", { name: button, exact: true }) });
    await dialog.waitFor();
    const id = await dialog.getAttribute("aria-labelledby");
    await dialog.getByRole("button", { name: button, exact: true }).click();
    await page.locator(`[aria-labelledby="${id}"]`).waitFor({ state: "detached" });
  };
  const tab = (name) => page.getByRole("tab", { name: new RegExp(`^${name}`) });
  const openGroup = async (id) => {
    const header = page.locator(`#voice-group-${id} > .voice-group-h`);
    if ((await header.getAttribute("aria-expanded")) !== "true") await header.click();
  };
  const shot = async (name) => {
    if (writeBaseline) await page.screenshot({ path: path.join(baselineDir, `${name}.png`) });
  };

  await page.goto(new URL("voice", local).href, { waitUntil: "domcontentloaded" });
  await page.locator("#voice-mode-card").waitFor();
  assert.equal(await page.getByRole("tab").count(), 4, "官方链路只有 服务 / 设备 / 扩展 / 记录与诊断（n69：唤醒词并入设备）");
  assert.equal(await page.getByRole("tab", { name: /^唤醒词与音量/ }).count(), 0, "“唤醒词与音量”标签已并入“设备”");
  await tab("设备").click();
  assert.match(await page.locator("#voice-wake-volume-mode-hint").innerText(), /页面顶部“语音链路”/, "非内置链路提示到顶部语音链路卡片切换");
  assert.equal(await page.locator("#voice-group-wake-word").count(), 0, "非内置链路只提示切换，不渲染唤醒词入口");
  assert.equal(await page.locator("#voice-group-builtin-devices").count(), 0, "非内置链路不渲染内置设备列表");
  await tab("服务").click();
  assert.equal(await page.getByRole("tab", { name: /^模型/ }).count(), 0, "官方链路的模型在小智控制台配置");
  assert.equal(await page.getByRole("tab", { name: /^隐私与家居/ }).count(), 0, "非内置链路不显示隐私与家居标签");
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-mode"), "official", "全息孪生舱默认跟随官方链路");
  assert.match(await page.locator("#voice-moss-twin-source").innerText(), /CLOUD MCP TELEMETRY/);
  // 1. 服务：官方链路只显示接入点与地址；其他链路的面板不渲染（不是折叠）
  await tab("服务").click();
  assert.ok(await page.locator("#voice-endpoint-card").isVisible(), "官方链路显示接入点");
  assert.equal(await page.locator("#voice-builtin-card").count(), 0, "官方链路不渲染内置服务面板");
  assert.equal(await page.locator("#voice-server-card").count(), 0, "官方链路不渲染自建服务端面板");
  await shot("voice-official");
  await tab("设备").click();
  assert.ok(await page.locator("#voice-group-endpoints").isVisible(), "官方链路渲染接入点状态");
  assert.equal(await page.locator("#voice-panel-devices #voice-group-tools").count(), 0, "n69：内置工具不在设备标签");

  // 1b. 内置工具与第三方 MCP 是两类东西：内置工具只能开关，增删用可复制的 AI 提示词模板（2026-11-02）；n69 起两者都在“扩展”
  await tab("扩展").click();
  assert.ok(await page.locator("#voice-panel-features #voice-group-tools").isVisible(), "官方链路在扩展标签渲染工具开关与调用统计");
  await openGroup("tools");
  assert.equal(await page.locator("#voice-tool-prompt-copy-add").count(), 1);
  assert.equal(await page.locator("#voice-tool-prompt-copy-remove").count(), 1);
  await page.locator("#voice-tool-prompts > summary").click();
  assert.match(await page.locator("#voice-tool-prompt-add pre").textContent(), /crates\/laser-tools\/src\/lib\.rs/, "新增模板给出真实改代码路径");
  assert.match(await page.locator("#voice-tool-prompt-remove pre").textContent(), /laser_safe_action_tool/, "删除模板保护急停工具");
  await page.locator("#voice-tool-prompt-copy-add").click();
  await page.waitForFunction(() => {
    const box = document.querySelector("#voice-group-tools");
    return Boolean(box && (/已复制/.test(box.textContent) || /剪贴板/.test(box.textContent)));
  }, null, { timeout: 5_000 });
  assert.match(await page.locator("#voice-group-mcp > .voice-group-h").innerText(), /第三方 MCP 工具/, "外部工具仍叫第三方 MCP 工具，和内置工具区分开");
  await openGroup("mcp");
  assert.ok(await page.locator("#voice-mcp-card").getByText(/MOSS 内置工具/).isVisible(), "MCP 卡说明与内置工具的区别");
  await tab("设备").click();

  // 2. 连接：只保存官方接入点这一组；未保存标记在切换标签后仍在，填写不丢
  await tab("服务").click();
  assert.equal(await page.locator("#voice-group-settings-builtinPorts").count(), 0, "内置端口不属于官方链路");
  await openGroup("settings-official");
  const save = page.locator("#voice-settings-save-official");
  assert.ok(await save.isDisabled(), "没改动不能保存");
  await page.locator("#voice-official-endpoint").fill("wss://api.example.test/mcp/?token=fake-ui-token");
  assert.ok(await page.locator("#voice-group-settings-official .voice-dirty").isVisible(), "分组显示未保存");
  await tab("服务").click();
  assert.ok(await tab("服务").locator(".voice-tab-dot").isVisible(), "标签显示未保存");
  await tab("服务").click();
  assert.equal(await page.locator("#voice-official-endpoint").inputValue(), "wss://api.example.test/mcp/?token=fake-ui-token", "切换标签不丢填写");
  await save.click();
  await page.locator("#voice-group-settings-official [role=status]").filter({ hasText: "已保存官方接入点" }).filter({ hasText: "正在重新连接小智" }).waitFor();
  assert.deepEqual(last("voice_save_settings").args.settings, { official_mcp_endpoint: "wss://api.example.test/mcp/?token=fake-ui-token" }, "只提交本组字段");
  assert.equal(await page.locator("#voice-group-settings-official .voice-dirty").count(), 0);
  assert.equal(await tab("服务").locator(".voice-tab-dot").count(), 0);
  assert.ok(!(await page.locator("body").innerText()).includes("fake-ui-token"), "令牌不回显");

  // 3. 切到自建：确认框 → 切换；启动、停止服务端
  await page.locator("#voice-mode-self_hosted").click();
  await shot("voice-mode-confirm");
  await confirmDialog("确认切换");
  await page.locator("#voice-mode-self_hosted[aria-checked=true]").waitFor();
  assert.equal(last("voice_set_mode").args.mode, "self_hosted");
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-mode"), "self_hosted", "切换自建链路后孪生舱同步为 LOCAL SERVER BRIDGE");
  assert.match(await page.locator("#voice-moss-twin-source").innerText(), /LOCAL SERVER BRIDGE/);
  assert.equal(await page.getByRole("tab", { name: /^隐私与家居/ }).count(), 0, "自建链路同样不显示隐私与家居标签");
  await tab("服务").click();
  assert.ok(await page.locator("#voice-server-card").isVisible(), "自建链路显示服务端");
  await page.locator("#voice-server-start").click();
  await page.locator("#voice-server-state").filter({ hasText: "运行中" }).waitFor();
  await shot("voice-self-hosted");
  await page.locator("#voice-server-stop").click();
  await page.locator("#voice-server-state").filter({ hasText: "未运行" }).waitFor();
  // 意外退出：显示退出码、不自动重启，可再次启动
  state.server = { state: "exited", owned: true, exit_code: 1 };
  await page.locator("#voice-server-state").filter({ hasText: "意外退出" }).waitFor({ timeout: 10_000 });
  assert.ok(await page.getByText("退出码 1，不会自动重启").isVisible());
  await shot("voice-crashed");
  await page.locator("#voice-server-start").click();
  await page.locator("#voice-server-state").filter({ hasText: "运行中" }).waitFor();

  // 4. 切到内置：服务端仍在运行时再问是否停止
  await page.locator("#voice-mode-builtin").click();
  await confirmDialog("确认切换");
  await confirmDialog("停止服务端");
  await page.locator("#voice-mode-builtin[aria-checked=true]").waitFor();
  assert.equal(last("voice_server_stop") !== undefined, true);
  assert.equal(state.server.state, "stopped");
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-mode"), "builtin", "切换内置链路后孪生舱同步为 REALTIME AUDIO TWIN");
  assert.match(await page.locator("#voice-moss-twin-source").innerText(), /REALTIME AUDIO TWIN/);

  // 5. 内置大模型：先测试、后保存，只提交这一项
  await tab("模型").click();
  await openGroup("builtin-model-llm");
  const llmSave = page.locator("#voice-builtin-model-save-llm");
  await page.locator("#voice-builtin-model-url-llm").fill("https://llm.example.test/v1");
  await page.locator("#voice-builtin-model-key-llm").fill("fake-llm-key");
  await page.locator("#voice-builtin-model-name-llm").fill("fake-model");
  assert.ok(await llmSave.isDisabled(), "未测试不能保存");
  await page.locator("#voice-builtin-model-test-llm").click();
  await page.locator("#voice-builtin-model-result-llm").filter({ hasText: "测试通过" }).waitFor();
  await llmSave.click();
  await page.locator("#voice-group-builtin-model-llm [role=status]").filter({ hasText: "已保存大模型设置" }).waitFor();
  assert.deepEqual(Object.keys(last("voice_builtin_models_save").args.input), ["llm"]);
  assert.equal(await page.locator("#voice-builtin-model-key-llm").inputValue(), "", "保存后清空密钥");

  // 5b. 内置语音识别：一键复用大模型地址与密钥，自动读取并筛选出可用于语音识别的模型
  await openGroup("builtin-model-asr");
  await page.locator("#voice-builtin-reuse-peer-asr").click();
  assert.equal(await page.locator("#voice-builtin-model-url-asr").inputValue(), "https://llm.example.test/v1", "一键填入大模型基础地址");
  await page.locator("#voice-builtin-model-options-asr").getByRole("option").first().waitFor();
  assert.equal(await page.locator("#voice-builtin-model-options-asr").getByRole("option").count(), 1, "展开时自动筛选出可用于语音识别的 1 个模型");
  await page.locator("#voice-builtin-model-scope-toggle-asr").click();
  assert.equal(await page.locator("#voice-builtin-model-options-asr").getByRole("option").count(), 3, "点显示全部可查看服务商返回的全部 3 个模型");
  await page.locator("#voice-builtin-model-name-asr").fill("sense");
  assert.equal(await page.locator("#voice-builtin-model-options-asr").getByRole("option").count(), 1, "输入关键词实时筛出目标语音识别模型");
  await page.locator("#voice-builtin-model-options-asr").getByRole("option", { name: /SenseVoiceSmall/ }).click();
  assert.equal(await page.locator("#voice-builtin-model-name-asr").inputValue(), "FunAudioLLM/SenseVoiceSmall");
  // 5b-2. 内置语音合成：一键复用地址、点选模型自动填入默认音色、测试通过后显示“试听语音”按钮
  await openGroup("builtin-model-tts");
  await page.locator("#voice-builtin-reuse-peer-tts").click();
  await page.locator("#voice-builtin-model-options-tts").getByRole("option", { name: /MOSS\/TTS-1/ }).click();
  assert.equal(await page.locator("#voice-builtin-model-voice-tts").inputValue(), "alloy", "选定合成模型后自动填入默认音色");
  await page.locator("#voice-builtin-model-test-tts").click();
  await page.locator("#voice-builtin-model-play-tts").waitFor();
  // 5c. 顶部四栏自由混搭下拉框（大模型 + 语音识别 + 语音合成 + 拾音判停）：直接在顶部下拉框选本地合成与本地判停，下方卡片仅用于配置模型或下载
  await page.locator("#voice-builtin-mix-select-tts").selectOption("local:kokoro");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已切换为本地语音合成" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.tts, true, "语音合成切换为本地模型");
  assert.equal(last("voice_local_save").args.settings.tts_model, "kokoro");
  assert.equal("asr" in last("voice_local_save").args.settings, false, "单分区切换补丁不跨区携带语音识别开关");
  assert.equal(Boolean(state.builtin.local.settings.asr), false, "语音识别保持云端不变");
  await page.locator("#voice-builtin-mix-select-vad").selectOption("local:silero_vad");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已切换为本地拾音判停（Silero VAD）" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.vad, true, "拾音判停切换为本地 Silero VAD");
  assert.equal(last("voice_local_save").args.settings.vad_model, "silero_vad");
  await page.locator("#voice-builtin-mix-select-vad").selectOption("energy");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已切换为内置能量判停" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.vad, false, "可随时切回内置能量判停");
  await page.locator("#voice-builtin-mix-select-tts").selectOption("cloud");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已切换为云端语音合成" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.tts, false, "可随时在顶部下拉框切回云端语音合成");
  // 5d. 顶部五栏按真实语音执行链路排序（拾音判停 -> 语音识别 -> Jev 前置粗筛 -> 大模型 -> 语音合成），且右上角偏好设置不再重复渲染 Jev
  const mixOrder = await page.locator("#voice-builtin-mix-bar .voice-mix-field").evaluateAll((els) => els.map((el) => el.id));
  assert.deepEqual(mixOrder, ["voice-builtin-mix-vad", "voice-builtin-mix-asr", "voice-builtin-mix-jev", "voice-builtin-mix-llm", "voice-builtin-mix-tts"]);
  await page.getByTitle("系统偏好设置").click();
  await page.locator(".settings-modal").waitFor();
  assert.equal(await page.locator("#settings-jev").count(), 0, "桌面端右上角系统设置已移除重复的 Jev 配置，仅保留语音中心唯一入口");
  await page.locator(".settings-modal .mat-modal-close").click();
  await page.locator("#voice-builtin-mix-select-jev").selectOption("jev:typesafe/jev-1.14");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "尚未配置 OpenRouter API Key" }).waitFor();
  assert.equal(await page.locator("#voice-builtin-jev-voice-enabled").count(), 0, "n69：Jev 语音开关只在顶部第 3 栏，Jev 卡片不再重复一个勾选框");
  assert.match(await page.locator("#voice-builtin-jev-voice-state").innerText(), /未开启/);
  assert.match(await page.locator("#voice-group-builtin-model-jev > .voice-group-h").innerText(), /Jev 前置粗筛 · OpenRouter 云端（高级）/);
  await page.locator("#voice-builtin-apikey-jev").fill("sk-or-test-jev");
  await page.locator("#voice-builtin-jev-feedback-enabled").check();
  await page.locator("#voice-builtin-model-test-jev").click();
  await page.locator("#voice-builtin-model-result-jev").filter({ hasText: "测试通过" }).waitFor();
  await page.locator("#voice-builtin-model-save-jev").click();
  await page.locator("#voice-group-builtin-model-jev [role=status]").filter({ hasText: "已保存Jev 决策模型" }).waitFor();
  await page.locator("#voice-builtin-mix-select-jev").selectOption("jev:typesafe/jev-1.13");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已切换为 Jev 决策（typesafe/jev-1.13）" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.jev_filter_enabled, true);
  assert.equal(last("voice_local_save").args.settings.jev_model, "typesafe/jev-1.13");
  // 5d-2. 本地 OneJev 1.6 (0.8B / 4B / 9B) 与 Laya 自由混搭：未下载项引导到下载区，已安装项可在顶部下拉或行内一键选用并展示硬件加速提示
  await page.locator("#voice-builtin-mix-select-jev").selectOption("local:onejev_1_6_9b");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "尚未下载" }).waitFor();
  assert.match(await page.locator("#voice-local-onejev-env-hint").innerText(), /Apple Metal GPU 加速/);
  assert.match(await page.locator("#voice-local-jev-runtime").innerText(), /本地 Jev 启动失败：OneJev 模型加载超时/);
  // n69：已下载的本地 Jev 行不再有自己的“选用”按钮（它和顶部第 3 栏写的是同一个开关），改为跳到顶部下拉
  assert.equal(await page.locator("#voice-local-use-onejev_1_6_0_8b").count(), 0, "本地 Jev 行不再重复一个“选用”开关");
  await page.locator("#voice-local-pick-onejev_1_6_0_8b").click();
  await page.waitForFunction(() => document.activeElement?.id === "voice-builtin-mix-select-jev");
  await page.locator("#voice-builtin-mix-select-jev").selectOption("local:onejev_1_6_0_8b");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "OneJev 1.6 0.8B" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.jev_model, "onejev_1_6_0_8b");
  assert.equal(await page.locator("#voice-builtin-mix-select-jev").inputValue(), "local:onejev_1_6_0_8b");
  assert.match(await page.locator("#voice-builtin-jev-voice-state").innerText(), /已开启（用本地模型）/, "Jev 卡片只读显示顶部开关的状态");
  await page.locator("#voice-builtin-mix-select-jev").selectOption("local:onejev_1_6_4b");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "OneJev 1.6 4B" }).filter({ hasText: "Apple Metal GPU 加速" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.jev_model, "onejev_1_6_4b");
  await page.locator("#voice-builtin-mix-select-jev").selectOption("local:laya_multilingual");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "Laya 决策模型" }).filter({ hasText: "Apple Metal MPS" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.jev_model, "laya_multilingual");
  await page.locator("#voice-builtin-mix-select-jev").selectOption("off");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已关闭 Jev 语音筛选" }).waitFor();
  assert.equal(last("voice_local_save").args.settings.jev_filter_enabled, false);
  // 5d-3. 2026-11-07：多个模型可同时下载（点一个不锁其他行）；OneJev 排队显示“等待推理引擎”；Laya 运行环境自动准备、可取消
  const layaModel = state.builtin.local.models.find((item) => item.id === "laya_multilingual");
  const savedLayaEnv = structuredClone(state.builtin.local.laya_env);
  layaModel.installed = false;
  state.builtin.local.laya_env = { ready: false, python_found: false, laya_serve_found: false, accelerator: "cpu", managed: false, summary: "尚未准备 Laya 运行环境：点“下载”或“准备运行环境”后自动创建虚拟环境" };
  state.builtin.local.laya_setup = { state: "idle" };
  await page.locator("#voice-local-download-onejev_1_6_9b").click();
  assert.equal(await page.locator("#voice-local-download-onejev_1_6_9b").isDisabled(), true, "正在请求的模型按钮加锁");
  assert.equal(await page.locator("#voice-local-delete-onejev_1_6_4b").isDisabled(), false, "其他模型不受影响");
  await page.locator("#voice-local-group-jev").filter({ hasText: "等待推理引擎" }).waitFor();
  await page.locator("#voice-local-laya-setup").filter({ hasText: "尚未准备 Laya 运行环境" }).waitFor();
  await page.locator("#voice-local-download-laya_multilingual").click();
  await page.locator("#voice-local-laya-setup").filter({ hasText: "第 4/6 步" }).filter({ hasText: "已用时 1:05" }).waitFor();
  assert.match(await page.locator("#voice-local-laya-setup").innerText(), /Downloading torch/);
  await page.locator("#voice-local-laya-setup-cancel").click();
  await page.locator("#voice-local-laya-setup").filter({ hasText: "已取消准备" }).waitFor();
  await page.locator("#voice-local-laya-setup-start").filter({ hasText: "准备运行环境" }).waitFor();
  assert.equal(last("voice_local_laya_setup").args.action, "cancel");
  layaModel.installed = true;
  delete layaModel.progress;
  delete state.builtin.local.models.find((item) => item.id === "onejev_1_6_9b").progress;
  state.builtin.local.laya_env = savedLayaEnv;
  state.builtin.local.laya_setup = { state: "idle" };
  // 5e. n69：模型标签顺序——混搭栏 → 识别 → 大模型 → 合成 → 视觉理解 → Jev（高级）→ 本地模型 → 对话体验
  const modelOrder = await page.locator("#voice-builtin-models-panel > .voice-group").evaluateAll((els) => els.map((el) => el.id));
  assert.deepEqual(modelOrder, [
    "voice-group-builtin-model-asr", "voice-group-builtin-model-llm", "voice-group-builtin-model-tts", "voice-group-vision",
    "voice-group-builtin-model-jev", "voice-group-local-models", "voice-group-experience",
  ]);
  const modelHeadings = await page.locator("#voice-builtin-models-panel > .voice-section-h").allInnerTexts();
  assert.deepEqual(modelHeadings, ["云端模型（密钥只存本机）", "本地模型（断网可用）", "对话体验"], "不再有“内置服务 · 本地模型与体验”这种混在一起的标题");
  assert.equal(await page.locator("#voice-panel-features #voice-group-vision").count(), 0, "视觉理解从扩展移到模型标签");
  // 6. 本地模型与对话体验分开保存：保存体验时不带上本地模型未保存的改动
  await openGroup("local-models");
  await openGroup("experience");
  assert.equal(await page.locator("#voice-screen-card-style").count(), 0, "n69：对话体验里不再有画风下拉（只在设备 › 副屏预览切换）");
  assert.equal(await page.locator("#voice-jev-filter-enabled").count(), 0, "n69：对话体验里不再有 Jev 开关勾选框");
  assert.match(await page.locator("#voice-jev-filter-state").innerText(), /未开启/, "对话体验只读显示 Jev 开关状态（上面刚在顶部关掉）");
  assert.equal(await page.locator("#voice-jev-suboptions").count(), 0, "Jev 未开启时不显示细项");
  await page.getByLabel("本地合成语速（0.5 ~ 2.0）").fill("1.5");
  await page.locator("#voice-barge-in").check();
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已保存对话体验设置" }).waitFor();
  const localPayload = last("voice_local_save").args.settings;
  assert.equal(localPayload.barge_in, true);
  assert.equal("tts_speed" in localPayload, false, "局部补丁不携带本地模型分组字段");
  assert.equal(state.builtin.local.settings.tts_speed ?? 1, 1, "本地模型的未保存改动没有被一起提交");
  assert.ok(await page.locator("#voice-group-local-models .voice-dirty").isVisible(), "本地模型仍显示未保存");
  // 6b. 顶部切换开启 Jev 后再修改并保存语音体验其他字段，绝不把 Jev 开关覆盖回旧值 false
  await page.locator("#voice-builtin-mix-select-jev").selectOption("jev:typesafe/jev-1.14");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已切换为 Jev 决策（typesafe/jev-1.14）" }).waitFor();
  await page.locator("#voice-jev-filter-state").filter({ hasText: "已开启（typesafe/jev-1.14）" }).waitFor();
  await page.locator("#voice-jev-suboptions").waitFor();
  await page.locator("#voice-barge-in-ms").fill("460");
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已保存对话体验设置" }).waitFor();
  assert.equal(state.builtin.local.settings.jev_filter_enabled, true, "保存语音体验插话时长后 Jev 仍保持开启");
  // 6c. n51：切换《流浪地球》MOSS 领航空间站风格 + 工坊补充说明 + 自定义同音纠偏
  await page.locator("#voice-persona-style").selectOption("moss_station");
  await page.locator("#voice-persona-prompt").fill("工坊主力机为翼宿 5W 蓝光机");
  await page.locator("#voice-asr-corrections").fill("红松木=樟子松");
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已保存对话体验设置" }).waitFor();
  assert.equal(state.builtin.local.settings.persona_style, "moss_station");
  assert.equal(state.builtin.local.settings.persona_prompt, "工坊主力机为翼宿 5W 蓝光机");
  assert.deepEqual(state.builtin.local.settings.custom_asr_corrections, ["红松木=樟子松"]);
  // 6d. n59：新增自建角色并选中，保存后立即生效、不重启
  await page.locator("#voice-custom-persona-add").click();
  await page.getByLabel("自建角色 1 名字").fill("老师傅");
  await page.getByLabel("自建角色 1 人设").fill("话少，先说结论");
  await page.locator("#voice-persona-style").selectOption({ label: "老师傅（自建） — 话少，先说结论" });
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已立即生效" }).waitFor();
  const rolePatch = last("voice_local_save").args.settings;
  assert.deepEqual(Object.keys(rolePatch).sort(), ["custom_personas", "persona_style"], "只提交角色字段");
  assert.equal(rolePatch.custom_personas[0].name, "老师傅");
  assert.equal(rolePatch.persona_style, rolePatch.custom_personas[0].id);
  // 6e. 删除正在使用的自建角色：选中项回到默认角色，可以直接保存
  await page.locator("#voice-custom-personas").getByRole("button", { name: "删除" }).click();
  assert.equal(await page.locator("#voice-persona-style").inputValue(), "workshop");
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已立即生效" }).waitFor();
  const deletePatch = last("voice_local_save").args.settings;
  assert.deepEqual(deletePatch.custom_personas, []);
  assert.equal(deletePatch.persona_style, "workshop");
  await page.locator("#voice-group-experience").scrollIntoViewIfNeeded();
  await shot("voice-builtin");

  // 7. Agent（Claude Code / Codex）：默认关闭；开启要有工作目录与 CLI；严格模式没开时说清楚；运行中可以停止
  await tab("扩展").click();
  await openGroup("agent");
  const agentGroup = page.locator("#voice-group-agent");
  assert.ok(/未开启|读取中/.test(await agentGroup.locator(".voice-group-summary").innerText()), "默认未开启");
  assert.ok(await page.locator("#voice-agent-save").isDisabled(), "没改动不能保存");
  await page.locator("#voice-agent-enabled").check();
  assert.ok(await agentGroup.getByText("开启前必须填写工作目录").isVisible(), "缺工作目录时说明原因");
  assert.ok(await page.locator("#voice-agent-save").isDisabled(), "缺工作目录不能保存");
  await agentGroup.locator("input.text-input").nth(0).fill("C:\\agent-workspace");
  await agentGroup.locator("input.text-input").nth(1).fill("C:\\tools\\codex.exe");
  assert.equal(await page.locator("#voice-agent-save").isDisabled(), false, "填好工作目录与 CLI 后可以保存");
  await page.locator("#voice-agent-save").click();
  await agentGroup.locator("[role=status]").filter({ hasText: "已保存Agent 设置" }).waitFor();
  assert.deepEqual(last("voice_agent_save").args.input, { enabled: true, workspace: "C:\\agent-workspace", codex_executable: "C:\\tools\\codex.exe", claude_executable: "" }, "开启状态与路径一起提交");
  assert.ok(await agentGroup.getByText("严格模式当前未开启，Agent 不会运行").isVisible(), "严格模式没开时给出原因");
  assert.match(await agentGroup.locator(".voice-group-summary").innerText(), /需要严格模式/);
  await shot("voice-agent");
  // 小智发起并经桌面批准后：桌面能停止运行中的任务，停止后记为已停止
  state.agent.strict_mode = true;
  state.agent.runs = [{ id: "agentrun-1", approval_id: "a1", provider: "codex", workspace_name: "agent-workspace", status: "running", stoppable: true, output: "正在读 README", error_code: null }];
  const stopButton = agentGroup.getByRole("button", { name: /停止/ });
  await stopButton.waitFor({ timeout: 15_000 });
  await stopButton.click();
  await confirmDialog("停止任务");
  await agentGroup.locator("[role=status]").filter({ hasText: "已请求停止" }).waitFor();
  assert.deepEqual(last("voice_agent_stop").args, { runId: "agentrun-1" }, "停止的是这条运行记录");
  await agentGroup.getByText("已停止").first().waitFor();
  assert.equal(await agentGroup.getByRole("button", { name: /停止/ }).count(), 0, "已结束的任务不再提供停止");
  await page.locator("#voice-agent-enabled").uncheck();
  await page.locator("#voice-agent-save").click();
  await agentGroup.locator("[role=status]").filter({ hasText: "已保存Agent 设置" }).waitFor();
  assert.equal(last("voice_agent_save").args.input.enabled, false, "可以再关掉");
  assert.match(await page.locator("#voice-group-music .voice-group-summary").innerText(), /目录不可用/, "换机音乐目录缺失时摘要显示目录不可用");
  await openGroup("music");
  assert.ok(await page.locator("#voice-group-music").getByText("本次启动已跳过本地音乐").isVisible(), "换机音乐目录缺失时展示降级提示");

  // 7b. 扩展 -> 独立本地知识库：免填项目名直接添加资料目录，勾选/取消勾选立即切换挂载状态（n50）
  await openGroup("knowledge");
  await page.locator("#voice-knowledge-add").click();
  await page.locator("#voice-group-knowledge").getByText("椴木工艺笔记", { exact: true }).waitFor();
  assert.match(await page.locator("#voice-group-knowledge .voice-group-summary").innerText(), /已启用 1 \/ 1 个/, "挂载知识库后摘要显示已启用数量");
  await page.getByLabel("对话时启用 椴木工艺笔记").uncheck();
  assert.match(await page.locator("#voice-group-knowledge .voice-group-summary").innerText(), /未启用/, "取消勾选后摘要更新为未启用");
  await page.getByLabel("对话时启用 椴木工艺笔记").check();
  assert.match(await page.locator("#voice-group-knowledge .voice-group-summary").innerText(), /已启用 1 \/ 1 个/, "重新勾选即挂载");
  // 8. 隐私与家居：四项各自下拉，点开哪一项就在主页面就地展开对应设置（n26，2026-10-18）。
  await tab("隐私与家居").click();
  for (const key of ["memory", "plugins", "speaker", "home"]) {
    const item = page.locator(`#voice-private-${key}-item`);
    assert.equal(await item.count(), 1, `${key} 是一项下拉`);
    assert.equal(await item.evaluate((el) => el.open), false, `${key} 默认收起`);
  }
  const alignEdges = await page.evaluate(() => {
    const twin = document.querySelector("#voice-moss-twin").getBoundingClientRect();
    const firstTab = document.querySelector("#voice-page-tab-service").getBoundingClientRect();
    const memoryBox = document.querySelector("#voice-private-memory-item").getBoundingClientRect();
    return { twinLeft: Math.round(twin.left), twinRight: Math.round(twin.right), tabLeft: Math.round(firstTab.left), memLeft: Math.round(memoryBox.left), memRight: Math.round(memoryBox.right) };
  });
  assert.equal(alignEdges.tabLeft, alignEdges.twinLeft, "语音标签栏左边缘与上方孪生卡片严格对齐");
  assert.equal(alignEdges.memLeft, alignEdges.twinLeft, "隐私与家居折叠项左边缘与上方卡片严格对齐");
  assert.equal(alignEdges.memRight, alignEdges.twinRight, "隐私与家居折叠项右边缘与上方卡片严格对齐");
  assert.equal(await page.locator(".voice-private-item-body .voice-card").count(), 0, "未展开时不挂载设置卡");
  assert.equal(await page.locator("#voice-firmware-card, .firmware-card").count(), 0, "固件不在隐私与家居");
  if (process.env.VOICE_QA_DIR) await page.screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-privacy-tab.png") });
  const memoryItem = page.locator("#voice-private-memory-item");
  await memoryItem.locator("> summary").click();
  assert.equal(await memoryItem.evaluate((el) => el.open), true, "点标题展开记忆设置");
  await memoryItem.getByRole("heading", { name: "对话记忆" }).waitFor();
  assert.equal(await memoryItem.locator("#context-memory-switch").isChecked(), false, "合一记忆总开关默认关闭");
  assert.equal(await memoryItem.locator("#voice-private-memory-advanced").count(), 0, "不再嵌套第二层长期记忆折叠框");
  if (process.env.VOICE_QA_DIR) await page.screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-privacy-tab-open.png") });
  await memoryItem.locator("> summary").click();
  assert.equal(await memoryItem.evaluate((el) => el.open), false, "再点收起");
  // n67：固件升级在“设备”标签，收起时不挂载（不后台轮询），展开后就地显示固件卡
  await tab("设备").click();
  assert.equal(calls.filter((call) => call.command === "voice_firmware_status").length, 0, "固件组收起时不查询");
  await openGroup("device-firmware");
  await page.locator("#voice-group-device-firmware #voice-firmware-card").waitFor();
  assert.ok(calls.some((call) => call.command === "voice_firmware_status"), "展开后在主窗口直接读取固件状态");
  await tab("记录与诊断").click();
  // 8a. n68：服务事件带级别 / 分类 / 怎么办 / 技术细节，可只看错误和警告，覆盖计数明确提示，可导出诊断包
  await page.locator("#voice-approvals-card").getByText("原因：控制器忙，未开始加工（错误码 controller_busy）").waitFor();
  assert.match(await page.locator("#voice-group-approvals").innerText(), /最近 1 条/);
  await openGroup("builtin-events");
  const eventsCard = page.locator("#voice-builtin-events-card");
  assert.equal(await eventsCard.locator(".voice-event").count(), 3, "全部保留的事件都显示，不再只截 20 条");
  await eventsCard.getByText("有 12 条较早的日常记录已被覆盖（最多保留 300 条）").waitFor();
  await eventsCard.getByText(/本地音乐目录当前不可用/).waitFor();
  const errorEvent = eventsCard.locator('.voice-event[data-level="error"]');
  assert.match(await errorEvent.innerText(), /错误[\s\S]*密钥\/权限[\s\S]*怎么办：密钥填错/);
  await errorEvent.getByText("技术细节").click();
  await errorEvent.getByText("服务方原话：Incorrect API key provided").waitFor();
  assert.match(await eventsCard.locator('.voice-event[data-level="warn"]').innerText(), /怎么办：检查设备供电和 Wi-Fi/, "没有自带建议时用分类的大白话建议");
  await page.locator("#voice-events-only-problems").check();
  assert.equal(await eventsCard.locator(".voice-event").count(), 2, "只看错误和警告");
  await page.locator("#voice-events-only-problems").uncheck();
  await page.locator("#voice-diagnostics-export").click();
  await page.locator("#voice-diagnostics-export-result").getByText(/诊断包已保存：moss-voice-diagnostics-1790000000\.json（3 条事件/).waitFor();
  assert.ok(last("voice_diagnostics_export"), "导出走后端命令");
  await openGroup("live-debug-traces");
  assert.match(await page.locator("#voice-turn-timings").innerText(), /第 2 轮 · 没识别出文字，已忽略 · fake-asr · 设备 aa:bb:cc:11:22:33/);
  assert.equal(await page.locator("#voice-live-debug").getByText(/0ms|100% 生效/).count(), 0, "不再出现未测量的 0ms / 100% 说法");
  // 8b. n51：记录与诊断 -> 仅内存实时对话排障镜（默认关闭，开启后展示原话与 0ms 纠偏并可一键追加纠偏规则）
  await openGroup("live-debug-traces");
  assert.equal(await page.locator("#voice-debug-trace-switch").isChecked(), false, "实时对话调试默认关闭");
  await page.locator("#voice-debug-trace-switch").check();
  await page.locator("#voice-live-debug").getByText("帮我切椴木书签").waitFor();
  await page.locator("#voice-live-debug").getByRole("button", { name: "纠偏此句" }).click();
  await page.locator("#voice-debug-right-word").fill("帮我切东北椴木书签");
  await page.locator("#voice-debug-add-correction").click();
  await page.locator("#voice-live-debug").getByText(/已添加同音纠偏/).waitFor();
  assert.ok(state.builtin.local.settings.custom_asr_corrections.includes("帮我切段木书签=帮我切东北椴木书签"));
  await page.locator("#voice-debug-trace-switch").uncheck();
  assert.equal(state.builtin.debug_traces.length, 0, "关闭开关立即清空内存排障记录");

  // 8c. n51 + n52（n69 起在“设备”标签）：在线小智硬件快捷控制（320×240 彩屏识别、一键静音记忆 0%、投送雕刻预览、一键切换长按说话抗噪模式）
  await tab("服务").click();
  assert.ok(await page.locator("#voice-panel-service #voice-builtin-card").isVisible(), "服务标签只放内置服务启停与连接地址");
  assert.ok(await page.locator("#voice-group-builtin-service").isVisible());
  assert.equal(await page.locator("#voice-panel-service #voice-builtin-devices, #voice-panel-service #voice-goggle-bot-card").count(), 0, "设备表与副屏预览不在服务标签");
  await tab("设备").click();
  const deviceOrder = await page.locator("#voice-devices-panel > .voice-group").evaluateAll((els) => els.map((el) => el.id));
  assert.deepEqual(deviceOrder, ["voice-group-builtin-devices", "voice-group-wake-word", "voice-group-screen-preview", "voice-group-device-firmware"], "设备标签：已连接设备 → 唤醒词 → 副屏预览 → 固件升级");
  await page.locator("#voice-builtin-devices").getByRole("button", { name: "控制与信息" }).click();
  const ctrlBox = page.locator("#voice-device-controls-aa\\:bb\\:cc\\:11\\:22\\:33");
  await ctrlBox.waitFor();
  await ctrlBox.getByText("320×240 彩屏").waitFor();
  await ctrlBox.getByRole("button", { name: "静音" }).click();
  await ctrlBox.getByText(/已静音/).waitFor();
  assert.deepEqual(last("voice_device_request").args, { deviceId: "aa:bb:cc:11:22:33", request: "set_volume", value: { volume: 0 } });
  await ctrlBox.getByLabel("设备音量").focus();
  await page.keyboard.press("ArrowRight");
  await ctrlBox.getByRole("button", { name: "应用" }).first().click();
  await ctrlBox.getByText("音量已设为 5%").waitFor();
  assert.deepEqual(last("voice_device_request").args, { deviceId: "aa:bb:cc:11:22:33", request: "set_volume", value: { volume: 5 } }, "扬声器音量在快捷控制里调，仍走 set_volume");
  assert.equal(await page.locator('[id^="voice-volume-"]').count(), 0, "n69：不再另有一份“MOSS 说话音量”滑块");
  await ctrlBox.getByRole("button", { name: "长按说话（抗噪）" }).click();
  await ctrlBox.getByText(/长按说话（抗噪）/).first().waitFor();
  assert.deepEqual(last("voice_device_request").args, { deviceId: "aa:bb:cc:11:22:33", request: "set_talk_mode", value: { mode: "press_to_talk" } });
  await ctrlBox.getByRole("button", { name: /投送.*预览/ }).click();
  await ctrlBox.getByText(/已将.*预览.*推送到小智/).waitFor();
  assert.deepEqual(last("voice_device_request").args, { deviceId: "aa:bb:cc:11:22:33", request: "push_preview" });
  // 对话体验里的画风只读行“去副屏预览”跳到设备标签并展开副屏预览组
  await tab("模型").click();
  assert.match(await page.locator("#voice-screen-card-style-state").innerText(), /硬核太空舱/);
  await page.locator("#voice-screen-card-style-goto").click();
  await page.locator("#voice-page-tab-devices[aria-selected=true]").waitFor();
  assert.equal(await page.locator("#voice-group-screen-preview > .voice-group-h").getAttribute("aria-expanded"), "true", "跳转后副屏预览组已展开");
  const botCard = page.locator("#voice-goggle-bot-card");
  await botCard.waitFor();
  assert.equal(await botCard.locator(".voice-bot-gif-img").count(), 5, "展示 5 组 160×120 动态场景屏");
  await botCard.getByText("太空舱雕刻表盘").waitFor();
  assert.match(await botCard.locator('[data-pose="engrave"] .voice-bot-gif-img').getAttribute("src"), /#engrave-moss$/);
  await page.locator("#voice-card-style-goggle_bot").click();
  await botCard.getByText("戴护目镜雕刻").waitFor();
  assert.match(await botCard.locator('[data-pose="engrave"] .voice-bot-gif-img').getAttribute("src"), /#engrave-bot$/);
  await page.locator("#voice-bot-showcase-notice").getByText(/可爱像素小机器人/).waitFor();
  assert.deepEqual(last("voice_local_save").args, { settings: { screen_card_style: "goggle_bot" } });
  await page.locator("#voice-push-bot-engrave").click();
  await page.locator("#voice-bot-showcase-notice").getByText(/戴护目镜雕刻/).waitFor();
  assert.deepEqual(last("voice_device_request").args, { deviceId: "aa:bb:cc:11:22:33", request: "push_bot_pose", value: { pose: "engrave", style: "goggle_bot" } });
  await page.locator("#voice-card-style-moss_hud").click();
  await botCard.getByText("太空舱雕刻表盘").waitFor();
  assert.deepEqual(last("voice_device_request").args, { deviceId: "aa:bb:cc:11:22:33", request: "push_bot_pose", value: { pose: "engrave", style: "moss_hud" } });
  // 8d. n67 / n69：唤醒词卡在“设备”标签就地显示（原“唤醒词与音量”标签已并入）
  const wakeGroup = page.locator("#voice-group-wake-word");
  await wakeGroup.locator("#voice-wakeword-card").waitFor();
  assert.equal(await wakeGroup.locator("#voice-wakeword-text").inputValue(), "你好小智", "唤醒词卡在主窗口就地读取当前唤醒词");
  await shot("voice-devices");
  // 8e. 记住的旧标签 "wake" 重新打开时落到“设备”
  await page.evaluate(() => localStorage.setItem("moss_voice_tab", "wake"));
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator("#voice-page-tab-devices[aria-selected=true]").waitFor();
  await page.locator("#voice-group-builtin-devices").waitFor();
  // 9. 窄屏无横向滚动
  await page.setViewportSize({ width: 820, height: 1000 });
  await tab("设备").click();
  assert.equal(await page.locator("#voice-group-endpoints").count(), 0, "内置链路不渲染接入点状态");
  assert.ok(await page.locator("#voice-group-builtin-devices").isVisible(), "内置链路设备标签显示已连接设备");
  assert.equal(await page.locator("#voice-panel-devices #voice-group-tools").count(), 0, "内置工具在扩展标签");
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  assert.ok(overflow <= 1, `窄屏横向溢出 ${overflow}px`);
  await shot("voice-narrow");

  assert.deepEqual(errors, [], "不应有前端未处理异常");
  console.log(`VOICE_CENTER_BROWSER_TEST=PASS calls=${calls.length}`);
  await context.close();
} finally {
  if (browser) await browser.close();
  await vite.close();
}
