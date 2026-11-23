# Claude Code 入口

项目说明与全部规则在 `AGENTS.md`（所有 AI 工具共用），此处导入，不复制内容。
只适用于 Claude Code 的补充说明写在本文件导入行之后。

@AGENTS.md

## 调度 agy

用户让 Claude 把改动交给 agy（Antigravity CLI）时（2026-11-23 用户确认，为让 agy 提示缓存稳定命中）：

- 分工：agy 只读代码、改代码、跑迭代级检查；收尾级验证与提交由 Claude 在外层跑。agy 里不跑要等几分钟的命令，等待会让缓存过期。
- 调用：`command agy --dangerously-skip-permissions --model gemini-4-carbon-high --output-format json -p "$(cat <提示文件>)" > <scratchpad>/agy-<任务>.json`，后台运行。结果含 `conversation_id` 与 `usage.cache_read_tokens` / `input_tokens`，每轮把命中率报给用户。
- 续接：同一任务的返工、补修用 `--conversation <id>`，提示只写增量；只有独立审查才新开会话。提示文件固定约束在前、本轮任务在后，引用路径不粘贴大段内容；要看进度用 `--output-format stream-json`。
- agy 运行期间不改 `AGENTS.md`、`~/.gemini/GEMINI.md`（改了缓存全失效）；启动前确认没有其他会话在改同一批文件。
- agy 的结论只是声明：Claude 自己看 diff、截图、跑验证后再报告。
