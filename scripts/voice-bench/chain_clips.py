# -*- coding: utf-8 -*-
"""生成“整条链路连在一起”测试用的设备端说话片段（16 kHz s16，未编码；上行 Opus 由 Rust 测试按固件节奏实时编码发送）。
每段：2 秒前导（静音或噪声，用来看会不会被噪声抢答）+ 说话 + 2.5 秒尾巴（同噪声）。
说话用 macOS `say` 的 Tingting 合成（正常语速与 -r 260 两档），另加一段真人录音（如 SenseVoice 包里的 test_wavs/zh.wav）。
噪声档：0 安静、999 突发噪声（敲击 / 气泵咔嗒）、888 持续机器噪声（风扇嗡声 + 风噪，信噪比约 3–5 dB）。
用法：python3 -I scripts/voice-bench/chain_clips.py <输出目录> <真人 zh.wav> [噪声档，默认 0,999] [commands：只生成停止 / 退出命令词片段]"""
import array, json, os, random, subprocess, sys, wave

RATE = 16000

def read_wav(path):
    with wave.open(path) as f:
        assert f.getnchannels() == 1 and f.getsampwidth() == 2
        pcm = array.array("h"); pcm.frombytes(f.readframes(f.getnframes()))
        return resample(pcm, f.getframerate())

def resample(pcm, src):
    if src == RATE: return pcm
    n = len(pcm) * RATE // src; o = array.array("h")
    for i in range(n):
        x = i * src / RATE; j = int(x); t = x - j
        b = pcm[j + 1] if j + 1 < len(pcm) else pcm[j]
        o.append(int(pcm[j] * (1 - t) + b * t))
    return o

def say(voice, text, rate=None, out_dir="."):
    tmp = os.path.join(out_dir, "_say.wav")
    subprocess.run(["say", "-v", voice, *(["-r", str(rate)] if rate else []), "-o", tmp, "--data-format=LEI16@16000", text], check=True)
    pcm = read_wav(tmp); os.remove(tmp)
    return trim(pcm)

def trim(pcm):
    loud = [i for i, s in enumerate(pcm) if abs(s) > 300]
    return pcm[max(0, loud[0] - 800): loud[-1] + 800]

def gain(pcm, peak=12000):
    m = max(abs(s) for s in pcm) or 1
    return array.array("h", [int(s * peak / m) for s in pcm])

def noise(pcm, level, seed):
    rnd = random.Random(seed)
    if level == 0: return pcm
    if level == 999:
        # 突发噪声：轻微底噪 + 每 0.6–1.0 秒一次 80–160 ms 的宽带冲击（气泵咔嗒、敲击、关门）
        o = array.array("h", [max(-32768, min(32767, s + int(rnd.gauss(0, 120)))) for s in pcm])
        i = rnd.randint(2000, 9000)
        while i < len(o):
            length = rnd.randint(1280, 2560); amp = rnd.uniform(3000, 8000)
            for k in range(i, min(len(o), i + length)):
                o[k] = max(-32768, min(32767, o[k] + int(rnd.gauss(0, amp) * (1 - (k - i) / length))))
            i += length + rnd.randint(9600, 16000)
        return o
    if level == 888:
        # 持续机器噪声：排风扇/气泵的 100 Hz 嗡声 + 谐波 + 一阶低通的宽带风噪，响度接近说话（信噪比约 5 dB）
        import math
        o = array.array("h"); lp = 0.0
        for k, s in enumerate(pcm):
            t = k / RATE
            hum = 900 * math.sin(2 * math.pi * 100 * t) + 450 * math.sin(2 * math.pi * 200 * t + 1) + 250 * math.sin(2 * math.pi * 300 * t + 2)
            lp = 0.85 * lp + 0.15 * rnd.gauss(0, 5200)
            o.append(max(-32768, min(32767, int(s + hum + lp))))
        return o
    return array.array("h", [max(-32768, min(32767, s + int(rnd.gauss(0, level)))) for s in pcm])


def main():
    out, human = sys.argv[1:3]
    levels = [int(x) for x in sys.argv[3].split(",")] if len(sys.argv) > 3 else [0, 999]
    # 第 4 个参数为 commands 时只生成命令词片段（停止 / 退出），用来测命令词在噪声里认不认得出
    commands_only = len(sys.argv) > 4 and sys.argv[4] == "commands"
    os.makedirs(out, exist_ok=True)
    sentences = [
        ("status", "帮我看看激光机现在什么状态。"),
        ("power", "把功率调到百分之六十，速度一千五。"),
        ("start", "开始雕刻吧。"),
        ("wood", "椴木三毫米用多大功率能切透？"),
        ("chat", "今天有点累，陪我聊两句。"),
    ]
    speech = []  # (名字, 参考文本, pcm, 说明：pause700 句中停顿 / exit 退出词 / stop 停止词)
    voices = (("Tingting", "f", None), ("Tingting", "fast", 260))
    if commands_only:
        commands = (("stop", "停。", "stop"), ("stopdown", "停下。", "stop"), ("exit", "退下。", "exit"),
                    ("exit1", "退一下。", "exit"), ("bye", "再见。", "exit"))
        for voice, tag, rate in voices:
            for name, text, note in commands:
                speech.append((f"{tag}-{name}", text, gain(say(voice, text, rate, out)), note))
    else:
        speech.append(("human-zh", "开饭时间早上九点至下午五点", gain(trim(read_wav(human))), ""))
        for voice, tag, rate in voices:
            for name, text in sentences:
                speech.append((f"{tag}-{name}", text, gain(say(voice, text, rate, out)), ""))
            # 句中想一想：“帮我把功率调到……百分之六十”，停顿 700 ms
            a = say(voice, "帮我把功率调到", rate, out); b = say(voice, "百分之六十。", rate, out)
            speech.append((f"{tag}-pause", "帮我把功率调到百分之六十。", gain(a + array.array("h", [0] * (RATE * 7 // 10)) + b), "pause700"))
        # 退出词放最后：应当说完再见就断开（设备回到待机，不再“聆听中”）
        for voice, tag, rate in voices:
            speech.append((f"{tag}-exit", "退下。", gain(say(voice, "退下。", rate, out)), "exit"))

    meta = []
    lead, tail = RATE * 2, RATE * 5 // 2
    for level in levels:
        for n, (name, ref, pcm, note) in enumerate(speech):
            clip = array.array("h", [0] * lead) + pcm + array.array("h", [0] * tail)
            clip = noise(clip, level, 7 * n + level)
            f = f"{level}-{name}.s16"
            with open(os.path.join(out, f), "wb") as fp:
                fp.write(clip.tobytes())
            meta.append({"file": f, "name": name, "level": level, "ref": ref, "note": note,
                         "on_ms": lead * 1000 // RATE, "off_ms": (lead + len(pcm)) * 1000 // RATE,
                         "len_ms": len(clip) * 1000 // RATE})
    with open(os.path.join(out, "meta.json"), "w", encoding="utf-8") as fp:
        json.dump(meta, fp, ensure_ascii=False, indent=0)
    print(len(meta), "clips")


if __name__ == "__main__":
    main()
