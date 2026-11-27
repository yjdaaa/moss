/**
 * 语音中心页的纯逻辑（技术书 §9.4 / §9.5.1，实施清单 P4-08）：不依赖 React，便于单元测试。
 * 页面通过 Tauri 命令与桌面端通信（见 voiceApi.js）；浏览器、局域网、手机端没有语音中心。
 */

// 按职责拆分到 voiceState/ 目录下的各模块，这里统一转出，保持原有导入路径不变。
export * from "./voiceState/sections.js";
export * from "./voiceState/diagnostics.js";
export * from "./voiceState/devices.js";
export * from "./voiceState/experience.js";
export * from "./voiceState/localModels.js";
export * from "./voiceState/settings.js";
export * from "./voiceState/jev.js";
export * from "./voiceState/modelSources.js";
export * from "./voiceState/pipelineTest.js";
export * from "./voiceState/agent.js";
export * from "./voiceState/twin.js";
export * from "./voiceState/asrCorpus.js";
export * from "./voiceState/aiBench.js";
export * from "./voiceState/benchRecordings.js";
