import os
import math
from PIL import Image, ImageDraw, ImageFont

def get_fonts():
    font_bold_path = "C:/Windows/Fonts/msyhbd.ttc"
    font_reg_path = "C:/Windows/Fonts/msyh.ttc"
    font_mono_path = "C:/Windows/Fonts/consola.ttf"

    if not os.path.exists(font_bold_path):
        font_bold_path = "C:/Windows/Fonts/msyh.ttc"

    return {
        "title": ImageFont.truetype(font_bold_path, 34),
        "subtitle": ImageFont.truetype(font_reg_path, 15),
        "header": ImageFont.truetype(font_bold_path, 20),
        "subhead": ImageFont.truetype(font_bold_path, 16),
        "body": ImageFont.truetype(font_reg_path, 14),
        "caption": ImageFont.truetype(font_reg_path, 12),
        "code": ImageFont.truetype(font_mono_path, 13) if os.path.exists(font_mono_path) else ImageFont.truetype(font_reg_path, 12),
        "badge": ImageFont.truetype(font_bold_path, 12),
    }

def draw_hud_card(draw, x, y, w, h, title="", subtitle="", border_color=(0, 229, 255, 180), bg_color=(14, 22, 40, 240)):
    # 绘制背景
    draw.rounded_rectangle([x, y, x + w, y + h], radius=8, fill=bg_color, outline=border_color, width=1)
    # 四角发光准星
    c_len = 10
    # 左上
    draw.line([(x, y), (x + c_len, y)], fill=(0, 229, 255, 255), width=2)
    draw.line([(x, y), (x, y + c_len)], fill=(0, 229, 255, 255), width=2)
    # 右上
    draw.line([(x + w, y), (x + w - c_len, y)], fill=(0, 229, 255, 255), width=2)
    draw.line([(x + w, y), (x + w, y + c_len)], fill=(0, 229, 255, 255), width=2)
    # 左下
    draw.line([(x, y + h), (x + c_len, y + h)], fill=(0, 229, 255, 255), width=2)
    draw.line([(x, y + h), (x, y + h - c_len)], fill=(0, 229, 255, 255), width=2)
    # 右下
    draw.line([(x + w, y + h), (x + w - c_len, y + h)], fill=(0, 229, 255, 255), width=2)
    draw.line([(x + w, y + h), (x + w, y + h - c_len)], fill=(0, 229, 255, 255), width=2)

def draw_arrow(draw, x1, y1, x2, y2, color=(0, 229, 255, 220), width=2, text=""):
    draw.line([(x1, y1), (x2, y2)], fill=color, width=width)
    # 箭头头部
    angle = math.atan2(y2 - y1, x2 - x1)
    arrow_size = 8
    p1 = (x2 - arrow_size * math.cos(angle - math.pi / 6), y2 - arrow_size * math.sin(angle - math.pi / 6))
    p2 = (x2 - arrow_size * math.cos(angle + math.pi / 6), y2 - arrow_size * math.sin(angle + math.pi / 6))
    draw.polygon([ (x2, y2), p1, p2 ], fill=color)

# ==============================================================================
# 渲染图 1：端到端全流程闭环流程图
# ==============================================================================
def render_pipeline_image(output_path):
    W, H = 2500, 1650
    img = Image.new("RGBA", (W, H), (7, 11, 22, 255))
    draw = ImageDraw.Draw(img)
    fonts = get_fonts()

    # 1. 顶部标题栏
    draw.ellipse([45, 38, 75, 68], fill=(255, 42, 68, 255), outline=(255, 255, 255, 200), width=2)
    draw.ellipse([54, 47, 66, 59], fill=(255, 255, 255, 255))
    draw.text((90, 32), "MOSS 激光创作中枢 · 端到端全流程闭环拓扑图", font=fonts["title"], fill=(241, 245, 253))
    draw.text((92, 74), "PROJECT MOSS 550W · END-TO-END LASER MANUFACTURING & 5-STAGE SAFETY BUS PIPELINE", font=fonts["subtitle"], fill=(159, 176, 204))

    # 顶栏右侧状态徽章
    draw.rounded_rectangle([W - 320, 35, W - 50, 75], radius=6, fill=(16, 28, 52, 240), outline=(0, 229, 255, 200), width=1)
    draw.text((W - 300, 46), "● UEG-550W COMMAND // READY", font=fonts["badge"], fill=(0, 229, 255))

    # 8 个阶段板块卡片布局 (2行4列网格)
    col_w = 560
    row_h = 660
    xs = [50, 650, 1250, 1850]
    y_top = 130
    y_bottom = 850

    # ---------------- 阶段 1: 触发与输入源 ----------------
    draw_hud_card(draw, xs[0], y_top, col_w, row_h, border_color=(0, 229, 255, 180))
    draw.text((xs[0] + 20, y_top + 18), "01 · 创作与触发输入源 (Input Tier)", font=fonts["header"], fill=(0, 229, 255))
    draw.line([(xs[0] + 20, y_top + 50), (xs[0] + col_w - 20, y_top + 50)], fill=(0, 229, 255, 80), width=1)

    inputs = [
        ("【文字排版模式】", "字符内容、字体选择、字号大小与毫米物理排版", (0, 229, 255)),
        ("【照片图像模式】", "外部图片上传、灰度转换、裁剪与黑白光栅化", (0, 229, 255)),
        ("【Excalidraw 画板】", "矢量手绘、几何形体、文字元素排版自由组合", (0, 229, 255)),
        ("【外部 G-code 导入】", "支持第三方 CAD/CAM 软件生成的 NC 雕刻代码导入", (0, 229, 255)),
        ("【材料实验室模式】", "自动生成功率-速度测试阵列矩阵 (Calibration Grid)", (255, 170, 0)),
        ("【小智 AI / 智能体】", "通过原生 MCP 协议 (Model Context Protocol) 自动调用", (255, 42, 68)),
        ("【局域网配对安全鉴权】", "Token 授权认证守卫，保障移动端与车间无线安全", (0, 245, 155)),
    ]
    cur_y = y_top + 65
    for tag, desc, color in inputs:
        draw.rounded_rectangle([xs[0] + 20, cur_y, xs[0] + col_w - 20, cur_y + 68], radius=6, fill=(20, 32, 58, 200), outline=(50, 75, 110, 180))
        draw.text((xs[0] + 32, cur_y + 12), tag, font=fonts["subhead"], fill=color)
        draw.text((xs[0] + 32, cur_y + 38), desc, font=fonts["caption"], fill=(180, 195, 220))
        cur_y += 82

    # ---------------- 阶段 2: 材料与工艺参数演算 ----------------
    draw_hud_card(draw, xs[1], y_top, col_w, row_h, border_color=(255, 170, 0, 180))
    draw.text((xs[1] + 20, y_top + 18), "02 · 材料工艺参数演算 (Material Engine)", font=fonts["header"], fill=(255, 170, 0))
    draw.line([(xs[1] + 20, y_top + 50), (xs[1] + col_w - 20, y_top + 50)], fill=(255, 170, 0, 80), width=1)

    mat_boxes = [
        ("【材料工艺数据库】", "内置 laser_materials.json (椴木/亚克力/皮革/纸板/软木等)", (255, 170, 0)),
        ("【厚度与材质匹配】", "智能识别工件厚度 (如 1mm, 3mm, 5mm)，读取最优参数组合", (241, 245, 253)),
        ("【核心制造工艺三要素】", "① 激光功率区间: 最小/最大功率 (S0 - S1000)\n② 进给切削速率: 雕刻/切割速度 (mm/min)\n③ 循环加工遍数: 单遍/多遍切削 (Passes)", (0, 229, 255)),
        ("【专家自定义覆盖】", "支持用户临时微调速度与功率，或保存新材料进入库", (180, 195, 220)),
        ("【工艺规则安全校验】", "防止出现零功率空打或超速跳步等异常工艺参数", (0, 245, 155)),
    ]
    cur_y = y_top + 65
    for title, desc, color in mat_boxes:
        h_box = 118 if "三要素" in title else 75
        draw.rounded_rectangle([xs[1] + 20, cur_y, xs[1] + col_w - 20, cur_y + h_box], radius=6, fill=(24, 34, 52, 200), outline=(70, 85, 110, 180))
        draw.text((xs[1] + 32, cur_y + 12), title, font=fonts["subhead"], fill=color)
        lines = desc.split("\n")
        ly = cur_y + 36
        for line in lines:
            draw.text((xs[1] + 32, ly), line, font=fonts["caption"], fill=(180, 195, 220))
            ly += 20
        cur_y += h_box + 16

    # ---------------- 阶段 3: 制造核心编译引擎 ----------------
    draw_hud_card(draw, xs[2], y_top, col_w, row_h, border_color=(0, 229, 255, 180))
    draw.text((xs[2] + 20, y_top + 18), "03 · 制造核心编译引擎 (laser-core)", font=fonts["header"], fill=(0, 229, 255))
    draw.line([(xs[2] + 20, y_top + 50), (xs[2] + col_w - 20, y_top + 50)], fill=(0, 229, 255, 80), width=1)

    core_boxes = [
        ("【光栅二值抖动算法】", "Floyd-Steinberg 与 Atkinson 算法，将照片转为激光雕刻点阵", (0, 229, 255)),
        ("【矢量轮廓拟合简化】", "Ramer-Douglas-Peucker (RDP) 算法平滑曲线，精简 G-code 行数", (0, 229, 255)),
        ("【G-code 指令装配器】", "规范生成 G90/G21 绝对坐标、G00 空走、G01 线性切削、M03/M05 开关光", (241, 245, 253)),
        ("【物理尺寸与耗时预估】", "严密演算工件宽×高物理毫米尺寸，运行 Time Model 预估加工秒数", (255, 170, 0)),
        ("【高保真仿真预览图】", "生成 1:1 工件仿真预览图，供人工与 AI 审查加工路径与边界", (0, 245, 155)),
    ]
    cur_y = y_top + 65
    for title, desc, color in core_boxes:
        draw.rounded_rectangle([xs[2] + 20, cur_y, xs[2] + col_w - 20, cur_y + 92], radius=6, fill=(20, 32, 58, 200), outline=(50, 75, 110, 180))
        draw.text((xs[2] + 32, cur_y + 12), title, font=fonts["subhead"], fill=color)
        draw.text((xs[2] + 32, cur_y + 40), desc, font=fonts["caption"], fill=(180, 195, 220))
        cur_y += 108

    # ---------------- 阶段 4: 安全状态机与双重门禁 ----------------
    draw_hud_card(draw, xs[3], y_top, col_w, row_h, border_color=(255, 42, 68, 180))
    draw.text((xs[3] + 20, y_top + 18), "04 · 安全状态机与双重门禁 (Runtime)", font=fonts["header"], fill=(255, 42, 68))
    draw.line([(xs[3] + 20, y_top + 50), (xs[3] + col_w - 20, y_top + 50)], fill=(255, 42, 68, 80), width=1)

    gate_boxes = [
        ("【64位防篡改任务指纹】", "生成 Sha256 唯一指纹哈希，确保下发数据与预览图严格一致", (255, 42, 68)),
        ("【状态机: preview_ready】", "工作流进入预备态，锁定当前加工参数与工件，禁止未审直发", (241, 245, 253)),
        ("【防误触双重确认机制】", "前端或 AI 审查预览无误后，必须显式调用 confirm_send 下达发射", (255, 170, 0)),
        ("【二次发送防撞保护 (RepeatGuard)】", "若连续发送相同指纹，系统强制拦截，必须确认“材料已换位复位”！\n彻底杜绝原位二次高能重复烧蚀引发的起火燃烧事故！", (255, 42, 68)),
        ("【装载硬件执行流水线】", "所有安全门禁全部放行后，任务正式挂载入 GRBL 底层执行队列", (0, 245, 155)),
    ]
    cur_y = y_top + 65
    for title, desc, color in gate_boxes:
        h_box = 115 if "RepeatGuard" in title else 82
        draw.rounded_rectangle([xs[3] + 20, cur_y, xs[3] + col_w - 20, cur_y + h_box], radius=6, fill=(35, 25, 40, 220) if "Repeat" in title else (20, 32, 58, 200), outline=(120, 40, 55, 200) if "Repeat" in title else (50, 75, 110, 180))
        draw.text((xs[3] + 32, cur_y + 12), title, font=fonts["subhead"], fill=color)
        lines = desc.split("\n")
        ly = cur_y + 38
        for line in lines:
            draw.text((xs[3] + 32, ly), line, font=fonts["caption"], fill=(180, 195, 220))
            ly += 20
        cur_y += h_box + 18

    # ---------------- 阶段 5: 5级全域物理安全急停总线 (下排第1列) ----------------
    draw_hud_card(draw, xs[0], y_bottom, col_w, row_h, border_color=(255, 42, 68, 220))
    draw.text((xs[0] + 20, y_bottom + 18), "05 · 5级全域物理急停总线 (Safety E-Stop)", font=fonts["header"], fill=(255, 42, 68))
    draw.line([(xs[0] + 20, y_bottom + 50), (xs[0] + col_w - 20, y_bottom + 50)], fill=(255, 42, 68, 80), width=1)

    estops = [
        ("【1. 前端 UI 软急停按键】", "操作员在界面随时一键下达 Emergency Stop 最高优先级指令", (255, 42, 68)),
        ("【2. Windows 控制台关闭拦截】", "捕获控制台关闭、Ctrl+C 中断信号，退出前瞬间切断激光", (241, 245, 253)),
        ("【3. 进程 Panic Hook 同步拦截】", "Rust 运行时即便遭遇极端未捕获异常，Panic Hook 强制切断出光", (255, 170, 0)),
        ("【4. 硬件脱机与心跳掉线检测】", "检测串口拔出或网络超时，立即熔断当前作业，杜绝失控出光", (255, 42, 68)),
        ("【5. 物理行程软限位越界保护】", "坐标若超出机械限位行程，立即制动防撞机", (241, 245, 253)),
        ("【瞬间广播制动指令】", "GlobalShutdownCoordinator 瞬间发送 0x18 软复位 + M05 强制关光！", (0, 245, 155)),
    ]
    cur_y = y_bottom + 65
    for title, desc, color in estops:
        draw.rounded_rectangle([xs[0] + 20, cur_y, xs[0] + col_w - 20, cur_y + 78], radius=6, fill=(35, 20, 30, 220), outline=(130, 35, 50, 200))
        draw.text((xs[0] + 32, cur_y + 12), title, font=fonts["subhead"], fill=color)
        draw.text((xs[0] + 32, cur_y + 40), desc, font=fonts["caption"], fill=(200, 190, 205))
        cur_y += 92

    # ---------------- 阶段 6: 硬件抽象与通信驱动 (下排第2列) ----------------
    draw_hud_card(draw, xs[1], y_bottom, col_w, row_h, border_color=(0, 229, 255, 180))
    draw.text((xs[1] + 20, y_bottom + 18), "06 · 硬件抽象与通信驱动 (laser-device)", font=fonts["header"], fill=(0, 229, 255))
    draw.line([(xs[1] + 20, y_bottom + 50), (xs[1] + col_w - 20, y_bottom + 50)], fill=(0, 229, 255, 80), width=1)

    drv_boxes = [
        ("【通信链路双模自适应】", "支持【物理硬件串口】与【局域网无线网络】双通道动态切换", (0, 229, 255)),
        ("【物理串口驱动】", "基于 serialport 库，支持 CH340/CP2102，自适应 115200 波特率", (241, 245, 253)),
        ("【无线网络协议驱动】", "基于 tokio-net 异步 TCP/Telnet 客户端，连接无线 GRBL 主板 (端口23)", (255, 170, 0)),
        ("【GRBL 协议流控状态机】", "逐行发送与 ok/error 应答追踪，精准监控机床缓冲区深度防溢出", (0, 229, 255)),
        ("【最高优先级中断抢占】", "急停指令 0x18 无视普通 G-code 队列，以 OOB 模式直接瞬时穿透", (255, 42, 68)),
    ]
    cur_y = y_bottom + 65
    for title, desc, color in drv_boxes:
        draw.rounded_rectangle([xs[1] + 20, cur_y, xs[1] + col_w - 20, cur_y + 92], radius=6, fill=(20, 32, 58, 200), outline=(50, 75, 110, 180))
        draw.text((xs[1] + 32, cur_y + 12), title, font=fonts["subhead"], fill=color)
        draw.text((xs[1] + 32, cur_y + 40), desc, font=fonts["caption"], fill=(180, 195, 220))
        cur_y += 108

    # ---------------- 阶段 7: 物理数控机床与执行 (下排第3列) ----------------
    draw_hud_card(draw, xs[2], y_bottom, col_w, row_h, border_color=(0, 245, 155, 180))
    draw.text((xs[2] + 20, y_bottom + 18), "07 · 物理机床执行与闭环传感 (Hardware)", font=fonts["header"], fill=(0, 245, 155))
    draw.line([(xs[2] + 20, y_bottom + 50), (xs[2] + col_w - 20, y_bottom + 50)], fill=(0, 245, 155, 80), width=1)

    hw_boxes = [
        ("【两轴步进电机轨迹驱动】", "X 轴与 Y 轴双驱步进电机高精插补运动，驱动光头到达切削坐标", (0, 245, 155)),
        ("【高能激光管聚焦出光】", "高压脉冲激励激光管出光，激光斑点高温烧蚀工件表面", (241, 245, 253)),
        ("【辅助气泵与风道】", "自动协同排烟与吹气，防止焦黑并保护聚焦透镜免受污染", (180, 195, 220)),
        ("【实时机械状态采样】", "循环轮询 '?' 获取机床实时物理坐标 (WPos/MPos) 与限位开关联锁", (0, 229, 255)),
        ("【烧蚀完成与到位信号】", "硬件返回全部执行确认信号，作业完成触发安全复位", (255, 170, 0)),
    ]
    cur_y = y_bottom + 65
    for title, desc, color in hw_boxes:
        draw.rounded_rectangle([xs[2] + 20, cur_y, xs[2] + col_w - 20, cur_y + 92], radius=6, fill=(18, 36, 45, 200), outline=(40, 95, 75, 180))
        draw.text((xs[2] + 32, cur_y + 12), title, font=fonts["subhead"], fill=color)
        draw.text((xs[2] + 32, cur_y + 40), desc, font=fonts["caption"], fill=(180, 215, 200))
        cur_y += 108

    # ---------------- 阶段 8: 闭环反馈与态势呈现 (下排第4列) ----------------
    draw_hud_card(draw, xs[3], y_bottom, col_w, row_h, border_color=(0, 229, 255, 180))
    draw.text((xs[3] + 20, y_bottom + 18), "08 · 闭环反馈与态势呈现 (Feedback)", font=fonts["header"], fill=(0, 229, 255))
    draw.line([(xs[3] + 20, y_bottom + 50), (xs[3] + col_w - 20, y_bottom + 50)], fill=(0, 229, 255, 80), width=1)

    fb_boxes = [
        ("【作业终态判定收敛】", "汇聚作业最终状态: completed(成功) / cancelled(取消) / failed(异常)", (0, 229, 255)),
        ("【实时双向事件广播】", "通过 WebSocket / Tauri IPC 实时推送加工百分比、行号与耗时", (241, 245, 253)),
        ("【HUD 全息监视舱】", "前端 Chamber 视口动态更新十字激光准星、实时坐标点轨迹与雷达网格", (0, 229, 255)),
        ("【日志记录与审计追踪】", "自动记入操作日志与安全审计日志，支持失败回溯分析", (180, 195, 220)),
        ("【完成告警与蜂鸣通知】", "触发声光提示，复位激光头至安全停靠原点 (Home Point)", (0, 245, 155)),
    ]
    cur_y = y_bottom + 65
    for title, desc, color in fb_boxes:
        draw.rounded_rectangle([xs[3] + 20, cur_y, xs[3] + col_w - 20, cur_y + 92], radius=6, fill=(20, 32, 58, 200), outline=(50, 75, 110, 180))
        draw.text((xs[3] + 32, cur_y + 12), title, font=fonts["subhead"], fill=color)
        draw.text((xs[3] + 32, cur_y + 40), desc, font=fonts["caption"], fill=(180, 195, 220))
        cur_y += 108

    # 阶段间连接主箭头线
    draw_arrow(draw, xs[0] + col_w, y_top + 330, xs[1], y_top + 330, color=(0, 229, 255, 255), width=3)
    draw_arrow(draw, xs[1] + col_w, y_top + 330, xs[2], y_top + 330, color=(0, 229, 255, 255), width=3)
    draw_arrow(draw, xs[2] + col_w, y_top + 330, xs[3], y_top + 330, color=(0, 229, 255, 255), width=3)
    # 折返向下进入底层
    draw_arrow(draw, xs[3] + col_w // 2, y_top + row_h, xs[3] + col_w // 2, y_bottom, color=(255, 42, 68, 255), width=3)
    # 下排从右到左再到完成反馈
    draw_arrow(draw, xs[0] + col_w, y_bottom + 330, xs[1], y_bottom + 330, color=(255, 42, 68, 255), width=3)
    draw_arrow(draw, xs[1] + col_w, y_bottom + 330, xs[2], y_bottom + 330, color=(0, 245, 155, 255), width=3)
    draw_arrow(draw, xs[2] + col_w, y_bottom + 330, xs[3], y_bottom + 330, color=(0, 229, 255, 255), width=3)

    # 保存
    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    img.save(output_path, "PNG")
    print(f"Pipeline image rendered successfully: {output_path}")

# ==============================================================================
# 渲染图 2：7层全栈技术架构图
# ==============================================================================
def render_tech_stack_image(output_path):
    W, H = 2500, 1750
    img = Image.new("RGBA", (W, H), (7, 11, 22, 255))
    draw = ImageDraw.Draw(img)
    fonts = get_fonts()

    # 1. 顶部标题栏
    draw.ellipse([45, 38, 75, 68], fill=(0, 229, 255, 255), outline=(255, 255, 255, 200), width=2)
    draw.ellipse([54, 47, 66, 59], fill=(255, 255, 255, 255))
    draw.text((90, 32), "MOSS 激光创作中枢 · 7 层全栈技术架构图", font=fonts["title"], fill=(241, 245, 253))
    draw.text((92, 74), "PROJECT MOSS 550W · 7-TIER ARCHITECTURE SPECIFICATION & CRATE WORKSPACE TOPOLOGY", font=fonts["subtitle"], fill=(159, 176, 204))

    draw.rounded_rectangle([W - 320, 35, W - 50, 75], radius=6, fill=(16, 28, 52, 240), outline=(0, 229, 255, 200), width=1)
    draw.text((W - 300, 46), "● 8 CRATES + 2 APPS WORKSPACE", font=fonts["badge"], fill=(0, 229, 255))

    # 7 个层级的卡片，从上到下排列
    tier_w = W - 100
    tier_h = 195
    tier_gap = 25
    y_start = 125

    tiers_data = [
        {
            "num": "第 1 层",
            "name": "展现与人机交互层 (Presentation & Client Tier)",
            "color": (0, 229, 255),
            "cards": [
                ("React 19 + Vite 构建系统", "超快热重载、生产高效分块、现代前端工程生态", (0, 229, 255)),
                ("@excalidraw/excalidraw", "深度二次开发的专业手绘矢量激光 CAD 画板", (241, 245, 253)),
                ("Zustand 状态总线", "极简零样板响应式全局状态，负责材料与作业流转", (0, 229, 255)),
                ("太空舱 HUD 3.0 CSS", "高对比深空光场、动态 Canvas 粒子、流浪地球天体罗盘", (255, 42, 68)),
                ("核心路由视图矩阵", "HomePage (中枢控制台) / MaterialLabPage (材料库) / DrawPage (画板)", (0, 245, 155)),
            ]
        },
        {
            "num": "第 2 层",
            "name": "跨端宿主与桌面容器层 (Cross-Platform Host Tier)",
            "color": (0, 245, 155),
            "cards": [
                ("Tauri 2.0 原生容器", "Rust 极小体积桌面壳，内存占用远低于 Electron", (0, 245, 155)),
                ("Microsoft WebView2", "Windows 原生硬件加速 Webview 渲染内核", (241, 245, 253)),
                ("系统托盘 (TrayIcon)", "后台静默常驻、托盘右键控制与一键呼出", (0, 245, 155)),
                ("Tauri IPC 命令管道", "原生跨进程命令调用与零延迟主动事件推送", (0, 229, 255)),
                ("Android 跨端原生扩展", "移动端工程构建支持 (gen/android 出包)", (255, 170, 0)),
            ]
        },
        {
            "num": "第 3 层",
            "name": "网络服务与外部智能体生态层 (Services & AI Tier)",
            "color": (255, 42, 68),
            "cards": [
                ("laser-host 异步服务器", "内置 Axum/Actix 工业级异步 Web HTTP 宿主服务", (255, 42, 68)),
                ("laser-api 控制器", "RESTful 路由、局域网配对安全 Token 鉴权中间件", (241, 245, 253)),
                ("WebSocket 事件广播流", "双向毫秒级状态广播 (作业进度、实时坐标回传)", (0, 229, 255)),
                ("laser-mcp 协议引擎", "Model Context Protocol 标准协议服务器，赋予空间智能", (255, 42, 68)),
                ("外部 AI 语音与智能体", "小智音箱 / Claude / 外部大模型自主下发加工任务", (255, 170, 0)),
            ]
        },
        {
            "num": "第 4 层",
            "name": "业务运行时与安全作业调度层 (Runtime & Workflow Tier)",
            "color": (255, 170, 0),
            "cards": [
                ("laser-runtime 状态机", "严格的工作流有限状态机管理 (pending -> running -> completed)", (255, 170, 0)),
                ("RepeatSendGuard 碰撞守卫", "任务指纹核验，拦截同一指纹重复下发，防烧穿起火", (255, 42, 68)),
                ("GlobalShutdownCoordinator", "全域 5 级物理安全急停总线协调器 (前端/Ctrl+C/Panic/掉线)", (255, 42, 68)),
                ("Tokio 异步作业执行队列", "高并发长周期切削任务异步隔离，防界面死锁", (241, 245, 253)),
                ("急停优先级抢占中断", "0x18 / M05 中断无视队列瞬态直达硬件", (0, 245, 155)),
            ]
        },
        {
            "num": "第 5 层",
            "name": "制造算法与核心编译引擎层 (Core Manufacturing Engine Tier)",
            "color": (0, 229, 255),
            "cards": [
                ("光栅二值抖动矩阵", "Floyd-Steinberg 与 Atkinson 算法，灰度图转激光点阵", (0, 229, 255)),
                ("RDP 矢量几何拟合", "Ramer-Douglas-Peucker 算法精简优化矢量走刀路径", (241, 245, 253)),
                ("G-code 汇编装配编译器", "生成标准工业数控 G-code (G90/G21/G00/G01/M03/M05)", (0, 229, 255)),
                ("laser_materials.json", "材料工艺库自动推荐模型与校准矩阵算法", (255, 170, 0)),
                ("laser-contracts 契约规范", "强类型数据模型、Serde 跨语言序列化契约与错误枚举", (0, 245, 155)),
            ]
        },
        {
            "num": "第 6 层",
            "name": "硬件抽象与通信驱动层 (Hardware Abstraction & Driver Tier)",
            "color": (0, 245, 155),
            "cards": [
                ("laser-device 硬件抽象层", "统一屏蔽串口与网络差异，抽象标准流控收发接口", (0, 245, 155)),
                ("serialport 物理串口驱动", "跨平台物理串口管理 (CH340 / CP2102，115200 波特率)", (241, 245, 253)),
                ("tokio-net 网络无线驱动", "异步 TCP / Telnet 客户端通信驱动 (端口 23)", (255, 170, 0)),
                ("GRBL 协议流控状态机", "逐行发送、ok/error 响应解析与机床缓冲区深度管理", (0, 229, 255)),
                ("物理急停 OOB 瞬发器", "0x18 / M05 瞬发通道，脱机物理快速熔断", (255, 42, 68)),
            ]
        },
        {
            "num": "第 7 层",
            "name": "物理数控机床与执行机构层 (Physical CNC Hardware Tier)",
            "color": (200, 215, 235),
            "cards": [
                ("GRBL 工业数控主板", "ESP32 / STM32 / Arduino / 无线专用雕刻机控制板", (241, 245, 253)),
                ("X/Y 两轴步进电机", "双轴高精度细分步进电机与同步带滑轨机械结构", (0, 229, 255)),
                ("高能激光发生器管", "450nm 蓝光 / 1064nm 红外激光头与光学聚焦透镜组", (255, 42, 68)),
                ("物理急停大红钮", "硬件级常闭断电物理大红钮与光电极限制动开关", (255, 42, 68)),
                ("辅助气泵与风道", "智能协同气动辅助吹气泵与烟雾排风净化机组", (0, 245, 155)),
            ]
        },
    ]

    for idx, tier in enumerate(tiers_data):
        y = y_start + idx * (tier_h + tier_gap)
        draw_hud_card(draw, 50, y, tier_w, tier_h, border_color=(*tier["color"], 180))

        # 标题栏
        draw.text((75, y + 16), f"{tier['num']} · {tier['name']}", font=fonts["header"], fill=tier["color"])
        draw.line([(75, y + 46), (50 + tier_w - 25, y + 46)], fill=(*tier["color"], 70), width=1)

        # 内部 5 个卡片并排
        c_w = (tier_w - 70) // 5
        c_h = 120
        c_y = y + 58
        for c_idx, (c_title, c_desc, c_col) in enumerate(tier["cards"]):
            c_x = 75 + c_idx * (c_w + 10)
            draw.rounded_rectangle([c_x, c_y, c_x + c_w, c_y + c_h], radius=6, fill=(18, 28, 48, 200), outline=(50, 75, 110, 160))
            draw.text((c_x + 12, c_y + 12), c_title, font=fonts["subhead"], fill=c_col)
            draw.text((c_x + 12, c_y + 40), c_desc, font=fonts["caption"], fill=(175, 192, 215))

    os.makedirs(os.path.dirname(output_path), exist_ok=True)
    img.save(output_path, "PNG")
    print(f"Tech stack image rendered successfully: {output_path}")

if __name__ == "__main__":
    print("Rendering Architecture & Pipeline Images...")
    p1 = os.path.abspath("assets/moss-project-pipeline.png")
    p2 = os.path.abspath("assets/moss-tech-stack-architecture.png")
    render_pipeline_image(p1)
    render_tech_stack_image(p2)
    print("All images generated successfully!")
