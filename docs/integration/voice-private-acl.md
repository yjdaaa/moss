# 语音中心敏感 ACL 与本地安全窗口

状态：2026-10-11，软件修复完成待本机安装版 GUI 验证。

本轮新增 `voice_speaker_manage`（声纹登记、元数据/取消/CAS删除）和 `voice_firmware_status`（只读下载记录），同样只授予 `voice-memory.json` 的 `voice-private` 固定打包窗口；两条命令都复核原始窗口URL，Android拒绝。普通回环页面不能读取姓名、向量或交付记录；声纹向量在所有返回值中均不序列化。固件传输和声纹识别均保持关闭。

## 原因

MOSS 桌面主窗口在 `apps/desktop/src-tauri/src/lib.rs` 的 setup 中执行
`window.navigate(http://127.0.0.1:<实际端口>/)`，解决原先的同源访问问题，
但该窗口此时是 Tauri **remote origin**，而非 `tauri.localhost` 打包资产。
`voice-memory.json` 和 `voice-plugins.json` 原为 `windows:["main"]`
且没有 `remote`；因此 `voice_plugins_status` 与 `voice_memory_list`
在远程主窗口中被 Tauri ACL 拒绝。它们的返回值可能包含私人摘要、已保存的
插件配置/密钥存在状态，绝不能直接在 `default.json` 或
`remote.urls=["http://127.0.0.1:*"]` 中追加这六个敏感命令。

## 变更及安全边界

1. 保留主窗口原有 backend loopback navigation 与黑屏修复，不调整 server CORS 或给后端增加私密 REST API。
2. 使用非敏感 `open_voice_private` 从主窗口打开新建 `voice-private` WebviewWindow，其实际来源必须是安装包中的 `WebviewUrl::App("voice-private.html")`。非 `main`、非当前后端同源或非打包主页面的调用均拒绝。再次点击复用已有安全窗口，不新建无限多窗口。
3. `frontend/vite.config.mjs` 将 `index.html` 与 `voice-private.html` 同时作为独立入口，后者只渲染 VoicePluginsCard 与 VoiceMemoryCard 并以 `voice_builtin_status` 读取设备列表。默认主窗口不再挂载这两张会调用敏感命令的卡片，仅展示“打开本地安全设置窗口”按钮。
4. `voice-plugins.json` 与 `voice-memory.json` 保留全部原有权限和“无 remote”限制，只把授权窗口标签从旧的 `main` 切换到唯一 `voice-private`。**敏感权限没有加入**包含 `windows:["*"]`、`remote.urls` 的 `default.json`；那里仅新增不返回数据的打开窗口命令。Tauri ACL 仍是第一层门禁。
5. 六个敏感 Tauri 处理函数再验证 `WebviewWindow.label()=="voice-private"`，完整来源只能是 `tauri://localhost/voice-private.html` 或 `http(s)://tauri.localhost/voice-private.html`（无额外端口/查询参数/片段）；错误返回固定 `voice_acl_denied`，不读数据库。安全窗口同时阻断跳转到任意外部/loopback 页面，关闭安全窗口时真正销毁，不走主窗口的激光任务关闭拦截。
6. 移动端不创建该窗口，敏感命令仍 `desktop_only`。只查询插件密钥是否已设置，绝不回显实际值；私人记忆只能在该本地可信窗口展示和编辑。没有对普通浏览器、局域网或小智设备开放访问。

### 2026-10-11 NV15 Home Assistant 敏感设置隔离

- 新增 `VoiceHomeCard` 到本地 `voice-private.html`；主窗口不再渲染 Home 设置。`voice-home.json` 只授权 `voice-private` 且没有 remote/wildcard，四项 `voice_home_status/save/token_save/discover` 全部通过 native `require_voice_private_asset` 检查，防回环 HTTP 页面直接使用敏感指令。
- HA 令牌使用 Windows DPAPI，普通设置文件不含令牌；Home 状态只回传令牌存在标志与修订，不回传原文。主页按钮仅打开已存在的本地安全窗口。
- 前端 `voicePrivacyAcl.test.mjs` 新增 NV15 ACL 测试。仍需真实 Windows WebView2 手动验收，没有扩大远程主窗口权限。

### 2026-10-11 NV15-1 历史备份风险审计权限

- 新增 `voice_home_backup_audit`，权限仅在无 `remote` 的 `voice-home.json` 授予 `voice-private`；后端强制校验打包资源 URL，普通 HTTP 主窗口无权调用。
- 仅在明确点击后，Rust 原生系统文件选择器选单个文件；命令不接受路径参数，也不把路径交给前端。只读有界分类返回固定风险类别、字节数和 `safe_to_share=false`，未知始终视为敏感；不能用于验证历史备份已完全脱敏，不读取真实文件进行自动化验收。
- Windows 实际文件选择、取消、拒绝链接和 GUI 错误呈现仍需用户使用安装版手工确认。

### 2026-10-14 NV19-08 长期记忆

- 新增 `capabilities/voice-context.json`，只授予 `voice-private`、没有 `remote` 与 `webviews`；两条命令 `voice_context_manage`、`voice_context_pick_folder` 开头都调用 `require_voice_private_asset`。资料文件夹只能经原生对话框选择，路径不回传前端。
- `VoiceContextCard` 只挂在 `voicePrivate.jsx`，主窗口 `/voice` 页面不引用它，也不调用这两条命令（`voicePrivacyAcl.test.mjs` 与 `moss-desktop` 的 `voice_context_permissions_are_packaged_private_window_only` 检查）。主窗口的 `voice_save_settings` 不接受 `builtin_context`，不能打开检索 / 提取或更换当前项目（`moss-voice` center 测试）。
- 仍需在安装版的真实 WebView2 中点击验收（HW-17）。

## 验证

- Rust `moss-desktop` ACL 检查：remote/wildcard/default 中没有敏感权限，专用 capability 限定 `voice-private` 且缺省不启用 remote；敏感函数额外校验页面来源，恶意主窗口、其他 HTTP、伪造端口、路径与 query/fragment 均拒绝。
- 前端 `voicePrivacyAcl.test.mjs`：确认主窗口不加载敏感卡片、打包独立入口存在、所有六项 Tauri 命令都经过 Rust guard、原有 `window.navigate(parsed)` 仍保留。
- Vite 输出真实 `dist/voice-private.html` 与带哈希的 JS，`dist/index.html` 仍存在；需通过 `npm run build` 复验。
- 全量已验收：桌面 Rust 23/23、Rust workspace 630/630、前端 106/106（新增 3 项 ACL 回归）、FakeGrbl E2E 32/32；Windows release EXE / 普通 NSIS exit 0，Android aarch64 APK exit 0，APK 内原生 `.so` 与本轮编译库 SHA-256 一致。具体文件体积与 SHA-256 见 `CHANGELOG.md` 同日 ACL 修复条目；普通 NSIS 仍非 NV18 专用 sidecar 包。
- **最后需要实际 Windows WebView2 GUI 点击验收**：打开当轮新 `target/release/moss-desktop.exe` → 语音中心 → 内置服务 → “打开本地安全设置窗口”，确认新窗口中的插件配置与私人记忆列表正常加载或正常为空且没有 `not allowed by ACL`；关闭新窗口，确认主窗口仍显示且没有黑屏。用户无需输入任何真实密钥、保存/清空记忆或连接设备。没有执行这次手动验收之前只标 `software tested / GUI unverified`。

关联：`apps/desktop/src-tauri/{build.rs,src/lib.rs,src/voice_builtin.rs,capabilities/{default,voice-memory,voice-plugins}.json}`，
`frontend/{vite.config.mjs,voice-private.html,src/voicePrivate.jsx,src/voiceApi.js,src/components/VoiceBuiltinCard.jsx,tests/voicePrivacyAcl.test.mjs}`。
