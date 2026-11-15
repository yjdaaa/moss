# -*- coding: utf-8 -*-
"""汇总 real_local_chain_matrix（crates/moss-voice-server/src/tests/chat/chain.rs）的逐段记录：
python3 -I scripts/voice-bench/chain_report.py <结果.jsonl>"""
import json, re, statistics, sys, unicodedata

DIG = "零一二三四五六七八九"
def cn(n):
    if n < 10: return DIG[n]
    out = ""; zero = False
    for v, u in [(1000, "千"), (100, "百"), (10, "十")]:
        d = n // v; n %= v
        if d:
            if zero: out += "零"; zero = False
            out += ("" if (v == 10 and d == 1 and not out) else DIG[d]) + u
        elif out: zero = True
    if n:
        if zero: out += "零"
        out += DIG[n]
    return out
def spoken(t):
    t = re.sub(r"(\d+)\s*[%％]", lambda m: "百分之" + cn(int(m.group(1))), t or "")
    return re.sub(r"\d+", lambda m: cn(int(m.group(0))) if len(m.group(0)) <= 4 else m.group(0), t)
def norm(t):
    return "".join(ch for ch in unicodedata.normalize("NFKC", spoken(t)).lower() if ch.isalnum())
def edits(ref, hyp):
    r, h = norm(ref), norm(hyp)
    d = list(range(len(h) + 1))
    for i in range(1, len(r) + 1):
        prev, d[0] = d[0], i
        for j in range(1, len(h) + 1):
            cur = d[j]; d[j] = min(d[j] + 1, d[j - 1] + 1, prev + (r[i - 1] != h[j - 1])); prev = cur
    return d[len(h)], len(r)

rows = [json.loads(l) for l in open(sys.argv[1], encoding="utf-8")]
# 组合名与 chain.rs::combos() 一一对应
NAMES = {"rec": "Silero v4+SenseVoice+Kokoro1.0", "energy": "能量判停+SenseVoice+Kokoro1.0",
         "ten": "TEN+SenseVoice+Kokoro1.0", "qwen3": "Silero v4+Qwen3-ASR+Kokoro1.0",
         "firered": "Silero v4+FireRed+Kokoro1.0", "kokoro11": "Silero v4+SenseVoice+Kokoro1.1",
         "matcha": "Silero v4+SenseVoice+Matcha", "smartturn": "Silero v4+Smart Turn+SenseVoice+Kokoro1.0",
         "denoise": "Silero v4+GTCRN降噪+SenseVoice+Kokoro1.0", "silero6": "Silero v6.2+SenseVoice+Kokoro1.0",
         "silero6_gtcrn": "Silero v6.2+GTCRN+SenseVoice+Kokoro1.0", "silero6_dpdf": "Silero v6.2+DPDFNet+SenseVoice+Kokoro1.0",
         "silero6_turn": "Silero v6.2+Smart Turn+SenseVoice+Kokoro1.0", "silero6_qwen3": "Silero v6.2+Qwen3-ASR+Kokoro1.0",
         "silero6_kws": "Silero v6.2+命令词+SenseVoice+Kokoro1.0", "silero6_qwen3_kws": "Silero v6.2+命令词+Qwen3-ASR+Kokoro1.0"}
STOP_SPEECH = ("已发送停止请求", "停止请求发送失败")
LEVELS = {0: "安静", 999: "突发噪声", 888: "持续机器噪声"}

def analyse(r):
    on, off = r["on_sent"], r["off_sent"]
    ev = r["events"]
    # 流式识别会不断刷新同一句的中间结果；一轮的最终文字 = 该轮 tts_start 前最后一条 stt
    finals, last = [], None
    for t, kind, text in ev:
        if kind == "stt": last = (t, text)
        elif kind == "tts_start" and last: finals.append(last); last = None
    if last: finals.append(last)
    noise = [f for f in finals if f[0] < on]                 # 还没开口就出了识别结果
    speech = [f for f in finals if on <= f[0] <= off + 2500]
    late = [f for f in finals if f[0] > off + 2500]          # 尾巴噪声又触发了一轮
    early = [t for t in r["turns"] if on < t["start"] < off]  # 话没说完就开始回答（抢话）
    answer = next((t for t in r["turns"] if t["start"] >= off - 200), None)
    hyp = "".join(text for _, text in speech)
    e, n = edits(r["ref"], hyp)
    stopped = any(kind == "say" and text.startswith(STOP_SPEECH) for _, kind, text in ev)
    return dict(noise=len(noise) + len(late), early=len(early), e=e, n=n, hyp=hyp, stopped=stopped,
                latency=(answer["first_audio"] - off) if answer else None,
                stt_lag=(speech[-1][0] - off) if speech else None,
                stall=max((t["longest_stall"] for t in r["turns"]), default=0),
                dropped=sum(t["dropped"] for t in r["turns"]),
                closed=r["closed"], close_lag=(r["closed"] - off) if r["closed"] else None)

def pct(v, q):
    v = sorted(v); return v[min(len(v) - 1, int(round(q * (len(v) - 1))))] if v else float("nan")

print("| 组合 | 噪声 | 说完→听见回答 中位/最慢 | 识别字错率 | 话没说完被抢答 | 噪声误触发 | 最长卡顿 | 说退出词断开 | 说“停”执行停止 | 误执行命令 |")
print("|---|---|---|---|---|---|---|---|---|---|")
detail = []
for combo in NAMES:
    for level in sorted({r['level'] for r in rows}):
        rs = [r for r in rows if r["combo"] == combo and r["level"] == level]
        if not rs: continue
        a = [(r, analyse(r)) for r in rs]
        talk = [(r, x) for r, x in a if r["note"] not in ("exit", "stop")]
        lat = [x["latency"] for _, x in talk if x["latency"] is not None]
        miss = sum(1 for _, x in talk if x["latency"] is None)
        e = sum(x["e"] for _, x in talk); n = sum(x["n"] for _, x in talk)
        exits = [(r, x) for r, x in a if r["note"] == "exit"]
        ok_exit = sum(1 for _, x in exits if x["closed"])
        stops = [(r, x) for r, x in a if r["note"] == "stop"]
        ok_stop = sum(1 for _, x in stops if x["stopped"])
        wrong = sum(1 for r, x in a if (r["note"] != "exit" and x["closed"]) or (r["note"] != "stop" and x["stopped"]))
        latency = (f"{statistics.median(lat)/1000:.2f} / {max(lat)/1000:.2f} s" if lat else "—") + (f"（{miss} 句没回答）" if miss else "")
        cer = f"{100*e/n:.1f}%" if n else "—"
        print(f"| {NAMES[combo]} | {LEVELS[level]} | {latency} | {cer} | {sum(x['early'] for _, x in talk)} | {sum(x['noise'] for _, x in a)} | "
              f"{max(x['stall'] for _, x in a)} ms | {ok_exit}/{len(exits)} | {ok_stop}/{len(stops)} | {wrong} |")
        for r, x in a:
            if r["note"] in ("exit", "stop"):
                bad = not (x["closed"] if r["note"] == "exit" else x["stopped"])
                if bad:
                    detail.append(f"{combo:9} {level:3} {r['name']:12} 听成「{x['hyp']}」 命令没执行")
                continue
            if x["e"] or x["early"] or x["noise"] or x["latency"] is None or x["closed"] or x["stopped"]:
                detail.append(f"{combo:9} {level:3} {r['name']:12} 听成「{x['hyp']}」 错{x['e']} 抢话{x['early']} 误触发{x['noise']} 延迟{x['latency']} 断开{x['closed']}")
print()
print("\n".join(detail))
