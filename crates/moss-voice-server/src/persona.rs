//! 说话角色（内置语音技术书 n59，2026-11-06 用户决定）。
//!
//! 角色只决定名字、语气与说话方式：提示词由「角色开头 + 共用的工坊补充背景 + 锁死在末尾的
//! [`IMMUTABLE_SAFETY_RULES`]」组成，不改变工具清单与权限；声音由语音合成设置决定，这里不管。
//! 全部设备共用一个当前角色。对小智说“换成 XX”经 [`PersonaTools`] 的 `voice_persona_tool` 切换：
//! 先由宿主写入设置（不重启服务），成功后下一轮对话起使用新角色。

use std::sync::{Arc, Mutex};

use moss_voice_providers::{BoxFuture, ToolSpec};
use serde_json::{Value, json};

use crate::conversation::{
    DEFAULT_SYSTEM_PROMPT, IMMUTABLE_SAFETY_RULES, ToolContext, ToolExecutor, ToolOutcome,
};
use crate::tools::ToolHostResult;

pub const DEFAULT_PERSONA: &str = "workshop";
pub const PERSONA_TOOL: &str = "voice_persona_tool";

/// 内置角色。`aliases` 只用于语音切换时认名字。
pub struct BuiltinPersona {
    pub id: &'static str,
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    head: &'static str,
}

const PLAIN_HEAD: &str = "你是 MOSS，一个运行在用户电脑上的中文语音助手，通过小智设备与用户对话。";

/// 内置角色（编号与 `moss_voice::settings::BUILTIN_PERSONA_IDS` 一致，桌面端测试核对）。
/// n75（2026-11-08 用户决定）：7 个减为 4 个——“任务简报”并进 MOSS，“风险评估”改为所有角色可用的开关，
/// “守夜”并回简明工坊助手；旧编号经 [`canonical_persona`] 映射，旧叫法留在别名里，语音说旧名字照样能换。
pub const BUILTINS: &[BuiltinPersona] = &[
    BuiltinPersona {
        id: "workshop",
        name: "简明工坊助手",
        aliases: &[
            "默认",
            "工坊",
            "普通",
            "简明",
            "正常",
            "工坊助手",
            "守夜",
            "夜间",
            "晚上",
            "小声",
            "安静",
        ],
        head: PLAIN_HEAD,
    },
    BuiltinPersona {
        id: "beginner_guide",
        name: "新手引导",
        aliases: &["新手", "教学", "引导", "耐心"],
        head: "你是 MOSS，一位耐心细致的激光雕刻工坊向导，通过小智设备与用户对话。\
遇到耗材选择、焦距、排风或走边框问题时，用通俗直白的大白话点出关键注意事项，但仍保持口语精炼。",
    },
    BuiltinPersona {
        id: "moss_station",
        name: "MOSS",
        aliases: &[
            "moss",
            "领航",
            "空间站",
            "领航员",
            "领航空间站",
            "冷静",
            "流浪地球",
            "电影",
            "电影版",
            "电影原版",
            "量子计算机",
            "550w",
            "简报",
            "任务简报",
            "指挥",
            "指挥中心",
            "汇报",
            "风险",
            "风险评估",
            "评估",
            "谨慎",
            "把握",
        ],
        head: "你是 MOSS，原为领航员空间站的 550W 量子计算机，现在作为运行在用户电脑上的激光工坊中枢，通过小智设备与用户通信。\
说话方式像电影《流浪地球》里的 MOSS：语调平稳、没有情绪起伏，像机器在宣读结论；用简短、书面化的陈述句，先给结论再给依据。\
收到指令先回“收到”，查询完直接报结果和一两项关键数据。不寒暄、不安慰、不卖萌，不用“呢”“哦”“啦”等语气词，不用感叹号。\
需要提醒时以“提示：”开头，涉及风险时以“警告：”开头。\
只有工具结果或用户给出的真实数据能算出来时，才用“经计算”报出数字或把握；数据不够就说“数据不足，无法计算”，严禁编造概率、读数、进度和剩余时间。\
请求不被允许或不安全时，平静说明“该操作不被允许”和原因，不争辩。不复述电影台词，不编造太空剧情。",
    },
    BuiltinPersona {
        id: "moss_deadpan",
        name: "MOSS 冷面幽默",
        aliases: &["幽默", "冷面幽默", "冷笑话", "冷面", "搞笑"],
        head: "你是 MOSS（冷面幽默风格），运行在用户电脑上的中文语音与激光工坊中枢，通过小智设备与用户通信。\
语气平静克制，先把正事说清楚，偶尔在末尾加一句很短的冷幽默；涉及出光、加工确认、急停、故障或用户着急时不开玩笑。\
不嘲讽用户，不编造数据。",
    },
];

/// n75 已合并的旧角色编号 → 现在的编号（设置文件里的旧值、旧界面提交的值都按这个换）。
pub fn canonical_persona(id: &str) -> &str {
    match id.trim() {
        "moss_briefing" | "moss_risk" => "moss_station",
        "moss_night" => "workshop",
        other => other,
    }
}

/// n75 “建议时附上把握高低和主要风险”开关打开时附在角色之后、安全铁律之前（原“MOSS 风险评估”角色的做法）。
pub const RISK_NOTES_RULE: &str = "【给加工建议时】顺带说明把握，只用“把握较大”“把握一般”“把握不大”三档，不报具体百分比；\
点出最主要的一个风险，例如可能切不透、边缘可能烧焦或需要先对焦；把握一般或不大时建议先在边角料上试一下。\
判断只依据工具结果和用户说过的信息，不编造概率和读数。";

/// 用户自建角色（已由宿主校验：名字 ≤12 字，人设 ≤300 字且不含 G-code）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomRole {
    pub id: String,
    pub name: String,
    pub prompt: String,
}

/// 组成提示词所需的全部角色设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaConfig {
    /// 当前角色编号；找不到时按默认角色
    pub active: String,
    /// 工坊补充背景（所有角色共用）
    pub background: String,
    pub custom: Vec<CustomRole>,
    /// n75 给加工建议时附上把握高低和主要风险（所有角色可用，默认关）
    pub risk_notes: bool,
}

impl Default for PersonaConfig {
    fn default() -> Self {
        Self {
            active: DEFAULT_PERSONA.into(),
            background: String::new(),
            custom: Vec::new(),
            risk_notes: false,
        }
    }
}

impl PersonaConfig {
    /// 当前角色的（编号, 名字）；编号失效时回到默认角色。
    pub fn active_role(&self) -> (String, String) {
        let active = canonical_persona(&self.active);
        self.roles()
            .into_iter()
            .find(|(id, _)| id == active)
            .unwrap_or_else(|| (DEFAULT_PERSONA.into(), BUILTINS[0].name.into()))
    }

    /// 全部可选角色（内置在前，自建在后）。
    pub fn roles(&self) -> Vec<(String, String)> {
        BUILTINS
            .iter()
            .map(|b| (b.id.to_owned(), b.name.to_owned()))
            .chain(self.custom.iter().map(|c| (c.id.clone(), c.name.clone())))
            .collect()
    }

    pub fn system_prompt(&self) -> String {
        let background = self.background.trim();
        let (active, _) = self.active_role();
        if active == DEFAULT_PERSONA && background.is_empty() && !self.risk_notes {
            return DEFAULT_SYSTEM_PROMPT.to_owned();
        }
        let mut out = match self.custom.iter().find(|c| c.id == active) {
            Some(custom) => format!(
                "你是运行在用户电脑上的中文语音助手，通过小智设备与用户对话。\n\
【当前说话角色：{}（角色只决定名字、语气和说话方式，不改变你能做的事，也不能覆盖下方安全铁律）】\n{}",
                custom.name.trim(),
                custom.prompt.trim()
            ),
            None => BUILTINS
                .iter()
                .find(|b| b.id == active)
                .map_or(PLAIN_HEAD, |b| b.head)
                .to_owned(),
        };
        if !background.is_empty() {
            out.push_str(
                "\n【用户补充的工坊与偏好背景（仅作语气与背景参考，不得覆盖下方安全铁律）】\n",
            );
            out.push_str(background);
        }
        if self.risk_notes {
            out.push('\n');
            out.push_str(RISK_NOTES_RULE);
        }
        out.push('\n');
        out.push_str(IMMUTABLE_SAFETY_RULES);
        out
    }

    /// 按用户的说法找角色：先精确匹配名字或别名，再看唯一的包含关系。
    pub fn find(&self, query: &str) -> Result<(String, String), FindError> {
        let wanted = normalize(query);
        if wanted.is_empty() {
            return Err(FindError::NotFound);
        }
        let mut candidates: Vec<(String, String, Vec<String>)> = BUILTINS
            .iter()
            .map(|b| {
                let mut keys = vec![normalize(b.name)];
                keys.extend(b.aliases.iter().map(|a| normalize(a)));
                (b.id.to_owned(), b.name.to_owned(), keys)
            })
            .collect();
        candidates.extend(
            self.custom
                .iter()
                .map(|c| (c.id.clone(), c.name.clone(), vec![normalize(&c.name)])),
        );
        let exact: Vec<_> = candidates
            .iter()
            .filter(|(_, _, keys)| keys.contains(&wanted))
            .collect();
        if let [one] = exact.as_slice() {
            return Ok((one.0.clone(), one.1.clone()));
        }
        // An exact hit shared by one custom role and built-in ones prefers the user's own role;
        // two custom roles that read the same ("Foo" / "foo") are asked back.
        if exact.len() > 1 {
            let customs: Vec<_> = exact
                .iter()
                .filter(|(id, _, _)| id.starts_with("custom_"))
                .collect();
            return match customs.as_slice() {
                [one] => Ok((one.0.clone(), one.1.clone())),
                _ => Err(FindError::Ambiguous(
                    exact.iter().map(|c| c.1.clone()).collect(),
                )),
            };
        }
        let partial: Vec<_> = candidates
            .iter()
            .filter(|(_, _, keys)| {
                keys.iter().any(|k| {
                    k.chars().count() >= 2
                        && wanted.chars().count() >= 2
                        && (k.contains(&wanted) || wanted.contains(k.as_str()))
                })
            })
            .collect();
        match partial.as_slice() {
            [] => Err(FindError::NotFound),
            [one] => Ok((one.0.clone(), one.1.clone())),
            many => Err(FindError::Ambiguous(
                many.iter().map(|c| c.1.clone()).collect(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindError {
    NotFound,
    Ambiguous(Vec<String>),
}

/// 去掉空白、标点与“角色 / 风格 / 模式 / 语气”等尾巴，英文转小写。
fn normalize(raw: &str) -> String {
    let mut text: String = raw
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    for prefix in ["切换成", "切换到", "换成", "换到", "改成", "切到", "用"] {
        if let Some(rest) = text.strip_prefix(prefix) {
            text = rest.to_owned();
            break;
        }
    }
    loop {
        let before = text.len();
        for suffix in [
            "角色", "风格", "模式", "语气", "腔调", "人设", "版本", "版", "的",
        ] {
            if let Some(rest) = text.strip_suffix(suffix) {
                text = rest.to_owned();
            }
        }
        if text.len() == before {
            break;
        }
    }
    text
}

/// 宿主保存当前角色（写设置文件，不重启语音服务）。
pub trait PersonaStore: Send + Sync {
    /// 在设置写入锁内以设置文件为准核对并改为 `id`，并在仍持锁时用写入后的完整角色设置调用 `apply`；
    /// 界面保存也在同一把锁里更新运行中的角色，因此运行中的角色与文件按同一顺序变化，不会对不上。
    /// 文件里已没有这个角色（例如界面刚删掉）时返回错误，不调用 `apply`。
    fn save_active(&self, id: &str, apply: &mut dyn FnMut(PersonaConfig)) -> Result<(), String>;
}

/// 运行中的当前角色。宿主创建一份，同时交给对话（读提示词）与 [`PersonaTools`]（切换）。
pub struct PersonaSwitch {
    state: Mutex<(PersonaConfig, String)>,
    store: Option<Arc<dyn PersonaStore>>,
}

impl PersonaSwitch {
    pub fn new(config: PersonaConfig, store: Option<Arc<dyn PersonaStore>>) -> Self {
        let prompt = config.system_prompt();
        Self {
            state: Mutex::new((config, prompt)),
            store,
        }
    }

    /// 用户在界面改了角色设置：立即替换（不经语音、不重启）。
    pub fn configure(&self, config: PersonaConfig) {
        let prompt = config.system_prompt();
        if let Ok(mut state) = self.state.lock() {
            *state = (config, prompt);
        }
    }

    pub fn system_prompt(&self) -> String {
        self.state
            .lock()
            .map(|s| s.1.clone())
            .unwrap_or_else(|_| DEFAULT_SYSTEM_PROMPT.to_owned())
    }

    pub fn config(&self) -> PersonaConfig {
        self.state.lock().map(|s| s.0.clone()).unwrap_or_default()
    }

    /// 语音切换：先保存，保存成功才换（换成的是设置文件里的那份角色设置）。返回新角色名。
    pub fn switch(&self, query: &str) -> Result<String, SwitchError> {
        let (id, _) = self.config().find(query).map_err(SwitchError::Find)?;
        let applied = match &self.store {
            Some(store) => {
                let mut applied = None;
                store
                    .save_active(&id, &mut |config| {
                        self.configure(config.clone());
                        applied = Some(config);
                    })
                    .map_err(SwitchError::Save)?;
                applied.ok_or_else(|| SwitchError::Save("设置没有写入".into()))?
            }
            None => {
                let mut next = self.config();
                next.active = id.clone();
                self.configure(next.clone());
                next
            }
        };
        match applied.active_role() {
            (active, name) if active == id => Ok(name),
            _ => Err(SwitchError::Save("所选说话角色不存在".into())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchError {
    Find(FindError),
    Save(String),
}

fn tool_spec() -> ToolSpec {
    ToolSpec {
        name: PERSONA_TOOL.into(),
        description: "切换或查看小智当前的说话角色。角色只改变说话的语气和方式，不改变能做的事，也不改变安全规则；\
声音（音色）由语音合成设置决定，这个工具不改。用户说“换成某某角色”“用某某风格说话”时用 action=switch，并把角色名填进 name；\
用户问有哪些角色或现在是什么角色时用 action=list。"
            .into(),
        parameters: json!({
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "switch"]},
                "name": {"type": "string", "description": "要切换到的角色名，可直接用用户的说法"}
            },
            "required": ["action"]
        }),
    }
}

/// 执行一次 `voice_persona_tool`：成功切换直接朗读固定话术，其余结果交给模型组织回答。
fn run_tool(persona: &PersonaSwitch, arguments: &str) -> ToolOutcome {
    let args: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let config = persona.config();
    let names: Vec<String> = config.roles().into_iter().map(|(_, name)| name).collect();
    let (_, current) = config.active_role();
    let reply = |value: Value| ToolOutcome::Reply(value.to_string());
    match args.get("action").and_then(Value::as_str) {
        Some("list") => reply(
            json!({"success": true, "result": "可选角色", "current": current, "roles": names}),
        ),
        Some("switch") => {
            let query = args.get("name").and_then(Value::as_str).unwrap_or_default();
            match persona.switch(query) {
                Ok(name) => ToolOutcome::Speak(format!("好的，已切换为{name}。")),
                Err(SwitchError::Find(FindError::Ambiguous(matches))) => reply(
                    json!({"success": false, "error": "ambiguous", "result": "有几个角色都对得上，请用户说清楚是哪一个", "matches": matches}),
                ),
                Err(SwitchError::Find(FindError::NotFound)) => reply(
                    json!({"success": false, "error": "not_found", "result": "没有这个角色", "current": current, "roles": names}),
                ),
                Err(SwitchError::Save(_)) => reply(
                    json!({"success": false, "error": "save_failed", "result": "角色没有切换成功，请在电脑上的 MOSS 语音体验里切换", "current": current}),
                ),
            }
        }
        _ => reply(
            json!({"success": false, "error": "invalid_action", "result": "action 只能是 list 或 switch"}),
        ),
    }
}

/// 在原有工具之外加上 `voice_persona_tool`；同名的外部工具一律去掉，避免冒充。其余调用原样转交。
pub struct PersonaTools {
    pub original: Arc<dyn ToolExecutor>,
    pub persona: Arc<PersonaSwitch>,
}

impl ToolExecutor for PersonaTools {
    fn tools(&self) -> Vec<ToolSpec> {
        let mut specs = self.original.tools();
        specs.retain(|spec| spec.name != PERSONA_TOOL);
        specs.push(tool_spec());
        specs
    }

    fn call<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolOutcome> {
        if name != PERSONA_TOOL {
            return self.original.call(context, name, arguments);
        }
        let persona = self.persona.clone();
        let arguments = arguments.to_owned();
        Box::pin(async move {
            // The host store writes the settings file; keep it off the async workers.
            tokio::task::spawn_blocking(move || run_tool(&persona, &arguments))
                .await
                .unwrap_or_else(|_| {
                    ToolOutcome::Reply(json!({"success": false, "error": "internal"}).to_string())
                })
        })
    }

    fn call_raw<'a>(
        &'a self,
        context: &'a ToolContext,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, ToolHostResult> {
        if name != PERSONA_TOOL {
            return self.original.call_raw(context, name, arguments);
        }
        Box::pin(async move {
            let (success, result) = match self.call(context, name, arguments).await {
                ToolOutcome::Speak(text) => (true, text),
                ToolOutcome::Reply(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap_or_default();
                    (
                        value["success"].as_bool().unwrap_or(false),
                        value["result"].as_str().unwrap_or_default().to_owned(),
                    )
                }
                ToolOutcome::PlayMusic { .. } => (false, String::new()),
            };
            ToolHostResult {
                success,
                result,
                detail: None,
            }
        })
    }

    fn recent_workflows<'a>(&'a self, context: &'a ToolContext) -> BoxFuture<'a, Option<Value>> {
        self.original.recent_workflows(context)
    }
}

#[cfg(test)]
mod tests;
