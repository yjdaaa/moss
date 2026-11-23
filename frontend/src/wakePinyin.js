// 唤醒词显示文字 → 拼音（2026-11-23 用户决定：改显示文字时拼音自动跟着更新，读错了用户自己改）。
// 按输入法的做法用词组判断多音字（pinyin-pro 自带词库，如“银行”hang、“行长”zhang）。
// 词库约 300 KB，只在唤醒词卡里按需加载，不进主包。
import { customPinyin, pinyin } from "pinyin-pro";

// pinyin-pro 默认把“嗨”读 hei，唤醒词里几乎都读 hai（“嗨 MOSS”）
customPinyin({ 嗨: "hāi" });

// 英文部分：MOSS 按中文读作“莫斯”；其他英文原样保留，交给用户改
const LATIN_READINGS = { moss: ["mo", "si"] };
const DIGITS = ["ling", "yi", "er", "san", "si", "wu", "liu", "qi", "ba", "jiu"];

/** 把显示文字转成设备识别用的拼音命令词：小写、无声调、ü 写作 v、音节用空格隔开。 */
export function wakeTextToPinyin(text) {
  const parts = pinyin(String(text ?? ""), { toneType: "none", type: "array", v: true, nonZh: "consecutive" });
  const out = [];
  for (const part of parts) {
    for (const token of part.toLowerCase().match(/[a-z]+|\d+/g) ?? []) {
      if (/^\d+$/.test(token)) out.push(...[...token].map((digit) => DIGITS[Number(digit)]));
      else out.push(...(LATIN_READINGS[token] ?? [token]));
    }
  }
  return out.join(" ");
}
