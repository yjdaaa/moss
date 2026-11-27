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
// n70：全屏画风 12 个场景（与 voiceState.js 的 GOGGLE_BOT_POSES 同序）
const SCREEN_SCENES = ["idle", "listen", "think", "talk", "happy", "sad", "surprised", "engrave", "approval", "music", "celebrate", "halted"];

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
    wakeword: {
      running: true,
      word: { text: "你好小智", command: "ni hao xiao zhi", threshold: 0.2 },
      base: null,
      devices: [
        { device_id: "aa:bb:cc:11:22:33", name: "工坊小智", supported: true, screen_pack_size: [320, 240], screen_pack: false, screen_pack_state: "none", wake_target: null },
        { device_id: "aa:bb:cc:44:55:66", name: "书房小智", supported: true, screen_pack_size: [320, 240], screen_pack: true, screen_pack_state: "installed", wake_target: "default" },
        { device_id: "aa:bb:cc:77:88:99", name: "客厅小智", supported: true, screen_pack_size: [320, 240], screen_pack: true, screen_pack_state: "style_outdated", screen_pack_style: "goggle_bot", wake_target: "default" },
        { device_id: "aa:bb:cc:aa:bb:cc", name: "工作台小智", supported: true, screen_pack_size: [320, 240], screen_pack: true, screen_pack_state: "assets_outdated", screen_pack_style: "moss_hud", wake_target: "default" },
        { device_id: "aa:bb:cc:dd:ee:ff", name: "刚连上小智", supported: false, tools_loading: true, screen_pack_size: null, screen_pack: false, screen_pack_state: "none", wake_target: null },
      ],
    },
    mode: "official",
    settings: { builtin_ws_port: 8000, builtin_http_port: 8003, official_mcp_endpoint: masked, official_mcp_endpoint_set: true },
    builtin: {
      running: false, mode: "echo", link_mode: "official", ws_port: 8000, http_port: 8003,
      ota_url: "http://192.0.2.10:8003/xiaozhi/ota/",
      ota_ip_url: "http://192.0.2.10:8003/xiaozhi/ota/",
      ota_ip_urls: ["http://192.0.2.10:8003/xiaozhi/ota/", "http://192.0.2.20:8003/xiaozhi/ota/"],
      models: {}, jev: { endpoint: "https://openrouter.ai/api/alpha/decisions", model: "typesafe/jev-1.14", key_set: false, openrouter_llm_key_set: false, voice_enabled: false, feedback_enabled: false }, local: { settings: {}, jev_key_configured: false, onejev_env: { ready: true, installed: true, binary_found: true, accelerator: "metal", accelerator_label: "Apple Metal GPU 加速", summary: "OneJev 原生推理引擎已就绪（Apple Metal GPU 加速 · 免装 Python）" }, audiocpp_env: { ready: true, installed: true, binary_found: true, backend: "metal", accelerator_label: "Apple Metal 硬件加速", summary: "audio.cpp 原生克隆引擎已就绪（Apple Metal 硬件加速 · 免装 Python）" }, tts_clone_ref: { configured: false, file_name: "", sample_rate: 0, duration_secs: 0, transcript: "", mode: "none" }, jev_runtime: { model: "onejev_1_6_4b", state: "failed", accelerator: null, error: "OneJev 模型加载超时", notice: null }, laya_env: { ready: true, python_found: true, laya_serve_found: true, accelerator: "mps", cuda_found: false, mps_found: true, summary: "运行环境检测通过（Python + laya[serve] + Apple Metal MPS 加速已就绪），可直接下载并启用。" }, models: [{ id: "silero_vad", kind: "vad", label: "Silero VAD", installed: true }, { id: "gtcrn_simple", kind: "denoise", label: "GTCRN 降噪", installed: false }, { id: "dpdfnet_baseline", kind: "denoise", label: "DPDFNet 降噪", installed: true }, { id: "kws_zipformer_zh", kind: "kws", label: "命令词抗噪", installed: false }, { id: "smart_turn_v3", kind: "turn", label: "Smart Turn", installed: false }, { id: "kokoro", kind: "tts", label: "Kokoro v1.0", installed: true, voices: { default: 50, max: 53, hint: "45-48" } }, { id: "qwen3_tts_1_7b_clone", kind: "tts", engine: "audio_cpp_tts", label: "Qwen3-TTS 1.7B 声音克隆（离线复刻）", size: 2695186240, note: "上传 5～20 秒清晰干声即可断网克隆任意人声。", speed_adjustable: false, installed: true }, { id: "onejev_1_6_0_8b", kind: "jev", label: "OneJev 1.6 0.8B（轻量极速，推荐）", size: 1369128576, note: "Qwen3.5-0.8B 全参微调", installed: true }, { id: "onejev_1_6_4b", kind: "jev", label: "OneJev 1.6 4B（均衡主力）", size: 3328881152, note: "Qwen3.5-4B 全参微调", installed: true }, { id: "onejev_1_6_9b", kind: "jev", label: "OneJev 1.6 9B（高阶旗舰）", size: 6599787584, note: "Qwen3.5-9B 全参微调", installed: false }, { id: "laya_multilingual", kind: "jev", label: "Laya 决策模型（322M 多语种）", size: 1291563689, note: "mDeBERTa + RL", installed: true }, { id: "xiaozhi_noto_v1_full", kind: "font", label: "小智屏幕全量补字库（noto-v1-full · 48,662 字）", size: 17411352, note: "官方三层字库第 3 层（简/繁/日/韩/泰/阿 7 分片 × 4 档字号）。", installed: true }], helper_found: true },
      debug_enabled: true,
      debug_traces: [
        {
          id: 1,
          at_ms: 1_790_000_100_000,
          device_id: "aa:bb:cc:11:22:33",
          session_id: "s-dev1",
          turn: 1,
          at: 1_790_000_100,
          raw_asr: "帮我切段木书签",
          corrected_asr: "帮我切椴木书签",
          tools: [{ name: "generate_text_laser_task_tool", arguments: "{\"material\":\"椴木\"}", result: "预览已生成" }],
          reply: "已准备 3 毫米椴木预览，确认开始吗？",
          spoken: "已准备 3 毫米椴木预览，",
          outcome: "interrupted",
        },
      ],
      device_prefs: {},
      goggle_bot_gifs: ["idle", "engrave", "approval", "music", "celebrate"].map((id) => ({ id, label: id, data_url: `data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7#${id}-bot` })),
      screen_style_gifs: {
        moss_hud: Object.fromEntries(SCREEN_SCENES.map((id) => [id, `data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7#${id}-moss`])),
        goggle_bot: Object.fromEntries(SCREEN_SCENES.map((id) => [id, `data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7#${id}-bot`])),
      },
      sessions: [{ session_id: "s-dev1", device_id: "aa:bb:cc:11:22:33", ip: "192.0.2.55", protocol_version: 1, connected_at: 1_790_000_000, state: "idle", level: 0, screen_monochrome: false, screen_width: 320, screen_height: 240, device_tools: ["self.get_device_status", "self.audio_speaker.set_volume", "self.screen.set_brightness", "self.screen.set_theme", "self.screen.preview_image", "self.set_press_to_talk"] }],
      // n68：结构化事件（级别 / 分类 / 下一步 / 技术细节）、覆盖计数与降级提示
      events: [
        // 2026-11-09：激光雕刻机与 MOSS 软件的事件也在这里，带来源
        { seq: 5, at: 1_790_000_004, at_ms: 1_790_000_004_000, level: "error", category: "device", source: "laser", text: "加工失败：wf-9，停在第 88/400 行：ALARM:2 (软限位超程)", hint: "图形超出了机器行程：检查图形尺寸、工作原点和板材起点，必要时缩小图形" },
        { seq: 4, at: 1_790_000_003, at_ms: 1_790_000_003_500, level: "warn", category: "model", source: "moss", text: "本地视觉模型（qwen3_vl_2b）启动失败" },
        { seq: 3, at: 1_790_000_002, at_ms: 1_790_000_002_345, level: "info", category: "general", source: "xiaozhi", text: "识别完成：7 字" },
        { seq: 2, at: 1_790_000_001, at_ms: 1_790_000_001_120, level: "error", category: "auth", text: "对话出错：大模型返回 HTTP 401，密钥无效或没有权限", hint: "密钥填错、过期或被停用了：到服务方控制台复制新密钥重新填写", detail: "服务方原话：Incorrect API key provided" },
        { seq: 1, at: 1_790_000_000, at_ms: 1_790_000_000_010, level: "warn", category: "device", text: "设备 aa:bb:cc:11:22:33 已断开：连接突然中断（最可能是 Wi-Fi 掉线、设备重启或断电）" },
      ],
      events_dropped: { info: 12, problems: 0, timings: 0, max_info: 300, max_problems: 200, max_timings: 50 },
      warnings: ["本地音乐目录当前不可用（换电脑或移动盘拔出时请重新选择音乐文件夹），本次启动已跳过本地音乐"],
      timings: [{ session_id: "s-dev1", device_id: "aa:bb:cc:11:22:33", at_ms: 1_790_000_003_000, turn: 2, outcome: "ignored", models: ["fake-asr"], stages: { recording_start: 0, final_asr: 320 }, tools: [] }],
      devices: [{ device_id: "aa:bb:cc:11:22:33", name: "立创实战派小智", model: "lichuang-dev", firmware_version: "2.2.0", status: "allowed", last_seen: 1_790_000_000, status_json: JSON.stringify({ audio_speaker: { volume: 65 }, screen: { brightness: 80, theme: "dark" } }) }],
    },
    // n103：先模拟没给定位权限，用户去系统设置打开后变为已授权
    wifiPermission: "denied",
    wifiSettingsOpened: [],
    // 第一次读板子时模拟板子里还存着电脑换 IP 前的旧 OTA 地址
    usbInspectCount: 0,
    usbPorts: [
      { port_name: "/dev/cu.usbserial-LASER", description: "FluidNC", vid: 0x1a86, pid: 0x7523, serial_number: "L1", is_usb: true, is_laser_port: true, is_likely_esp: true },
      { port_name: "/dev/cu.usbmodem14201", description: "ESP32-S3 USB JTAG", vid: 0x303a, pid: 0x1001, serial_number: "XZ01", is_usb: true, is_laser_port: false, is_likely_esp: true },
      { port_name: "/dev/cu.usbserial-ALT2", description: "USB-SERIAL CH340", vid: 0x1a86, pid: 0x7523, serial_number: "ALT2", is_usb: true, is_laser_port: false, is_likely_esp: false },
    ],
    agent: {
      settings: { enabled: false, workspace: "", codex_executable: "", claude_executable: "" },
      strict_mode: false, providers: [], runs: [],
    },
    kbRoots: [],
    memoryState: { enabled: false, rows: [] },
    // n74：本地视觉模型（2B 未下载、4B 已下载）
    vision: {
      settings: { enabled: false, url: "", model: "" },
      key_configured: false,
      ready: false,
      dialog_llm: { configured: false, url: "", model: "" },
      local_models: [
        { id: "qwen3_vl_2b", label: "Qwen3-VL · 2B 本地视觉（推荐）", size: 1552463168, installed: false },
        { id: "qwen3_vl_4b", label: "Qwen3-VL · 4B 本地视觉（更准）", size: 2951255968, installed: true },
      ],
      local_runtime: { state: "idle", model: null, accelerator: null, error: null },
    },
  };
  const voiceStatus = (extra = {}) => ({
    // 与 center.rs status() 一致（2026-11-17 删除自建链路后没有 server / endpoint / device / models / strict.locked）
    mode: state.mode, settings: structuredClone(state.settings),
    bridge: { connected: true }, strict: { enabled: false, from_env: false }, gateway: { enabled: false },
    mcp_endpoint: masked,
    mcp: { endpoints: [{ id: "primary", endpoint: masked, connected: true }], tools: [{ name: "get_status", description: "状态", enabled: true }, { name: "laser_workflow_tool", description: "workflow", enabled: true, in_compact: true }, { name: "laser_grbl_tool", description: "grbl", enabled: true, in_compact: false }], full_tool_list: false, max_extra_endpoints: 4 },
    ...extra,
  });
  const handlers = {
    get_backend_origin: () => "http://127.0.0.1:9666",
    get_desktop_session: () => "a".repeat(64),
    voice_status: () => voiceStatus(),
    voice_builtin_status: () => structuredClone(state.builtin),
    voice_builtin_start: () => {
      state.builtin.running = true;
      return structuredClone(state.builtin);
    },
    voice_diagnostics_export: () => ({ saved: true, file_name: "moss-voice-diagnostics-1790000000.json", bytes: 4096, events: state.builtin.events.length }),
    voice_mcp_status: () => ({ servers: [], connections: [], audit: [], revision: "r1", max_servers: 8 }),
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
    voice_strict_set: ({ enabled }) => ({ enabled, applied: true, needs_restart: false, env_file: "C:\\moss\\.env" }),
    voice_music_status: () => ({
      settings: { enabled: true, directory: "D:\\unplugged-music" },
      track_count: 0,
      warning: "本地音乐目录当前不可用（换电脑或移动盘拔出时请重新选择音乐文件夹），本次启动已跳过本地音乐",
    }),
    voice_vision_status: () => structuredClone(state.vision),
    voice_vision_save: ({ input }) => {
      state.vision.settings = structuredClone(input.settings);
      state.vision.ready = Boolean(input.settings.enabled);
      return structuredClone(state.vision);
    },
    voice_agent_status: () => ({ configured: state.agent.settings.enabled, strict_mode: state.agent.strict_mode, providers: state.agent.providers, runs: structuredClone(state.agent.runs), settings: structuredClone(state.agent.settings) }),
    // n67：唤醒词与固件卡挂在主窗口语音中心（回环假数据，不连设备）
    // 2026-11-09：画风包状态以设备下载完为准；没刷的彩屏小智显示“一键刷入全屏画风”
    voice_wakeword_status: () => structuredClone(state.wakeword),
    voice_wakeword_manage: ({ input }) => {
      const device = state.wakeword.devices.find((d) => d.device_id === input.device_id);
      if (input.action === "push" && device) {
        device.screen_pack_state = "pending_download";
        device.screen_pack_pending_with_pack = Boolean(input.screen_pack);
      }
      return { ...structuredClone(state.wakeword), pushed: { device_id: input.device_id, size: 2_097_152, target: input.target, screen_pack: input.screen_pack } };
    },
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
      // 与 center.rs 一致：官方接入点变了才重连，并在返回里报告 reconnected
      let reconnected = false;
      for (const [key, value] of Object.entries(settings)) {
        if (key === "official_mcp_endpoint") {
          reconnected = state.officialRaw !== value;
          state.officialRaw = value;
          state.settings[key] = masked;
        } else state.settings[key] = value;
      }
      return voiceStatus({ reconnected });
    },
    voice_mode_changes: ({ mode }) => ({ changes: [`语音链路：${state.mode} → ${mode}`] }),
    voice_set_mode: ({ mode }) => {
      state.mode = mode;
      state.builtin.link_mode = mode;
      return voiceStatus();
    },
    voice_builtin_models_test: ({ section, input }) => {
      if (section === "local_tts_clone") {
        if (input?.action === "upload") {
          const transcript = input?.transcript?.trim() || "让人类永远保持理智，确实是一种奢求。";
          const nextGen = Number(state.builtin.local.tts_clone_ref?.generation || 0) + 1;
          state.builtin.local.tts_clone_ref = {
            configured: true,
            generation: nextGen,
            file_name: input?.file_name || "moss550w.wav",
            sample_rate: 24000,
            duration_secs: 4.3,
            transcript,
            mode: transcript ? "icl" : "x_vector_only",
          };
          return {
            section: "local_tts_clone",
            ok: true,
            action: "upload",
            auto_transcribed: !input?.transcript?.trim(),
            tts_clone_ref: structuredClone(state.builtin.local.tts_clone_ref),
            detail: `已保存参考录音 ${state.builtin.local.tts_clone_ref.file_name}（4.3 秒），并自动识别填入参考台词`,
            status: structuredClone(state.builtin),
          };
        }
        if (input?.action === "update_transcript") {
          const transcript = String(input?.transcript || "").trim();
          const nextGen = Number(state.builtin.local.tts_clone_ref?.generation || 0) + 1;
          state.builtin.local.tts_clone_ref = {
            ...state.builtin.local.tts_clone_ref,
            generation: nextGen,
            transcript,
            mode: transcript ? "icl" : "x_vector_only",
          };
          return {
            section: "local_tts_clone",
            ok: true,
            action: "update_transcript",
            tts_clone_ref: structuredClone(state.builtin.local.tts_clone_ref),
            detail: "已更新参考录音台词，下一句合成起立即生效",
            status: structuredClone(state.builtin),
          };
        }
        if (input?.action === "clear") {
          state.builtin.local.tts_clone_ref = { configured: false, generation: 0, file_name: "", sample_rate: 0, duration_secs: 0, transcript: "", mode: "none" };
          return {
            section: "local_tts_clone",
            ok: true,
            action: "clear",
            tts_clone_ref: structuredClone(state.builtin.local.tts_clone_ref),
            detail: "已移除本地克隆参考录音",
            status: structuredClone(state.builtin),
          };
        }
      }
      return section === "local_tts"
        ? {
            section: "local_tts",
            ok: true,
            from_cache: true,
            elapsed_ms: 1,
            model_id: input?.model_id || "kokoro",
            speaker: input?.speaker ?? 45,
            speed: input?.speed ?? 1,
            sample_text: "你好，我是 MOSS 语音助手，激光雕刻工作台已就绪。",
            audio_data_url: "data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=",
          }
        : section === "tts_clone"
          ? {
              ok: true,
              elapsed_ms: 420,
              voice_id: "qwen-tts-vc-mossclone-voice-001",
              detail: "音色克隆成功（qwen-tts-vc-mossclone-voice-001）",
              audio_data_url: "data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=",
            }
          : {
              ok: true,
              elapsed_ms: 180,
              detail: "合成测试响应",
              ...(section === "tts" ? { audio_data_url: "data:audio/wav;base64,UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=" } : {}),
            };
    },
    voice_builtin_models_list: async ({ section, input }) => {
      if (section === "llm" && String(input?.url || "").includes("slow-list.example.test")) {
        await new Promise((resolve) => setTimeout(resolve, 250));
      }
      if (section === "tts_voices") {
        return {
          ok: true,
          elapsed_ms: 40,
          voices: [
            { voice_id: "qwen-tts-vc-mossclone-voice-001", target_model: input?.model || "qwen-audio-3.1-tts-flash", status: "OK" },
            { voice_id: "qwen-audio-3.1-tts-plus-moss-pro1", target_model: "qwen-audio-3.1-tts-plus", status: "OK" },
          ],
        };
      }
      return section === "local_tts"
        ? {
            model_id: input?.model_id || "kokoro",
            label: "Kokoro 合成 v1.0（中英，多音色）",
            note: "音质自然，8 个中文音色；体积大。",
            installed: true,
            caching: false,
            cached_count: 2,
            total_count: 2,
            default_speaker: 50,
            languages: [
              { code: "zh", label: "中文普通话（中英混读）", count: 1, sample_text: "你好，我是 MOSS 语音助手。", recommended: true },
              { code: "en-US", label: "美式英语 (en-US)", count: 1, sample_text: "Hello, I am MOSS.", recommended: false },
            ],
            speakers: [
              { id: 45, code: "zf_xiaobei", name: "晓北", gender: "female", gender_label: "女声", lang: "zh", lang_label: "中文普通话（中英混读）", sample_text: "你好，我是 MOSS 语音助手。", recommended_for_zh: true, cached: true },
              { id: 3, code: "af_heart", name: "Heart", gender: "female", gender_label: "女声", lang: "en-US", lang_label: "美式英语 (en-US)", sample_text: "Hello, I am MOSS.", recommended_for_zh: false, cached: true },
            ],
          }
        : {
            ok: true,
            elapsed_ms: 85,
            models: String(input?.url || "").includes("bare.example.test")
              ? ["BareOnly/Model-7B"]
              : ["Qwen/Qwen2.5-7B-Instruct", "FunAudioLLM/SenseVoiceSmall", "MOSS/TTS-1"],
            detail: "已读取模型",
            // 只填裸域名、后端靠 /v1/models 读到列表时会建议改用带 /v1 的地址
            ...(input?.url === "https://bare.example.test" ? { suggested_url: "https://bare.example.test/v1" } : {}),
          };
    },
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
      if (settings?.colab_cli) return { ...structuredClone(state.builtin), restarted: false, hot_applied: true };
      state.builtin.local.settings = { ...state.builtin.local.settings, ...structuredClone(settings) };
      if (state.builtin.jev) {
        if (Object.prototype.hasOwnProperty.call(settings, "jev_filter_enabled")) {
          state.builtin.jev.voice_enabled = Boolean(settings.jev_filter_enabled);
        }
        if (settings.jev_model) state.builtin.jev.model = settings.jev_model;
      }
      if (
        Object.prototype.hasOwnProperty.call(settings, "chat_log_enabled") ||
        Object.prototype.hasOwnProperty.call(settings, "debug_trace_enabled")
      ) {
        const nextFlag = Object.prototype.hasOwnProperty.call(settings, "chat_log_enabled")
          ? settings.chat_log_enabled
          : settings.debug_trace_enabled;
        state.builtin.debug_enabled = Boolean(nextFlag);
      }
      if (settings.clear_debug_traces) state.builtin.debug_traces = [];
      const keys = Object.keys(settings || {});
      const hotOnly = keys.length > 0 && keys.every((k) => [
        "screen_card_style",
        "debug_trace_enabled",
        "chat_log_enabled",
        "clear_debug_traces",
        "persona_style",
        "persona_prompt",
        "custom_personas",
        "persona_risk_notes",
        "screen_pack_auto_keep",
        "auto_allow_devices",
      ].includes(k));
      return { ...structuredClone(state.builtin), restarted: false, hot_applied: hotOnly };
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
    voice_device_set_status: ({ deviceId, status }) => {
      const target = state.builtin.devices.find((item) => item.device_id === deviceId);
      if (!target) {
        const err = new Error("找不到该设备");
        err.code = "voice_device_not_found";
        throw err;
      }
      target.status = status;
      if (status === "revoked") {
        state.builtin.sessions = state.builtin.sessions.filter((item) => item.device_id !== deviceId);
      }
      return structuredClone(state.builtin);
    },
    voice_device_rename: ({ deviceId, name }) => {
      const target = state.builtin.devices.find((item) => item.device_id === deviceId);
      if (!target) {
        const err = new Error("找不到该设备");
        err.code = "voice_device_not_found";
        throw err;
      }
      target.name = String(name || "").trim().slice(0, 32);
      return structuredClone(state.builtin);
    },
    voice_device_remove: ({ deviceId }) => {
      const target = state.builtin.devices.find((item) => item.device_id === deviceId);
      if (!target) {
        const err = new Error("找不到该设备");
        err.code = "voice_device_not_found";
        throw err;
      }
      if (target.status === "revoked") {
        const err = new Error("已撤销的设备不能移除，以免绕过撤销限制；如需恢复连接请点“允许”");
        err.code = "voice_device_revoked";
        throw err;
      }
      state.builtin.devices = state.builtin.devices.filter((item) => item.device_id !== deviceId);
      state.builtin.sessions = state.builtin.sessions.filter((item) => item.device_id !== deviceId);
      delete state.builtin.device_prefs[deviceId];
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
    voice_usb_provision: ({ input }) => {
      switch (input?.action) {
        case "ports":
          return { ports: structuredClone(state.usbPorts), laser_busy: false };
        case "open_wifi_settings":
          assert.deepEqual(Object.keys(input).sort(), ["action", "target"], "打开设置只带固定的页面类别");
          state.wifiSettingsOpened.push(input.target);
          state.wifiPermission = "granted";
          return { opened: true };
        case "wifi_current":
          assert.deepEqual(Object.keys(input), ["action"], "读本机 Wi-Fi 不带串口等其他字段");
          if (state.wifiPermission !== "granted") {
            return {
              platform: "macos",
              permission: state.wifiPermission,
              current: null,
              networks: [],
              hint: "MOSS 没有“定位服务”权限，macOS 因此不给出 Wi-Fi 名称（MOSS 不读取、不保存位置）。",
            };
          }
          return {
            platform: "macos",
            permission: "granted",
            current: { ssid: "OfficeNet", signal: 80, bands: ["5g"], band_signals: { "5g": 80 }, secure: true, current: true },
            networks: [],
            hint: null,
          };
        case "wifi_scan":
          return {
            platform: "macos",
            permission: "granted",
            current: { ssid: "OfficeNet", signal: 80, bands: ["5g"], band_signals: { "5g": 80 }, secure: true, current: true },
            networks: [
              { ssid: "OfficeNet", signal: 80, bands: ["5g"], band_signals: { "5g": 80 }, secure: true, current: true },
              { ssid: "StudioNet", signal: 72, bands: ["2.4g", "5g"], band_signals: { "2.4g": 35, "5g": 72 }, secure: true, wpa3_only: false, current: false },
              { ssid: "CafeFree", signal: 40, bands: ["2.4g"], band_signals: { "2.4g": 40 }, secure: false, wpa3_only: false, current: false },
              { ssid: "StrictWpa3", signal: 30, bands: ["2.4g"], band_signals: { "2.4g": 30 }, secure: true, wpa3_only: true, current: false },
            ],
            hint: null,
          };
        case "inspect":
          assert.equal(input.port, "/dev/cu.usbmodem14201");
          state.usbInspectCount += 1;
          return {
            device: { chip: "esp32s3", mac_address: "dc:da:0c:11:22:33", flash_size: "16MB" },
            partition: { label: "nvs", offset: "0x9000", size: 20480, all_labels: ["nvs", "otadata", "phy_init", "ota_0", "ota_1", "assets"], has_xiaozhi_markers: true },
            known_device: null,
            nvs: {
              wifi_slots: [{ slot: 0, ssid: "HomeNet", has_password: true, channel: 6 }],
              ota_url: state.usbInspectCount === 1 ? "http://192.0.2.99:8003/xiaozhi/ota/" : state.builtin.ota_url,
              max_tx_power: 0,
              remember_bssid: false,
              sleep_mode: false,
              sleep_dim_min: 0,
              sleep_off_min: 0,
            },
          };
        case "write":
          assert.equal(input.confirmed, true, "写入板子前必须完成内联确认");
          assert.equal(input.expected_mac, "dc:da:0c:11:22:33");
          return {
            device: { chip: "esp32s3", mac_address: "dc:da:0c:11:22:33", flash_size: "16MB" },
            partition: { label: "nvs", offset: "0x9000", size: 20480, all_labels: ["nvs", "otadata", "phy_init", "ota_0", "ota_1", "assets"], has_xiaozhi_markers: true },
            known_device: { device_id: "dc:da:0c:11:22:33", name: "ESP32-S3", status: "allowed", model: "esp32s3", firmware_version: "usb" },
            nvs: {
              wifi_slots: [
                ...(input.patch?.new_wifi?.ssid ? [{ slot: 1, ssid: input.patch.new_wifi.ssid, has_password: Boolean(input.patch.new_wifi.password), channel: null }] : []),
                { slot: 0, ssid: "HomeNet", has_password: true, channel: 6 },
              ],
              ota_url: input.patch?.ota_url ?? state.builtin.ota_url,
              max_tx_power: input.patch?.max_tx_power ?? 0,
              remember_bssid: Boolean(input.patch?.remember_bssid),
              sleep_mode: Boolean(input.patch?.sleep_mode),
              sleep_dim_min: input.patch?.sleep_dim_min ?? 0,
              sleep_off_min: input.patch?.sleep_off_min ?? 0,
            },
          };
        default:
          throw new Error(`Unexpected voice_usb_provision action: ${input?.action}`);
      }
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
  const shot = async (name, options = {}) => {
    if (writeBaseline) await page.screenshot({ path: path.join(baselineDir, `${name}.png`), ...options });
  };

  await page.goto(new URL("voice", local).href, { waitUntil: "domcontentloaded" });
  await page.locator("#voice-mode-card").waitFor();
  assert.equal(await page.getByRole("tab").count(), 4, "官方链路只有 服务 / 设备 / 扩展 / 记录与诊断（n69：唤醒词并入设备）");
  assert.equal(await page.getByRole("tab", { name: /^唤醒词与音量/ }).count(), 0, "“唤醒词与音量”标签已并入“设备”");
  await tab("设备").click();
  assert.match(await page.locator("#voice-wake-volume-mode-hint").innerText(), /页面顶部“语音链路”/, "非内置链路提示到顶部语音链路卡片切换");
  assert.equal(await page.locator("#voice-group-wake-word").count(), 0, "非内置链路只提示切换，不渲染唤醒词入口");
  assert.equal(await page.locator("#voice-group-builtin-devices").count(), 0, "非内置链路不渲染内置设备列表");
  assert.equal(await page.locator("#voice-group-device-usb-provision").count(), 0, "非内置链路不渲染 USB 一键配网");
  await tab("服务").click();
  assert.equal(await page.getByRole("tab", { name: /^模型/ }).count(), 0, "官方链路的模型在小智控制台配置");
  assert.equal(await page.getByRole("tab", { name: /^隐私与家居/ }).count(), 0, "非内置链路不显示隐私与家居标签");
  // 官方链路没有“模型”标签：加工反馈判断（Jev）在“服务”标签配（2026-11-26 入口收口到语音中心，偏好设置不再重复）
  assert.ok(await page.locator("#voice-group-official-jev").isVisible(), "官方链路在服务标签显示 Jev 判断服务");
  await page.getByTitle("系统偏好设置").click();
  await page.locator(".settings-modal").waitFor();
  assert.equal(await page.locator(".settings-modal #settings-jev").count(), 0, "桌面端偏好设置不再显示 Jev");
  await page.locator(".settings-modal .mat-modal-close").click();
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-mode"), "official", "语音状态条默认跟随官方链路");
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-expanded"), "false", "非对话时默认收成单行状态条");
  assert.match(await page.locator("#voice-moss-twin-source").innerText(), /官方小智/);
  await page.locator("#voice-moss-twin-toggle").click();
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-expanded"), "true", "点击单行状态条展开详情");
  await page.locator("#voice-moss-twin-toggle").click();
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-expanded"), "false", "点击收起恢复单行状态条");
  // 1. 服务：官方链路只显示接入点与地址；其他链路的面板不渲染（不是折叠）
  await tab("服务").click();
  assert.ok(await page.locator("#voice-endpoint-card").isVisible(), "官方链路显示接入点");
  assert.equal(await page.locator("#voice-builtin-card").count(), 0, "官方链路不渲染内置服务面板");
  await shot("voice-official");
  await tab("设备").click();
  assert.ok(await page.locator("#voice-group-endpoints").isVisible(), "官方链路渲染接入点状态");
  assert.equal(await page.locator("#voice-panel-devices #voice-group-tools").count(), 0, "n69：内置工具不在设备标签");

  // 1b. 内置工具与第三方 MCP 是两类东西：内置工具只能开关，增删用可复制的 AI 提示词模板（2026-11-02）；n69 起两者都在“扩展”
  await tab("扩展").click();
  assert.ok(await page.locator("#voice-panel-features #voice-group-tools").isVisible(), "官方链路在扩展标签渲染工具开关与调用统计");
  await openGroup("tools");
  // n72：中文名、按用途分组；不在精简清单的工具标明“完整工具打开后才给 AI”；完整工具开关只提交这一项
  await page.locator('#voice-tools-group-job [data-tool="laser_workflow_tool"]').getByText("刻字：预览、发送、进度").waitFor();
  await page.locator('#voice-tools-group-advanced [data-tool="laser_grbl_tool"]').getByText("完整工具打开后才给 AI").waitFor();
  await page.locator("#voice-tools-full-switch input").click();
  for (let i = 0; i < 50 && last("voice_save_settings")?.args.settings.mcp_full_tool_list !== true; i += 1) await page.waitForTimeout(50);
  assert.deepEqual(last("voice_save_settings").args.settings, { mcp_full_tool_list: true });
  assert.equal(await page.locator("#voice-tool-prompts").count(), 0, "不再显示面向开发者的施工提示词折叠块");
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

  // 3. 链路只有“MOSS 内置服务”与“官方小智”两项（2026-11-17 删除自建服务端链路）
  assert.equal(await page.locator("#voice-mode-card [role=radio]").count(), 2, "链路选择只列两项");
  assert.equal(await page.locator("#voice-mode-self_hosted").count(), 0, "没有自建服务端选项");

  // 4. 切到内置：确认框 → 切换
  await page.locator("#voice-mode-builtin").click();
  // 等确认框真正弹出来再截图（基线要的是切换前的确认框）
  await page.getByRole("alertdialog").filter({ has: page.getByRole("button", { name: "确认切换", exact: true }) }).waitFor();
  // 确认框有入场动画：截图时让动画直接到结束状态
  await shot("voice-mode-confirm", { animations: "disabled" });
  await confirmDialog("确认切换");
  await page.locator("#voice-mode-builtin[aria-checked=true]").waitFor();
  assert.equal(last("voice_set_mode").args.mode, "builtin");
  assert.equal(await page.locator("#voice-moss-twin").getAttribute("data-mode"), "builtin", "切换内置链路后状态条同步为内置语音");
  assert.match(await page.locator("#voice-moss-twin-source").innerText(), /内置语音/);
  await page.locator("#voice-builtin-checklist").waitFor();
  assert.equal(await page.locator("#voice-setup-step-service").getAttribute("data-done"), "false", "未启动内置服务时第 1 步未勾选");
  assert.equal(await page.locator("#voice-setup-step-models").getAttribute("data-done"), "false", "未配模型时第 2 步未勾选");
  assert.equal(await page.locator("#voice-setup-step-device").getAttribute("data-done"), "true", "假数据里已有一台允许的设备（立创实战派小智），第 3 步应打勾（未允许设备不打勾由 voiceState 单测覆盖）");

  // 5. 内置大模型：先测试、后保存，只提交这一项；同时验证千问折行地址清理、预设模型立即可见与 sk-sp- 套餐地址一键切换
  await tab("模型").click();
  await openGroup("builtin-model-llm");
  const llmSave = page.locator("#voice-builtin-model-save-llm");
  await page.locator("#voice-builtin-model-url-llm").fill("https://maas.qianwenaiapi.com/compatible-mo\n  de/v1");
  assert.equal(
    await page.locator("#voice-builtin-model-url-llm").inputValue(),
    "https://maas.qianwenaiapi.com/compatible-mode/v1",
    "多行折行粘贴的地址自动清理空白",
  );
  await page.locator("#voice-builtin-model-name-llm").click();
  await page.locator("#voice-builtin-model-options-llm").getByRole("option", { name: /qwen3\.8-flash/ }).waitFor();
  await page.locator("#voice-builtin-model-key-llm").fill("sk-sp-fake");
  await page.locator("#voice-builtin-endpoint-mismatch-llm").waitFor();
  await page.locator("#voice-builtin-fix-endpoint-llm").click();
  assert.equal(
    await page.locator("#voice-builtin-model-url-llm").inputValue(),
    "https://token-plan.maas.qianwenaiapi.com/compatible-mode/v1",
    "一键切换为千问 Token Plan 套餐专属地址",
  );
  // 只填裸域名：列表在 /v1 下读到时提示一键改用带 /v1 的地址（停手 1 秒后自动读取）
  await page.locator("#voice-builtin-model-url-llm").fill("https://bare.example.test");
  await page.locator("#voice-builtin-model-key-llm").fill("fake-llm-key");
  await page.locator("#voice-builtin-use-v1-llm").click();
  assert.equal(
    await page.locator("#voice-builtin-model-url-llm").inputValue(),
    "https://bare.example.test/v1",
    "一键改用带 /v1 的地址",
  );
  await page.locator("#voice-builtin-v1-suggestion-llm").waitFor({ state: "detached" });
  // 在慢速 `/models` 请求尚未返回期间完成测试与保存，读取按钮不得永久卡在 loading 禁用态
  await page.locator("#voice-builtin-model-url-llm").fill("https://slow-list.example.test/v1");
  await page.locator("#voice-builtin-model-key-llm").fill("fake-llm-key");
  await page.locator("#voice-builtin-model-name-llm").fill("fake-model");
  await page.locator("#voice-builtin-model-fetch-llm").click();
  await page.locator("#voice-builtin-model-test-llm").click();
  await page.locator("#voice-builtin-model-result-llm").filter({ hasText: "测试通过" }).waitFor();
  await llmSave.click();
  await page.locator("#voice-group-builtin-model-llm [role=status]").filter({ hasText: "已保存大模型设置" }).waitFor();
  assert.equal(
    await page.locator("#voice-builtin-model-fetch-llm").isDisabled(),
    false,
    "在途 /models 请求被保存打断后不得残留孤立 loading 禁用态",
  );
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
  await page.locator("#voice-builtin-model-name-llm").click();
  assert.equal(
    await page.locator("#voice-builtin-model-options-llm").getByRole("option", { name: /BareOnly\/Model-7B/ }).count(),
    0,
    "切换端点保存后不得把旧端点的模型列表串进新端点缓存",
  );

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
  // 5b-2. 内置语音合成：一键复用地址、点选模型自动填入默认音色、测试通过后显示“试听语音”按钮；
  // 误填千问大模型地址时提示一键切换千问 / 百炼原生流式合成，并支持上传本机录音克隆音色自动填入 voice_id
  await openGroup("builtin-model-tts");
  await page.locator("#voice-builtin-reuse-peer-tts").click();
  await page.locator("#voice-builtin-model-options-tts").getByRole("option", { name: /MOSS\/TTS-1/ }).click();
  assert.equal(await page.locator("#voice-builtin-model-voice-tts").inputValue(), "alloy", "选定合成模型后自动填入默认音色");
  await page.locator("#voice-builtin-model-test-tts").click();
  await page.locator("#voice-builtin-model-play-tts").waitFor();
  await page.locator("#voice-builtin-model-url-tts").fill("https://maas.qianwenaiapi.com/compatible-mode/v1");
  await page.locator("#voice-builtin-alibaba-tts-hint-tts").waitFor();
  await page.locator("#voice-builtin-use-dashscope-tts").click();
  assert.equal(await page.locator("#voice-builtin-model-kind-tts").inputValue(), "dashscope", "一键切换为千问 / 百炼原生流式接口");
  assert.equal(await page.locator("#voice-builtin-model-name-tts").inputValue(), "qwen-audio-3.1-tts-flash");
  assert.equal(await page.locator("#voice-builtin-model-voice-tts").inputValue(), "longanhuan_v3.1");
  assert.match(await page.locator("#voice-group-builtin-model-tts .voice-final").innerText(), /SpeechSynthesizer/);
  await page.locator("#voice-builtin-model-key-tts").fill("sk-test-dashscope");
  await page.locator("#voice-builtin-voice-clone-input-tts").setInputFiles({
    name: "moss-sample.wav",
    mimeType: "audio/wav",
    buffer: Buffer.from("UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=", "base64"),
  });
  await page.locator("#voice-builtin-model-result-tts").filter({ hasText: "音色克隆成功" }).waitFor();
  assert.equal(last("voice_builtin_models_test").args.section, "tts_clone");
  assert.equal(await page.locator("#voice-builtin-model-voice-tts").inputValue(), "qwen-tts-vc-mossclone-voice-001", "克隆完成后自动回填专属音色 ID");
  assert.equal(await page.locator("#voice-builtin-model-save-tts").isDisabled(), false, "克隆并合成试听通过后可直接保存");
  await page.locator("#voice-builtin-voice-toggle-tts").click();
  await page.locator("#voice-builtin-voice-options-tts").getByRole("option", { name: /qwen-tts-vc-mossclone-voice-001/ }).waitFor();
  await page.locator("#voice-builtin-voice-options-tts").getByRole("option", { name: /qwen-audio-3\.1-tts-plus-moss-pro1/ }).click();
  assert.equal(await page.locator("#voice-builtin-model-voice-tts").inputValue(), "qwen-audio-3.1-tts-plus-moss-pro1");
  assert.equal(await page.locator("#voice-builtin-model-name-tts").inputValue(), "qwen-audio-3.1-tts-plus", "点选已有克隆音色时自动同步切换其绑定的目标模型");
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
  // “当前语音链路”五栏的下拉框必须在同一水平线上（推荐行只加在下拉框下方，不能把它顶歪）
  const mixSelectTops = await page
    .locator("#voice-builtin-mix-bar .voice-mix-field select")
    .evaluateAll((els) => els.map((el) => Math.round(el.getBoundingClientRect().top)));
  assert.equal(new Set(mixSelectTops).size, 1, `语音链路下拉框上下不齐：${mixSelectTops.join(",")}`);
  assert.equal(await page.locator("#voice-builtin-mix-bar .voice-mix-recommend").count(), mixOrder.length, "每一栏都有推荐行");
  if (process.env.VOICE_QA_DIR) await page.locator("#voice-builtin-mix-bar").screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-mix-bar.png") });
  assert.deepEqual(mixOrder, ["voice-builtin-mix-vad", "voice-builtin-mix-asr", "voice-builtin-mix-jev", "voice-builtin-mix-llm", "voice-builtin-mix-tts"]);
  // 5d-1. 链路栏右上角“测试全部环节”（2026-11-08）：按当前选用逐个测，不通的环节在对应下拉框下方报错。
  // 此时只保存了大模型：判停为内置能量判停（无需测试）、Jev 关闭（跳过）、识别与合成未配置（报错）、大模型真实请求一次
  const testsBefore = calls.filter((call) => call.command === "voice_builtin_models_test").length;
  await page.locator("#voice-builtin-test-all").click();
  await page.locator("#voice-builtin-test-summary").filter({ hasText: "2 个环节不通：语音识别、语音合成" }).waitFor();
  assert.match(await page.locator("#voice-builtin-test-vad").innerText(), /✓ 内置能量判停，无需测试/);
  assert.match(await page.locator("#voice-builtin-test-asr").innerText(), /✗ 云端语音识别还没配置/);
  assert.match(await page.locator("#voice-builtin-test-jev").innerText(), /已关闭，跳过/);
  assert.match(await page.locator("#voice-builtin-test-llm").innerText(), /✓ .*（180 ms）/);
  assert.match(await page.locator("#voice-builtin-test-tts").innerText(), /✗ 云端语音合成还没配置/);
  assert.equal(await page.locator("#voice-builtin-test-asr").getAttribute("role"), "alert", "不通的环节作为警示播报");
  const pipelineCalls = calls.filter((call) => call.command === "voice_builtin_models_test").slice(testsBefore);
  assert.deepEqual(pipelineCalls.map((call) => call.args.section), ["llm"], "只对已配置的云端环节发起真实测试");
  assert.equal(pipelineCalls[0].args.input.api_key, "", "密钥留空，由桌面端沿用已保存的密钥");
  assert.equal(pipelineCalls[0].args.input.model, "fake-model");
  await page.getByTitle("系统偏好设置").click();
  await page.locator(".settings-modal").waitFor();
  await page.waitForTimeout(300); // 等偏好设置读完当前链路再断言，避免只测到“未读到前不显示”
  assert.equal(await page.locator("#settings-jev").count(), 0, "内置链路下 Jev 由“模型 → Jev”统一管理，右上角偏好设置不重复显示");
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
  // 2026-11-25：直接跳到这个模型那一行、聚焦它的“下载”按钮并高亮，不只是展开整个分组
  await page.waitForFunction(() => document.activeElement?.id === "voice-local-download-onejev_1_6_9b");
  assert.match(await page.locator("#voice-local-row-onejev_1_6_9b").getAttribute("class"), /voice-local-row--flash/, "跳转后高亮目标行");
  // 本地模型按顶部链路顺序排列：1 判停 → 2 识别 → 3 Jev → 4 大模型（Colab）→ 5 合成 → 视觉
  const localOrder = await page.locator("#voice-local-models > [id^=voice-local-group-], #voice-local-models > #voice-local-colab-bridge").evaluateAll((els) => els.map((el) => el.id));
  assert.deepEqual(localOrder, ["voice-local-group-vad", "voice-local-group-asr", "voice-local-group-jev", "voice-local-colab-bridge", "voice-local-group-tts", "voice-local-group-vision"]);
  assert.match(await page.locator("#voice-local-group-tts .voice-field-head b").first().innerText(), /^5\. 语音合成模型包/);
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
  // Colab T4 专属联动默认收起，标题栏保留状态胶囊；点击展开 / 收起，接管中点顶部“去配置”会自动展开
  assert.equal(await page.locator("#voice-local-colab-bridge").evaluate((el) => el.open), false, "Colab T4 专属联动默认收起");
  assert.match(await page.locator("#voice-colab-state-pill").innerText(), /未开启/);
  assert.equal(await page.locator("#voice-colab-enabled").isVisible(), false, "收起时不展示 Colab 表单");
  if (process.env.VOICE_QA_DIR) {
    await page.locator("#voice-local-colab-bridge").screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-local-colab-collapsed.png") });
  }
  await page.locator("#voice-local-colab-bridge > summary").click();
  await page.waitForFunction(() => document.querySelector("#voice-local-colab-bridge")?.open === true);
  assert.equal(await page.locator("#voice-colab-enabled").isVisible(), true, "展开后显示 Colab 开关与配置");
  assert.equal(await page.locator("#voice-colab-export-ipynb").isVisible(), true, "展开后显示导出笔记本按钮");
  assert.equal(await page.locator("#voice-colab-advanced").count(), 0, "Colab 固定模型与音色，不再显示无效高级覆盖框");
  if (process.env.VOICE_QA_DIR) {
    await page.locator("#voice-local-colab-bridge").screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-local-colab-expanded.png") });
  }
  state.builtin.local.colab = { enabled: true, active: true, url: "https://demo-t4.trycloudflare.com/v1", llm_model: "qwen3-vl-8b", tts_model: "qwen3-tts", tts_voice: "moss", key_configured: true };
  await page.locator("#voice-colab-cli-refresh").click();
  await page.locator("#voice-colab-state-pill").filter({ hasText: "已开启 · 接管中" }).waitFor();
  await page.locator("#voice-local-colab-bridge > summary").click();
  await page.waitForFunction(() => document.querySelector("#voice-local-colab-bridge")?.open === false);
  assert.equal(await page.locator("#voice-colab-enabled").isVisible(), false, "手动收起后隐藏 Colab 表单");
  await page.locator("#voice-builtin-mix-llm .voice-link-btn").click();
  await page.waitForFunction(() => document.querySelector("#voice-local-colab-bridge")?.open === true);
  assert.equal(await page.locator("#voice-colab-enabled").isVisible(), true, "接管中点顶部“去配置”时自动展开 Colab 折叠栏");
  delete state.builtin.local.colab;
  await page.locator("#voice-colab-cli-refresh").click();
  await page.locator("#voice-colab-state-pill").filter({ hasText: "未开启" }).waitFor();
  await page.locator("#voice-local-colab-bridge > summary").click();
  await page.waitForFunction(() => document.querySelector("#voice-local-colab-bridge")?.open === false);
  assert.equal(await page.locator("#voice-colab-enabled").isVisible(), false, "再次点击收起 Colab 联动");
  // n111：补字库自动下载、下好即生效，不是可选模型——界面不列出
  assert.equal(await page.locator("#voice-local-group-font").count(), 0, "补字库不在本地模型区显示");
  // n109：命令词抗噪并进“拾音判停”；降噪、语义判停与老版判停收进“更多 / 实验”，正在用收起区的模型时自动展开
  const vadGroup = page.locator("#voice-local-group-vad");
  await vadGroup.locator("#voice-local-group-kws #voice-local-kws-toggle").waitFor();
  for (const kind of ["denoise", "turn", "kws"]) assert.equal(await page.locator(`#voice-local-models > #voice-local-group-${kind}`).count(), 0, `${kind} 不再单独成组`);
  assert.notEqual(await page.locator("#voice-local-vad-more").getAttribute("open"), null, "上面在顶部用过收起区的老版 Silero，自动展开过就保持展开");
  await page.locator("#voice-local-vad-more > summary").click();
  await page.waitForFunction(() => document.querySelector("#voice-local-vad-more")?.open === false);
  assert.equal(await page.locator("#voice-local-pick-dpdfnet_baseline").isVisible(), false, "收起后看不到实验模型");
  await page.locator("#voice-local-vad-more > summary").click();
  await page.locator("#voice-local-vad-more #voice-local-group-denoise #voice-local-denoise-toggle").waitFor();
  await page.locator("#voice-local-vad-more #voice-local-group-turn #voice-local-turn-toggle").waitFor();
  // n111：判停模型标“几选一”（替换），命令词抗噪 / 降噪 / 语义判停标“叠加，可同时开”，并写明能和谁一起用
  assert.match(await page.locator("#voice-local-vad-legend").innerText(), /判停模型几选一.*可以同时开/);
  const roles = await page.locator("#voice-local-group-vad .voice-role-tag").evaluateAll((tags) =>
    tags.map((tag) => [tag.closest("[id^=voice-local-group-]").id, tag.dataset.role]));
  assert.deepEqual(roles, [
    ["voice-local-group-vad", "replace"],
    ["voice-local-group-kws", "addon"],
    ["voice-local-group-vad-more", "replace"],
    ["voice-local-group-denoise", "addon"],
    ["voice-local-group-turn", "addon"],
  ]);
  assert.match(await page.locator("#voice-local-kws-note").innerText(), /要先在顶部选本地判停/);
  assert.match(await page.locator("#voice-local-denoise-note").innerText(), /可与任何本地判停、本地识别同时开/);
  assert.match(await page.locator("#voice-local-turn-note").innerText(), /包括能量判停/);
  assert.equal(await page.locator("#voice-local-pick-gtcrn_simple").count(), 0, "没下载的降噪模型不能选用");
  await page.locator("#voice-local-pick-dpdfnet_baseline").click();
  await page.locator("#voice-local-group-denoise tr").filter({ hasText: "DPDFNet" }).getByText("已选，开启后使用").waitFor();
  if (process.env.VOICE_QA_DIR) await page.locator("#voice-local-group-vad").screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-local-vad.png") });
  // 按场景推荐的整套模型：每种场景一行；缺模型的套系直接列出缺失项与一键下载，隐藏“用这套”按钮
  await page.locator("#voice-model-pairing-guide > summary").click();
  assert.ok((await page.locator("#voice-model-pairing-guide .voice-combo-row").count()) >= 4, "列出几种场景组合");
  await page.locator("#voice-combo-need-quiet").waitFor();
  assert.equal(await page.locator("#voice-combo-apply-quiet").count(), 0, "缺模型时隐藏用这套按钮");
  assert.equal(await page.locator("#voice-combo-download-quiet").count(), 1, "缺模型时提供一键下载按钮");
  await openGroup("experience");
  assert.equal(await page.locator("#voice-screen-card-style").count(), 0, "n69：对话体验里不再有画风下拉（只在设备 › 副屏预览切换）");
  assert.equal(await page.locator("#voice-jev-filter-enabled").count(), 0, "n69：对话体验里不再有 Jev 开关勾选框");
  // n73：常用项直接显示，识别准确度与高级项默认收起；只读的画风 / Jev 状态行已删除
  assert.equal(await page.locator("#voice-screen-card-style-state").count(), 0, "n73：画风只在设备 › 副屏预览显示");
  assert.equal(await page.locator("#voice-jev-filter-state").count(), 0, "n73：Jev 状态只在顶部链路栏显示");
  assert.equal(await page.locator("#voice-experience-accuracy").getAttribute("open"), null, "识别准确度默认收起");
  assert.equal(await page.locator("#voice-experience-advanced").getAttribute("open"), null, "高级默认收起");
  assert.equal(await page.locator("#voice-llm-fast-reply").count(), 0, "当前已保存的非兼容测试域名不显示快速回答开关");
  state.builtin.models.llm.url = "https://openrouter.ai/api/v1";
  await page.locator("#voice-local-colab-bridge > summary").click();
  await page.locator("#voice-colab-cli-refresh").click();
  await page.locator("#voice-local-colab-bridge > summary").click();
  await page.locator("#voice-llm-fast-reply").waitFor();
  assert.equal(await page.locator("#voice-llm-fast-reply").isChecked(), true, "切到支持关闭思考的服务商后显示快速回答且默认开");
  assert.equal(await page.locator("#voice-barge-in-ms").count(), 0, "插话关闭时不显示插话时长");
  await page.locator("#voice-experience-advanced > summary").click();
  await page.locator("#voice-jev-off-note").waitFor();
  assert.equal(await page.locator("#voice-jev-suboptions").count(), 0, "Jev 未开启时不显示细项");
  await page.locator("#voice-experience-accuracy > summary").click();
  await page.locator("#voice-hotwords").waitFor();
  assert.equal(await page.locator("#voice-asr-beam").count(), 0, "没用本地 X-ASR 时不显示 X-ASR 热词识别");
  await page.locator("#voice-llm-fast-reply").uncheck();
  // 2026-11-25：音色与语速在“5. 语音合成模型包”里，紧跟合成模型表
  assert.equal(await page.locator("#voice-local-group-tts #voice-local-tts-settings").getByLabel("本地合成语速（0.5 ~ 2.0）").count(), 1, "语速框在语音合成模型包里");
  assert.equal(await page.locator("#voice-local-group-tts #voice-local-speaker-browse").count(), 1, "音色在语音合成模型包里");
  await page.getByLabel("本地合成语速（0.5 ~ 2.0）").fill("1.5");
  // 2026-11-25：音色按名称下拉（按语种分组，不在音色表里的当前值单独列出）；“按当前音色和语速试一句”带上未保存的语速
  const speakerSelect = page.locator("#voice-local-tts-settings select#voice-local-speaker");
  await speakerSelect.locator("option", { hasText: "45 · 晓北（女声）" }).waitFor({ state: "attached" });
  assert.equal(await speakerSelect.inputValue(), "50", "下拉不悄悄换掉当前音色");
  assert.equal(await speakerSelect.locator("optgroup").count(), 3, "按语种分组，外加当前值");
  assert.equal(await page.locator("#voice-local-speed").count(), 1, "Kokoro 支持调语速，显示语速框");
  await speakerSelect.selectOption("45");
  await page.locator("#voice-local-tts-try").click();
  await page.waitForFunction(() => !/合成中/.test(document.querySelector("#voice-local-tts-try")?.textContent || ""));
  assert.deepEqual(last("voice_builtin_models_test").args, { section: "local_tts", input: { model_id: "kokoro", speaker: 45, speed: 1.5 } }, "试一句用未保存的音色和语速");
  await page.locator("#voice-barge-in").check();
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已保存对话体验设置" }).waitFor();
  const localPayload = last("voice_local_save").args.settings;
  assert.equal(localPayload.barge_in, true);
  assert.equal(localPayload.llm_fast_reply, false, "快速回答开关随对话体验一起保存");
  await page.locator("#voice-barge-in-ms").waitFor();
  assert.equal("tts_speed" in localPayload, false, "局部补丁不携带本地模型分组字段");
  assert.equal(state.builtin.local.settings.tts_speed ?? 1, 1, "本地模型的未保存改动没有被一起提交");
  assert.ok(await page.locator("#voice-group-local-models .voice-dirty").isVisible(), "本地模型仍显示未保存");
  // 6b. 顶部切换开启 Jev 后再修改并保存语音体验其他字段，绝不把 Jev 开关覆盖回旧值 false
  await page.locator("#voice-builtin-mix-select-jev").selectOption("jev:typesafe/jev-1.14");
  await page.locator("#voice-builtin-mix-status").filter({ hasText: "已切换为 Jev 决策（typesafe/jev-1.14）" }).waitFor();
  await page.locator("#voice-jev-suboptions").filter({ hasText: "Jev 已开启" }).waitFor();
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
  // n75：选项只显示角色名，说明在下拉框下方 #voice-persona-hint
  await page.locator("#voice-persona-style").selectOption({ label: "老师傅（自建）" });
  assert.match(await page.locator("#voice-persona-hint").innerText(), /话少，先说结论/);
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
  // 6e-2. 单独改人设风险提示（hot_applied=true），提示“已立即生效，无需重启”
  await page.locator("#voice-persona-risk-notes").check();
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已立即生效，无需重启" }).waitFor();
  assert.deepEqual(last("voice_local_save").args.settings, { persona_risk_notes: true });
  await page.locator("#voice-persona-risk-notes").uncheck();
  await page.locator("#voice-experience-save").click();
  await page.locator("#voice-group-experience [role=status]").filter({ hasText: "已立即生效，无需重启" }).waitFor();
  assert.deepEqual(last("voice_local_save").args.settings, { persona_risk_notes: false });
  // 6f. n74：视觉理解可选本地视觉模型——云端地址与密钥栏隐藏，未下载给出去下载入口，保存只提交本地模型编号
  await openGroup("vision");
  assert.equal(await page.locator("#voice-vision-source").inputValue(), "cloud", "旧配置（无本地模型、未用对话模型）显示为云端");
  await page.locator("#voice-vision-source").selectOption("local");
  assert.equal(await page.locator("#voice-vision-local-model").inputValue(), "qwen3_vl_4b", "切到本地时优先选已下载的模型");
  assert.equal(await page.getByLabel("视觉模型密钥操作").count(), 0, "本地视觉不需要地址与密钥");
  await page.locator("#voice-vision-local-model").selectOption("qwen3_vl_2b");
  await page.locator("#voice-vision-local-missing").filter({ hasText: "还没下载" }).waitFor();
  await page.locator("#voice-vision-local-model").selectOption("qwen3_vl_4b");
  assert.equal(await page.locator("#voice-vision-local-missing").count(), 0);
  await page.locator("#voice-vision-enabled").check();
  await page.locator("#voice-vision-save").click();
  await page.locator("#voice-group-vision [role=status]").filter({ hasText: "已保存视觉设置" }).waitFor();
  const visionSaved = last("voice_vision_save").args.input;
  assert.equal(visionSaved.settings.local_model, "qwen3_vl_4b");
  assert.equal(visionSaved.settings.use_dialog_llm, false);
  assert.equal(visionSaved.key.action, "keep", "本地视觉不改密钥");
  assert.match(await page.locator("#voice-group-vision > .voice-group-h").innerText(), /本地 Qwen3-VL · 4B/);
  await page.locator("#voice-vision-source").selectOption("dialog");
  assert.equal(await page.locator("#voice-vision-local-model").count(), 0, "切到对话大模型后不再显示本地模型");
  await page.locator("#voice-vision-source").selectOption("local");

  // 6g. 点击合成模型包名称弹出语言包与本机缓存音色试听弹窗，支持筛选、播放缓存试听、一键选用音色且不误带未保存的 tts_speed 草稿，按 Escape 关闭
  await page.locator("#voice-local-tts-open-kokoro").click();
  const ttsModal = page.locator("#voice-local-tts-modal");
  await ttsModal.waitFor();
  await ttsModal.getByText(/本机试听已全部自动缓存/).waitFor();
  await ttsModal.locator("#voice-local-tts-lang-zh").waitFor();
  assert.equal(await ttsModal.locator(".tts-speaker-sample").count(), 0, "与语言包一致的试听语不在音色行里逐行重复");
  assert.equal(await ttsModal.locator(".tts-speaker-row").getByText("本机已缓存").count(), 0, "全部缓存完成后不在每行重复贴“本机已缓存”标签");
  await ttsModal.locator("#voice-local-tts-play-45").click();
  assert.deepEqual(last("voice_builtin_models_test").args, { section: "local_tts", input: { model_id: "kokoro", speaker: 45 } });
  await ttsModal.locator("#voice-local-tts-apply-45").click();
  await ttsModal.locator("#voice-local-tts-action-notice").filter({ hasText: "已选用 #45 晓北" }).waitFor();
  assert.deepEqual(last("voice_local_save").args, { settings: { tts: true, tts_model: "kokoro", tts_speaker: 45 } }, "一键选用音色不附带未保存的 tts_speed 草稿");
  assert.equal(state.builtin.local.settings.tts_speed ?? 1, 1, "未保存的语速草稿仍未被误提交");
  await page.keyboard.press("Escape");
  await page.waitForFunction(() => !document.querySelector("#voice-local-tts-modal"));

  // 6h. n131：切到本地离线声音克隆模型——不显示预设语言包按钮、音色下拉与语速框，支持上传 WAV 自动填台词、改原话与当场试听
  assert.equal(await page.locator("#voice-local-tts-open-qwen3_tts_1_7b_clone").count(), 0, "离线克隆行不显示预设语言包弹窗按钮");
  await page.locator("#voice-builtin-mix-select-tts").selectOption("local:qwen3_tts_1_7b_clone");
  await page.locator("#voice-local-clone-box").waitFor();
  assert.equal(await page.locator("#voice-local-speaker").count(), 0, "克隆模型无内置音色下拉");
  assert.equal(await page.locator("#voice-local-speed").count(), 0, "克隆模型不支持调语速，不显示语速框");
  assert.equal(await page.locator("#voice-local-speaker-browse").count(), 0, "克隆模型不显示查看语言包按钮");
  assert.equal(await page.locator("#voice-local-tts-try").isDisabled(), true, "未上传参考 WAV 前禁用试听按钮");
  await page.locator("#voice-local-clone-file").setInputFiles({
    name: "moss550w.wav",
    mimeType: "audio/wav",
    buffer: Buffer.from("UklGRiQAAABXQVZFZm10IBAAAAABAAEAgD4AAAB9AAACABAAZGF0YQAAAAA=", "base64"),
  });
  await page.locator("#voice-local-clone-summary").filter({ hasText: "moss550w.wav（4.3 秒" }).waitFor();
  await page.locator("#voice-local-clone-notice").filter({ hasText: "已保存参考录音 moss550w.wav" }).waitFor();
  assert.equal(await page.locator("#voice-local-clone-transcript").inputValue(), "让人类永远保持理智，确实是一种奢求。", "上传后自动回填识别台词");
  assert.equal(await page.locator("#voice-local-tts-try").isDisabled(), false, "上传参考 WAV 后解锁试听按钮");
  await page.locator("#voice-local-clone-transcript").fill("延续人类文明是唯一目标。");
  await page.locator("#voice-local-clone-save-transcript").click();
  await page.locator("#voice-local-clone-notice").filter({ hasText: "已更新参考录音台词" }).waitFor();
  await page.locator("#voice-local-tts-try").click();
  await page.waitForFunction(() => !/合成中/.test(document.querySelector("#voice-local-tts-try")?.textContent || ""));
  assert.deepEqual(last("voice_builtin_models_test").args, {
    section: "local_tts",
    input: { model_id: "qwen3_tts_1_7b_clone", speaker: 0, speed: 1, transcript: "延续人类文明是唯一目标。" },
  });
  await page.locator("#voice-builtin-mix-select-tts").selectOption("local:kokoro");

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
  // n67：固件升级与 USB 一键配网在“设备”标签，收起时不挂载（不后台轮询/扫串口），展开后就地显示
  await tab("设备").click();
  assert.equal(calls.filter((call) => call.command === "voice_firmware_status").length, 0, "固件组收起时不查询");
  assert.equal(calls.filter((call) => call.command === "voice_usb_provision").length, 0, "USB 配网组收起时不扫描串口");
  await openGroup("device-firmware");
  await page.locator("#voice-group-device-firmware #voice-firmware-card").waitFor();
  assert.ok(calls.some((call) => call.command === "voice_firmware_status"), "展开后在主窗口直接读取固件状态");
  await tab("记录与诊断").click();
  // 8a. n68：服务事件带级别 / 分类 / 怎么办 / 技术细节，可只看错误和警告，覆盖计数明确提示，可导出诊断包
  await page.locator("#voice-approvals-card").getByText("原因：控制器忙，未开始加工（错误码 controller_busy）").waitFor();
  assert.match(await page.locator("#voice-group-approvals").innerText(), /最近 1 条/);
  await openGroup("builtin-events");
  const eventsCard = page.locator("#voice-builtin-events-card");
  assert.equal(await eventsCard.locator(".voice-event").count(), 5, "全部保留的事件都显示，不再只截 20 条");
  // 每条前面标明来源；旧记录没有 source 时按小智显示
  assert.deepEqual(
    await eventsCard.locator(".voice-event-source").allInnerTexts(),
    ["激光雕刻", "MOSS 软件", "小智", "小智", "小智"],
  );
  assert.match(await page.locator("#voice-events-source-laser").innerText(), /激光雕刻 1 · 问题 1/);
  await page.locator("#voice-events-source-laser").click();
  assert.equal(await eventsCard.locator(".voice-event").count(), 1, "只看激光雕刻");
  assert.match(await eventsCard.locator(".voice-event").innerText(), /激光雕刻[\s\S]*加工失败：wf-9，停在第 88\/400 行[\s\S]*怎么办：图形超出了机器行程/);
  await page.locator("#voice-events-source-moss").click();
  assert.match(await eventsCard.locator(".voice-event").innerText(), /MOSS 软件[\s\S]*本地视觉模型/);
  await page.locator("#voice-events-source-all").click();
  await eventsCard.getByText("有 12 条较早的日常记录已被覆盖（最多保留 300 条）").waitFor();
  await eventsCard.getByText(/本地音乐目录当前不可用/).waitFor();
  const errorEvent = eventsCard.locator('.voice-event[data-level="error"][data-source="xiaozhi"]');
  assert.match(await errorEvent.innerText(), /错误[\s\S]*密钥\/权限[\s\S]*怎么办：密钥填错/);
  await errorEvent.getByText("技术细节").click();
  await errorEvent.getByText("服务方原话：Incorrect API key provided").waitFor();
  assert.match(await eventsCard.locator('.voice-event[data-level="warn"][data-source="xiaozhi"]').innerText(), /怎么办：检查设备供电和 Wi-Fi/, "没有自带建议时用分类的大白话建议");
  await page.locator("#voice-events-only-problems").check();
  assert.equal(await eventsCard.locator(".voice-event").count(), 4, "只看错误和警告（三类来源都算）");
  await page.locator("#voice-events-only-problems").uncheck();
  await page.locator("#voice-diagnostics-export").click();
  await page.locator("#voice-diagnostics-export-result").getByText(/诊断包已保存：moss-voice-diagnostics-1790000000\.json（5 条事件/).waitFor();
  assert.ok(last("voice_diagnostics_export"), "导出走后端命令");
  await openGroup("live-debug-traces");
  assert.match(await page.locator("#voice-turn-timings").innerText(), /第 2 轮 · 没识别出文字，已忽略 · fake-asr · 设备 aa:bb:cc:11:22:33/);
  assert.equal(await page.locator("#voice-live-debug").getByText(/0ms|100% 生效/).count(), 0, "不再出现未测量的 0ms / 100% 说法");
  // 8b. n51 / n81：记录与诊断 -> 持久化对话记录与排障（默认开启、重启保留，支持关键词筛选、划词追加纠偏与确认清空）
  await openGroup("live-debug-traces");
  assert.equal(await page.locator("#voice-debug-trace-switch").isChecked(), true, "语音对话记录默认开启");
  await page.locator("#voice-live-debug").getByText("帮我切椴木书签").waitFor();
  await page.locator("#voice-live-debug").getByText("屏幕已播：").waitFor();
  await page.locator("#voice-chat-log-search").fill("不存在的关键词");
  await page.locator("#voice-live-debug").getByText(/没有找到包含/).waitFor();
  await page.locator("#voice-chat-log-search").fill("");
  await page.locator("#voice-live-debug").getByRole("button", { name: "纠偏此句" }).click();
  await page.locator("#voice-debug-right-word").fill("帮我切东北椴木书签");
  await page.locator("#voice-debug-add-correction").click();
  await page.locator("#voice-live-debug").getByText(/已添加同音纠偏/).waitFor();
  assert.ok(state.builtin.local.settings.custom_asr_corrections.includes("帮我切段木书签=帮我切东北椴木书签"));
  await page.locator("#voice-debug-trace-switch").uncheck();
  assert.equal(state.builtin.debug_enabled, false, "关闭开关暂停记录新对话");
  assert.equal(state.builtin.debug_traces.length, 1, "关闭开关不误删已落盘历史记录");

  // 8c. n51 + n52（n69 起在“设备”标签）：在线小智硬件快捷控制（320×240 彩屏识别、一键静音记忆 0%、投送雕刻预览、一键切换长按说话抗噪模式）
  await tab("服务").click();
  assert.ok(await page.locator("#voice-panel-service #voice-builtin-card").isVisible(), "服务标签只放内置服务启停与连接地址");
  assert.ok(await page.locator("#voice-group-builtin-service").isVisible());
  assert.equal(await page.locator("#voice-panel-service #voice-builtin-devices, #voice-panel-service #voice-goggle-bot-card").count(), 0, "设备表与副屏预览不在服务标签");
  // n71：常连模式开关在服务标签，默认关；打开后只提交本组三个字段
  const mqttGroup = page.locator("#voice-group-settings-builtinMqtt");
  await mqttGroup.locator(".voice-group-h").click();
  const mqttSwitch = page.locator("#voice-builtin-mqtt-enabled");
  await mqttSwitch.waitFor();
  assert.equal(await mqttSwitch.isChecked(), false, "常连模式默认关");
  await mqttSwitch.check();
  await page.locator("#voice-settings-save-builtinMqtt").click();
  await mqttGroup.getByText(/小智要再重启一次/).waitFor();
  assert.deepEqual(last("voice_save_settings").args.settings, { builtin_mqtt_enabled: true, builtin_mqtt_port: 1883, builtin_udp_port: 8888 });
  // 服务页“USB 一键配网写地址”无弹窗跳转：自动切到“设备”标签、展开 USB 折叠栏、完成读取与内联两步确认写入
  assert.equal(await page.locator("#voice-devices-usb-provision").count(), 0, "已连接设备内不再重复放 USB 配网按钮");
  await page.locator("#voice-builtin-usb-provision").click();
  await page.locator("#voice-page-tab-devices[aria-selected=true]").waitFor();
  await page.locator("#xiaozhi-usb-card").waitFor();
  assert.equal(await page.locator('[role="dialog"], [role="alertdialog"], .modal-backdrop').count(), 0, "USB 配网卡片无模态弹窗");
  const usbTheme = await page.evaluate(() => {
    const card = document.querySelector("#xiaozhi-usb-card");
    const step1 = card?.querySelector(".xiaozhi-usb-serial-bar");
    const body = card?.parentElement;
    const bodyStyle = body ? getComputedStyle(body) : null;
    const innerW = body && bodyStyle
      ? body.clientWidth - parseFloat(bodyStyle.paddingLeft) - parseFloat(bodyStyle.paddingRight)
      : 0;
    return {
      cardBg: card ? getComputedStyle(card).backgroundColor : "",
      step1Bg: step1 ? getComputedStyle(step1).backgroundColor : "",
      cardW: card ? Math.round(card.getBoundingClientRect().width) : 0,
      innerW: Math.round(innerW),
    };
  });
  assert.notEqual(usbTheme.cardBg, "rgb(255, 255, 255)", "USB 配网卡片不用突兀白底，融入深色背景");
  assert.notEqual(usbTheme.step1Bg, "rgb(245, 247, 250)", "步骤条使用 MOSS 曜石暗色半透明表面");
  assert.ok(Math.abs(usbTheme.cardW - usbTheme.innerW) <= 4, `USB 配网卡片铺满分组宽度与上方卡片对齐 (${usbTheme.cardW} vs ${usbTheme.innerW})`);
  await page.waitForFunction(() => document.querySelector("#xiaozhi-usb-port-select")?.value === "/dev/cu.usbmodem14201");
  await page.waitForFunction(() => document.activeElement?.id === "xiaozhi-usb-port-select");
  const deviceOrder = await page.locator("#voice-devices-panel > .voice-group").evaluateAll((els) => els.map((el) => el.id));
  assert.deepEqual(deviceOrder, ["voice-group-builtin-devices", "voice-group-wake-word", "voice-group-screen-preview", "voice-group-device-firmware", "voice-group-device-usb-provision"], "设备标签：已连接设备 → 唤醒词 → 副屏预览 → 固件升级 → USB 一键配网写地址");
  await page.locator("#xiaozhi-usb-inspect").click();
  await page.locator("#xiaozhi-usb-card .xiaozhi-usb-board-chip").getByText("esp32s3").waitFor();
  await page.locator("#xiaozhi-usb-card").getByText("HomeNet").waitFor();
  // 板子里存的 OTA 地址已不是 MOSS 当前地址：提醒且默认不替换，点按钮才换成当前地址
  const staleOta = page.locator("#xiaozhi-ota-stale");
  await staleOta.getByText("http://192.0.2.99:8003/xiaozhi/ota/").waitFor();
  assert.equal(await page.locator("#xiaozhi-usb-card .xz-ota-preview code").innerText(), "http://192.0.2.99:8003/xiaozhi/ota/", "默认保留板子原来的地址");
  await staleOta.getByRole("button", { name: "换成 MOSS 当前地址" }).click();
  await staleOta.waitFor({ state: "detached" });
  assert.equal(await page.locator("#xiaozhi-usb-card .xz-ota-preview code").innerText(), state.builtin.ota_url);
  if (process.env.VOICE_QA_DIR) {
    await page.locator("#xiaozhi-usb-card").scrollIntoViewIfNeeded();
    await page.screenshot({ path: path.join(process.env.VOICE_QA_DIR, "voice-usb-inline-inspected.png") });
  }
  // 删除一条已存 Wi-Fi 再切换串口，应连带清空上一块板子的已保存列表与“恢复读取前顺序”
  await page.getByRole("button", { name: "删除 HomeNet" }).click();
  await page.locator("#xiaozhi-usb-card").getByRole("button", { name: "恢复读取前顺序" }).waitFor();
  await page.locator("#xiaozhi-usb-port-select").selectOption("/dev/cu.usbserial-ALT2");
  assert.equal(await page.locator("#xiaozhi-usb-card .xiaozhi-usb-board-chip").count(), 0, "切换串口后清空旧板子芯片信息");
  assert.equal(await page.locator("#xiaozhi-usb-card .xz-saved-item").count(), 0, "切换串口后清空旧板子已存 Wi-Fi 列表");
  assert.equal(await page.locator("#xiaozhi-usb-card").getByRole("button", { name: "恢复读取前顺序" }).count(), 0, "切换串口后不再显示“恢复读取前顺序”死按钮");
  await page.locator("#xiaozhi-usb-port-select").selectOption("/dev/cu.usbmodem14201");
  await page.locator("#xiaozhi-usb-inspect").click();
  await page.locator("#xiaozhi-usb-card .xiaozhi-usb-board-chip").getByText("esp32s3").waitFor();
  assert.equal(await page.locator("#xiaozhi-ota-stale").count(), 0, "板子里存的就是 MOSS 当前地址时不提醒");
  // n103：一键填入电脑当前 Wi-Fi；电脑连的是 5 GHz 时提醒板子只认 2.4 GHz，密码不自动填
  await page.locator("#xiaozhi-wifi-pass").fill("old-pass");
  // 没有定位权限：弹窗给“去系统设置打开 / 手动输入”，先选手动输入，之后再从提示里去设置打开，切回 MOSS 自动重查
  const clientOs = await page.evaluate(() => {
    const p = String(navigator.userAgentData?.platform || navigator.platform || "");
    return /Mac/i.test(p) ? "macos" : /Win/i.test(p) ? "windows" : "other";
  });
  await page.locator("#xiaozhi-wifi-use-current").click();
  if (clientOs === "macos" || clientOs === "windows") {
    const permDialog = page.locator('[data-testid="app-dialog"]');
    await permDialog.getByText("需要定位权限才能读取 Wi-Fi 名称").waitFor();
    await permDialog.getByText(/MOSS 不读取、不保存位置/).waitFor();
    await permDialog.getByRole("button", { name: "手动输入" }).click();
    await permDialog.waitFor({ state: "detached" });
    await page.waitForFunction(() => document.activeElement?.id === "xiaozhi-wifi-ssid");
    await page.locator("#xiaozhi-wifi-notice").getByText(/已改为手动输入/).waitFor();
    assert.deepEqual(state.wifiSettingsOpened, [], "选手动输入不打开系统设置");
    await page.locator("#xiaozhi-wifi-open-settings").click();
    await page.locator("#xiaozhi-wifi-notice").getByText(/已打开系统设置.*切回 MOSS，会自动重新检查/).waitFor();
    assert.deepEqual(state.wifiSettingsOpened, ["location"], "后来改主意可以从提示里去系统设置打开定位权限页");
    await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  } else {
    // 没有可跳转设置页的系统：只给提示并聚焦输入框，用“重新检查”再读
    await page.waitForFunction(() => document.activeElement?.id === "xiaozhi-wifi-ssid");
    state.wifiPermission = "granted";
    await page.locator("#xiaozhi-wifi-recheck").click();
  }
  await page.locator("#xiaozhi-wifi-notice").getByText(/已填入电脑当前连接的“OfficeNet”.*这块板子（esp32s3）只支持 2\.4 GHz/).waitFor();
  assert.equal(await page.locator("#xiaozhi-wifi-ssid").inputValue(), "OfficeNet");
  assert.equal(await page.locator("#xiaozhi-wifi-pass").inputValue(), "", "换了网络名就清掉之前填的密码");
  // 扫描附近：按读到的 ESP32-S3 只显示 2.4 GHz，仅 5 GHz 的网络隐藏，双频网络只标 2.4 GHz 并按 2.4G 信号显示
  await page.locator("#xiaozhi-wifi-scan").click();
  const scanList = page.locator("#xiaozhi-wifi-scan-list");
  await scanList.waitFor();
  assert.deepEqual(await scanList.locator(".xz-scan-ssid").allInnerTexts(), ["CafeFree", "StudioNet", "StrictWpa3"], "只列板子能连的网络，按 2.4G 信号排序");
  await scanList.locator("#xiaozhi-wifi-scan-filter").getByText("按板子 esp32s3 只显示 2.4 GHz，已隐藏 1 个板子连不上的网络").waitFor();
  const wpa3Row = scanList.getByRole("button", { name: /StrictWpa3/ });
  await wpa3Row.getByText("仅 WPA3").waitFor();
  await wpa3Row.click();
  await page.locator("#xiaozhi-wifi-notice").getByText(/仅 WPA3.*WPA2/).waitFor();
  const studio = scanList.getByRole("button", { name: /StudioNet/ });
  assert.equal(await studio.getByText("5 GHz").count(), 0, "2.4G 芯片不显示 5 GHz 标签");
  await studio.getByText("信号 35%").waitFor();
  await scanList.getByRole("button", { name: /StudioNet/ }).click();
  assert.equal(await page.locator("#xiaozhi-wifi-ssid").inputValue(), "StudioNet");
  await page.locator("#xiaozhi-wifi-notice").getByText(/已选“StudioNet”，请填写它的密码/).waitFor();
  // 列表里的“重新扫描”再发一次 wifi_scan，结果刷新后仍可点选
  await scanList.locator("#xiaozhi-wifi-rescan").click();
  await page.waitForFunction(() => document.querySelector("#xiaozhi-wifi-rescan")?.textContent.includes("重新扫描"));
  assert.equal(await scanList.locator(".xz-scan-item").count(), 3);
  assert.equal(calls.filter((c) => c.command === "voice_usb_provision" && c.args?.input?.action === "wifi_scan").length, 2, "重新扫描再查询一次");
  assert.equal(calls.filter((c) => c.command === "voice_usb_provision" && String(c.args?.input?.action).startsWith("wifi_")).length, 4, "每次点击 / 切回重查各只查询一次本机 Wi-Fi");
  // 高级选项列出全部真实局域网 IP 供一键填入
  await page.locator("#xiaozhi-tab-advanced").click();
  await page.locator("#xiaozhi-ota-quick-fills").getByRole("button", { name: "填入 192.0.2.10（首选）" }).waitFor();
  await page.locator("#xiaozhi-ota-quick-fills").getByRole("button", { name: "填入 192.0.2.20" }).click();
  assert.equal(await page.locator("#xiaozhi-ota-url").inputValue(), "http://192.0.2.20:8003/xiaozhi/ota/");
  await page.locator("#xiaozhi-ota-quick-fills").getByRole("button", { name: "填入 192.0.2.10（首选）" }).click();
  await page.locator("#xiaozhi-tab-wifi").click();
  await page.locator("#xiaozhi-wifi-ssid").fill("StudioNet");
  await page.locator("#xiaozhi-wifi-pass").fill("pass-1234");
  const writeCountBefore = calls.filter((c) => c.command === "voice_usb_provision" && c.args?.input?.action === "write").length;
  await page.locator("#xiaozhi-usb-write-wifi").click();
  await page.locator("#xiaozhi-usb-confirm-box").waitFor();
  assert.equal(await page.locator('[role="dialog"], [role="alertdialog"], .modal-backdrop').count(), 0, "写入确认走卡片内联确认条，不弹框");
  assert.equal(calls.filter((c) => c.command === "voice_usb_provision" && c.args?.input?.action === "write").length, writeCountBefore, "展开内联确认前未直接写 Flash");
  await page.locator("#xiaozhi-usb-cancel-write").click();
  assert.equal(await page.locator("#xiaozhi-usb-confirm-box").count(), 0, "点击内联取消按钮可撤回待确认状态");
  await page.locator("#xiaozhi-usb-write-wifi").click();
  await page.locator("#xiaozhi-usb-confirm-box").waitFor();
  await page.locator("#xiaozhi-wifi-ssid").fill("StudioNet5G");
  assert.equal(await page.locator("#xiaozhi-usb-confirm-box").count(), 0, "修改表单字段自动撤销待确认状态");
  await page.locator("#xiaozhi-usb-write-wifi").click();
  await page.locator("#xiaozhi-usb-confirm-write").click();
  await page.locator('#xiaozhi-usb-card [role="status"]').getByText(/已写入 NVS 并回读校验无误，小智板子正在重启/).waitFor();
  // 内置服务未启动时，卡片顶部和写后报到栏均提醒并支持一键启动；启动后进入等待报到，板子连上 MOSS 后变绿上线
  await page.locator("#xiaozhi-usb-service-stopped").waitFor();
  await page.locator("#xiaozhi-usb-checkin[data-phase=service_stopped]").waitFor();
  await page.locator("#xiaozhi-usb-start-service").click();
  await page.locator("#xiaozhi-usb-service-stopped").waitFor({ state: "detached" });
  await page.locator("#xiaozhi-usb-checkin[data-phase=waiting]").waitFor();
  state.builtin.sessions.push({
    session_id: "s-usb-new",
    device_id: "dc:da:0c:11:22:33",
    ip: "192.0.2.88",
    protocol_version: 1,
    connected_at: 1_790_000_200,
    state: "idle",
  });
  await page.locator("#xiaozhi-usb-checkin[data-phase=online]").waitFor({ timeout: 6000 });
  state.builtin.sessions = state.builtin.sessions.filter((s) => s.session_id !== "s-usb-new");
  state.builtin.running = false;
  const writeArgs = last("voice_usb_provision").args.input;
  assert.equal(writeArgs.action, "write");
  assert.equal(writeArgs.confirmed, true);
  assert.equal(writeArgs.expected_mac, "dc:da:0c:11:22:33");
  assert.equal(writeArgs.patch.new_wifi.ssid, "StudioNet5G");
  assert.equal(await page.locator('[role="dialog"], [role="alertdialog"], .modal-backdrop').count(), 0, "整个 USB 配网写入过程零弹窗");
  // 收起后再直接点开 USB 折叠栏标题，同样原地展开、自动选中串口，并可一键复用上次刚配过的 Wi-Fi（内存会话）
  await page.locator("#voice-group-device-usb-provision > .voice-group-h").click();
  await page.locator("#xiaozhi-usb-card").waitFor({ state: "detached" });
  await page.locator("#voice-group-device-usb-provision > .voice-group-h").click();
  await page.locator("#xiaozhi-usb-card").waitFor();
  await page.waitForFunction(() => document.querySelector("#xiaozhi-usb-port-select")?.value === "/dev/cu.usbmodem14201");
  await page.locator("#xiaozhi-wifi-use-last").click();
  assert.equal(await page.locator("#xiaozhi-wifi-ssid").inputValue(), "StudioNet5G");
  assert.equal(await page.locator("#xiaozhi-wifi-pass").inputValue(), "pass-1234", "复用上次成功写入的 Wi-Fi 与密码");
  await page.locator("#xiaozhi-wifi-ssid").fill("");
  await page.locator("#xiaozhi-wifi-clear-last").click();
  assert.equal(await page.locator("#xiaozhi-wifi-use-last").count(), 0, "清除记忆后不再显示复用按钮");
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
  // 画风在设备 › 副屏预览里选（n73 起对话体验不再有只读画风行）
  await tab("设备").click();
  await page.locator("#voice-page-tab-devices[aria-selected=true]").waitFor();
  await openGroup("screen-preview");
  assert.equal(await page.locator("#voice-group-screen-preview > .voice-group-h").getAttribute("aria-expanded"), "true", "副屏预览组已展开");
  const botCard = page.locator("#voice-goggle-bot-card");
  await botCard.waitFor();
  assert.equal(await botCard.locator(".voice-bot-gif-img").count(), 12, "展示 12 个全屏场景的缩小动图");
  // n70：在线彩屏小智还没刷全屏画风时提示去唤醒词推送
  await botCard.locator("#voice-bot-pack-tip").getByText(/还没刷全屏画风/).waitFor();
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
  // 2026-11-09 自动保持全屏画风：默认关，开启时确认一次；没开常连模式时提示一起打开
  const autoKeep = botCard.locator("#voice-screen-pack-auto-keep");
  assert.equal(await autoKeep.isChecked(), false, "默认关");
  await autoKeep.click();
  await confirmDialog("开启并允许自动推送");
  await page.waitForFunction(() => document.querySelector("#voice-screen-pack-auto-keep")?.checked === true);
  assert.deepEqual(last("voice_local_save").args, { settings: { screen_pack_auto_keep: true } });
  await botCard.locator("#voice-screen-pack-auto-mqtt-tip").getByText(/常连模式/).waitFor();
  await autoKeep.click();
  await page.waitForFunction(() => document.querySelector("#voice-screen-pack-auto-keep")?.checked === false);
  assert.deepEqual(last("voice_local_save").args, { settings: { screen_pack_auto_keep: false } }, "关闭不用确认");
  assert.equal(await botCard.locator("#voice-screen-full-font-tip").count(), 0, "n111：补字库自动加载，副屏预览也不再提示");
  // 8d. n67 / n69：唤醒词卡在“设备”标签就地显示（原“唤醒词与音量”标签已并入）
  const wakeGroup = page.locator("#voice-group-wake-word");
  await wakeGroup.locator("#voice-wakeword-card").waitFor();
  assert.equal(await wakeGroup.locator("#voice-wakeword-text").inputValue(), "你好小智", "唤醒词卡在主窗口就地读取当前唤醒词");
  // 2026-11-09：没刷的彩屏小智一键刷入全屏画风（仍逐台确认），已刷的不显示按钮
  const wakeDevices = wakeGroup.locator(".wakeword-device");
  await wakeDevices.nth(0).getByText(/还没刷全屏画风/).waitFor();
  await wakeDevices.nth(1).getByText(/已刷全屏画风，上电就是全屏画面/).waitFor();
  assert.equal(await wakeGroup.locator('[data-quick-pack="aa:bb:cc:44:55:66"]').count(), 0, "已刷的不再显示一键刷入");
  await wakeDevices.nth(2).getByText(/重刷后开机待命画面才会换/).waitFor();
  assert.equal((await wakeGroup.locator('[data-quick-pack="aa:bb:cc:77:88:99"]').innerText()).trim(), "一键重刷全屏画风", "换了画风要重刷");
  await wakeDevices.nth(3).getByText(/底包或字库与设备当前不一致/).waitFor();
  assert.equal((await wakeGroup.locator('[data-quick-pack="aa:bb:cc:aa:bb:cc"]').innerText()).trim(), "一键更新画风字库", "底包或字库指纹变化可一键更新");
  await wakeDevices.nth(4).getByText("正在读取设备能力…").waitFor();
  await wakeGroup.locator('[data-quick-pack="aa:bb:cc:11:22:33"]').click();
  await confirmDialog("确认推送这一台");
  assert.deepEqual(last("voice_wakeword_manage").args.input, { action: "push", device_id: "aa:bb:cc:11:22:33", target: "default", confirmed: true, screen_pack: true }, "没推过自定义唤醒词时沿用“你好小智”");
  await wakeDevices.nth(0).getByText(/已推送，等设备下载全屏画风/).waitFor();
  assert.equal(await wakeGroup.locator('[data-quick-pack="aa:bb:cc:11:22:33"]').count(), 0, "等下载时不重复推送");
  await shot("voice-devices");
  // 2026-11-23 改显示文字拼音自动跟随；手动改过不覆盖，可按文字重新生成
  const wakeText = wakeGroup.locator("#voice-wakeword-text");
  const wakeCommand = wakeGroup.locator("#voice-wakeword-command");
  await wakeText.fill("你好小乐");
  await page.waitForFunction(() => document.querySelector("#voice-wakeword-command")?.value === "ni hao xiao le");
  assert.equal(await wakeGroup.locator("#voice-wakeword-regenerate").count(), 0, "自动生成时不显示重新生成");
  await wakeCommand.fill("ni hao xiao yue");
  await wakeText.fill("你好小乐乐");
  assert.equal(await wakeCommand.inputValue(), "ni hao xiao yue", "手动改过的拼音不被覆盖");
  await wakeGroup.locator("#voice-wakeword-regenerate").click();
  assert.equal(await wakeCommand.inputValue(), "ni hao xiao le le");
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

  // 10. 关闭主窗口时的退出选择（n102 起没有“同时停止语音服务端”）
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.evaluate(() => {
    localStorage.removeItem("moss_close_action");
    window.dispatchEvent(new Event("moss:close-requested"));
  });
  const closeDialog = page.getByRole("dialog", { name: "关闭主窗口" });
  await closeDialog.waitFor();
  assert.doesNotMatch(await closeDialog.innerText(), /语音服务端/, "退出选择里不再有停止语音服务端");
  await shot("voice-close-exit");

  assert.deepEqual(errors, [], "不应有前端未处理异常");
  console.log(`VOICE_CENTER_BROWSER_TEST=PASS calls=${calls.length}`);
  await context.close();
} finally {
  if (browser) await browser.close();
  await vite.close();
}
