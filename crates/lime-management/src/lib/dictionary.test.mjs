import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";
const source = await readFile(new URL("./dictionary.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } });
const { parseDictionary } = await import("data:text/javascript;base64," + Buffer.from(outputText).toString("base64"));

test("dictionary import preserves valid entries and rejects the whole invalid import", () => {
  const entries = [{ pinyin: "shi jie", text: "世界", weight: 4 }];
  assert.deepEqual(parseDictionary(JSON.stringify(entries)), entries);
  for (const invalid of [{}, null, { pinyin: "", text: "世界", weight: 1 }, { pinyin: "shi jie", text: " ", weight: 1 }, { pinyin: "shi jie", text: "世界", weight: 1.5 }]) {
    assert.throws(() => parseDictionary(JSON.stringify([...entries, invalid])));
  }
  assert.throws(() => parseDictionary("{}"));
  assert.throws(() => parseDictionary("bad json"));
});
