import type { DictionaryEntry } from "@/api/types";

export function parseDictionary(text: string): DictionaryEntry[] {
  const parsed: unknown = JSON.parse(text);
  if (!Array.isArray(parsed)) throw new Error("词库 JSON 必须是条目数组");
  return parsed.map((value: unknown) => {
    if (!value || typeof value !== "object") throw new Error("词库条目格式无效");
    const entry = value as Partial<DictionaryEntry>;
    if (typeof entry.pinyin !== "string" || !entry.pinyin.trim() || typeof entry.text !== "string" || !entry.text.trim() || typeof entry.weight !== "number" || !Number.isInteger(entry.weight)) {
      throw new Error("词库条目必须包含拼音、词条和整数词频");
    }
    return { pinyin: entry.pinyin, text: entry.text, weight: entry.weight };
  });
}
