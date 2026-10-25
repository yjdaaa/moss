# 数据库表结构与旧 JSON 字段映射

状态：设计于 2026-09-14；2026-09-22 按 `crates/laser-runtime/src/db.rs` 现状核对并补充 schema v3 的 Jev 密钥表；2026-10-06 补充 schema v4（`approvals`、审计写入）
代码：`crates/laser-runtime/src/{db,store,root,migration}.rs`
历史依据：旧 `.runtime/` 数据统计（652 条 workflow、379 个文字任务、65 个 job、22 个校准）来自迁移前的一次性采样

---

## 1. 总原则

1. 一个数据库文件 `<RuntimeRoot>/moss.sqlite`，WAL 模式，`schema_version` 表记录版本。`InstallRoot` 只存放程序文件；数据库绝不默认写入 `InstallRoot`。`RuntimeRoot::resolve` 的解析顺序：显式参数 → 可执行文件同级 `.runtime/` → `%APPDATA%\moss` → 当前目录 `.runtime/`；宿主与桌面端都传 `(None, None)`，因此 Windows 上实际为 `%APPDATA%\moss`。没有 `MOSS_RUNTIME_DIR` 环境变量。
2. G-code、预览图、上传图片、生成图片仍是文件，目录布局与现在一致；数据库只存相对路径与 SHA-256。
3. 旧 JSON 里字段类型不稳定（同一字段有时是字符串有时是数字，见 §8），所以**所有业务记录整条 JSON 以保真为目标存入 `payload` 列，另抽少量字段做索引列**。除 §7 明确规定的路径重映射、旧任务安全终止和字段补记外，其余字段保持原值；所有变换必须进入迁移报告。`payload` 是存储与迁移层面的保真手段，不是业务层的数据模型：`laser-runtime` 在读取入口把 `payload` 反序列化并做宽松类型映射（字符串数字转数值、缺省补齐），产出 `laser-contracts` 中的强类型 struct；转换失败的记录标 `stale` 并给出字段级原因，不静默吞掉。`laser-tools`、`laser-device`、`laser-core` 不得直接持有或解析 `serde_json::Value` 形式的 `payload`。写回时由 struct 序列化生成新的 `payload`，保证工具和接口的对外行为不变；索引列只用于查询和列表。
4. 时间统一存 Unix 秒浮点（`REAL`），与旧数据一致，不转 ISO。
5. 数据库内路径存相对于 `RuntimeRoot` 的字符串，正斜杠分隔；导入时先按历史根做重映射，再校验解析后的绝对路径仍位于允许的 `RuntimeRoot` 子目录内（§7）。接口返回路径时只返回文件名（`RuntimeRoot::exposed_path` 支持回环返回相对路径，但当前路由统一只给文件名），任何请求不返回 `RuntimeRoot`/`InstallRoot` 绝对路径。
6. 每张表都有 `created_at`、`updated_at`。schema v2 为 `workflows`、`text_tasks`、`jobs`、`calibrations` 各配一张通用 `_history` 表（列为 `history_id, record_id, payload, updated_at, replaced_at, reason`，`UNIQUE(record_id, payload)`）；`material_params_history` 与 `material_params` 同列另加 `replaced_at`、`reason`。
7. 并发模型：单写连接。`laser-runtime` 持有单个 `Mutex<Connection>`（WAL、`busy_timeout`、`synchronous=NORMAL`），所有工具、路由与后台 job worker 全部经 `RuntimeStore` 共享该连接，写天然串行，不引入多连接写；同一时刻只允许一个 host 进程打开本库，启动前必须持有 `<RuntimeRoot>/moss.lock`，锁句柄保持到 host 完全退出。Windows 使用 `LockFileEx`，Rust 可使用 `fs4` 或 `fd-lock`；崩溃后依赖操作系统释放锁，不删除锁文件。启动时发现锁被占用即退出并提示已有实例。后台 job worker 的 G-code 发送循环不访问数据库：取消走进程内 `CancellationToken`，进度只写内存原子值；发送线程只在终态把 `send_result` 写回 workflow 或 calibration 记录的 `payload`。当前没有把进度落到 `jobs` 表的持久化任务（见 §3.4）。
8. 启动恢复：host 启动时把 `jobs.status='running'` 与 workflow 活动态（如 `sending`）的残留记录标为 `failed`，在 `result`/`detail` 注明 host 在完成前重启；不自动重发，不恢复未重新确认的危险任务（规格 §10）。

## 2. workflow 存法（已确认：方案 A）

| 方案 | 做法 | 好处 | 代价 |
|---|---|---|---|
| A 整条 JSON + 索引列（推荐） | `payload` 存整条，另抽 `status`、`source_type`、`material`、`created_at` 等 8 列做索引 | 652 条历史记录 100% 能导入；工具逻辑几乎不用改；以后想拆列随时可以从 payload 重建 | 想按 `input` 里某个深层字段查要用 SQLite 的 `json_extract`，比普通列慢一点，本项目数据量下感觉不到 |
| B 全部拆成列 | `input`、`artifacts`、`confirmation_snapshot` 各拆成几十列 | 查询直观 | 旧数据字段类型不齐（§8 列了 9 个字段类型混用），拆列导入会大量失败或要写一堆兼容转换；25 个工具的读写全部重写；以后加字段就要改表 |

用户于 2026-09-11 确认采用方案 A。

## 3. 表结构

### 3.1 `schema_version`

| 列 | 类型 | 说明 |
|---|---|---|
| version | INTEGER PK | 当前 13（代码常量 `laser_runtime::SCHEMA_VERSION`）。v1 建全部表并播种内置材料；v2 为 `jobs` 增加 `updated_at`，创建 6 个索引（`workflows(status,updated_at)`、`workflows(source_type)`、`workflows(material,thickness_mm)`、`jobs(updated_at)`、`text_tasks(updated_at)`、`calibrations(updated_at)`）与 4 张通用 `_history` 表；v3 新增 `app_secrets` 保存桌面端 Jev API Key；v4 新增 `approvals` 表（§3.12）与索引 `approvals(status,expires_at)`、`audit_log(at)`；v5 为 `approvals` 增加 `request` 列；v6 新增内置语音服务的设备登记表 `voice_devices`（§3.13）；v7 新增按设备保存摘要的 `voice_memory` 表（§3.14，NV10-1）；v8 新增独立的 `voice_agent_runs` 表（§3.15，NV13）；v9 新增 NV16 `voice_speakers`、`voice_person_memory` 与 NV17 `voice_firmware_delivery`；v10 新增 NV19 `context_memories` 与 `context_extraction_runs`（§3.16）；v11 新增 FTS5 全文索引 `context_memories_fts` 与三个同步触发器（§3.16）；v12 新增 NV19-05 知识库 `context_knowledge_roots` / `_dirs` / `_files` 与 `context_knowledge_fts`（§3.17）；v13 新增 NV17 固件清单 `voice_firmware_artifacts`（§3.18），并为 `voice_firmware_delivery` 增加 `firmware_version`、`token`（部分唯一索引）与 `expires_at` | 
| applied_at | REAL | |

Schema 迁移由 `laser-runtime` 自管，不引入 `refinery`。启动时读取已应用的最高版本，按版本号顺序执行内置迁移；每个版本在事务中完成，只有全部语句成功后才写入 `schema_version`。迁移失败必须回滚并阻止业务读写，不得把半完成的 schema 当作可用。迁移器需支持内存库测试、重复执行幂等和中断后重试。

### 3.9 `app_secrets`

| 列 | 类型 | 说明 |
|---|---|---|
| name | TEXT PK | 密钥名称：`jev_api_key`；`voice.device_token_secret`（内置语音服务签发设备 token 的密钥，首次启动内置服务时随机生成，任何接口都不返回） |
| value | TEXT | 按用户确认的桌面自用方案保存 API Key 原文；普通状态接口只返回遮罩值 |
| updated_at | REAL | Unix 秒时间 |

该表只保存应用密钥，不保存普通界面设置。桌面端默认显示遮罩，用户点击眼睛后才调用 `/api/jev/reveal` 请求完整值。

### 3.2 `workflows`（对应 `.runtime/laser_workflows/*.json`）

| 列 | 类型 | 来源 | 说明 |
|---|---|---|---|
| workflow_id | TEXT PK | `workflow_id` | Rust 版新建记录形如 `wf-<unix秒>-<序号>`，画板预览为 `draw-…`；迁移的旧记录保留原 `wf_` + 32 位 hex |
| status | TEXT | `status` | 实测值：preview_ready、failed、sending、feedback_recorded、completed、cancelled |
| source_type | TEXT | `source_type` | text、image、image_summary、prepared_gcode、空 |
| material | TEXT | `input.material` | 索引用 |
| thickness_mm | REAL | `input.thickness_mm` | 导入时字符串转数字，转不了存 NULL，payload 保留原值 |
| laser_mode | TEXT | `input.laser_mode` | engrave / cut |
| connection_mode | TEXT | `input.connection_mode` | serial / network |
| gcode_path | TEXT | `artifacts.gcode_file` | 重映射后的相对路径 |
| gcode_sha256 | TEXT | `artifacts.gcode_sha256` | |
| has_snapshot | INTEGER | `confirmation_snapshot` 非空 | 0/1 |
| stale | INTEGER | 迁移时判定 | 1 表示引用文件不存在 |
| created_at | REAL | `created_at` | |
| updated_at | REAL | `updated_at` | |
| payload | TEXT | 整条 JSON | 工具读写以此为准 |

索引：`(status, updated_at DESC)`、`(source_type)`、`(material, thickness_mm)`。
`recent` 动作按 `updated_at DESC` 取。

### 3.3 `text_tasks`（对应 `.runtime/lasergrbl_text_tasks/<id>/task.json`）

| 列 | 类型 | 来源 |
|---|---|---|
| task_id | TEXT PK | `task_id` |
| status | TEXT | `status` |
| text | TEXT | `text` |
| material | TEXT | `material` |
| thickness_mm | REAL | `thickness_mm` |
| laser_mode | TEXT | `laser_mode` |
| attempt_count | INTEGER | `len(attempts)` |
| task_dir | TEXT | 目录相对路径，`attempt_N.gcode/png` 仍放这里 |
| stale | INTEGER | |
| created_at / updated_at | REAL | |
| payload | TEXT | 整条 JSON |

### 3.4 `jobs`（合并 `lasergrbl_jobs` 与 `lasergrbl_network_jobs`）

两类 job 字段高度重合，合一张表，用 `transport` 区分。

**现状（2026-09-21）**：本表只由迁移器写入历史 job，并在启动恢复时被批量标为 `failed`。运行时发送进度保存在 `laser-api` 内存中的作业注册表（`ActiveJobs`）与 workflow / calibration 记录 `payload.send_result` 中，`laser-api` 没有写入 `RecordKind::Job` 的代码。下表与随后的 worker 落库描述是原设计，尚未实现。

| 列 | 类型 | 来源 | 说明 |
|---|---|---|---|
| job_id | TEXT PK | `job_id` | |
| workflow_id | TEXT FK | `workflow_id` | 可空；非空时关联 `workflows.workflow_id`，发送门禁要求快照有效；校准 job 可仅使用 `calibration_id` 与校准会话 SHA 门禁 |
| transport | TEXT | 串口固定 `serial`；网络取 `transport` | serial / telnet / http |
| status | TEXT | `status` | pending、running、completed、failed、cancelled（`models.py:24`） |
| port | TEXT | 串口 `port` | |
| baudrate | INTEGER | 串口 `baudrate` | |
| host | TEXT | 网络 `host` | |
| http_port / telnet_port | INTEGER | 网络 | |
| timeout | REAL | 网络 `timeout` | |
| gcode_path | TEXT | `prepared.gcode_file` | 重映射 |
| expected_gcode_sha256 | TEXT | `expected_gcode_sha256` | 旧记录缺失时迁移为 `failed`；发送前必须非空且与当前文件匹配 |
| process_id | INTEGER | `process_id` | 历史兼容列。导入旧数据置 NULL；默认同进程 worker 形态下新记录固定 NULL；仅当触发 规格 §16 的桌面子进程回退形态时记录子进程 OS PID，供进程树清理 |
| calibration_id | TEXT FK | `calibration_id` | 可空；校准矩阵 job 可不关联 workflow，但必须关联存在的 calibration，并在发送前校验校准会话 G-code SHA |
| created_at / started_at / finished_at | REAL | 同名 | |
| progress_lines / progress_updated_at | INTEGER / REAL | 新增 | 持久化任务节流写入的已发送行数与时间；旧数据 NULL |
| payload | TEXT | 整条 JSON | 含 `prepared`、`result` |

后台发送 worker 改为读写此表而非 job 文件，但只在状态迁移时写库：`pending→running` 一次、终态（completed/failed/cancelled）一次，两次都在发送循环之外；`process_id` 见上表说明。取消不经数据库：`cancel` 由 host 内存中的 job 注册表向该 job 的 `tokio::sync::watch`（或等价的原子标志）发出取消信号，worker 每行发送前只读一次内存值，零锁零 I/O；worker 收到信号后按 规格 §4.5 的取消序列（`!` → 停止顺序流 → `?` 轮询到 Hold → `0x18`）处理，再写一次终态 `cancelled`。进度（已发送行数、当前行、最近状态报告）只写入内存中的原子计数与 `watch` 快照，worker 本身不落库；host 的持久化任务每秒读取快照写入 `jobs.progress_lines`、`jobs.progress_updated_at` 两个独立列（新增，避免每次重写整条 `payload`），任务结束时才把完整结果合并进 `payload.result`。旧 Python 的 "kill 子进程" 取消方式不带入 Rust 版。只有同时满足 `status='pending'`、`expected_gcode_sha256` 非空且格式有效、worker 在实际发送前重新计算当前 G-code 文件 SHA-256 与 `expected_gcode_sha256` 相等，并且满足以下任一授权凭据时才允许发送：关联 workflow 存在且 `has_snapshot=1`，或关联 calibration 存在且校准会话的 G-code SHA 与重算值匹配。任一条件不满足，worker 必须把 job 标记为 `failed`，写入安全终止说明，并跳过所有串口/网络 G-code 发送。

### 3.5 `calibrations`（对应 `lasergrbl_calibrations/*.json`，`.gcode` 仍是文件）

| 列 | 类型 | 来源 |
|---|---|---|
| calibration_id | TEXT PK | `calibration_id` |
| status | TEXT | `status` |
| material / thickness_mm / laser_mode / engraving_mode / matrix_kind | 同名 | |
| gcode_path | TEXT | `gcode_file` 重映射 |
| job_id | TEXT | `job_id`，可空 |
| selected_cell_number | INTEGER | `selected_cell.cell_number`，可空 |
| created_at / updated_at | REAL | |
| payload | TEXT | 整条，含 `cells`、`stats`、`base_params` |

### 3.6 `materials`（对应 `lasergrbl_materials.json`，结构：材料 → 别名 → 厚度 → 模式 → 参数格）

拆成三张表，因为冲突规则按"格"判定，整条存 JSON 无法逐格比对。

`materials`

| 列 | 类型 | 说明 |
|---|---|---|
| name | TEXT PK | 材料名，如 椴木 |
| created_at / updated_at | REAL | |

`material_aliases`

| 列 | 类型 |
|---|---|
| alias | TEXT PK |
| material_name | TEXT FK |

`material_params`（每行一格）

| 列 | 类型 | 来源 | 说明 |
|---|---|---|---|
| id | INTEGER PK | | |
| material_name | TEXT FK | | |
| thickness_key | TEXT | 旧 JSON 的厚度键，如 "3"、"4" | 保留原字符串，不转数字，避免 3 与 3.0 分裂 |
| laser_mode | TEXT | engrave / cut | |
| engraving_mode | TEXT | raster / outline / 空 | cut 时为空 |
| laser_min_power / laser_max_power | INTEGER | 同名 | |
| feed_rate / travel_rate | INTEGER | 同名 | |
| threshold | INTEGER | 同名 | -1 表示自动 |
| passes | INTEGER | 同名 | |
| pixel_size_mm | REAL | 同名 | |
| source | TEXT | 同名 | manual / calibration / default 等，照原值 |
| updated_at | REAL | 同名 | |
| notes | TEXT | 同名 | |
| extra | TEXT | 未列出的其他键 | JSON，防丢字段 |

唯一约束：`(material_name, thickness_key, laser_mode, engraving_mode)`。

#### 3.6.1 内置预设与首次建库（2026-09-14 决定，落法 A1）

Python 版把内置 `DEFAULT_MATERIAL_PARAMS`（椴木 3mm、亚克力 3mm、皮革 1.5mm、纸板 1mm，各含 engrave 与 cut）作为永久底层，每次加载都与用户文件合并；删除内置格子时文件里留 `null`/`{}` 墓碑防止被填回。Rust 版不保留这套双层机制：

1. `schema_version` 升到 1 后、导入任何旧文件之前，把预设播种为普通行。预设的扁平 `engrave` 按 Python `include_outline_for_flat_defaults=True` 拆为 `raster` 与 `outline` 两格同值，加 `cut` 共 12 格；这些行的 `source`、`updated_at`、`notes` 为 NULL（Python 默认值里没有这三个键，导出时省略，保持级别 1 等价）。
2. 播种之后预设行与用户行无区别：删除即删行，不写墓碑；`material_params_history` 照常记录。
3. 迁移旧文件时（§7 第 5 步），旧文件中内置材料的某格为 `null` 或 `{}`，视为用户已删除该格：跳过该格的播种（或删除已播种的行），并计入迁移报告；旧文件中的非空格与预设同格且参数不同，走 规格 §7.2 冲突规则。
4. `LASER_MATERIAL_PARAMS_FILE` 缺失时不算错误：库内即为预设，`GET /v1/material-lab/bootstrap` 的 `error` 为 `null`。

#### 3.6.2 导出/导入 JSON Schema 冻结（2026-09-14 决定，选项 A）

`laser-contracts` 按下表建强类型；`material_params_tool(export)`、`/api/material-lab/export`、`manage(import)`、`GET /v1/material-lab/bootstrap` 的 `materials` 与迁移器共用同一套。

| 节点 | 类型与规则 |
|---|---|
| 顶层 `version` | 导出恒为 JSON 整数 `1`，不存表，由导出器写常量。加载本地旧文件与迁移时宽松归一化：`1`、`1.0`、`"1"`（去首尾空白）归一为 1，缺失按 1；其他值判为数据错误（见 §7 第 5 步、§8）。`manage(import)` 严格：只接受数值 `1`/`1.0`，否则 400 原文案 `导入材料库格式不支持：需要 version=1 的 JSON 对象`，与 Python 版原文案一致 |
| 顶层 `materials` | 对象，键为材料名（`materials.name`）；导出总是存在，可为 `{}` |
| 材料 `aliases` | 字符串数组，导出总是存在（无别名为 `[]`），按插入顺序、去重、不排序；导入与迁移取并集不冲突（§7.2）；不接受 `null` 或非字符串项，出现即导入报错、迁移隔离该材料 |
| 材料 `thicknesses` | 对象，键为厚度字符串，写入时经与 Python `_format_mm` 相同的规则规范（`3`、`1.5`，去掉多余小数零），读入时保留原键字符串不转数字；导出总是存在，材料无任何格时为 `{}` |
| 模式节点 `engrave` | 对象，键只允许 `raster`、`outline`，至少一个；旧扁平形态（直接是参数格）在加载/迁移时归一为 `{"raster": 格}`（用户数据）或 `{"raster": 格, "outline": 格}`（内置预设），与 Python `_merge_material_params` 一致；导出不再出现扁平形态 |
| 模式节点 `cut` | 直接是参数格 |
| 墓碑 | 导出不出现 `null` 或 `{}` 的模式节点或格；导入与迁移遇到 `null`/`{}` 视为该格不存在（§3.6.1 第 3 条） |
| 参数格必填 | `laser_min_power`、`laser_max_power`、`feed_rate`、`travel_rate`、`threshold`、`passes` 整数；`pixel_size_mm` 数值。范围与 Python `_normalize_params` 一致：功率 0..S_MAX，`feed_rate`/`travel_rate` ≥1，`threshold` -1..255（-1 自动），`passes` 1..max_passes，`pixel_size_mm` >0。`manage(save)` 缺失按 api-contracts.md §3.10 默认值补齐；导入/迁移不补默认值，缺失存 NULL、导出省略该键（保真）；数值字段入库统一转为数字，无法转换返回 400 |
| 参数格可选 | `source`、`notes` 字符串，`updated_at` REAL；缺失存 NULL，导出省略该键；`notes` 为 `""` 时保留空串。`revision`、`history`、`machine_profile_id`、`confidence`、`evidence` 及未知键进 `extra` JSON，导出原样回填 |
| 删除语义 | 删格即删行；一个厚度下无任何格时删除该厚度键；材料无任何厚度时删除材料及其别名（Python 对非内置材料的行为，A1 后对内置材料同样适用） |

`material_params_history`：与 `material_params` 同列，另加 `replaced_at`、`reason`（import_overwrite / manual_edit / calibration_select）。覆盖前把旧行拷进去。

导出到 JSON 时按 §3.6.2 冻结结构重建（顶层 `version` 恒为整数 1、`materials`，材料下 `aliases` 与 `thicknesses`，engrave 下 raster/outline，cut 单层；已对真实 `lasergrbl_materials.json` 核实：5 种材料、`version` 为整数 1、0 个墓碑节点、25 格中 17 格带 source/updated_at/notes、7 格只有 7 个数值键、1 格无 notes）。验收按 规格 §20.1 级别 1：导出结果解析后与导入源 deep-equal，数值按数值比较；对象键序、空白和逐字节文本不作为通过条件。旧文件 `updated_at` 有 int 与 float 混用（25 格中 13 int、5 float），经 REAL 列往返后 `1782816917` 导出为 `1782816917.0`，文本形态不可还原但数值相等。

### 3.7 `ai_state`（对应 `ai_laser_gcode_state/state.json`）

已建表，Rust 版无读写方（对应的 Python 图片引擎已移除）。单行表：`id INTEGER PK CHECK(id=1)`、`payload TEXT`、`updated_at REAL`。含 `recent_material`、`image_search_candidates`。

### 3.8 `gui_calibration`（对应 `lasergrbl_gui_calibration.json`）

已建表，Rust 版无读写方（LaserGRBL GUI 自动化工具已移除）。

| 列 | 类型 |
|---|---|
| target | TEXT PK（port / connect / start） |
| payload | TEXT |
| saved_at | REAL |

### 3.9 `uploads`（对应 `laser_web_uploads/`，现在无 JSON 元数据）

只由迁移器写入；`/api/upload` 不登记本表，上传文件直接落盘到 `RuntimeRoot/uploads/`。

| 列 | 类型 | 说明 |
|---|---|---|
| upload_id | TEXT PK | 文件名去扩展名 |
| path | TEXT | 相对路径 |
| sha256 | TEXT | 导入时计算 |
| size_bytes | INTEGER | |
| mime | TEXT | 按扩展名 |
| created_at | REAL | 文件名前缀时间戳 |

### 3.10 `auth_pairings`（新增，对应规格 §17）

已建表，当前无写入方：`laser_api::AuthManager` 把配对码、失败计数、锁定与 token 哈希全部保存在内存 `HashMap`，进程重启后配对失效。下表为原设计。

| 列 | 类型 | 说明 |
|---|---|---|
| token_hash | TEXT PK | bearer 解码后的 32 字节随机值 SHA-256 |
| pairing_id | TEXT UNIQUE | 不透明配对标识，供审计引用，不由 token 推导 |
| device_name | TEXT | 移动端上报 |
| created_at / last_seen_at | REAL | |
| revoked | INTEGER | |
| revoked_at | REAL | 撤销时间，可空 |

配对码本身只在内存，5 分钟过期，不落库。长期 bearer 有效直到本地管理入口把 `revoked=1`，或 host 身份重置使既有配对全部失效；不保存明文 token，不提供远程延长有效期操作。

### 3.11 `audit_log`（新增，对应规格 §8 安全操作记录）

已建表。`RuntimeDb::append_audit` / `recent_audit` 按 `laser-contracts::AuditEntry` 读写；2026-10-06（P1-08）起审批服务与小智接入点写入。`client` 另有 `system`（审批过期、执行失败等由服务自身触发的变化）。`detail` 只放审批 ID、类别、摘要、工具名、错误码、原因与被删除的字段名，不放设备地址、凭据或文件路径。

| 列 | 类型 |
|---|---|
| id | INTEGER PK |
| at | REAL |
| client | TEXT（desktop / gateway / mcp / 配对 ID / local，由 `audit_client(&CallerContext)` 得出） |
| action | TEXT（`AuditAction`：approval_created / approval_approved / approval_rejected / approval_expired / approval_executed / approval_failed / confirmed_field_stripped / r4_denied / strict_mode_blocked） |
| target | TEXT（workflow_id / job_id / 材料格） |
| result | TEXT（`AuditResult`：ok / denied / failed） |
| detail | TEXT（JSON，不含凭据；由 `laser-contracts::AuditDetail` 序列化而来，工具层不拼裸 JSON） |

### 3.12 `approvals`（schema v4，技术书 §7.1）

桌面审批记录。类型见 `laser-contracts::ApprovalRecord`；状态迁移规则 `ApprovalStatus::can_transition_to`：pending → approved / rejected / expired，approved → executed / expired / failed，executed → failed；任何状态都回不到 pending 或 approved，所以一次授权最多执行一次。

| 列 | 类型 | 说明 |
|---|---|---|
| approval_id | TEXT PK | `apr_` + 随机 |
| status | TEXT | pending / approved / rejected / expired / executed / failed |
| kind | TEXT | workflow_confirm_send / calibration_send / safe_action / framing |
| target_record | TEXT | workflow_id / calibration_id |
| digest | TEXT | 技术书 §5.4 摘要 |
| summary | TEXT | 人类可读摘要，不含路径和凭据 |
| source | TEXT | gateway / mcp_bridge / lan / voice（内置语音，NV4；`session_id` 记发起设备的 Device-Id） |
| session_id / request_id | TEXT | 可空；UNIQUE(source, request_id) 保持不变。2026-10-10 起 request_id 存 scope-v1 哈希（可信调用方/会话 + 原编号），不接受客户端自报身份；旧值保留可读但不与新键复用。不能验证独立来源的旧 MCP 桥接请求以 NULL 禁用编号复用。 |
| connection | TEXT | JSON（`ApprovalConnection`），创建时解析出的连接参数，执行时直接使用。只在本地库，`ApprovalRecord` 序列化时不输出 |
| created_at / decided_at / expires_at / updated_at | REAL | Unix 秒；`decided_at` 可空 |
| outcome | TEXT | JSON（`ApprovalOutcome`），执行结果摘要，可空；`phase` 为 `running` / `completed` / `failed` / `cancelled` / `unknown`（进程在发送中中断，`error_code=result_unknown`） |
| request | TEXT | v5。JSON，原始执行参数（已去掉连接字段、身份字段与 `request_id`），批准后据此重放执行并重算摘要；只在本地库，`ApprovalRecord` 序列化时不输出。v4 时创建的行为空 |

旧版程序打开 v4 库：迁移只执行 `current < N` 的步骤、不拒绝更高版本，因此忽略这张新表；旧表结构未改。`gateway_requests` 随可选网关（P2b）再加。

撤销语音设备时，设备状态与该设备来源、尚为 pending/approved 的审批一起更新为 revoked/expired；重新允许不恢复审批。审批创建在同一数据库锁下校验设备状态与已登记会话的有效标记。新同编号请求必须匹配原摘要与保存参数，否则返回冲突；本次不清空记录、不升级 schema。

### 3.13 `voice_devices`（schema v6，内置语音技术书 NV1）

内置语音服务登记的小智设备。新设备第一次请求 OTA 或连接时登记为待允许，桌面端允许后才签发 token。

| 列 | 类型 | 说明 |
| --- | --- | --- |
| device_id | TEXT PK | 设备 MAC（固件请求头 `Device-Id`） |
| client_id | TEXT | 设备 UUID（`Client-Id`）。与已登记值不同时，已允许的设备退回待允许（防止他人冒用 MAC 取得 token），已撤销的保持撤销 |
| status | TEXT | `pending` 待允许 / `allowed` 已允许 / `revoked` 已撤销 |
| name | TEXT | 用户起的名字（最长 32 字），默认空 |
| model / firmware_version | TEXT | OTA 上报的型号（`board.type`）与固件版本；空值不覆盖已有记录 |
| last_ip | TEXT | 最近来访的局域网地址（只在本机显示） |
| first_seen / last_seen / decided_at | REAL | Unix 秒；`decided_at` 为最近一次允许或撤销的时间，可空 |

### 3.14 `voice_memory`（schema v7，NV10-1）

仅保存用户选择启用后的设备级文字摘要，不保存完整录音、原始聊天、审批或密钥。`summary` 最长 1500 Unicode 字符；后续 NV10-2/3/4 才接入配置开关、异步总结与桌面编辑功能，不能因数据库表存在就认定记忆已启用。

| 列 | 类型 | 说明 |
| --- | --- | --- |
| device_id | TEXT PK | 设备登记标识，上层必须校验许可/身份 |
| summary | TEXT NOT NULL | 默认空字符串 |
| revision | INTEGER NOT NULL | 从 0 开始；编辑、清空或总结成功均递增 |
| updated_at | REAL NOT NULL | Unix 秒 |

`RuntimeDb::ensure_voice_memory` 仅首次插入，不改旧摘要；`voice_memory` / `voice_memories` 查询；`update_voice_memory_if_revision` 使用 SQL `UPDATE ... WHERE device_id=? AND revision=?`，过期结果返回 `None` 而不是无条件 UPSERT。迁移单事务、可重试；NV10-1 有针对性迁移失败与清空竞态单测，完整会话/配置/真实模型验证仍未开始。

### 3.15 voice_agent_runs（schema v8，NV13）

独立存储 AgentRun 状态/摘要/输出，不保存完整 prompt、CLI 密钥或继承的环境变量。approval_id 有 UNIQUE 约束，单次审批至多对应一条 run；task_digest/config_revision 为 SHA-256 指纹。

| 列 | 类型 | 含义 |
|---|---|---|
| id | TEXT PK | 独立 AgentRun 编号 |
| approval_id | TEXT UNIQUE NOT NULL | 对应的唯一审批 |
| provider / workspace | TEXT | codex/claude_code，本地隐蔽路径；桌面返回目录名 |
| task_digest / config_revision | TEXT | 任务及当前配置指纹 |
| status | TEXT | running、completed、failed、cancelled、timed_out、result_unknown |
| output / truncated | TEXT / INTEGER | 最多 256 KiB 的有界结果及是否截断 |
| started_at / finished_at | REAL / REAL NULL | Unix 秒 |
| error_code | TEXT NULL | 短错误码 |

插入 run 与 consume_agent_run 审批/审计使用同一事务；运行结束只能写一次终态，最近只保留最多 50 条终态记录。启动恢复将运行中标成 result_unknown 且不重跑。自 2026-10-15 起真实 CLI 执行器在严格模式且用户开启后可用（见 [安全入口](safety.md) §4.18）；非 Windows 平台仍拒绝运行。

### 3.16 `context_memories` / `context_extraction_runs`（schema v10，NV19-02）

结构化长期记忆，与 NV10 `voice_memory` 并存：迁移不搬迁、不修改旧摘要，旧摘要只能由用户在私有窗口导入为候选（`ContextService::import_legacy`）。取值约定见 [内置语音实施细则 §11 NV19-01](../integration/moss-native-voice-implementation.md#nv19)；规则先由 `moss-voice-server::context` 检查，表上的 CHECK 约束是最后一道关。

| 列 | 类型 | 含义 |
|---|---|---|
| id | TEXT PK | `ctx-` + 32 位随机十六进制 |
| scope_kind / scope_id | TEXT | device / person / project / workspace + 宿主生成的编号（1～64） |
| category | TEXT | preference、device_fact、technical_note、task_context、troubleshooting_experience |
| content | TEXT | 有效记录 1～1000 字（去空白后非空）；删除后为空串 |
| verification | TEXT | candidate、confirmed、invalidated |
| source_kind / source_ref | TEXT / TEXT NULL | 六种来源 + `<前缀>:<token>` 内部引用（≤128），不含 URL、路径或凭据 |
| device_model / firmware | TEXT NULL | 适用机型 / 固件（固件不能脱离机型单独出现） |
| revision | INTEGER | 从 1 开始，每次修改 +1 |
| created_at / updated_at / expires_at / deleted_at | REAL | Unix 秒；`deleted_at` 非空即为墓碑 |

索引 `context_memories_scope(scope_kind,scope_id,category,verification,deleted_at)`、`context_memories_source(scope_kind,scope_id,source_ref)`。`RuntimeDb`（`context_store.rs`）：`insert_context_memory` 只接受 revision 1 的有效行；`update_context_memory_if_revision` 只改未删除、未作废的行，已确认不能降回候选，冲突返回 `None`；删除 / `clear_context_scope` 只留墓碑（清空内容与适用范围、revision+1），让迟到的后台写入因版本不符失败；`context_source_seen` 连墓碑一起查，删掉的导入不会被重新导入。

v11（NV19-04）：外部内容 FTS5 虚表 `context_memories_fts(content)`，`tokenize='trigram'`，以 `context_memories.rowid` 关联；触发器 `context_memories_fts_ai` / `_ad` / `_au`（仅正文变化时）同步，迁移末尾 `rebuild` 一次。墓碑正文为空，删除后索引不留内容。检索接口 `match_context_memories`（每个词恰为 3 字以上的短语、OR、bm25 排序）与 `like_context_memories`（多个短词 OR，`!` 为转义字符），都只返回本范围未删除、未作废、未到期的记录。

`context_extraction_runs` 保存每次候选提取的变更清单及 `reverted_at`（NV19-03）。提取只能新增候选，所以 `adds_json` 只有 `{id,revision}`，`skipped_json` 只有 `{op,reason}`，`updates_json` / `deletes_json` 恒为 `[]`，清单不含记忆正文；四列都必须是不超过 16 KiB 的 JSON 数组。接口：`insert_context_extraction_run`（不能直接写入已撤销）、`context_extraction_run`、`list_context_extraction_runs`（按范围，新的在前）、`mark_context_extraction_reverted`（只能成功一次）。

### 3.17 `context_knowledge_*`（schema v12，NV19-05）

用户明确选择的资料目录。只存目录级摘要与文件指纹，原文（L2）不入库。

| 表 | 列 | 说明 |
|---|---|---|
| `context_knowledge_roots` | id、scope_kind（project / workspace）、scope_id、label（目录名，≤128）、root_path（规范化本机路径，≤1024，不进提示词）、revision（≥1）、created_at、indexed_at | 同一范围同一路径唯一 |
| `context_knowledge_dirs` | id、root_id、rel_dir（`/` 分隔，根目录为空串）、l0（1～256 字）、l1（1～4000 字）、file_count（≥1） | `(root_id, rel_dir)` 唯一；FTS5 外部内容表 `context_knowledge_fts(l0, l1)`，trigram，三个触发器同步 |
| `context_knowledge_files` | id、root_id、dir_id、rel_path、sha256（64）、size（>0）、mtime | `(root_id, rel_path)` 唯一；索引 `(dir_id)` |

`RuntimeDb`（`knowledge_store.rs`）：`insert_knowledge_root`（revision 1、未建索引）、`knowledge_root(s)`、`replace_knowledge_index`（单事务：revision 相符才整体替换目录与文件并 revision+1，文件必须属于本次的目录，任何约束失败整体回滚）、`delete_knowledge_root_if_revision`（单事务删除三张表的行；数据库未开外键，不依赖级联）、`knowledge_dirs`、`knowledge_files`、`search_knowledge_dirs`（本范围内 L0 / L1 命中三字片段（FTS5）或短词（LIKE，`!` 转义）的目录，去重合并）。

### 3.18 `voice_firmware_artifacts` 与固件推送列（schema v13，NV17）

固件文件本身不入库，只登记 SHA-256 与来源信息；文件存在运行目录 `firmware/<sha256>.bin`，发送前重新读取并核对 SHA-256。

| 表 / 列 | 类型 | 含义 |
|---|---|---|
| `voice_firmware_artifacts.sha256` | TEXT PK | 固件文件 SHA-256（64 位十六进制，CHECK 长度） |
| `voice_firmware_artifacts.version` / `project` | TEXT | 从 ESP32 应用镜像头读出的版本（1～32 字符）与项目名（≤32 字符） |
| `voice_firmware_artifacts.size` / `created_at` | INTEGER / REAL | 字节数（>0）与导入时间 |
| `voice_firmware_delivery.firmware_version` | TEXT NOT NULL DEFAULT '' | 这次推送的固件版本；设备上报同版本才算升级（旧记录为空串） |
| `voice_firmware_delivery.token` | TEXT NULL | 一次性下载令牌（32 位十六进制），部分唯一索引 `voice_firmware_token`；取消或上报后置空 |
| `voice_firmware_delivery.expires_at` | REAL NOT NULL DEFAULT 0 | 推送到期时间；过期的推送不再下发也不再可下载（旧记录为 0，视为已失效） |

`RuntimeDb`（`firmware_store.rs`）：`add_firmware_artifact`（同 SHA-256 重复导入不变）、`list_firmware_artifacts`、`delete_firmware_artifact`（还有有效推送时返回 false 不删除）、`create_firmware_offer`（同一设备尚未开始下载的旧推送被替换；版本必须与已导入固件一致）、`active_firmware_offer`、`firmware_offer_by_token`、`firmware_device_reported`（上报新版本即置为已上报并作废令牌）、`cancel_firmware_offer`（未开始下载的删除，已开始下载的作废令牌并保留记录）。v13 迁移在事务内先查 `pragma_table_info` 再决定是否加列，失败回滚后可以重试。

## 4. 废弃的旧结构

Python 版 `RuntimeStore` 与 `TaskRecord` 定义过 `tasks/`、`jobs/` 目录和 `schema_version=1`，但没有任何工具使用，`.runtime/laser_runtime/` 目录也不存在。不迁移，不建表。

## 5. `RuntimeStore` trait

实际签名（`crates/laser-runtime/src/store.rs`）：

```rust
pub enum RecordKind { Workflow, TextTask, Job, Calibration }

pub struct StoredRecord { pub kind: RecordKind, pub id: String, pub status: Option<String>, pub payload: serde_json::Value }

pub trait RuntimeStore {
    fn get(&self, kind: RecordKind, id: &str) -> Result<Option<StoredRecord>, SchemaError>;
    fn put(&self, record: &StoredRecord) -> Result<(), SchemaError>;
    fn set_status(&self, kind: RecordKind, id: &str, status: &str, now: f64) -> Result<bool, SchemaError>;
    fn list_recent(&self, kind: RecordKind, limit: usize) -> Result<Vec<StoredRecord>, SchemaError>;
}
```

实现：`SqliteStore`（生产）、`MemoryStore`（测试与无数据库回退）。`put` 整条覆盖并同步刷新索引列。材料库不经此 trait，而是 `RuntimeDb::import_material_json` / `export_material_library`；读取入口 `decode_workflow` / `decode_job` / `decode_text_task` / `decode_calibration` 把 `payload` 宽松映射为 `laser-contracts` 强类型。

## 6. 表与工具、接口的对应

| 表 | 运行时写入方 | 运行时读取方 |
|---|---|---|
| workflows | `/api/workflow`（含 MCP `laser_workflow_tool` 转发）、`/api/draw/preview`、`/api/text-task/*`、发送线程终态回写 | 同左，`/api/material-lab/recent`，`/v1/events` |
| text_tasks | 无（仅迁移器） | 无 |
| jobs | 无（仅迁移器；启动恢复批量置 `failed`） | 无 |
| calibrations | `/api/material-lab/preview` / `send` / `select-cell`、发送线程终态回写 | `/api/material-lab/status` |
| materials / material_aliases / material_params(_history) | `/api/material-lab/manage`（save/delete/import）、`select-cell` | `/v1/material-lab/bootstrap`、`/api/material-lab/export`、`/api/ui/material-*` |
| ai_state、gui_calibration、uploads、auth_pairings | 无（`uploads` 仅迁移器） | 无 |
| audit_log | `laser-api` 审批服务（`ApprovalService`）、`laser-host` 进程内后端（接入点 R4 拒绝） | `RuntimeDb::recent_audit`（测试） |

## 7. 迁移流程（`moss-host migrate --from <旧根>`）

1. 普通迁移先按本节 `schema_version` 规则升级目标数据库 schema；schema 迁移失败则停止，不进入数据导入。`--dry-run` 只检查待执行的 schema 和数据迁移，不写入任何 schema 或业务数据。
2. 读旧根下的数据目录，只读。历史布局不统一：早期记录把 `.lasergrbl_text_tasks/` 放在仓库根下，现布局在 `.runtime/` 下，同一根内两种布局并存；按步骤 3 的布局映射定位，不假设单一布局。
3. 路径重映射分两步：a) 历史根前缀替换——`--from` 可传多个历史根（实测 payload 同时含当前源目录与更早的 `moss-xiaozhi-mcp-main` 两代根，产物路径引用共 1581 处）；b) 目录布局映射——`<根>/.lasergrbl_text_tasks/` → `<根>/.runtime/lasergrbl_text_tasks/`。替换后文件不存在则该记录 `stale=1`。仅做单根前缀替换实测 1037 处 stale（约 65%），两步映射后仅 3 处缺失，迁移验收以此为基线并在报告中逐条列出。根从命令参数传入，不硬编码。
4. 按 §3 逐表导入。主键已存在时：payload 完全相同跳过；不同则以 `updated_at` 新者为准，旧者进 `_history`（材料表除外，走 §8 冲突规则）。旧 job 的 `pending`、`running`、`queued` 状态一律强制改写为 `failed`（或等价终态），并在 canonical payload 的 `detail`/`result` 中写入“旧系统未完成任务，迁移时安全终止”；无法关联 workflow、缺少 G-code SHA 或缺少确认快照的旧 job 同样不得进入发送队列。
5. 材料库：先按 §3.6.1 播种内置预设，再读 `LASER_MATERIAL_PARAMS_FILE`。文件缺失不是错误，报告记 `materials: preset_only`。顶层 `version` 按 §3.6.2 宽松归一化，无法归一化则整个文件判为数据错误：不导入任何材料，文件路径与原始值写入报告 `errors`，迁移其余表继续。内置格子的 `null`/`{}` 墓碑按 §3.6.1 第 3 条处理并计数。
6. `uploads` 扫描目录建索引，计算 SHA-256。
7. `--dry-run` 只输出计划：各表新增/跳过/冲突/stale 数量，材料冲突逐格清单；不得写入 schema 或业务数据。
8. 输出迁移报告到目标运行目录的 `migration-<时间戳>.json`。
9. 幂等：重复执行结果相同。

材料冲突规则见 §3.6；命令行入口 `moss-host migrate --from <旧根> [--target <运行目录>] [--dry-run] [--on-conflict keep|overwrite]`。

## 8. 旧数据类型不一致清单（导入时需容错）

| 字段 | 实测类型 | 处理 |
|---|---|---|
| 材料库顶层 `version` | 现文件为 int 1；历史上可能出现 `1.0` 或 `"1"` | 迁移与本地旧文件加载：`1`、`1.0`、`"1"` 归一为 1，缺失按 1；其他值整个文件判数据错误并隔离（§3.6.2、§7 第 5 步）。`manage(import)` 不走此规则，沿用 Python 严格判断 |
| workflow `input.thickness_mm` | float 171、str 26、int 3 | 索引列尝试转 REAL，失败存 NULL；payload 原样 |
| workflow `input.font_size` | int 121、str 42 | 同上 |
| workflow `input.network_telnet_port` | int 36、str 42 | 同上 |
| snapshot `pixel_size_mm`、`overscan_mm`、`power_percent` | float 与 str 混用 | payload 原样；宽松转换只发生在 `laser-runtime` 读取入口（§1 第 3 条），出口是强类型 struct，业务层不接触 `serde_json::Value` |
| snapshot `threshold`、`invert`、`bidirectional`、`laser_min/max_power`、`travel_rate`、`s_max`、`feed_rate`、`passes` | 数字/布尔与 str 混用 | 同上 |
| text_task `generation_options.trim_tolerance` | float 187、int 4 | 同上 |
| calibration `connection.network_timeout` | float 18、int 1 | 同上 |
| workflow `artifacts.image_file` | str 173、null 9 | 可空 |
| `runtime_job` | 空 dict 543、含 job_id 30、只有 last_error 79 | 整条存 payload |

这份清单同时是 Rust 侧反序列化的容错测试用例来源。

## 8.1 NV9 第三方 MCP 持久字段（2026-10-10）

本次没有 ALTER TABLE，schema 仍为 6。`approvals.kind` 新增 `external_tool`，`target_record` 为前缀工具名；隐藏 `connection` 使用 `{mode:"external",server,fingerprint}`，隐藏 request 保存参数、客户端摘要、device_id 和 voice_session_id。外部配置／凭据／策略变化使未消费审批过期；消费与 `outcome.phase=running` 在同一事务，执行期间异常退出后恢复为 failed/unknown，不重试。完成的结果只记受控摘要，不存下游完整响应。

`audit_log.action` 新增 `external_mcp_config_changed`、`external_tool_requested`、`external_tool_completed`、`external_tool_denied`；复用已有审批事件。`app_secrets` 使用 `voice.mcp.<服务>.<env或header>.<字段>.v-<标识>`。普通 settings.json 仅保存引用；先写新版本凭据再原子替换配置，避免跨文件失败使目标与密钥错配。崩溃时可能残留不可达旧版本，暂不做全库扫描删除。

**兼容性**：虽然表版本未变，旧程序不认识新枚举值；回退旧版应用需使用升级前备份。配置和凭据修改、查询字段及限制见 [第三方 MCP 契约](../integration/moss-external-mcp.md)。
