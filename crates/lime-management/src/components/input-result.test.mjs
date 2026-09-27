import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import ts from "typescript";

const require = createRequire(import.meta.url);
const sourceRoot = fileURLToPath(new URL("../", import.meta.url));
const modules = new Map();
function loadSource(path) {
  const file = [path, `${path}.ts`, `${path}.tsx`].find(existsSync);
  if (!file) throw new Error(`Missing source module: ${path}`);
  if (modules.has(file)) return modules.get(file).exports;
  const module = { exports: {} };
  modules.set(file, module);
  const { outputText } = ts.transpileModule(readFileSync(file, "utf8"), {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, esModuleInterop: true },
    fileName: file,
  });
  const sourceRequire = (specifier) => specifier.startsWith("@/")
    ? loadSource(resolve(sourceRoot, specifier.slice(2)))
    : specifier.startsWith(".") ? loadSource(resolve(dirname(file), specifier)) : require(specifier);
  new Function("require", "module", "exports", outputText)(sourceRequire, module, module.exports);
  return module.exports;
}

const { InputResult } = loadSource(resolve(sourceRoot, "components/input-result.tsx"));
const candidate = (text) => ({ displayText: text, commitText: text });
const data = (overrides = {}) => ({
  timestampMs: null, endToEndMs: null, precedingText: "", preedit: "nihao", rimeCandidates: [], finalCandidates: [], diagnostics: [], llmPerformance: null, rimeMs: null, model: null, contextUsed: null, serviceState: "rime_only", ...overrides,
});
const diagnostic = (overrides = {}) => ({ index: 0, rimeCandidate: null, llmCandidate: null, logprob: null, logprobs: [], mismatch: null, displayCandidate: null, hasDisplayCandidate: false, ...overrides });
const render = (value, effectiveCount = 3) => renderToStaticMarkup(createElement(InputResult, { data: value, effectiveCount }));
function tableRows(html, label) {
  const table = html.match(new RegExp(`aria-label="${label}"[^>]*>([\\s\\S]*?)</table>`))?.[1];
  assert.ok(table, `${label} table is present`);
  const body = table.match(/<tbody[^>]*>([\s\S]*?)<\/tbody>/)?.[1];
  assert.ok(body);
  return [...body.matchAll(/<tr[^>]*>([\s\S]*?)<\/tr>/g)].map((row) => [...row[1].matchAll(/<td[^>]*>([\s\S]*?)<\/td>/g)].map((cell) => cell[1].replace(/<[^>]*>/g, "")));
}

test("candidate results retain fallback candidates from older service responses", () => {
  const html = render(data({ rimeCandidates: [candidate("你好"), candidate("拟好")], finalCandidates: [candidate("拟好"), candidate("你好")] }));
  assert.deepEqual(tableRows(html, "候选结果"), [["1", "你好", "拟好", "—", "—", "—", "拟好"], ["2", "拟好", "你好", "—", "—", "—", "你好"]]);
  assert.doesNotMatch(html, /<(?:details|summary)\b/);
});

test("explicit display candidates win and legacy rows respect the effective count", () => {
  const html = render(data({ diagnostics: [
    diagnostic({ llmCandidate: candidate("第一") }),
    diagnostic({ llmCandidate: candidate("第二") }),
    diagnostic({ llmCandidate: candidate("第三"), displayCandidate: candidate("真实展示"), hasDisplayCandidate: true }),
  ] }), 1);
  assert.deepEqual(tableRows(html, "候选结果").map((row) => row[6]), ["第一", "—", "真实展示"]);
});

test("score details retain aggregate rounding and withhold unscored token data", () => {
  const html = render(data({ diagnostics: [
    diagnostic({ llmCandidate: candidate("你好"), logprob: -0.03, logprobs: [-0.015, -0.015], mismatch: true }),
    diagnostic({ logprob: -1, logprobs: [-1], mismatch: false }),
  ] }));
  assert.deepEqual(tableRows(html, "候选结果"), [["1", "—", "你好", "-0.03", "-0.01, -0.02", "是", "你好"], ["2", "—", "—", "—", "—", "—", "—"]]);
});

test("candidates and scores share one always-visible table with metrics below it", () => {
  const html = render(data({
    rimeMs: 3,
    endToEndMs: 12,
    contextUsed: true,
    diagnostics: [diagnostic({ rimeCandidate: candidate("你好"), llmCandidate: candidate("拟好"), logprob: -0.5, logprobs: [-0.5], mismatch: false })],
    llmPerformance: { totalMs: 8, tokenizeMs: 1, decodeMs: 7, candidateCount: 2, scoredCount: 2, targetTokenCount: 4, batchCount: 1, mismatchCount: 0, contextTokenCount: 5, decodeInputTokenCount: 9, logprobOutputCount: 4, inferenceCountLimit: 1, omittedCandidateCount: 0, boundaryRollback: null },
  }));
  assert.equal([...html.matchAll(/<table\b/g)].length, 1);
  assert.doesNotMatch(html, /<(?:details|summary)\b/);
  assert.ok(!html.includes("评分详情"));
  assert.deepEqual([...html.matchAll(/<th\b[^>]*>([\s\S]*?)<\/th>/g)].map((cell) => cell[1]), ["#", "Rime 候选", "排序候选", "Logprob", "Logprobs", "边界不匹配", "展示候选"]);
  assert.deepEqual(tableRows(html, "候选结果"), [["1", "你好", "拟好", "-0.50", "-0.50", "否", "拟好"]]);
  const metricRows = [...html.matchAll(/<dt\b[^>]*>([\s\S]*?)<\/dt><dd\b[^>]*>([\s\S]*?)<\/dd>/g)].map((match) => [match[1], match[2]]);
  assert.deepEqual(metricRows, [
    ["端到端用时", "12 ms"], ["Rime 用时", "3 ms"], ["推理用时", "7 ms"], ["总评分用时", "8 ms"], ["分词用时", "1 ms"],
    ["送入候选", "2"], ["返回得分", "2"], ["目标 Token", "4"], ["解码批次", "1"], ["边界不匹配", "0"],
    ["上下文 Token", "5"], ["Decode 输入行", "9"], ["Logprob 输出行", "4"], ["推理次数上限", "1"], ["未评分候选", "0"], ["使用上文", "是"],
  ]);
  assert.ok(html.indexOf("</table>") < html.indexOf("<dl"));
  assert.match(html, /data-slot="table-container"[^>]*class="[^"]*overflow-x-auto/);
});

test("empty results retain a single full-width empty table row", () => {
  const html = render(data());
  assert.equal([...html.matchAll(/<table\b/g)].length, 1);
  assert.deepEqual(tableRows(html, "候选结果"), [["暂无候选"]]);
  assert.match(html, /colSpan="7"/);
  assert.doesNotMatch(html, /<(?:details|summary)\b/);
});

test("rollback detail uses and escapes service text without inferring text from context", () => {
  const html = render(data({ precedingText: "不能用来推断边界", llmPerformance: { boundaryRollback: { prefixTokenCount: 2, replayedTokenCount: 1, prefixText: "<script>\n ", replayedText: "尾部 & 内容" } } }));
  assert.match(html, /&lt;script&gt;\n /);
  assert.ok(html.includes("尾部 &amp; 内容"));
  assert.ok(!html.includes("<script>"));
  assert.ok(html.includes('aria-label="评分起点"'));
  const tokenOnly = render(data({ llmPerformance: { boundaryRollback: { prefixTokenCount: 0, replayedTokenCount: 1, prefixText: null, replayedText: null } } }));
  assert.ok(tokenOnly.includes("上文开头"));
  assert.ok(!tokenOnly.includes('aria-label="评分起点"'));
  assert.ok(!render(data()).includes("data-boundary-rollback"));
});
