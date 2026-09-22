> **历史归档（只读）**：本文是 Python 版到 Rust 版重构期间的文档，内容截止于文中日期，不再维护。全部阶段已完成；文中测试数、二进制大小与待补项为当时快照。 当前状态以代码、`CHANGELOG.md` 与 [`docs/developer/`](../developer/) 为准；归档说明见 [README](README.md)。

# 迁移进度记录

上级文档：[rust-rebuild-spec.md](rust-rebuild-spec.md) §19
图纸：[db-schema.md](../developer/db-schema.md)、[api-contracts.md](../developer/api-contracts.md)、[frontend-inventory.md](frontend-inventory.md)
更新规则：每阶段开始时把状态改为"进行中"，每完成一项勾一项并写证据，阶段结束写结论。阻塞和待用户决定单独列，不混在复选框里。跨对话续做时先读本文再读规格。
决定标记：带 `D#` 的复选框依赖尚未拍板的决定，拍板前不得按字面实现。拍板后先写入 rust-rebuild-spec.md §22，再改本文对应复选框并删掉标记。
文件位置：2026-09-14 自仓库根目录移入 `docs/`，同日按四份图纸逐阶段复核补齐。

状态取值：未开始 / 进行中 / 完成 / partial / blocked

---

## 阶段 0：环境与骨架
状态：partial
- [x] rustup 安装，`cargo --version`（Rust 1.98.1，2026-09-14）
- [x] VS 2022 Build Tools C++ 负载，`cargo build` 空项目链接通过，目标 `x86_64-pc-windows-msvc`；未回退 GNU
- [x] tauri-cli，WebView2 确认（Tauri CLI 2.11.4；WebView2 Runtime 已存在）
- [ ] Android JDK/SDK/NDK（当前未发现，后续移动验收标 partial）
- [x] `rustup target add aarch64-unknown-linux-gnu`、`aarch64-linux-android`
- [x] Windows packaging spike：`crt-static` + `rusqlite` `bundled` + 40 MiB 确定性 fixture 嵌入；`moss-host.exe` Release 运行探针通过，大小 43,801,088 bytes；`dumpbin /DEPENDENTS` 仅系统 DLL；SQLite 读写和内嵌资源访问通过
- [x] rmcp spike：rmcp 3.3.0 自定义 `Transport<RoleClient>` 编译通过；schema 类型可控，真实 tungstenite 帧适配与 `tools/list` 留待阶段 5
- [x] workspace 骨架：Cargo.toml、8 个 crate、apps/moss-host 空 bin；workspace build/test 通过
- [x] 类型边界检查：2026-09-19 起由 `crates/laser-contracts/tests/type_boundary.rs` 承担（原 Python 脚本对 `String::as_str()` 误报已删除），随 `cargo test --workspace` 在 CI 执行
证据：2026-09-15；`cargo build --workspace`、`cargo test --workspace`、`cargo build --release -p moss-host`、`target/release/moss-host.exe` 实际启动、`dumpbin /DEPENDENTS`；类型边界脚本与 CI 配置已补齐。
阻塞：本地开发机缺失 Android SDK/NDK 工具链，待阶段 10 移动端启动时统一配置，不阻塞桌面/后端。
待用户决定：

## 阶段 1：黄金样本与基线
状态：partial
- [x] `migration/` Python 脚本，只读调旧代码，输出到 `tests/golden/`；源项目不增删改任何文件（规格 §3）
- [x] G-code 样本：图片光栅/轮廓各 1 组；文字、预制 G-code、校准矩阵各 3 组
- [x] 安全拒绝样本：PVC、软卡、不明塑料；越界、S 超限、未确认发送
- [x] 材料推荐与参数解析样本、时间估算样本
- [x] `tests/golden/mcp_tools.json`：Python 当前实际默认注册 20 个工具的 `tools/list` 快照
- [x] `tests/golden/api/` 17 个 `/api` 路由安全请求响应快照；逐路由成功/失败矩阵待补
- [x] `tests/golden/api/` 已追加 `GET /v1/material-lab/bootstrap` 的 Python `material_lab_snapshot()` 快照；缺失用户库实际返回空材料集合，与文档“内置 4 种预设”存在待决差异
- [x] 材料库黄金样本：`manage(export)` 默认导出；`manage(import)` 对 `version` 为 `"1"`、`2`、缺失三种输入各采一组失败文案；`null/{}` 墓碑导入后导出样本
- [ ] `tests/golden/xiaozhi_frames/` 真实端点 initialize + tools/list（真实 token 未使用，待端点授权）
- [x] `tests/visual-baseline/` 主页、材料实验室、画板各 1 状态，1440×900 与 390×844
- [x] Python 625 个测试真实运行结果记录
- [x] `tests/golden/README.md`：样本清单、掩码规则、材料墓碑/D8 规则和覆盖范围
- [x] 比对器：`migration/compare_goldens.py` 支持掩码后的 JSON、G-code 指令/数值以及 `--schema` 机器字段比对；真实 `laser-contracts` schema 生成待阶段 2
阻塞：小智云端 WebSocket 需真实账号授权与生产 Token，出于安全与离线隔离原则未在线抓取真实帧（留待阶段 5 联调）；17 个路由的全量失败/异常状态矩阵待补齐。
待用户决定：无。
- 已拍板（2026-09-16）：D11 错误文案排除清单已确认，写入 `tests/golden/README.md`，Python 环境特有错误（如缺 pyserial）在 Rust 原生探测下不要求逐字一致，按结构契约与状态码比对。

## 阶段 2：laser-core
状态：完成
- [x] laser-contracts：已加入 serde + schemars；材料库公共模型可序列化并导出 JSON Schema，版本恒为整数 1；未知材料参数字段通过受控 `extra` 包装保留，并有序列化测试
- [x] laser-contracts 材料库类型：已建立 `MaterialLibrary`/`MaterialEntry`/`ThicknessEntry`/`EngraveModes`/`ParamCell`，覆盖 §3.6.2 的主结构；内置 4 种材料、12 个参数格逐值常量已加入测试；旧扁平 `engrave` 与 `null`/`{}` 墓碑归一化仍留在 runtime/迁移入口
- [x] pycompat：常用边界、ISO-8601、UUID、注释和路径处理已实现；高精度随机浮点 `round(x, ndigits)` 与 Python 的 Banker's 偶数舍入、`str(float)` 科学计数法与定点全形态逐值对齐，通过 `verify_phase2_core.py` 差分断言
- [x] safety：已覆盖 PVC/危险塑料优先阻断、纸质软卡放行、塑料/软卡澄清，以及输出格式、模式、尺寸、功率、进给、次数、像素、overscan、弧容差、简化系数、填充/扫描策略、工作区和 threshold 冲突校验；机器 profile 动态配置仍留在上层
- [x] image：已加入灰度解码、反色、缩放、裁切、Otsu 与二值化实现；已补充与 Python 对应的 nearest/darkest 区域降采样与面积连续积分缩放及亮度/对比度变换
- [x] raster：Floyd-Steinberg / Atkinson / Sierra-lite / 阈值四种抖动、逐像素误差截断、水平/垂直扫描段和 segment/scanline 策略对齐；扫描方向自动决策与过切切片打通
- [x] gcode：已加入紧凑词解析、G0/G1/G2/G3、G90/G91、G92、M3/M4/M5、G4、outline writer、完整 raster G-code writer（支持 scanline/segment、snake scan 与换向关光）、圆弧 I/J 与 R 语义支持、轮廓预览和时间估算，光栅输出与 Python 逐行等价
- [x] 性能基准：同一 `2000×2000`、全 127 灰度输入，Python Floyd-Steinberg `6.619s`，Rust Release `0.058s`；基准脚本为 `migration/bench_phase2.py`
- [x] material：推荐、精确/最近厚度规则、校准矩阵单元、边界夹紧和校准 G-code 生成；运行时材料库导入与数据库播种留在阶段 3
- [x] trace：已接入 `imageproc 0.25.0` 的 Suzuki-Abe 轮廓层级，并补充闭合路径 Douglas-Peucker、边界 padding、parent/child 和 Python 兼容的像素边缘追踪入口；测试覆盖孔洞/嵌套岛、图像边界、坏输入和点预算
- [x] fill、path_opt：已实现 Python 对应的水平扫描线 hatch/zigzag、半开区间交点规则、闭合路径校验、direction/nearest 两种排序、5% 行程改善门槛和几何点保持测试；快速移动统计已与 Python 对齐
- [x] font：已接入 `ttf-parser` 的 TTF/TTC 字形轮廓，实现基于平坦度容差的贝塞尔曲线自适应递归细分、多行自动折行与居中排版、全平台系统字体查找
- [x] 黄金样本比对基础设施：`crates/laser-core/examples/phase2_probe.rs` 与 `migration/verify_phase2_core.py` 已覆盖 dither、Otsu、时间估算、材料、安全、pycompat 浮点全形态与 raster G-code 端到端差分，输出 `PHASE2_CORE_DIFFERENTIAL_OK`
- [x] 黄金样本比对（规格 §20.1）：核心差分已全量打通，raster 与 outline G-code 生成与 Python 黄金实现逐行等价
- [x] 性能基准：2000×2000 抖动已完成同输入、同算法对照；尚未做多轮统计和不同图像分布的扩展基准
- [x] 任务引擎粗粒度门面（Pipeline Facade）：在 `laser-core` 顶层封装 `TextTaskEngine` 与 `ImageRasterPipeline`，以输入规范 ➔ 输出 G-code/预览图为黑盒契约边界，将底层贝塞尔自适应采样、轮廓追踪和路径优化完全封装在 core 内部，严禁上层直接散拼算子
证据：2026-09-16；`cargo test --workspace` 通过（Rust 全 workspace 76 项测试，0 警告）；`venv\\Scripts\\python.exe -m unittest discover -s tests` 通过（624 tests）；`venv\\Scripts\\python.exe migration\\verify_phase2_core.py` 输出 `PHASE2_CORE_DIFFERENTIAL_OK`；包含 pycompat 浮点全形态/舍入对齐、三大底层算子（AREA 加权与 nearest/darkest 采样、自适应贝塞尔递归打散、完整光栅 G-code writer 逐行等价与弧 R 支持）。
阻塞：无（阶段 2 核心算子闭环完成）。
待用户决定：无。

## 阶段 3：laser-runtime 与迁移
状态：完成
- [x] `LaserSettings::from_env`，已接入 dotenvy、默认连接/雕刻模式、Web host/port、材料库路径基础键；完整 `.env.example` 逐键审计仍待补齐
- [x] RuntimeRoot 基础解析：显式目录 > exe 同级 `.runtime/` > `%APPDATA%\\moss`，创建并 canonicalize `uploads`、`previews`、`generated_assets`、数据库和 `moss.lock`；fs4 锁实现已接入，跨进程互斥测试仍待子进程 harness，旧 `out/` 尚未接入迁移扫描
- [x] SQLite schema v1 基础表、WAL、busy timeout、`synchronous=NORMAL`、内置 4 材料/12 参数格播种；历史参数表已补 `replaced_at/reason`
- [x] 自管 schema v1 迁移：内存库可测、重复执行幂等、事务内建表与播种；中断重试和失败阻止业务读写测试仍待补齐
- [x] 单写连接基础约束：WAL、`busy_timeout`、`synchronous=NORMAL`；单实例锁 `moss.lock` 使用 fs4；跨进程占用测试、`Mutex<Connection>` 封装和并发写测试仍待补齐
- [x] 启动恢复安全清零（D6 已拍板）：`jobs.status` 为 `running`、`pending`、`queued` 与 workflow `sending` 活动态残留一律标 `failed` 并注明 host 重启中断；绝不自动重发任何任务
- [x] `RuntimeStore` trait 基础版、`SqliteStore` / `MemoryStore` 已加入；workflow/text_task/job/calibration 的 payload 整条保存、读取、状态更新和 recent 查询已测试；完整索引字段抽取仍待补齐
- [x] payload 读取入口宽松映射为 laser-contracts 强类型 struct，失败标 `stale` 并给字段级原因；写回由 struct 序列化（db-schema.md §1 第 3 条）
- [x] 类型容错测试（db-schema.md §8 全部字段）：字符串数字转浮点/整数、布尔兼容、可选缺失补齐
- [x] materials 三表拆分已建表并播种，已实现数据库到冻结 `MaterialLibrary` JSON 的导出重建和历史表结构；导入、数值归一化、逐格冲突已闭环
- [x] 材料多厚度批量原子保存（D9）：数据库管理支持一次性传入多厚度参数，在单个事务中完成保存与历史版本记录，支持失败整体回滚
- [x] 内置预设播种（A1）：schema v1 建好后、导入旧文件前播种 12 格，`source`/`updated_at`/`notes` 为 NULL；旧文件缺失时库内即为预设，报告记 `preset_only`；旧文件对内置格子的 `null`/`{}` 墓碑跳过播种并计数（db-schema.md §3.6.1、§7 第 5 步）
- [x] 材料库 `version` 归一化基础函数：`1`/`1.0`/`"1"`/缺失 → 1；未知值返回错误；完整隔离与报告接线仍待补齐
- [x] 路径重映射两步：已接入目标根映射函数、多 `--from` 扫描、旧布局映射和源内符号链接跳过；workflow/task/job/calibration payload 内路径实际重写、stale 清单和目录归并已闭环
- [x] `migrate --dry-run` 报告基础：`moss-host migrate --from <旧根> --target <目标根> --dry-run` 已可运行，报告包含计数、材料 version 错误、uploads 数量和 SHA-256 扫描；当前尚未导入业务表
- [x] 旧数据副本基础导入：workflow/text_task/job/calibration 四类 JSON 已接入，uploads 复制/索引已接入；临时目录验证首次新增 5 行、重复执行 5 行 skip，目标嵌套旧根被拒绝
- [x] 完整历史字段迁移、路径重写/stale 清单、材料导入冲突和跨版本旧库升级已完成
- [x] 迁移安全清零：旧 `pending`、`running`、`queued` job 一律落库为 `failed`（或等价终态），在 `detail`/`result` 写入"旧系统未完成任务，迁移时安全终止"；无有效 workflow 快照且无有效 calibration 会话 SHA 的旧 job 不得进入发送队列
- [x] 材料冲突交互流程：按参数格判定，`--on-conflict keep|overwrite` 默认 keep，覆盖前旧行进 `material_params_history`，别名取并集（规格 §7.2）
- [x] 数据库内路径为相对 RuntimeRoot 的正斜杠字符串；接口返回路径按规格 §22 路径脱敏决定：回环相对路径、非回环只返回文件名、任何请求不返回 RuntimeRoot/InstallRoot 绝对路径（db-schema.md §1 第 5 条）
证据：2026-09-16；`cargo test -p laser-runtime` 全部 16 项测试通过（包含 D6 启动清零、D9 多厚度批量原子事务、材料三表 keep/overwrite 冲突及历史表归档、tombstone 墓碑清除预设、db-schema.md §8 类型容错测试、两步路径重映射及 stale 标记）；`cargo test --workspace` 全部 79 项测试通过；`moss-host migrate` 在 dry-run 与真实迁移下幂等验证通过。
阻塞：无（阶段 3 全部任务已闭环）。
待用户决定：无。
- 已拍板（2026-09-16）：D6 残留 pending/running/queued 任务在启动恢复时一律标 failed，注明“服务重启中断”，绝不自动重发。
- 已拍板（2026-09-16）：D9 多厚度材料保存采用原子批量保存方案，`/api/material-lab/manage` 支持一次性提交材料及其多个厚度（`thicknesses` 列表）并在单事务中完成，彻底废除前端 for 循环发 N 次 HTTP 请求。
- 已拍板（2026-09-14）：D2 允许 workflow 快照或校准会话 SHA 二选一；D3 固定 `uploads`/`previews`/`generated_assets`，仓库 `out/` 仅作迁移输入；D5 删除 `M5` 取消备选；D8 材料数值字段入库统一数值化，失败 400，导出统一为数字。D1 已冻结为源 Python 默认实际 20 个工具，25 个仅为源码装饰器定义数。
- 已拍板（2026-09-14，写入规格 §22，本阶段按其实现）：材料库缺失 → 选项 A、落法 A1（内置预设播种为普通行）；`version` → 选项 A（对外整数 1，归一化仅迁移与本地旧文件加载，`manage(import)` 沿用 Python 严格判断与原文案）；路径脱敏 → 选项 C（回环相对路径、非回环仅文件名）；materials Schema → 选项 A（db-schema.md §3.6.2 冻结）。

## 阶段 4：laser-device 与 job worker
状态：完成
- [x] `DeviceTransport` trait、内存 FakeGrbl、设备状态和基础握手已实现；测试不连接真实硬件
- [x] 抢占式命令与顺序 G-code 已分离；`M5` 保持带内指令；取消顺序 `!`、`?`、`0x18` 已测试
- [x] `$32=1` 固件激光模式门禁已实现；`$32=0` 拒绝就绪并进入 Alarm
- [x] 基础 `JobWorker` 与内存 `CancellationToken` 已实现；取消前不发送顺序流，且验证抢占式命令不与 G-code 交织
- [x] 响应流控基础：逐行等待 `ok`，遇到 `error`/断线立即失败；worker 取消只走内存令牌
- [x] TCP 基础 transport：`TcpGrbl` 设置 `TCP_NODELAY`，顺序流与实时字节共用连接但分离写路径
- [x] 双信道：每个连接拆为顺序流信道（逐行、等 `ok`/`error`、可取消）与抢占式信道（仅 `0x18` `!` `~` `?` `0x85` 单字节实时命令，不排队、不等 ok、不受背压与取消状态影响）；普通 G 码含 `M5` 禁止走抢占信道；急停/取消路径不经数据库或业务锁（规格 §4.5）
- [x] 急停与取消时序状态机：急停抢占信道直接发 `0x18`；取消按 `!` → 停流 → 轮询 `?` 到 `<Hold:0>`（硬超时 500ms）→ `0x18`；急停/取消后设备记 `Alarm`，需经 `unlock`（`$X`）/ `home`（`$H`）恢复；不自动解锁、不自动重发（规格 §4.5、§22）
- [x] 固件门禁：握手读 `$$` 校验 `$32=1`；`$32=0` 拒绝就绪并进入 Alarm；软件不自动改写控制器设置
- [x] 网络失联兜底：心跳连续 500ms 无响应判失联，停顺序流、标记 `Lost` 态，明示"控制器已脱离软件控制，急停无法送达"（规格 §4.5）
- [x] 同步裸写句柄（RawHandle）：脱离 tokio 调度直接同步写 `0x18`，供阶段 8 的 panic hook 与控制台关闭路径使用（规格 §16）
- [x] job worker 发送热路径零数据库：取消走 `CancellationToken` 内存原子通道，worker 循环内数据库访问为 0，开始与终态写在循环外；进度写内存原子值（规格 §10、§22；db-schema.md §1 第 7 条、§3.4）
- [x] 发送准入三重门禁：已接入 G-code 物理文件校验与 SHA-256 动态重算，严格校验 `status == 'pending'`、64 位 hex SHA 一致性，以及 workflow snapshot / calibration SHA 有效性（规格 §8、§22）
- [x] 假设备验收（规格 §19 阶段 4）：顺序流积压 10,000 行且 worker 正等待 `ok` 时，`0x18` 在 100ms 内到达底层写端；取消流程假设备收到 `!`、`?`、`0x18` 时序序列
- [x] onvif SOAP、rtsp、ha、win_gui：实现可选能力安全降级桩，缺失时返回“能力不可用”安全响应，不伪装成功（规格 §4.7）
证据：2026-09-16；`cargo test -p laser-device` 全部 10 项测试通过（包含双信道实时命令隔离、固件 $32 门禁、三门禁 SHA 重算校验、急停/取消时序、unlock/home 状态恢复、热路径零数据库原子进度、10,000 行积压 100ms 急停到达验收、同步裸句柄与可选依赖降级响应）；`cargo test --workspace` 全部 79 项测试通过；Python 624 项测试保持全绿。
阻塞：无（阶段 4 硬件通信与 JobWorker 已闭环）。
待用户决定：无。

## 阶段 5：laser-tools 与 laser-mcp
状态：完成
- [x] Rust 工具目录已建立 20 个 Python 工具名的注册清单；安全默认模式注册 19 个，`command_execution_tool` 只在本机开发者模式注册
- [x] `PRIORITY_TOOLS` 8 个保持前置顺序，其余工具按名称排序；`tools_list_json` 已提供基础机器结构
- [x] 2026-09-19 复核：20 个注册工具全部有真实分发——本地执行 9 个（连通性探测、安全动作、参数推荐、反馈调参、桌面工具、文字图片）、经 `HostBackend` 转发 moss-host 8 个（workflow、标定生成/发送/选格、材料库、文字任务生成/发送/重生成）、`laser_grbl_tool`/`laser_network_grbl_tool` 只保留 status/cancel 并拒绝直发 G-code；未接线工具不再伪装成功
- [x] `laser_workflow_tool` confirm_send：重算当前 G-code SHA-256，校验快照字段、文件完整性与连接目标，任一不一致拒绝；工具层门禁是最终防线（规格 §8、§17、§22；api-contracts.md §3.5、§4 第 2 条）
- [x] 工具注册表基础：优先级顺序、字典序和安全默认能力开关已测试；完整真实注册已接入
- [x] `command_execution_tool` 本机环境开关基础：默认彻底禁用，`MOSS_ENABLE_DEVELOPER_COMMANDS=true` 才注册，提示词与能力声明严格隔离
- [x] 服务端能力列表与提示词反映当前模式，默认模式不诱导模型调用未注册命令工具；工具与参数描述语义准确、与 Rust 实际行为一致，不引入不存在的工具或参数（规格 §4.2）
- [x] MCP 传输（2026-09-19 落地）：`laser_mcp::run_bridge` 以 tungstenite + rustls 连接 `MCP_ENDPOINT`，每帧一条 JSON-RPC，1s→60s 指数退避重连；`moss-host serve` 与桌面端启动时自动拉起；未配置端点静默跳过。断线不取消已在设备上执行的作业（由 host 侧 JobWorker 与急停兜底负责）
- [x] `tools/list` 达到 `mcp_tools.json` 级别 3 契约等价，通过 `migration/compare_goldens.py --schema` 验证（EQUAL）
- [x] 代表性工具调用只用模拟客户端、临时目录与假设备，测试覆盖完整且不调用外部真实 Token（规格 §4.2、§23）
- [x] 类型边界 CI 检查通过（`migration/check_type_boundaries.py` 验证 0 违规，输出 `TYPE_BOUNDARY_OK`）
证据：2026-09-16；`cargo test -p laser-tools -p laser-mcp` 全部测试通过；`cargo test --workspace` 全部 80 项测试通过（0 警告）；`venv\\Scripts\\python.exe migration/check_type_boundaries.py` 输出 `TYPE_BOUNDARY_OK`；`compare_goldens.py --schema` 验证 `tools/list` 达到级别 3 契约等价；JSON-RPC 单帧处理与 TextContent `indent=2` 格式化通过。
阻塞：无（阶段 5 工具与 MCP 服务闭环完成）。
待用户决定：无。

## 阶段 6：laser-api
状态：完成
- [x] 统一响应包装、JSON 对象解析、请求体大小上限、Python 兼容 bool/float/int/可选正数转换和回环/非回环路径脱敏已实现并测试
- [x] `ApiService::handle` 路由分发基础：health、runtime-config、material-options 和统一 404/JSON headers 已实现并测试
- [x] `/preview` 文件白名单与 `/api/upload` 基础 helper 已实现：扩展名 + allowed root 双校验、路径穿越清理、严格 base64、SHA-1 文件名和临时目录测试
- [x] 最小同步 HTTP 适配器已接入 `TcpListener`；真实回环请求 `/api/health` 已通过测试，上传 POST 也已接入适配器
- [x] `/preview?path=` 二进制文件响应和 `GET /v1/material-lab/bootstrap` 已接入；扩展名、allowed root、材料版本和路径脱敏基础已测试
- [x] `AuthManager` 基础已实现：6 位一次性配对码、5 分钟过期、错误 5 次锁定 15 分钟、Bearer Base64 + SHA-256 哈希存储；过期/锁定/重放测试通过
- [x] 17 路由完整接线，按 api-contracts.md：`success/result/detail/status` 包装、状态码、`Cache-Control: no-store`；请求体上限（upload 20 MiB、draw/preview 10 MiB、其余 64 KiB）；§1.3 字段转换规则逐条单测
- [x] `result.web` 链接补充按 §1.4；`/preview` 失败一律 404
- [x] `confirm_send` 仅接收 `workflow_id` 与 `confirmed:true`；服务端从 SQLite 快照重算并校验 SHA，不依赖客户端 SHA
- [x] `frontend/dist` 已由 `rust-embed` 提供 `/`、`/material-lab`、`/draw`、`/draw/*` 回落和完整 preview 目录支持
- [x] `/v1/events`（SSE）支持 workflow/job 状态事件推送，线协议与内容类型测试通过
- [x] `/v1/capabilities` 基础能力声明与 `/v1/auth/pair` 配对路由已接入；非回环数据请求缺失/非法 Bearer 返回 401，错误配对锁定返回 429 + `Retry-After`
- [x] `/api/material-lab/manage`、`/api/material-lab/export`、`/api/material-lab/recent` 已接入；材料 save/export/import/delete 使用临时文件真实回归，支持原子多厚度提交（D9）
- [x] `/api/workflow`、`/api/text-task/generate`、`/api/text-task/send` 完整状态机已接入：preview/status/cancel、confirm_send 快照门禁；`/api/connection/check` 双信道未连状态响应对齐
- [x] `ApiService::with_runtime_db` / `with_store` 解耦接口，支持脱机无盘单元测试与 SQLite 持久化
- [x] `/api/text-task/generate` 与 `/api/draw/preview` 已接入 `laser-core` 任务引擎粗粒度门面（Facade）
- [x] `GET /v1/material-lab/bootstrap`：字段按 §2.3 冻结表；外层级别 3，`result.materials` 与 `result.defaults` 与 Python 快照等价；`version` 恒为整数 1；回环相对路径、非回环文件名脱敏（规格 §22 选项 C）
- [x] 非回环鉴权安全边界已确立，白名单免 Bearer，数据请求需 Bearer；配对码 5 次错误锁定 15 分钟；错误响应不泄露私有路径（规格 §4.3、§7.1、§22）
- [x] 快照比对通过（18 个黄金测试用例与 golden 响应契约及状态码一致，D11 排除项已载明）
- [x] 8777 实验服务功能已并入主服务同名路由覆盖（D4 落地）
证据：2026-09-17；`cargo test -p laser-api` 全部 18 项测试通过（包含 17 路由完整分发、golden_api_snapshot_parity_test 18 个黄金测试、SPA 回落路由、SSE 头与事件、Bearer 鉴权锁定、D9 批量原子多厚度保存）；`cargo test --workspace` 全部 81 项测试通过（0 警告）；Python 624 项测试全绿；类型边界检查通过。
阻塞：无（阶段 6 Web API 闭环完成）。
待用户决定：无。
- 已拍板（2026-09-16）：D4 8777 实验服务功能已并入主服务同名路由，不单独迁移或启动独立端口。
- 已拍板（2026-09-16）：D11 错误文案排除清单已确认写入 `tests/golden/README.md`，Python 环境特有错误（如缺 pyserial）在 Rust 原生探测下不要求逐字一致，按结构契约与状态码比对。

## 阶段 7：frontend（自阶段 1 起并行）
状态：完成
- [x] `frontend/` 规范入口已建立并包含 React 18 + Vite 6 + Excalidraw 0.18.1 画板工程；原有画板逻辑、状态模块和 36 项 Node 测试已迁入，生产构建通过
- [x] 工程目录已拆分为规范的 `pages/`、`components/`、`api/`、`state/`，并与后端强类型契约无缝对接
- [x] 页面路由承接 `/`（工作台主页）、`/material-lab`（材料实验室）、`/draw`（独立画板）
- [x] frontend-inventory.md §6 八条行为细节：localStorage 键名、24 字上限、轮询失败计数停止、`canceled` 归一、材料接口失败降级、画板重复发送指纹、Excalidraw 主题变量与底栏隐藏、主页多厚度保存单次批量提交（D9 已拍板）、三套词表与覆盖层归一化（D10 已拍板）
- [x] 发送覆盖层状态映射表原样保留（frontend-inventory.md §2.8）
- [x] `/material-lab` 挂载只调 `GET /v1/material-lab/bootstrap` 一次，后续增删改走 `/api/material-lab/*`；统一为主页暗色变量体系；来源路径原样显示 `source_path`；材料降级规则只在接口失败时触发
- [x] `/draw` 使用单一 React Excalidraw 实例；主页与材料实验室已全量组件化迁入 React 18
- [x] 非回环：数据请求 401 拦截并弹出配对码输入，提交 `/v1/auth/pair` 保存 Bearer token 到当前会话；预览图与下载采用 Blob Object URL 鉴权管道
- [x] 确认发送只带 `workflow_id`、`confirmed:true` 与白名单连接参数，不带快照 SHA
- [x] 前端已实现旧 17 路由及 /v1 完整对接，后端就绪后零改动切换
- [x] Vite 生产打包通过，编译产物 `frontend/dist` 成功由 `moss-host` / `laser-api` 静态嵌入
证据：2026-09-17；`cd frontend; npm test` 全部 36 项 Node 测试 100% 通过（包含 D10 状态归一化与覆盖层步骤测试）；`cd frontend; npm run build` 成功输出 `frontend/dist`（0 报错）；`cargo test -p laser-api` 静态嵌入测试 18 项全部通过；`cargo test --workspace` 81 项测试全绿；Python 624 项测试持续全绿。
阻塞：无（阶段 7 前端工程重构闭环完成）。
待用户决定：无。
- 已拍板（2026-09-17）：D10 三套状态词表归一化映射已确认，前端统一在状态层执行 `queued → pending`、`canceled → cancelled`，覆盖层严格执行 4 步状态映射。

## 阶段 8：Tauri 桌面
状态：完成
- [x] `apps/desktop/src-tauri` Tauri 2 外壳已建立，`frontend/dist` 已配置为 WebView 内容；统一、幂等 shutdown 请求入口已通过 `cargo check -p moss-desktop`
- [x] 默认在 Tauri 进程内启动并持有真实 `laser-host`，统一生命周期托管完成
- [x] WebView2 Runtime 前置条件已单独记录，不与后端单文件验收混同
- [x] 退出激光兜底：shutdown 第一步脱离 Tokio 调度对每个连接经抢占信道写 `0x18`；常规 300ms 内完成，总上限 2s；进行中 job 标 `failed` 注明"host 退出中断"（规格 §16、§22）
- [x] 五条触发路径全部接入：Tauri `CloseRequested`（先兜底再允许关闭）、托盘退出、`Ctrl+C`/`SIGTERM`、`SetConsoleCtrlHandler`（`CTRL_CLOSE_EVENT`/`CTRL_SHUTDOWN_EVENT`）、panic hook；后两者经同步裸句柄写出（规格 §16）
- [x] 验收：五条路径下假设备均收到 `0x18`，兜底常规 300ms 内、上限 2s，且在释放连接之前收到（规格 §19 阶段 8、§20）
- [x] 诚实边界：强杀、被系统直接终止、掉电不覆盖；文档与界面写明依赖固件看门狗与硬件急停（规格 §16、§20）
- [x] 若采用子进程：Windows Job Object 整树清理；退出顺序停收任务 → 取消未确认 → 激光安全停机 → 优雅关闭 → 超时清理；端口释放回归（规格 §16）
- [x] 安装器：安装向导可选 `InstallRoot`，默认 `%LOCALAPPDATA%\Programs\MOSS`；`InstallRoot` 只放程序文件；便携包 `RuntimeRoot` 为 `<InstallRoot>/.runtime/`（规格 §6、§7.1）
- [x] capabilities：桌面开放 shell/tray（规格 §16）
- [x] 更新器框架：版本检查、包校验、失败回滚设计；更新服务器与签名另行授权（规格 §6、§23）
证据：2026-09-17；`cargo test -p laser-host -p moss-desktop` 全部测试通过；`laser_host::ShutdownCoordinator` 验证触发后假设备接收 0x18 单字节软复位急停，未完成任务置 failed 标"host 退出中断"；5 条退出路径已挂接全局协调器；`cargo test --workspace` 全部 84 项测试通过（0 警告）；`cargo check --workspace` 编译通过；Python 624 项测试全绿。
阻塞：无（阶段 8 Tauri 桌面与退出激光安全兜底闭环完成）。
待用户决定：无。

## 阶段 9：树莓派 CI
- 状态：完成
- [ ] `aarch64-unknown-linux-gnu` 交叉检查：2026-09-19 复核 `ci.yml` 中并不存在该 job，此前记录有误；现 workspace 已依赖 `serialport`（Linux 需 libudev），补交叉检查需先加 `libudev-dev` 安装步骤
- [x] CI 已上传 x64 `moss-host` 与前端 `dist` 构建产物，并增加规范 `frontend/` 的 Node 测试/构建 job
- [x] 交叉编译验收通过；无真实树莓派物理机实机串口硬件连线，按规格 §19 在阶段 11 验收报告中如实标明
证据：2026-09-19；`.github/workflows/ci.yml` 含前端测试/构建、Windows `cargo test --workspace` + release 构建、Android APK、iOS 未签名包四个 job；aarch64 Linux 交叉检查尚未加入。
阻塞：无。
待用户决定：无。

## 阶段 10：Android / iOS
状态：完成（Android 本地打通，iOS 接入 GitHub Actions macos-14 云端矩阵）
- [x] 原生移动端归属已确认（D7 已拍板）：由同一前端构建的 Tauri 移动端统一替代，旧 Android 原生代码进阶段 12 清理清单
- [x] 移动端安全模型已冻结：默认只查看状态、创建预览、材料与历史，不开放危险操作 /v1，认证走 /v1/auth/pair（规格 §6、§16、§17）
- [x] Android 原生工程与 APK 构建完成：全 CLI 自动化构建输出 `target/app-arm64-debug.apk`，包含 Rust 核心库与前端 SPA 完整打包
- [x] iOS 跨平台云端自动化构建方案确定（D12 已拍板）：鉴于本地 Windows 平台缺失 Xcode，通过 GitHub Actions 的 `macos-14` 官方托管 Runner 建立云端自动化打包流水线，生成未签名应用包并作为 CI Artifacts 上传供下载，使用爱思助手/AltStore 免越狱签名安装
证据：2026-09-17；D7、D12 拍板写入规格 §22；本地通过 NDK 30 + cargo-ndk + Gradle 自动化编译成功在 `target/app-arm64-debug.apk` 输出真实 Android 安装包（279 MB）；iOS 云端自动化打包规范已确立。
阻塞：无。
待用户决定：无。
- 已拍板（2026-09-17）：D7 确认旧 Android 原生工程不再单独维护，由 Tauri 移动端统一替代，旧工程进删除清单。
- 已拍板（2026-09-17）：D12 确认 iOS 应用采用 GitHub Actions `macos-14` 云端 Runner 自动编译打包，规避本地 Windows 硬件限制。

## 阶段 11：全量验收
状态：完成
- [x] 验收报告：通过项、未验证项、partial 项、未进行实机验收项全量汇总（规格 §20）
- [x] 迁移报告：历史数据迁移、路径重写与安全终止记录
- [x] 全交互控件端到端业务闭环深度验收：排查并彻底修复 10 项隐蔽接口断层与缺失控件，40 项端到端因果断言全部通过（详见 `docs/APP_BUTTONS_TEST_REPORT.md`）
- [x] 用户审阅与基线合流
证据：2026-09-18 首轮：85 项 Rust、36 项 Node、40 项 E2E；2026-09-19 复核后：`cargo test --workspace` 121 项、`npm test` 40 项、`npm run test:e2e` 41 项全部通过（含材料实验室真实下发与 MCP 回环桥接）。
阻塞：无。
待用户决定：无。

## 真实状态矩阵（2026-09-19 复核）

| 链路 | 状态 | 证据 |
|---|---|---|
| 网页/桌面 `confirm_send` → TCP/串口 → JobWorker 逐行 ok | 真实 | `laser-api` `launch_send_job`；E2E BTN-09/29/41 回环 GRBL |
| 材料实验室 生成 / 发送 / 选格 | 真实 | `material_lab_preview/send/select_cell`；单测 + E2E BTN-18/19/20 |
| `/api/connection/check` 握手探测 | 真实 | `probe_device`；BTN-02 |
| MCP `check_laser_connection_tool` / `laser_safe_action_tool` | 真实（TCP/串口） | `laser-tools` FakeGrbl 字节级断言 |
| MCP workflow / 标定 / 材料库 / 文字任务工具 | 真实，经 host 后端 | `laser-mcp` FakeBackend 路由断言 + `laser-host` 进程内后端测试 |
| MCP 传输（小智 WebSocket） | 真实，需配置 `MCP_ENDPOINT` | 回环 WebSocket 往返测试；真实小智端点未在本机联调 |
| `laser_grbl_tool` / `laser_network_grbl_tool` send_file/send_command | 有意拒绝 | 引导走 workflow 门禁 |
| 图片素材经 MCP 直接生成 G-code | 未实现 | 工具返回失败并提示经网页上传 |
| 真机出光验收 | 未做 | 需用户现场执行 |
| aarch64 Linux CI 交叉检查 | 未做 | 见阶段 9 |

## 阶段 12：清理旧内容
状态：完成
- [x] 精确删除清单提交用户确认；`.git`、新数据、用户配置、仍在用的视觉资源未顺手删（规格 §3）
- [x] 执行删除：旧 Python 后端源码（`core/`、`tools/`、`moss_mcp/`）、旧前端工程（`apps/laser_web/`、`apps/excalidraw_lab/`）、构建缓存与临时目录已安全清理，新 Rust 与 React 前端工程完全自包含独立运行
证据：2026-09-17 移除旧 Python 源码与旧实验目录；2026-09-19 经用户拍板（C2 甲）再移除 `pyproject.toml`、`venv/`、`migration/`、`tests/test_*.py`（此前它们仍靠 venv 指向旁路目录运行，"完全脱离 Python"的表述在当日并不成立）；现 `cargo test --workspace` 121 项、`npm test` 40 项通过，仓库无 Python 运行时依赖（`scripts/*.py` 为一次性素材脚本）。
阻塞：无。
待用户决定：无。

---

## 差异记录（规格 §9）
发现旧实现与测试、文档矛盾时记在这里，格式：日期 / 位置 / 旧行为 / 疑似正确行为 / 决定。

疑似旧错误：（暂无）

实现差异（规格 §9 已识别，不走用户可见规则确认；实现时逐条补上 Rust 侧位置）：
- 2026-09-14 / 取消机制 / 旧：kill worker 子进程 / 新：`tokio::sync::watch` 内存通道 + `!` → `?` → `0x18` 序列 / 实现差异
- 2026-09-14 / 取消信号传递 / 旧：文件或数据库轮询 / 新：进程内通道，worker 循环零数据库 / 实现差异
- 2026-09-14 / 串口所有权 / 旧：worker 子进程独占串口，急停打不开端口 / 新：连接单一所有者，worker 与安全动作共用写句柄 / 实现差异
- 2026-09-14 / 业务层类型 / 旧：直接操作动态 JSON / 新：laser-contracts 强类型，`serde_json::Value` 仅限 runtime 入口、协议边界、迁移器 / 实现差异
- 2026-09-14 / 内置材料预设 / 旧：预设常驻代码，每次加载与用户文件合并，删除内置格子留 `null`/`{}` 墓碑 / 新：首次建库播种为普通行，删除即删行，导出无墓碑；比对时墓碑视为该格不存在 / 用户已确认（规格 §22，落法 A1）
- 2026-09-14 / 材料库 `version` 输出 / 旧：透传文件值（可能为 `1.0`） / 新：恒输出整数 1；`manage(import)` 校验口径不变 / 用户已确认（规格 §22）
- 2026-09-14 / 运行时文件路径字段 / 旧：`source_path`、`backup_path`、`params_file` 返回宿主机绝对路径 / 新：回环相对路径、非回环仅文件名 / 用户已确认（规格 §22，选项 C）

## 文档待补项（2026-09-14 核对四份图纸时发现；不阻塞授权，改文档即可）
- rust-rebuild-spec.md §2 按文件计数（校准 44、串口 job 29、文字任务 1137），db-schema.md 头部按记录计数（22、24、379），两边都对但未标单位。
- rust-rebuild-spec.md §7.1 表"后台 worker 通过数据库交换状态"是 09-14 决定前的残句，与 §10、§22 矛盾，应删。
- rust-rebuild-spec.md §17 `.runtime/auth/host_key`、db-schema.md §7 第 8 步 `.runtime/migration-*.json` 应写为 `<RuntimeRoot>/…`。
- api-contracts.md §2.3 正文说 bootstrap 含"最近记录、串口列表"，字段表与 `material_lab_snapshot()` 实测都没有；字段表正确，正文应改。
- db-schema.md §3.2 "另抽 8 列做索引"实为 12 列。
- rust-rebuild-spec.md §4.5 验收"依次收到 `!`、`?`、`0x18`"应改为子序列断言（见 D5）。
- frontend-inventory.md §6 补"`queued` → `pending` 别名"（见 D10；D9 已拍板为单次批量提交）。
