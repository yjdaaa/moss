# Python 黄金样本

这些文件原由 `migration/generate_goldens.py` 从源 Python 工作区只读生成。
Python 迁移目录已移除；当 API 契约发生经文档确认的变化时，必须同步更新对应
样本并在 `CHANGELOG.md` 记录原因，不能通过放宽比较器来掩盖差异。

## 当前基线

- MCP：源 Python 当前默认实际注册 20 个工具；25 个是源码装饰器定义数。
- 摄像头默认关闭；Home Assistant 与 LaserGRBL GUI 模块不注册。
- 不执行任何 `tools/call`，不连接真实设备，不读取真实 token。
- 采集过程在临时 `cwd` 和临时 `MOSS_RUNTIME_DIR` 中运行，并将材料文件指向不存在的路径。
- Python 测试结果保存在 `python-unittest-discover.txt`。

## 比对规则

- 普通 JSON：解析后 deep-equal；`"800"` 与 `800` 不相等。
- 材料参数：`manage(save)` 的数值字段在入库边界规范化为数字；`"800"` 导出为 `800`。
- 材料库中的 `null`/`{}` 墓碑按参数格不存在处理。
- `source_path`、`backup_path`、`params_file`、随机 ID、时间和绝对路径必须掩码；另做路径形态断言。
- G-code 按解析后的指令和数值比较，不按原始空白或注释逐字比较。
- 错误文案排除清单（D11）：Python 环境特有错误（如 `/api/ui/serial-ports` 缺 pyserial 时的 400 报错）在 Rust 原生探测实现下不要求逐字一致，按结构契约与状态码比对。

## 覆盖范围

已生成项由 `manifest.json` 和 `extended-manifest.json` 声明。当前已补齐文字、
校准矩阵、预制 G-code、时间估算、安全拒绝、材料推荐、材料库严格导入和
`GET /v1/material-lab/bootstrap` 的 Python 快照。尚未生成的真实小智端点帧、
完整视觉基线和逐路由成功/失败矩阵，必须在相应授权或采集器完成后再标记阶段完成。
