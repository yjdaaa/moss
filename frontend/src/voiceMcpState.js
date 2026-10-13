/** NV9 desktop-only form logic. Password fields are ephemeral and never prefilled. */
export const MCP_POLICIES = { disabled: "禁用", ask: "每次需批准", allow: "免批准（仅可信只读）" };
export function mcpForm(record) {
  const config = record?.config;
  const transport = config?.transport || {};
  const refs = transport.type === "stdio" ? transport.env_refs : transport.header_refs;
  return {
    name: config?.name || "", enabled: Boolean(config?.enabled), type: transport.type || "streamable_http",
    url: transport.url || "", command: transport.command || "", args: JSON.stringify(transport.args || [], null, 2),
    policies: { ...(config?.policies || {}) },
    credentials: Object.entries(refs || {}).map(([key, reference]) => ({ key, reference, value: "", saved: Boolean(record?.secret_set?.[reference]) })),
  };
}
export function mcpFormError(form) {
  if (!/^[A-Za-z0-9_-]{1,32}$/.test(form.name) || form.name.includes("__") || form.name.toLowerCase() === "device") return "服务名须为 1–32 位英文、数字、横线或下划线，不能含双下划线或使用 device";
  if (!["stdio", "streamable_http"].includes(form.type)) return "请选择支持的连接方式";
  if (form.credentials.length > 32) return "凭据最多 32 项";
  const keys = new Set();
  for (const item of form.credentials) {
    const key = item.key.trim();
    if (!key || key.length > 128 || keys.has(key)) return "凭据名称不能为空或重复";
    keys.add(key);
    if (form.type === "stdio" && !/^[A-Za-z0-9_]+$/.test(key)) return "环境变量名称只能使用英文、数字和下划线";
    if (form.type !== "stdio" && (!/^[a-z0-9!#$%&'*+.^_`|~-]+$/.test(key) || /^(mcp-|sec-)/.test(key) || ["host", "content-type", "content-length", "accept", "connection", "transfer-encoding"].includes(key))) return "请求头名称须小写，且不能覆盖协议字段";
    if (item.value.includes("\0") || new TextEncoder().encode(item.value).length > 16384) return "单项凭据超过限制或包含无效字符";
    if (form.enabled && !item.saved && !item.value) return "启用前请填写凭据；留空只保留原有值";
  }
  if (form.type === "stdio") {
    if (!form.command.trim() || /[\0\r\n]/.test(form.command) || form.command.length > 1024) return "请填写单个程序路径，不要在这里拼接命令和参数";
    try {
      const args = JSON.parse(form.args);
      if (!Array.isArray(args) || args.length > 64 || args.some((arg) => typeof arg !== "string" || arg.includes("\0") || new TextEncoder().encode(arg).length > 8192)) return "参数须为最多 64 项的 JSON 字符串数组";
    } catch { return "参数必须是 JSON 数组，例如 [\"server.js\"]"; }
  } else {
    try {
      const url = new URL(form.url);
      if (!["http:", "https:"].includes(url.protocol) || url.username || url.password || url.search || url.hash || form.url.length > 2048) return "地址须为 HTTP(S)，不能带用户名、密码、查询参数或片段";
      const loopback = url.hostname === "localhost" || url.hostname === "[::1]" || /^127\./.test(url.hostname);
      if (form.credentials.length && url.protocol !== "https:" && !loopback) return "非本机服务携带凭据时必须使用 HTTPS";
    } catch { return "请填写完整的 MCP HTTP(S) 地址"; }
  }
  if (Object.values(form.policies).some((p) => !Object.hasOwn(MCP_POLICIES, p))) return "工具权限无效";
  return "";
}
export function mcpSavePayload(form, revision) {
  const problem = mcpFormError(form);
  if (problem) throw new Error(problem);
  const kind = form.type === "stdio" ? "env" : "header";
  const entries = form.credentials.map((item) => {
    const key = item.key.trim();
    const prefix = `voice.mcp.${form.name}.${kind}.${key}`;
    const reference = item.reference === prefix || item.reference?.startsWith(`${prefix}.v-`) ? item.reference : prefix;
    return [key, reference, item.value];
  });
  const refs = Object.fromEntries(entries.map(([key, reference]) => [key, reference]));
  const transport = form.type === "stdio" ? { type: form.type, command: form.command.trim(), args: JSON.parse(form.args), env_refs: refs } : { type: form.type, url: form.url.trim(), header_refs: refs };
  return {
    expected_revision: revision,
    config: { name: form.name, enabled: form.enabled, transport, policies: { ...form.policies } },
    secrets: Object.fromEntries(entries.filter(([, , value]) => value !== "").map(([, reference, value]) => [reference, value])),
    confirmed: true,
    allow_without_approval: Object.values(form.policies).includes("allow"),
  };
}
export function mcpConnectionText(view) {
  const labels = { connected: "已连接", connecting: "正在连接", reconnecting: "等待重连", disabled: "已停用" };
  return labels[view?.state] || "尚未连接";
}
export function mcpConfirmation(form) {
  const action = !form.enabled ? "保存后保持停用。" : form.type === "stdio" ? "保存后会在你的电脑上启动这个程序，它拥有当前用户的文件和网络权限。" : "保存后会连接这个第三方服务。";
  const allow = Object.values(form.policies).includes("allow") ? "\n存在免批准工具：请确认这些工具可信且只读，服务自己的只读声明不算安全保证。" : "";
  return `${action}\n凭据只存本机凭据库，不回显。第三方工具只接入内置语音；默认每次执行前都需桌面批准。修改后旧审批失效。${allow}`;
}
