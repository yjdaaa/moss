import os
import math
from PIL import Image, ImageDraw, ImageFilter, ImageFont

def create_moss_icon(size=1024):
    # 创建透明 RGBA 画布
    canvas = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(canvas)

    center = size / 2.0

    # 辅助函数：绘制同心圆
    def draw_circle(radius, fill=None, outline=None, width=1):
        bbox = [center - radius, center - radius, center + radius, center + radius]
        draw.ellipse(bbox, fill=fill, outline=outline, width=width)

    # 1. 外部机械装甲底座 (外轮廓 - 带有高光倒角的精密钛金机械轮廓)
    # 使用圆角多边形/双层厚重金属圆环
    r_outer_housing = size * 0.47

    # 外部深空暗部微投影 (Drop shadow)
    shadow = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    s_draw = ImageDraw.Draw(shadow)
    s_draw.ellipse(
        [center - r_outer_housing + 10, center - r_outer_housing + 20,
         center + r_outer_housing - 10, center + r_outer_housing + 20],
        fill=(0, 0, 0, 180)
    )
    shadow = shadow.filter(ImageFilter.GaussianBlur(size * 0.03))
    canvas = Image.alpha_composite(shadow, canvas)
    draw = ImageDraw.Draw(canvas)

    # 2. 钛金装甲外圈 (Titanium Armor Ring) - 渐变金属质感
    # 从上到下由浅金属灰到深钛钢
    r_armor = size * 0.46
    for i in range(int(size * 0.05)):
        r = r_armor - i
        t = i / (size * 0.05)
        # 上亮下暗的金属反光
        gray_val = int(140 - 50 * t)
        draw_circle(r, outline=(gray_val, gray_val + 5, gray_val + 15, 255), width=2)

    # 金属外倒角亮边
    draw_circle(r_armor, outline=(220, 230, 245, 240), width=int(size * 0.005))
    draw_circle(r_armor - size * 0.05, outline=(50, 60, 75, 255), width=int(size * 0.004))

    # 3. 机械螺栓与工业刻度 (6颗高亮工业沉头螺栓)
    bolt_radius = r_armor - size * 0.025
    bolt_size = size * 0.018
    for angle_deg in [30, 90, 150, 210, 270, 330]:
        rad = math.radians(angle_deg)
        bx = center + bolt_radius * math.cos(rad)
        by = center + bolt_radius * math.sin(rad)
        # 螺孔暗圈
        draw.ellipse([bx - bolt_size, by - bolt_size, bx + bolt_size, by + bolt_size], fill=(25, 30, 40, 255), outline=(180, 195, 215, 200), width=int(size * 0.003))
        # 螺丝内六角中心
        draw.ellipse([bx - bolt_size * 0.45, by - bolt_size * 0.45, bx + bolt_size * 0.45, by + bolt_size * 0.45], fill=(200, 215, 235, 255))

    # 工业环形刻度线 (HUD Graduation Marks)
    r_marks = r_armor - size * 0.055
    for deg in range(0, 360, 6):
        rad = math.radians(deg)
        is_major = (deg % 30 == 0)
        mark_len = size * 0.022 if is_major else size * 0.010
        x1 = center + r_marks * math.cos(rad)
        y1 = center + r_marks * math.sin(rad)
        x2 = center + (r_marks - mark_len) * math.cos(rad)
        y2 = center + (r_marks - mark_len) * math.sin(rad)
        mark_color = (255, 80, 80, 230) if (deg % 90 == 0) else ((80, 220, 255, 200) if is_major else (120, 140, 165, 140))
        draw.line([(x1, y1), (x2, y2)], fill=mark_color, width=int(size * 0.004 if is_major else size * 0.002))

    # 4. 光学镜筒壁与抗反射遮光阶梯环 (Lens Barrel & Steps)
    r_barrel = r_marks - size * 0.03
    draw_circle(r_barrel, fill=(16, 22, 32, 255), outline=(40, 55, 75, 255), width=int(size * 0.004))

    # 内凹阴影与黑色消光螺纹
    for r in range(int(r_barrel - size * 0.005), int(r_barrel - size * 0.06), -int(size * 0.008)):
        draw_circle(r, outline=(8, 12, 18, 255), width=int(size * 0.003))

    # 5. MOSS 警示标志与红环光带 (Warning Red Accent Ring)
    r_accent = r_barrel - size * 0.06
    draw_circle(r_accent, outline=(255, 30, 45, 220), width=int(size * 0.006))
    draw_circle(r_accent - size * 0.006, outline=(60, 10, 15, 255), width=int(size * 0.004))

    # 6. 光圈叶片底衬 (Aperture Blades Structure)
    r_iris = r_accent - size * 0.025
    draw_circle(r_iris, fill=(12, 10, 16, 255), outline=(120, 20, 30, 180), width=int(size * 0.004))

    # 绘制精密光圈叶片交叉线条 (8片光圈机构)
    for k in range(8):
        blade_angle = math.radians(k * 45 + 15)
        bx1 = center + r_iris * math.cos(blade_angle)
        by1 = center + r_iris * math.sin(blade_angle)
        bx2 = center + (r_iris * 0.4) * math.cos(blade_angle + 1.2)
        by2 = center + (r_iris * 0.4) * math.sin(blade_angle + 1.2)
        draw.line([(bx1, by1), (bx2, by2)], fill=(45, 40, 52, 220), width=int(size * 0.003))

    # 7. MOSS 核心单眼量子光球透镜 (The Quantum Core Red Eye)
    # 这是整个图标的灵魂！极其鲜明、通透、深邃、炽热！
    r_core = r_iris * 0.82

    # 底层暗红至深血红渐变
    core_img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    c_draw = ImageDraw.Draw(core_img)

    steps = int(r_core)
    for r in range(steps, 0, -1):
        frac = r / steps  # 1 到 0
        # 边缘深红黑 (120, 0, 15)，向中心过渡到烈焰赤红 (255, 35, 20)，再到炽热橙红 (255, 120, 20)
        if frac > 0.4:
            f = (frac - 0.4) / 0.6
            red = int(255 - 135 * f)
            green = int(35 - 35 * f)
            blue = int(25 - 10 * f)
            alpha = 255
        elif frac > 0.15:
            f = (frac - 0.15) / 0.25
            red = 255
            green = int(140 - 105 * f)
            blue = int(45 - 20 * f)
            alpha = 255
        else:
            f = frac / 0.15
            red = 255
            green = int(255 - 115 * f)
            blue = int(240 - 195 * f)
            alpha = 255

        c_draw.ellipse(
            [center - r, center - r, center + r, center + r],
            fill=(red, green, blue, alpha)
        )

    # 核心光晕层 (Radial Glow)
    glow_layer = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    g_draw = ImageDraw.Draw(glow_layer)
    g_draw.ellipse(
        [center - r_core * 1.08, center - r_core * 1.08, center + r_core * 1.08, center + r_core * 1.08],
        fill=(255, 30, 45, 140)
    )
    g_draw.ellipse(
        [center - r_core * 0.85, center - r_core * 0.85, center + r_core * 0.85, center + r_core * 0.85],
        fill=(255, 80, 30, 180)
    )
    glow_layer = glow_layer.filter(ImageFilter.GaussianBlur(size * 0.025))

    # 合成透镜发光
    canvas = Image.alpha_composite(canvas, glow_layer)
    canvas = Image.alpha_composite(canvas, core_img)
    draw = ImageDraw.Draw(canvas)

    # 8. MOSS 核心内部高科技视网膜环 (HUD Reticle Rings & Crosshairs)
    # 同心量子感测刻线
    draw_circle(r_core * 0.65, outline=(255, 210, 190, 190), width=int(size * 0.003))
    draw_circle(r_core * 0.40, outline=(255, 240, 220, 220), width=int(size * 0.003))
    draw_circle(r_core * 0.20, outline=(255, 255, 255, 240), width=int(size * 0.002))

    # 十字微型激光准星刻度
    cross_gap = r_core * 0.22
    cross_len = r_core * 0.62
    for angle in [0, 90, 180, 270]:
        rad = math.radians(angle)
        dx = math.cos(rad)
        dy = math.sin(rad)
        px1 = center + cross_gap * dx
        py1 = center + cross_gap * dy
        px2 = center + cross_len * dx
        py2 = center + cross_len * dy
        draw.line([(px1, py1), (px2, py2)], fill=(255, 230, 220, 200), width=int(size * 0.004))
        # 准星末端垂直刻线
        perp_dx = -dy
        perp_dy = dx
        w = size * 0.012
        draw.line([(px2 - w * perp_dx, py2 - w * perp_dy), (px2 + w * perp_dx, py2 + w * perp_dy)], fill=(255, 200, 180, 220), width=int(size * 0.003))

    # 9. 光学透镜弧形高光与镀膜反光 (Coated Lens Arc Glare)
    # 模拟真实高档光学镜头的物理反光
    glare = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    glare_draw = ImageDraw.Draw(glare)

    # 顶部主弧形强光 (Primary Curved Highlight)
    glare_bbox = [
        center - r_core * 0.78, center - r_core * 0.85,
        center + r_core * 0.78, center + r_core * 0.15
    ]
    glare_draw.arc(glare_bbox, start=205, end=335, fill=(255, 255, 255, 220), width=int(size * 0.022))

    # 极光蓝青色镜头抗反射化学镀膜弧 (Cyan Anti-reflective coating flare)
    coating_bbox = [
        center - r_core * 0.88, center - r_core * 0.88,
        center + r_core * 0.88, center + r_core * 0.88
    ]
    glare_draw.arc(coating_bbox, start=135, end=215, fill=(50, 235, 255, 170), width=int(size * 0.014))

    # 底部弱二次反射弧
    glare_draw.arc(glare_bbox, start=35, end=145, fill=(255, 180, 180, 90), width=int(size * 0.010))

    # 中心超炽热白光聚焦点 (Hot Spot)
    hot_x = center - r_core * 0.22
    hot_y = center - r_core * 0.25
    glare_draw.ellipse(
        [hot_x - size * 0.035, hot_y - size * 0.028, hot_x + size * 0.035, hot_y + size * 0.028],
        fill=(255, 255, 255, 240)
    )
    # 小伴随光斑
    glare_draw.ellipse(
        [hot_x + size * 0.05, hot_y + size * 0.04, hot_x + size * 0.065, hot_y + size * 0.055],
        fill=(255, 255, 255, 180)
    )

    glare = glare.filter(ImageFilter.GaussianBlur(size * 0.004))
    canvas = Image.alpha_composite(canvas, glare)
    draw = ImageDraw.Draw(canvas)

    # 10. 底部精工刻印 MOSS 与 550W 品牌字样
    # 在金属下环绘制流浪地球标志性微字
    try:
        # 优先寻找常见无衬线英文字体
        font_paths = [
            "C:/Windows/Fonts/segoeuib.ttf",
            "C:/Windows/Fonts/arialbd.ttf",
            "C:/Windows/Fonts/consola.ttf"
        ]
        font = None
        for fp in font_paths:
            if os.path.exists(fp):
                font = ImageFont.truetype(fp, int(size * 0.032))
                break
        if font:
            text = "550W · MOSS"
            bbox = font.getbbox(text)
            tw = bbox[2] - bbox[0]
            th = bbox[3] - bbox[1]
            tx = center - tw / 2.0
            ty = center + r_armor - size * 0.042
            # 刻蚀凹陷效果：暗色内影 + 亮色边缘
            draw.text((tx + 1, ty + 1), text, font=font, fill=(220, 235, 255, 200))
            draw.text((tx, ty), text, font=font, fill=(20, 25, 35, 240))
    except Exception as e:
        print("Font render skip:", e)

    return canvas

def export_all_icons(canvas, output_dir):
    os.makedirs(output_dir, exist_ok=True)

    # 1. 保存 1024x1024 高清原图 PNG
    png_1024_path = os.path.join(output_dir, "icon-1024.png")
    canvas.save(png_1024_path, "PNG")

    # 2. 保存 512x512 PNG (Tauri 默认 icon.png 标准)
    icon_png_path = os.path.join(output_dir, "icon.png")
    im_512 = canvas.resize((512, 512), Image.Resampling.LANCZOS)
    im_512.save(icon_png_path, "PNG")
    print(f"Saved: {icon_png_path} (512x512)")

    # 3. 生成符合 Windows 标准的全套多分辨率 ICO 文件
    # 包含: 256, 128, 64, 48, 32, 24, 16
    ico_sizes = [256, 128, 64, 48, 32, 24, 16]
    ico_images = [canvas.resize((s, s), Image.Resampling.LANCZOS) for s in ico_sizes]

    icon_ico_path = os.path.join(output_dir, "icon.ico")
    ico_images[0].save(
        icon_ico_path,
        format="ICO",
        sizes=[(s, s) for s in ico_sizes],
        append_images=ico_images[1:]
    )
    print(f"Saved: {icon_ico_path} with sizes: {ico_sizes}")

if __name__ == "__main__":
    print("Generating MOSS Quantum Eye icon...")
    img = create_moss_icon(1024)
    target_dir = os.path.abspath("apps/desktop/src-tauri/icons")
    export_all_icons(img, target_dir)
    # 同时复制一份到 frontend/public 或 frontend/assets 供网页展示
    frontend_dir = os.path.abspath("frontend/public")
    os.makedirs(frontend_dir, exist_ok=True)
    img.resize((128, 128), Image.Resampling.LANCZOS).save(os.path.join(frontend_dir, "favicon.ico"), format="ICO")
    img.resize((192, 192), Image.Resampling.LANCZOS).save(os.path.join(frontend_dir, "icon-192.png"), "PNG")
    img.resize((512, 512), Image.Resampling.LANCZOS).save(os.path.join(frontend_dir, "icon-512.png"), "PNG")
    print("MOSS icon generation completed!")
