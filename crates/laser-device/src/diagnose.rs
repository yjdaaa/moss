/// 解析 GRBL / FluidNC 的错误、报警与硬件诊断信息，转换为结构化自然语言说明。
pub fn diagnose_device_message(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if let Some(msg) = trimmed.strip_prefix("[MSG:ERR:") {
        let err_text = msg.trim_end_matches(']').trim();
        return Some(format!("控制器硬件错误: {err_text}"));
    }
    if let Some(msg) = trimmed.strip_prefix("[MSG:WARN:") {
        let warn_text = msg.trim_end_matches(']').trim();
        return Some(format!("控制器警告: {warn_text}"));
    }
    let upper = trimmed.to_ascii_uppercase();
    if let Some(alarm_part) = upper.strip_prefix("ALARM:") {
        let code = alarm_part
            .split_whitespace()
            .next()
            .unwrap_or(alarm_part)
            .trim();
        let desc = match code {
            "1" => {
                "硬限位开关触发（物理撞机或轴触碰限位，控制器已紧急制动停机，建议重新执行 $H 回零）"
            }
            "2" => "软限位超程（目标运动坐标超出了设置的最大行程，已安全拦截，可执行 $X 解锁）",
            "3" => "运动中控制器复位（可能丢失步进步数，建议重新执行 $H 回零）",
            "4" => "对刀/探针初始状态错误（探针在探测前已处于闭合导通状态）",
            "5" => "对刀/探针未接触（在最大探测行程内未检测到接触信号）",
            "6" => "回零过程被复位中断",
            "7" => "回零过程中安全门打开",
            "8" => "回零脱开限位失败（建议增大 pull-off 距离或检查限位开关接线）",
            "9" => "回零未找到限位开关（未在搜索距离内触发开关，请检查接线或行程）",
            "10" => "双电机双限位脱开失败",
            "11" => {
                "启动引脚电平异常（限位开关或急停按键在上电时已处于触发闭合状态，请检查接线或引脚反相设置）"
            }
            "12" => "回零开关信号歧义（双限位开关触发顺序异常）",
            "13" => "硬停止保护触发（控制器强制断电保护）",
            "14" => "未回零安全锁定（机床启用了强制回零策略，请先执行 $H 回零再开始加工）",
            "15" => "固件初始化配置锁定（配置文件加载异常）",
            "16" => "IO 扩展芯片复位故障",
            "17" => {
                "文件作业中遇到无效 G-code 行，固件已停止作业（FluidNC GCodeError，原因见前一行 [MSG:ERR: Bad GCode: …]）"
            }
            "18" => "对刀过程触碰到了物理硬限位开关",
            _ => "控制器安全锁定，请检查机床状态",
        };
        return Some(format!("报警 ALARM:{code}：{desc}"));
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(error_part) = lower.strip_prefix("error:") {
        let code = error_part
            .split_whitespace()
            .next()
            .unwrap_or(error_part)
            .trim();
        let desc = match code {
            "1" => "G-code 语法错误：缺少字母前缀",
            "2" => "G-code 数值格式无效或缺少预期数值",
            "3" => "未识别或不支持的 '$' 系统命令",
            "4" => "设置值不能为负数",
            "5" => "该设置项在当前固件中已禁用",
            "6" => "步进脉冲宽度过短（最小 3 微秒）",
            "7" => "读取 EEPROM/NVS 设置失败，已恢复默认值",
            "8" => "该命令只能在 Idle 空闲状态执行",
            "9" => "控制器处于报警或锁定状态，拒绝执行移动指令（请先执行 $H 回零或 $X 解锁）",
            "10" => "软限位触发：目标坐标超出行程（请检查图形尺寸与原点）",
            "11" | "14" => "指令行超过 255 字节上限，固件已丢弃超出部分",
            "12" => "超过最大步进速率（进给 F 太高或 steps_per_mm 过大）",
            "13" => "安全门打开，固件拒绝解锁/运动",
            "15" => "点动目标超出机床行程",
            "16" => "点动命令 $J= 格式无效",
            "17" => "激光模式需要 PWM 输出引脚，当前主轴配置不满足",
            "18" => "配置中未定义回零轴，无法执行 $H",
            "19" => "固件不允许单轴回零",
            "20" => "不支持或无效的 G-code 指令",
            "21" => "同一行出现互斥的模态 G 指令（如 G0 与 G1）",
            "22" => "未设置进给速度 F（G1/G2/G3 前需要 F 值）",
            "23" => "G/M 指令值必须是整数",
            "24" => "同一程序段内存在冲突的轴控制指令",
            "25" => "同一行重复出现相同字母参数",
            "26" => "运动指令缺少轴坐标（G1 后无 X/Y）",
            "27" => "行号 N 无效",
            "28" => "指令缺少必需的参数值（如 G4 缺 P）",
            "29" => "不支持的工件坐标系（G59.1+）",
            "30" => "G53 只能与 G0/G1 同行使用",
            "31" => "该指令不允许带轴坐标",
            "32" => "圆弧指令缺少当前平面的轴坐标",
            "33" => "运动目标无效（圆弧端点无法到达）",
            "34" => "圆弧半径 R 无法构成有效圆弧",
            "35" => "圆弧缺少 I/J/K 偏移量",
            "36" => "行内存在未使用的多余参数",
            "37" => "G43.1 动态刀长补偿轴错误",
            "38" => "G-code 数值超出允许最大值",
            "39" => "P 参数超出允许范围",
            "40" => "启动引脚状态异常，拒绝运行",
            "60" => "SD 卡挂载失败（SD 卡未插入、接触不良或格式不支持，请格式化为 FAT32）",
            "61" | "66" => "无法打开或读取 SD 卡中的文件",
            "62" => "无法打开 SD 卡或本地目录",
            "63" => "目录不存在",
            "64" => "文件为空",
            "65" => "指定的文件在 SD 卡或内部 Flash 中不存在",
            "67" => "文件系统正忙（正在进行文件读写或已有脱机任务运行）",
            "68" | "69" => "删除 SD 卡文件或目录失败",
            "70" => "重命名文件失败",
            "80" => "设置数值超出允许范围",
            "81" => "设置字符串或参数无效",
            "82" => "创建文件失败（SD 卡满或只读）",
            "83" => "格式化文件系统失败",
            "90" => "向其他通道转发消息失败",
            "100" | "101" => "写入或读取 NVS 持久化设置失败",
            "110" => "Web 认证失败（密码错误）",
            "111" | "112" | "114" => "行/文件结束或无数据（通常不是错误）",
            "113" => "固件在执行中被复位",
            "120" => {
                "另一个通道正在运行作业（如 SD 卡任务或 WebUI 发起的加工），本连接的 G-code 被拒绝；等它结束或先停止它"
            }
            "130" => "点动 (Jog) 被取消",
            "150" => "引脚配置冲突（YAML 文件中 GPIO 编号重复或引脚不存在）",
            "151" => "运行时设置无效",
            "152" => "YAML 配置文件语法错误",
            "160" => "文件上传失败",
            "161" => "文件下载失败",
            "162" => "只读设置：$30/$32/$100 等在 FluidNC 中是 YAML 的只读代理，请改 config.yaml",
            "170" | "171" | "172" | "173" | "174" | "175" => "宏表达式语法或求值错误",
            "176" | "177" | "178" | "179" => "宏流程控制错误（仅在宏内允许，或内存/栈不足）",
            "180" => "参数赋值失败",
            "181" => "G-code 参数值无效",
            _ => "控制器指令错误",
        };
        return Some(format!("错误 error:{code}：{desc}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnose_device_message_translates_alarms_errors_and_fluidnc_msgs() {
        assert_eq!(
            diagnose_device_message("[MSG:ERR: Hard limit on X axis]"),
            Some("控制器硬件错误: Hard limit on X axis".into())
        );
        assert_eq!(
            diagnose_device_message("[MSG:WARN: Reset during motion]"),
            Some("控制器警告: Reset during motion".into())
        );
        let alarm1 = diagnose_device_message("ALARM:1").unwrap();
        assert!(alarm1.contains("硬限位开关触发"));
        let alarm2 = diagnose_device_message("ALARM:2").unwrap();
        assert!(alarm2.contains("软限位超程"));
        let err20 = diagnose_device_message("error:20").unwrap();
        assert!(err20.contains("不支持或无效的 G-code 指令"));
        let err9 = diagnose_device_message("error:9").unwrap();
        assert!(err9.contains("控制器处于报警或锁定状态"));
        assert_eq!(diagnose_device_message("ok"), None);
    }

    #[test]
    fn diagnose_device_message_translates_fluidnc_alarms_and_errors() {
        let alarm11 = diagnose_device_message("ALARM:11").unwrap();
        assert!(alarm11.contains("启动引脚电平异常"), "{alarm11}");
        let alarm14 = diagnose_device_message("ALARM:14").unwrap();
        assert!(alarm14.contains("未回零安全锁定"), "{alarm14}");
        let alarm18 = diagnose_device_message("ALARM:18").unwrap();
        assert!(
            alarm18.contains("对刀过程触碰到了物理硬限位开关"),
            "{alarm18}"
        );

        let err60 = diagnose_device_message("error:60").unwrap();
        assert!(err60.contains("SD 卡挂载失败"), "{err60}");
        let err65 = diagnose_device_message("error:65").unwrap();
        assert!(
            err65.contains("指定的文件在 SD 卡或内部 Flash 中不存在"),
            "{err65}"
        );
        let err150 = diagnose_device_message("error:150").unwrap();
        assert!(err150.contains("引脚配置冲突"), "{err150}");
        let err152 = diagnose_device_message("error:152").unwrap();
        assert!(err152.contains("YAML 配置文件语法错误"), "{err152}");
    }
}
