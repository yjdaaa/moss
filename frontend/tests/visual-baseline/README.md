# 前端视觉基线（UI-2）

改版后各页面的参考截图，用来判断之后的改动有没有意外改变外观。对应技术书 §9.5.5、实施清单 UI-08。

| 文件 | 内容 | 视口 |
| --- | --- | --- |
| `workbench.png` | 雕刻工作台三栏 | 1440 × 900 |
| `workbench-narrow.png` | 雕刻工作台窄屏（导航折叠为顶部横排） | 820 × 900 |
| `draw.png` | 创作画板（左画布 + 右三卡） | 1440 × 900 |
| `material-lab.png` | 材料实验室（材料库表格） | 1440 × 900 |
| `settings-modal.png` | 偏好设置弹窗（分区默认收起） | 1440 × 900 |
| `approval-card.png` | 全局审批卡（未勾选现场确认，“批准并开始”不可点） | 1440 × 900 |
| `approval-mismatch.png` | 审批卡显示 `approval_mismatch` 原因，按钮收起为“关闭” | 1440 × 900 |
| `approval-sending.png` | 批准后的发送覆盖层（四步 + 取消后续发送 + 急停 0x18） | 1440 × 900 |
| `voice-official.png` | 语音中心 · 官方链路 · 服务与设备（其他链路折叠） | 1440 × 1000 |
| `voice-mode-confirm.png` | 切换到自建链路前的确认框 | 1440 × 1000 |
| `voice-self-hosted.png` | 自建链路 · 服务端运行中（停止 / 重启） | 1440 × 1000 |
| `voice-crashed.png` | 自建服务端意外退出（退出码、不自动重启） | 1440 × 1000 |
| `voice-builtin.png` | 内置链路 · 模型标签：本地模型“未保存”、语音体验保存结果 | 1440 × 1000 |
| `voice-narrow.png` | 窄屏 · 连接标签（各组摘要、官方接入点保存结果） | 820 × 1000 |
| `voice-close-exit.png` | 关闭窗口时“同时停止语音服务端”（2026-10-07，未随 UI-3 更新） | 1440 × 900 |

## 生成方式与局限

- 语音中心 `voice-*.png`（除 `voice-close-exit.png`）于 2026-10-14 随 UI-3 重新生成：`cd frontend; node tests/voice-center.browser.mjs --baseline`（Vite 回环 + 有状态假 Tauri 桥，Edge 无头渲染）。

- 截图于 2026-10-06 生成：用 esbuild 打包 `frontend/src`，在 Chromium（SwiftShader WebGL）中渲染。
- 后端接口使用**预览桩数据**（固定的材料列表、端口 9666、小智“已配置未连接”），不是真实设备状态。
- 创作画板中间的 Excalidraw 在截图里是占位块，真机上是真实画布。
- 背景是动画，每次截图的等离子图案不同，对比时只看版面、卡片、按钮与文字，不比背景像素。
- 目前没有自动比对脚本；改版后人工对照。需要自动化时再加入像素比对（需排除背景层）。
