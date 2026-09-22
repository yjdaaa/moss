# 历史归档

本目录保存 Python 版到 Rust 版重构期间产生的规划、进度与验收文档。它们记录了当时的决定和证据，对追溯"为什么这样设计"有价值，但：

- 不再维护，内容截止于文件头部标注的日期。
- 不作为当前架构、接口或测试数字的依据。当前状态以代码、测试、`CHANGELOG.md` 和 [`docs/developer/`](../developer/) 为准。
- 已删除其中的本机私有路径；文中提到的 Python 模块、`migration/` 脚本、`venv` 与 `tests/test_*.py` 均已从仓库移除。

| 文件 | 日期 | 内容 | 归档原因 |
| --- | --- | --- | --- |
| [rust-rebuild-spec.md](rust-rebuild-spec.md) | 2026-09-14，决定表补至 09-17 | 重构的需求、技术方案与决定记录 D1~D12 | 第一部分需求与 §22 决定仍是设计依据；第二部分技术方案（tokio/axum、目录树、依赖表、`MOSS_RUNTIME_DIR`、`host_key`）未按稿实现，见 [架构总览](../developer/architecture.md) 的偏差清单 |
| [MIGRATION_STATUS.md](MIGRATION_STATUS.md) | 2026-09-19 | 阶段 0~12 的复选清单、状态矩阵与差异记录 | 全部阶段已完成；测试数与产物大小为当时快照；后续变更由 `CHANGELOG.md` 承载 |
| [ACCEPTANCE_REPORT.md](ACCEPTANCE_REPORT.md) | 2026-09-17，09-19 修订 | 全量验收结论与阶段表 | 其中"41 项 E2E 全部通过"已被 `CHANGELOG.md` 的 E2E 清理条目修正（11 项虚假用例已删除）；§4 清理清单已执行 |
| [APP_BUTTONS_TEST_REPORT.md](APP_BUTTONS_TEST_REPORT.md) | 2026-09-18 | 按钮 E2E 逐项结果、修复的缺陷、硬件链路接线对照 | §2 中 BTN-11/21/22/23/25/27/32/34/36/37 已被认定为虚假用例并删除；§3、§6、§7 的缺陷根因记录仍只存于此 |
| [frontend-inventory.md](frontend-inventory.md) | 2026-09-14 | 对旧 Python 版三套前端的逐行盘点与重构须保留行为 | 被盘点的源文件已删除；仍有效的状态映射与保留行为已抽取到 [前端行为契约](../developer/frontend-behavior.md) |
| [voice-image-gcode-engine.md](voice-image-gcode-engine.md) | 2026-07 | Python 版 `ai_laser_gcode_tool` 六个 action 的引擎与 summary 契约 | Rust 版该工具仅接受文字素材，图片由网页或桌面端上传后走 workflow；若重做图片引擎可参考 |
