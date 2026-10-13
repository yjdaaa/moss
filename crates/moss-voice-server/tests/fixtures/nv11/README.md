# NV11 供应商契约样本（2026-10-12）

这些文件全部是人工构造的合成数据，保留官方接口的字段结构，不是账号调用回执或实际天气/新闻/私人知识库内容。来源域名均为 `.example.test`，密钥只用测试常量，不抓取结果链接、文件或图片。

| 样本 | 锁定的接口依据 | 本地适配边界 |
| --- | --- | --- |
| qweather-current.json | [和风 current weather](https://dev.qweather.com/en/docs/api/weather/weather-current/)，2026-10-12 核对 | `/weather/v1/current/{lat}/{lon}`，API Key/JWT；必需单位、条件码、湿度、风速与来源声明；其他气象字段不回填 |
| tavily-search.json | [Tavily Search](https://docs.tavily.com/documentation/api-reference/endpoint/search)，2026-10-12 核对 | `/search`，Bearer、basic/5条/无原始正文；HTTP432/433为不可自动重试的额度耗尽 |
| metaso-search.json | 官方 SDK [model.py](https://github.com/meta-sota/metaso-sdk/blob/091bda14504a1169f63c8a408a36495c66e0b1b2/src/metaso_sdk/model.py)、[search.py](https://github.com/meta-sota/metaso-sdk/blob/091bda14504a1169f63c8a408a36495c66e0b1b2/src/metaso_sdk/search.py)、[client.py](https://github.com/meta-sota/metaso-sdk/blob/091bda14504a1169f63c8a408a36495c66e0b1b2/src/metaso_sdk/client.py) | `/api/open/search/v2`，普通搜索，不带会话/专题；专题 ID 在 SDK 中可选，不把专题文档的必填项套到普通搜索；仅公开URL引用，剔除上传文件元数据 |
| ragflow-retrieval.json | [RAGFlow v1 retrieval](https://ragflow.io/docs/v1.0.0-rc1/http_api_reference)，2026-10-12 核对 | `/api/v1/retrieval`，固定配置 dataset_ids；code=0、chunks及非空document_id/dataset_id，响应数据集必须在白名单；旧retcode接口拒绝 |
| newsnow-source.json | [NewsNow SourceResponse/NewsItem](https://github.com/newsnext/newsnow/blob/2173126f804bec0201769f59d933add6c4632d17/shared/types.ts) | `/api/s?id=`，success/cache及配置id匹配；updatedTime允许数字或非空字符串，pubDate数字/字符串/缺省；不猜测时间单位或日期 |
| rss-news.xml | RSS2的rss/channel/item结构及锁定quick-xml0.42.0事件行为 | title/link/pubDate/description；CDATA、预定义与数字实体，拒绝DTD/自定义实体、非法链接、截断XML及重复根；空channel有效 |

`plugins::contract_tests` 使用真实本机 TCP/TLS，临时自签证书作为测试根、固定假域名解析到127.0.0.1，仍验证证书；复用生产 HTTP builder 的禁止跳转/重试策略。生产连接3秒、总计10秒不变；仅超时夹具把测试总计改为600ms，延迟900ms。六家分别覆盖成功、401/403/429/432/433/500/302、缺字段/坏正文/显式空结果、有长度与chunked超大响应、准确1MiB边界、头/正文超时。额度码在公共有界HTTP层归类；不会因retryable提示自动发起重试。

Metaso HTTP200里的应用错误也按[官方错误码](https://metaso.cn/subject/8547516269457154048?ssi=hXHmJkx)核对：401/403→鉴权失败、4002→额度耗尽、400/4009→供应商拒绝`plugin_provider_rejected`（不重试），500/-500→网络/服务错误，未知码或缺字段→schema错误；不返回errMsg、会话或余额。SDK普通搜索与专题文档范围分开，不从专题必填字段推断全网搜索必须填专题ID。

另有配置与凭据隔离、撤销丢弃、新闻缓存配置变化、TTL/容量/并发去重、模型参数不能改写来源、转义后2000字符上限测试，以及实际React+假native的默认关闭/旧表单/供应商切换/独立Keep-Set-Clear/失败草稿/窄屏验证。只证明软件契约，不证明供应商在线兼容、额度、数据质量或真人设备体验。

运行：`cargo test -p moss-voice-server plugins --lib`；前端目录运行 `node tests/voice-plugins.browser.mjs`。全量和交付构建结果以实施TODO与CHANGELOG本次回执为准。
