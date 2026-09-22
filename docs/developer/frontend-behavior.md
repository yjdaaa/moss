# 前端行为契约

从已归档的 [旧前端盘点](../archive/frontend-inventory.md) 中抽取仍然有效、且有代码或测试固化的行为，按当前 `frontend/src` 核对。改这些行为前先改本文与对应测试。

## 1. 路由与 API 基址

| 路径 | 页面 | 主要调用 |
| --- | --- | --- |
| `/` | `HomePage` | `/api/ui/runtime-config`、`/api/ui/serial-ports`、`/api/ui/discover-controllers`、`/api/ui/material-options`、`/api/connection/check`、`/api/text-task/generate`、`/api/upload`、`/api/workflow`、`/api/system/info` |
| `/draw` | `DrawPage` | `/api/draw/preview`、`/api/connection/check`、`/api/device/framing`、`/api/workflow` |
| `/material-lab` | `MaterialLabPage` | `/v1/material-lab/bootstrap`（首屏一次）、`/api/material-lab/manage` / `recent` / `export` / `preview` / `send` / `status` / `select-cell` |

路由在 `main.jsx` 按 `pathname` 前缀切换，`popstate` 同步。API 基址由 `authSession.getApiBaseUrl` 决定：在 `127.0.0.1` 或 `localhost` 且非 Vite 端口（5173/5177/5178）时同源；在 Tauri 环境或 Vite 端口下固定为 `http://127.0.0.1:9666`。所有请求经 `apiFetch` 附加 Bearer token，收到 401 时派发 `laser-auth-required` 事件弹出配对窗口。

## 2. 状态词表与归一化

实现：`src/state/statusNormalize.js`；测试：`tests/statusNormalize.test.mjs`。

- workflow 状态：`collecting`、`preview_ready`、`sending`、`completed`、`failed`、`cancelled`、`feedback_recorded`。
- job 状态：`pending`、`running`、`completed`、`failed`、`cancelled`。
- 别名归一化：`queued → pending`，`canceled → cancelled`；空值归一为 `pending`。
- 终态集合：`completed`、`failed`、`cancelled`。

发送覆盖层映射（`STAGE_MAP`，必须原样保留）：

| 状态 | 步骤 | 中心文字 | 标题 |
| --- | --- | --- | --- |
| pending / queued | 0 | 等待 / 排队 | 等待任务 |
| preparing | 1 | 准备 | 准备发送 |
| running / sending | 2 | 发送中 | 正在向设备发送 |
| completed | 3 | 完成 | 发送完成 |
| failed | 2 | 失败 | 发送失败 |
| cancelled | 2 | 已取消 | 任务已取消 |

`extractWorkflowInfo` 从 `result.workflow` 或顶层取 `status`，从 `runtime_job` 取 `job_id`，并把 `speech` / `result` 作为播报文本。

## 3. 发送进度与轮询

- 确认发送前先调用 `/api/connection/check`，再调用 `/api/workflow` 的 `confirm_send` 并携带 `confirmed:true`、`workflow_id` 与连接参数；客户端不提交快照 SHA，由服务端重算。
- 进度通过轮询 `/api/workflow {action:"status"}` 获得，显示 `send_result.lines_sent / total_lines`、`target` 与 `phase`；到达终态停止轮询。连续 5 次查询失败或网络异常也停止轮询并把覆盖层置为 `failed`。
- `completed` 时若 `send_result.sd_hash_verified === false` 或带 `warning`，额外输出警告日志。
- 主页支持 `action:"resume"` 从指定行续传（HomePage 内存态路径）。

## 4. 画板重复发送防呆

实现：`src/repeatSendGuard.js`；测试：`tests/repeatSendGuard.test.mjs`。

- 预览成功后记录响应中的 64 位十六进制 `content_fingerprint`。
- 当前指纹与上次已接受指纹相同时，确认发送必须勾选"已移动材料/原点"确认框，否则 `canSubmitConfirmSend` 返回 false。
- 上次已接受指纹保存在 `localStorage` 键 `laserLab.repeatSend.v1.lastAcceptedFingerprint`。这只是浏览器侧提醒，不替代后端 `confirmed=true`、材料冻结与 SHA 门禁。

## 5. `localStorage` 键

| 键 | 页面 | 内容 |
| --- | --- | --- |
| `laser-forge-conn-mode`、`laser-forge-serial-port`、`laser-forge-baud`、`laser-forge-host`、`laser-forge-telnet-port` | 主页 | 连接方式与参数 |
| `laser-forge-mat`、`laser-forge-thick`、`laser-forge-text`、`laser-forge-font-size` | 主页 | 文字任务表单 |
| `laserLab.material`、`laserLab.thickness_mm`、`laserLab.width_mm`、`laserLab.height_mm`、`laserLab.mode`、`laserLab.network_host`、`laserLab.network_telnet_port`、`laserLab.vector_simplify_factor` | 画板 | 画板表单 |
| `laserLab.authToken` | 全局 | 局域网配对 token |
| `laserLab.repeatSend.v1.lastAcceptedFingerprint` | 画板 | 重复发送防呆 |
| `moss_close_action` | 全局 | 桌面端关闭行为（`ask` / `tray` / `exit`） |

## 6. 其他必须保留的行为

- 主页文字输入上限 24 字（`maxLength={24}`）。
- 串口下拉只显示 `/api/ui/serial-ports` 返回的真实串口，无结果时提示并允许手动输入；不再提供硬编码的 COM 预设。
- 材料下拉来自 `/api/ui/material-options`，只暴露材料名与厚度，不暴露功率/速度。
- 材料实验室保存成功后通过 `BroadcastChannel`（`laser-material-library-v1`）通知其他已打开页面刷新。
- 多厚度保存由后端单次 `/api/material-lab/manage` 原子完成，前端不循环发送 N 次请求。
- Excalidraw 主题变量覆盖 `--color-primary`（`styles.css`），语言 `zh-CN`。
- 桌面端关闭窗口时先弹出"最小化到托盘 / 退出"选择；选择可被记住到 `moss_close_action`。非 Tauri 环境下"退出"只关闭页面并显示会话终止提示。
- 视觉基线：`tests/visual-baseline/` 保存三个页面桌面与移动视口截图，用于人工核对布局，不做像素比对。
