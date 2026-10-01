import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("./benchmark-form.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
});
const { buildBenchmarkRequest, benchmarkProgress, benchmarkCurrentProgress, benchmarkErrorExamples } = await import("data:text/javascript;base64," + Buffer.from(outputText).toString("base64"));

function draft(rerankCount = "32", contextLimit = "128", id = "0") {
  return { id, rerankCount, contextLimit };
}

test("deduplicates the complete matrix before applying its 32-combination limit", () => {
  const models = Array.from({ length: 8 }, (_, index) => `model-${index}`);
  const selections = models.map((name) => ({ kind: "preset", name }));
  const configurations = [draft(), draft("128", "4096", "1"), draft("32", "128", "2")];
  const request = buildBenchmarkRequest(["full", "initials", "full"], [...selections, selections[0]], configurations);
  assert.deepEqual(request.modes, ["full", "initials"]);
  assert.deepEqual(request.models, selections);
  assert.deepEqual(request.configurations, [
    { llm_rerank_count: 32, preceding_text_char_limit: 128 },
    { llm_rerank_count: 128, preceding_text_char_limit: 4096 },
  ]);
  assert.throws(() => buildBenchmarkRequest(["full", "initials"], [...selections, { kind: "preset", name: "one-too-many" }], configurations), /最多评测 32/);
  assert.equal(configurations.length, 3);
});

test("requires every matrix dimension and accepts both numeric boundaries", () => {
  assert.throws(() => buildBenchmarkRequest([], [{ kind: "preset", name: "model" }], [draft()]), /拼音模式/);
  assert.throws(() => buildBenchmarkRequest(["full"], [], [draft()]), /模型/);
  assert.throws(() => buildBenchmarkRequest(["full"], [{ kind: "preset", name: "model" }], []), /配置/);
  assert.deepEqual(buildBenchmarkRequest(["full"], [{ kind: "preset", name: "model" }], [draft("1", "1"), draft("128", "4096")]).configurations, [
    { llm_rerank_count: 1, preceding_text_char_limit: 1 },
    { llm_rerank_count: 128, preceding_text_char_limit: 4096 },
  ]);
});

test("builds corpus-scoped requests with an explicit Rime-only model", () => {
  const request = buildBenchmarkRequest(
    ["full", "full"],
    [{ kind: "rime_only" }, { kind: "preset", name: "local" }, { kind: "rime_only" }],
    [draft()],
    ["zhihu", "zhihu", "classics"],
  );
  assert.deepEqual(request.models, [{ kind: "rime_only" }, { kind: "preset", name: "local" }]);
  assert.deepEqual(request.corpora, ["zhihu", "classics"]);
  assert.throws(() => buildBenchmarkRequest(["full"], [{ kind: "rime_only" }], [draft()], []), /语料/);
});

test("Rime-only requests ignore invalid LLM drafts and count only mode rows", () => {
  const request = buildBenchmarkRequest(["full", "initials"], [{ kind: "rime_only" }], [draft("", "")], ["zhihu"]);
  assert.deepEqual(request.configurations, []);
  assert.deepEqual(request.models, [{ kind: "rime_only" }]);
});

test("rejects incomplete, fractional, nonnumeric and out-of-range configuration edits", () => {
  for (const value of ["", "0", "-1", "129", "1.5", "NaN", "Infinity"]) {
    assert.throws(() => buildBenchmarkRequest(["full"], [{ kind: "preset", name: "model" }], [draft(value)]), /1–128/);
  }
  for (const value of ["", "0", "-1", "4097", "1.5", "NaN", "Infinity"]) {
    assert.throws(() => buildBenchmarkRequest(["full"], [{ kind: "preset", name: "model" }], [draft("32", value)]), /1–4096/);
  }
});

test("progress remains bounded without exposing case counts", () => {
  assert.ok(Math.abs(benchmarkProgress({ total: 3, completed: 1 }) - 100 / 3) <= Number.EPSILON * 100);
  assert.equal(benchmarkProgress({ total: 100, completed: 130 }), 100);
  assert.equal(benchmarkProgress({ total: 100, completed: -5 }), 0);
  assert.equal(benchmarkProgress({ total: 0, completed: 20 }), 0);
  assert.equal(benchmarkProgress({ total: -1, completed: 0 }), 0);
  assert.equal(benchmarkProgress({ total: Infinity, completed: 0 }), 0);
  assert.equal(benchmarkProgress({ total: 1, completed: NaN }), 0);
});

test("current item progress is scoped to one corpus run", () => {
  assert.equal(benchmarkCurrentProgress({ processed: 25, total: 100 }), 25);
  assert.equal(benchmarkCurrentProgress({ processed: 200, total: 100 }), 100);
  assert.equal(benchmarkCurrentProgress(null), 0);
});

test("expanded errors retain source text and stop at 40 without mutating the export", () => {
  const observations = [
    { caseId: "correct", correct: true, error: null },
    { caseId: "unknown", correct: null, error: null },
    { caseId: "exception", correct: null, error: "推理失败", context: " 原始\n上文 <保留> " },
    ...Array.from({ length: 45 }, (_, index) => ({ caseId: `wrong-${index}`, correct: false, error: null })),
  ];
  const result = { report: { observations } };
  const examples = benchmarkErrorExamples(result);
  assert.equal(examples.length, 40);
  assert.equal(examples[0].caseId, "exception");
  assert.equal(examples[0].context, " 原始\n上文 <保留> ");
  assert.equal(examples[39].caseId, "wrong-38");
  assert.equal(result.report.observations.length, 48);
  assert.deepEqual(benchmarkErrorExamples({ report: null }), []);
});
