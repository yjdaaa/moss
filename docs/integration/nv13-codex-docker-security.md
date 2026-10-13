# NV13 Codex Docker 隔离与模型 API 代理验收方案

> 状态：2026-10-11，**部分完成 / fail-closed**。本文件是测试证据与下一阶段接口计划，不是生产运行授权。

### 2026-10-12 当前环境重验与退出码修正

用户明确批准启动既有Docker Desktop并运行无账号/无模型/合成文件的隔离测试；实际Docker Server29.8.0已运行。固定官方Linux CLI镜像`sha256:baa32539eac9ecca7831bac5848c0ecab8a0a84986c5822a4d371fffa7d435ac`的0.162.1版本/help、只读/不挂载外部文件、无账号/私有环境、禁网/宿主回环、cgroup/capabilities/Seccomp及停止回收实测通过；Rust的对应opt-in探针在显式设置进程级`NV13_DOCKER_TEST_EXE`后也通过。

固定bwrap镜像`sha256:7e90320dc50afccbd1d036a21e7ee020365182cf599f5b75ac0e839360b150c6`仍失败：unshare exit1/`Operation not permitted`，真实Codex内层沙箱exit1/不能创建namespace。`test-nv13-codex-bwrap-sandbox.ps1`现可写schema1诊断回执，blocked或仅基本正向read都抛出失败，不再打印BLOCKED却退出0；negative_probes明确not_run、production_ready=false。两个CLI脚本核对固定镜像digest，并按不可变ID启动，避免标签漂移。清理只针对自身测试容器与新建临时目录。

当前[OpenAI官方沙箱说明](https://learn.chatgpt.com/docs/sandboxing)确认Linux/WSL2依赖bwrap/无特权user namespace；不能用文档代替本机验收。本轮未改变Seccomp、CAP_SYS_ADMIN、privileged、内核/全局AppArmor或Docker安全设置；`docker sandbox --help`报告该命令已移除，也不能当现成替代运行时。真实运行器与模型代理保持DisabledRunner。Windows0.157.1计划与Linux0.162.1测试版本不同，未把一方能力记录混用为另一方验收。

## 1. 本轮已验证的软件事实

| 项目 | 结果 | 证据 |
| --- | --- | --- |
| Windows Codex `:read-only` | 失败：工作区外测试文件可读，Windows 回环端口可连接 | 历次 NV13 负向测试记录 |
| Docker 外层隔离 | 通过：非 root、只读挂载、无默认路由、禁写、无宿主机凭据与 Docker socket | `test-nv13-linux-codex-image.ps1` |
| 官方 Linux Codex 0.162.1 | 镜像 `moss-nv13-codex-test:0.162.1`、只执行版本/help，未调用模型 | 官方 SHA-256/离线构建脚本 |
| bubblewrap 包 | Ubuntu 官方 APT 签名源安装 `0.6.1-1ubuntu0.3` 到派生测试镜像 | `build-nv13-codex-bwrap-test-image.ps1` |
| Codex 内层 Linux 沙箱 | **BLOCKED**：`unshare --user --map-root-user` 返回 Operation not permitted；bwrap 报无权限创建 namespace | `test-nv13-codex-bwrap-sandbox.ps1` |
| 临时 Docker 内部 HTTP 路径 | 通过：客户端收到假代理的人工 HTTP 200 回复；无外网默认路由、未到达测试用 Windows 监听器 | `test-nv13-internal-model-gateway.ps1` |
| 真实模型 API/认证/MCP | **未验证**：没有账号、密钥、真实模型请求或受控公网出口 | 本轮明确测试限制 |

派生镜像 `moss-nv13-codex-bwrap-test:0.162.1-jammy` 的构建时 SHA256 image ID 为 `sha256:7e90320dc50afccbd1d036a21e7ee020365182cf599f5b75ac0e839360b150c6`。mock 网关镜像 `moss-nv13-fake-api-gateway:jammy` 为 `sha256:fed673ea01303bc787097fc8a54ea4ced2a3c886e878978293bd64f7e57d3b6f`。均为**测试用镜像**，不应作为生产模型代理。

## 2. 建议生产隔离边界（尚未实现）

1. **可信审批入口**：只接受现有 NV13 `AgentRun` 单次桌面批准及当前有效 voice permit；任务摘要、工作目录、镜像 digest、只读参数、策略版本、会话 ID 必须在批准前后严格一致。模型不能更改路径、额外参数、网络模式或容器安全选项。
2. **一任务一容器**：新建随机命名的独立 Docker 内部网络和最小工作区挂载。默认非 root、`--read-only`、只读工作区、`--cap-drop ALL`、`no-new-privileges`、默认 Seccomp、pids≤8、CPU/内存/输出/运行时限、无 Docker socket/用户目录/凭据/设备映射。Windows 目录必须先做 canonical 与 reparse 检查。拒绝超出明确工作区的任何宿主机挂载。
3. **唯一代理入口**：Agent 只连本任务内网中的可信模型代理（无任何公网默认路由）。代理可拥有独立受限出口，但不能暴露任意 HTTP CONNECT/转发，必须按实际已验证的模型 endpoint 做精确 host+port+path+method 白名单；默认拒绝重定向、任意 URL、DNS 重绑定、IP 直连、私网/环回/link-local/元数据地址以及额外域名。TLS 证书验证和服务器名称校验不能关闭，限制请求体、响应体、连接、流量、金额与时间。
4. **密钥只留代理**：OpenAI/ChatGPT 长期认证资料不能写入 Agent 镜像、挂载、环境、argv、stdin 或日志。代理持有自己的最小授权认证方式；Agent 只能拿到按 run_id、设备、模型、允许端点和短时 TTL 绑定的**一次性代理令牌**（它可能被 Agent 内代码读取，所以不能视为长期秘密）。取消、权限撤销或失联须立即撤销令牌和代理出口。认证方式需与当前 Codex CLI/provider 配置兼容性核实，不能假设 Windows 登录可直接转用。
5. **Codex 配置/工具禁入**：从单独空的 `CODEX_HOME` 启动并显式加载可信配置；不继承 Windows `~/.codex`、插件、MCP 服务、shell 函数或外部环境。配置自定义 provider 的 `base_url` 指向私有网关；模型供应方的真正密钥只能由代理注入。Codex `model_providers.<id>.base_url`、`env_key`、`wire_api=responses` 需按实际 CLI 版本重新校验。不能默认信任项目自己的 `.codex` 或 AGENTS.md 能力。
6. **退出不可重放**：审批消耗与 run 插入使用现有事务；任务取消/撤销/超时触发 Docker kill+wait+网络删除+代理令牌撤销；重启后 running → `result_unknown`，不自动重新执行。不得只结束父进程而留下容器/代理。所有对外行为须审计且有界，任何失败默认关闭。

### 双层沙箱决策

在当前 Docker 默认 Seccomp + 非 root + no-new-privileges 下，Codex 0.162.1 的 bwrap **无法创建 user namespace**。不要用 `--privileged`、`CAP_SYS_ADMIN` 或 `seccomp=unconfined` 让测试变绿。若强制要求 Docker+Codex 双层沙箱，必须在独立授权的沙箱 VM / 支持 user namespace 的运行时上重做完整负向验收。若拟由 Docker 外层承担唯一 OS 沙箱，须先完成以上外联和文件防护安全评审；不能直接解锁真实 Codex。

## 3. 后续验收门槛

- [x] 官方 Linux Codex 版本/help 与文件 SHA-256 校验，无账号/模型调用
- [x] 仅新建假文件的 Docker 外层只读、禁写、未挂载目录不可见、禁网、8 进程与退出回收
- [x] 官方 Ubuntu bwrap 软件包安装
- [ ] 内层 bwrap user namespace 可用（当前 BLOCKED；不降低 Docker 安全配置）
- [x] 临时内部网络假 HTTP 代理可达、无外网默认路由；宿主机测试监听器不可达
- [ ] 真实代理严格 URL 白名单、TLS、DNS/IP 防绕过及不允许其他公网出口（未实现）
- [ ] 真实代理隔离认证/密钥不在容器中、令牌撤销/费用/请求限额（未实现）
- [ ] MOSS 设备身份→单次桌面批准→真实任务生命周期与撤销/崩溃/重启 E2E（未实现）
- [ ] Codex MCP/插件禁入与间接命令/意外文件读取负向测试（未实现）
- [ ] 如用户进一步批准，使用专用低权限真实账号验证只读任务；本轮**未获授权**
- [ ] NV18 专用安装包、小智设备收放音与自然打断、激光机安全急停，依原约定放最后实机验收

**当前生产门禁必须保持**：`REAL_CLI_SANDBOX_VERIFIED=false`，`DisabledRunner`，GUI 的 `enabled:true` 拒绝。所有测试镜像、容器和内网不能替代已审批的生产执行器。

### NV13 模型代理 HTTP 安全核心（2026-10-11：离线 mock，仅软件验收）

新增 `crates/laser-host/src/voice_agent/model_proxy.rs`。该 Rust 模块在所有构建中都**没有真实网络上游或凭据**，生产 `AgentCoordinator` 未注册它；只在 Rust 测试中通过系统随机临时回环 TCP socket 验证真实 HTTP 字节往返。它不代表可用的 Codex API 代理。

- 严格接受唯一的 `POST /v1/responses HTTP/1.1`、`Host: nv13-model-mock`、`Content-Type: application/json`，以及只含 `model=nv13-fixture-model`、`input`、`stream=false` 的 JSON。拒绝任意 CONNECT、其他 Host/路径/方法/绝对 URL、Request Smuggling、重复 Content-Length、Transfer-Encoding/Upgrade/额外头、任意工具/目标 URL 字段、其他模型与流式响应。
- 可信侧使用 OS CSPRNG 签发 32 字节一次性授权令牌；只储存 SHA-256 摘要，按 `run_id + device_id + 许可 AtomicBool` 验证。TTL 最大 30 秒、最多 16 个并发授权、每个 run 至多一个未消费令牌，校验成功后原子消费；撤销、到期、身份伪造或并发重放会拒绝。
- HTTP 最大 16 KiB，Headers 最多 8 KiB、Body 最多 4 KiB、合成 input 最多 500 字符、回复最多 8 KiB；有界审计（最多 32 条）仅保存固定状态及 run ID 哈希前缀，不记录 prompt、设备 ID、Bearer 或原始 HTTP。唯一返回 `NV13_FAKE_MODEL_REPLY_ONLY`，`usage` 固定为 0，**不是实际模型计费**。
- 8 项 Rust 负向与并发/撤销/单次消费/真实 loopback TCP 测试通过，`cargo check -p laser-host` 通过。生产仍 `REAL_CLI_SANDBOX_VERIFIED=false` + `DisabledRunner`。
- **仍未实现的正式能力**：出站 HTTPS/TLS/CA、API 端点 host/path/method 白名单及 DNS/IP 防绕过、可信账户密钥注入、实际费用/流量配额、流式取消、Docker 运行时挂接、审批 token 授权与应用崩溃恢复。假路由的安全不能外推到这些网络能力。

本轮完整软件回归：Rust 537/537、前端单测/构建、FakeGrbl E2E 32/32、Windows release EXE/普通 NSIS 和 Android aarch64 APK 均有成功回执，Android APK 嵌入的 Rust 库与当轮编译库 SHA-256 一致。产物哈希和未验收项见 CHANGELOG；普通 NSIS 非 NV18 专用安装包。

### 2026-10-11 NV13 TCP→单次审批令牌→固定 TLS 假上游完整软件链路

新增 `model_proxy/transport.rs`、`model_proxy/egress.rs`、`model_proxy/tls_client.rs`、`voice_agent/docker_plan.rs`，**仅软件夹具与内存信任边界，不挂接 MOSS 生产 `AgentRunner`**。

- **真实 TCP 数据面（合成）**：只在单测绑定系统分配的 `127.0.0.1:0`，`Content-Length` 逐帧上限、不得复用/管线化请求、2 秒慢请求总时限、80ms 取消轮询、8 连接上限，server 等待读取结束会回收全部连接 worker。不能由此声称生产开放端口或 Docker 隔离网络已接线。
- **HTTPS 纯出口校验**：固定 `POST https://api.openai.com/v1/responses`、TLS SNI `api.openai.com:443`；模拟 DNS 答案不能有任一环回、私网、CGNAT、链路本地、云元数据和保留 IPv4，IPv6 暂统一拒绝。拒绝混合安全/不安全解析、任意绝对 URL、改写 Host/路径/方法、重定向和超大 JSON。真实 DNS 解析与真实公网出口未运行，IP 白名单不能代替真正防火墙。
- **真实本机 TLS Mock**：使用 rcgen 的进程内生成证书和 rustls TLS 测试服务；客户端 reqwest 显式只准 HTTPS、不使用环境代理、不跟随重定向，预固定解析结果到临时 loopback socket，却仍以 `api.openai.com` 验证证书主机名。测试正确证书握手、错误主机名、302 到 loopback 不跟随、错误响应类型、128 KiB 上限、撤销前拒发及发送后弃结果。异步版本 25ms 取消轮询，revocation 后快速终止请求的等待并丢弃响应；**已传出上游字节无法撤回，不代表费用能取消**。
- **完整合成链路**：真实 TCP 客户端→HTTP Frame reader→授权校验 (run/device/实时 permit/一次性令牌)→固定假 HTTPS 模型服务→有限响应→client；对照负向测试，已经消费的授权不能重放；TLS 等待期间撤销后结果不会返回。只使用人工 `NV13_...` 假数据和假密钥，未登录、未向 OpenAI 发模型请求。
- **固定 Docker 安全计划**：`docker_plan.rs` 只容许准确匹配官方安装镜像 SHA256 的单次离线 `--version/--help/exec --help` 命令，不允许 shell、模型 prompt、账号/真实工程或任意挂载/网络；唯一可绑定的是含纯合成 canary 的随机临时目录，明确禁网、只读、非 root、cap-drop all、pids<=8、CPU/内存限制、唯一容器标签；Windows canonical `\\?\` 前缀只在已核验后转换为合法 drive path。Rust 特定环境下**实际 Docker `--version` 探针通过**，按唯一测试标签回收，不算真正 Agent 执行。
- **真实场景的剩余阻塞**：审批/运行器对接、内存令牌与进程隔离、可信账户密钥管理、真实公网 DNS/TLS/CA/IP pinning、防跨请求计费、MCP 禁入、真实模型协议和成本预算、Docker userns (bwrap 已知失败)、崩溃恢复与设备撤销的跨容器 E2E。模型 API 只在模拟 TLS 下验证，不能把 mock 当正式连通性；`REAL_CLI_SANDBOX_VERIFIED=false`，`DisabledRunner` 原样保留。
## 4. 可重放的测试脚本（项目根目录执行）

```powershell
.\scripts\build-nv13-codex-bwrap-test-image.ps1
.\scripts\test-nv13-codex-bwrap-sandbox.ps1
.\scripts\build-nv13-fake-api-gateway-image.ps1
.\scripts\test-nv13-internal-model-gateway.ps1
```

上述安装脚本依赖本机 Docker 和 Ubuntu 官方签名 APT 仓库；运行时验证禁止互联网连接。任何下载/包版本变更须核验签名并记录镜像 ID。模型 API 认证的设计依据：OpenAI Codex 配置文档关于自定义 provider base_url/env_key，及 OpenAI 沙箱安全指南关于把长期密钥留在可信代理端；此处仅作为待实现设计，而非声称当前程序已支持。

- https://developers.openai.com/docs/config-file/config-reference
- https://developers.openai.com/api/docs/guides/agents-api/environments/security
