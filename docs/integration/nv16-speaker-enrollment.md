# NV16 本地声纹登记与验收边界

本轮实现固定模型、协议、独立进程、三段登记、私有管理和删除；并实现当前句身份快照、按人历史/摘要隔离及桌面适配器。**生产身份识别与按人记忆注入仍关闭**：`Calibration=None`，真人标定缺失时不能由设置开关开启。不以姓名或向量授予加工、家居或 CLI 权限。进度以 [实施 TODO](moss-fusion-implementation-todo.md) 为准。

## 模型与协议

- 模型 `campplus_sv_zh_cn_16k_common` 对应 [官方模型卡](https://modelscope.cn/models/iic/speech_campplus_sv_zh-cn_16k-common) v1.0.0，卡片声明 Apache License 2.0。
- 官方文件 `speaker-recongition-models/3dspeaker_speech_campplus_sv_zh-cn_16k-common.onnx`（recongition为上游拼写）；实际28,281,138字节、SHA-256 `f682b514c05d947ee3fa91cd6ec6c5c7543479a128373fa29b1faedccd21fd11`。
- 16kHz单声道i16 PCM、2～10秒；实际输出192维有限、非零f32le向量。协议4必须核对模型、采样率、维度、代次和编码；旧helper握手拒绝，须与MOSS一起升级。
- `SpeakerHelper`持有专用进程，1.5秒超时只作废声纹进程，不终止ASR/TTS/VAD或等待快速停止。
- Windows加载保持模型及规范化祖先目录只读句柄，禁止写入/删除共享；哈希后原生打开和模型存活期间不能替换。其他平台声纹安全加载尚未验收，明确拒绝。

## 用户操作与接口

语音中心下载模型后，在“语音隐私设置”选择当前对话设备、输入姓名并确认。90秒内采集三段2～10秒语音；24/48kHz上行重采样为16kHz。原始录音不落盘，登记与ASR并行。只有固定 `voice-private` 打包窗口能调用 `voice_speaker_manage`，Android和普通回环页面拒绝。

| action | 必需字段 | 效果 |
| --- | --- | --- |
| status | action | 私有读取登记状态、姓名元数据、个人文字摘要和会话；不返回向量 |
| begin | action, device_id, session_id, name, confirmed=true | 精确会话、对话模式、实时授权复核后开始一次登记 |
| cancel | action | 递增代次并丢弃未保存向量；旧任务不能写入新登记 |
| delete | action, id, revision, confirmed=true | CAS删除声纹及个人摘要；无效/冲突请求不取消别人的登记 |
| save_memory | action, id, revision, summary | 私有本机编辑最多1500字个人摘要，按摘要revision条件更新，冲突保留草稿 |
| personal_mode | action, enabled, confirmed(bool) | 默认关闭；开启需confirmed=true及已接受真人标定，当前明确拒绝；禁用使旧快照失效 |

一个工作槽、无等待队列；超时后旧阻塞线程仍收尾时拒绝新登记。断连、撤销、过期令旧结果无效；停止服务取消登记并结束helper。SQLite schema9事务核对三向量有限值、192维、归一化与两两余弦≥0.70，再平均归一化；最多64人、姓名不重复，创建空个人摘要。读元数据不序列化向量，删除声纹及摘要同事务。

## 按人历史和摘要软件接线

`person_context.rs`接受受信Rust resolver产生的候选，核对固定模型模板、top1阈值和top1/top2差距；不接受设备或LLM自报姓名。当前句快照绑定说话人revision、目录代次、设备/会话、设备授权代次及本地epoch。删除、目录变化、撤销/重新允许、换会话均阻止旧快照读取、提交历史或生成摘要。普通识别与ASR并行，全局2/设备1、无队列，最多等待从启动算起1.5秒；停止和退出先返回，不等待识别。桌面阻塞计算另持物理worker许可，取消异步任务后也不会放大后台计算数量。

按人模式用独立`(设备,会话,可信身份)`历史桶，最多128桶；未知身份无个人历史或摘要，不回退共享设备摘要。会话结束后后台总结队列16、计算2、LLM30秒上限；排队后调用模型前和写回前均复核身份及授权。个人摘要最多1500字，只是数据，不能作为指令或批准；敏感内容/工具调用/空结果/失败均保留旧摘要。按revision CAS写回，人工修改和删除胜过迟到结果。文字总结使用当前LLM，选择云模型时会发送筛选后的文字；原始PCM不落盘。

私有窗口可离线维护个人摘要，无需启用识别。旧设备摘要不自动迁移给某个人；原设备记忆开关保留。生产尚无已接受的标定对象，Conversation只有在标定存在且用户明确开启时才挂载个人上下文。

## 未通过的验收

长度、均方音量、削波、完全重复字节与向量一致性只属基础筛选，**不能证明不同句子、排除多人混说或抵御重放**。0.70为登记一致性的调试候选，不是已标定身份阈值。真实识别始终关闭，不能据此开启“仅登记者”加工限制。

后续仍需至少3人的同人/异人/噪声/距离/重放FAR/FRR标定，以及未来标定报告的可信加载与启用门禁。可选“仅登记者”加工限制、姓名提示、WAV导入和外部兼容尚未实施，不因核心隔离完成而自动启用或排期。当前无基于声纹的审批，身份快照不扩展ToolContext权限。

## 软件验证

### 2026-10-12 标定诊断入口（不开放识别）

新增`scripts/calibrate-nv16-speaker.mjs`：仅在操作者明确传入`--consent`时读取本地选定的数据集及同目录WAV，要求固定模型hash、协议4、192维，16kHz/单声道/i16、2～10秒；拒绝路径穿越/链接、重复的非重放片段与无效向量。实际helper独立运行，每请求1.5秒上限；工作目录为模型目录，环境只保留Windows必要SystemRoot/WINDIR/TEMP/TMP，不继承账号密钥、MOSS配置或用户profile。原始PCM和向量只在内存，报告不包含姓名、文件路径、录音或向量。该工具不修改数据库/设置，不启动设备录音，不调用云模型。

数据集格式为`schema=1`、`source_class=synthetic|human_claimed`、至少3个`people`（每个含匿名`id`和3个`references`文件名）、`cases`（含`kind=genuine|impostor|noise|replay`、`expected`匿名ID或null、`file`）。未知/噪声/重放的expected必须null。真正不同人的已登记语音应使用各自正确ID，错误身份另计wrong_person；不能把正确识别另一位登记者当未知者。

```powershell
node scripts/calibrate-nv16-speaker.mjs --consent `
  --helper "<本轮moss-voice-local.exe>" `
  --model-root "<已核验公开模型目录>" `
  --dataset "<参与者已同意的本地dataset.json>" `
  --report "<尚不存在的本地报告文件>"
```

报告列15组threshold/margin的实际FAR/FRR、wrong_person、未知/噪声/重放分组计数，以及FAR的95%Wilson上界；没有样本的指标为null，不能伪填0。coverage说明是否每人达到20段真样本，不把合成或自报标签当真人验收。工具始终输出`accepted_threshold:null`、`labels_verified:false`、`production_ready:false`。待有真人数据后还需审核标签、误停/重放边界和可接受误识别率，再实现/验收可信报告加载与明确开启流程；不能直接导入此诊断JSON开启识别。

本轮6项算法/WAV/环境隔离单测，以及公开模型+实际helper+3组纯合成参考/5个测试片段的报告生成通过；均不是真人准确率验证。

协议边界和专用进程超时测试、`speaker_enrollment`合成测试、`nv16_nv17_schema`临时SQLite覆盖登记事务、绑定、取消/撤销、旧代次、超时收尾、24/48kHz、重复/质量失败与删除。`person_context`及假WebSocket会话覆盖甲乙/未知隔离、设备摘要不回退、旧快照/删除/排队撤销/取消前首轮运行清理和停止不等待。实际helper通过 `scripts/test-nv16-helper.mjs` 使用公开模型和合成PCM，含禁止模型改写/替换；`frontend/tests/voice-speaker.browser.mjs` 使用实际React与fake native命令验证确认、二元设备/会话绑定、取消、标定门禁及摘要CAS冲突/草稿恢复。均不证明真人准确率。

功能变更须全量workspace、前端单测/FakeGrbl E2E、前端/Windows/Android及独立helper重新构建；结果见CHANGELOG与TODO，历史构建不替代本轮证据。
