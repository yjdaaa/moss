# MOSS 项目上下文

> Claude Code 读取本文件；共享安全、开发、记录和验证规则统一维护在
> [`AGENTS.md`](./AGENTS.md)。本文件不复制共享规则，避免两份规范漂移。

@AGENTS.md

## 项目定位

MOSS 是小智 AI 生态中枢与激光数控雕刻上位机的 Rust 重构版：

- 后端为 Rust workspace，包含 `laser-core`、`laser-device`、`laser-runtime`、`laser-tools`、`laser-mcp`、`laser-api`、`laser-host` 等 crate。
- `apps/desktop/` 是 Tauri 2 桌面/Android 外壳；`apps/moss-host/` 是 HTTP/WS 宿主。
- `frontend/` 使用 React + Vite，负责数控画板、材料实验、进度和遥测交互。
- 下位机主要面向 FluidNC/GRBL 与 ESP32。涉及真实硬件时，以 `docs/developer/safety.md` 和相关 `docs/laser/` 文档为准。

不要仅凭本文件推断当前架构；改动前以代码、测试和对应文档为准。

## 常用命令

各项独立执行，先进入表中相对于仓库根目录的工作目录。

| 工作目录 | 检查或构建 | 命令 |
| --- | --- | --- |
| 仓库根目录 | Rust 测试 | `cargo test --workspace` |
| `frontend` | 前端测试 | `npm test` |
| `frontend` | 前端构建 | `npm run build` |
| `frontend` | E2E | `npm run test:e2e` |
| `apps/desktop/src-tauri` | Windows App | `cargo tauri build --debug` |
| `apps/desktop/src-tauri` | Android APK | `cargo tauri android build --debug --target aarch64 --apk` |

E2E 需要先构建 `moss-host`，并使用本地回环 GRBL 模拟器。Android 构建还依赖本机 Android/Gradle 环境；未满足条件时如实报告。

## 变更记录

`CHANGELOG.md` 是修复与功能记录的统一文档，不以聊天总结代替。
记录字段、测试与双端重建要求、例外及完成判定统一遵循 `AGENTS.md`
的“验证”“变更记录”和“完成标准”，不在此另设一套规则。

## 文档维护

- 指令应短、可执行、可验证；不要重复 README、代码注释或自动可发现的目录清单。
- 规则改变时同时检查 `AGENTS.md`，避免在两个文件中维护同一条约束。
- 只有稳定且对代理决策有帮助的事实才写入本文件；临时状态放在 issue、CHANGELOG 或专门文档中。
