import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

async function loadTypeScript(relativePath) {
  const source = await readFile(new URL(relativePath, import.meta.url), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext },
  });
  return import("data:text/javascript;base64," + Buffer.from(outputText).toString("base64"));
}

const { decodeBenchmarkDataset, decodeBenchmarkRunState } = await loadTypeScript("../api/decode.ts");
const { benchmarkResultAccuracy, sortBenchmarkResults } = await loadTypeScript("./benchmark.ts");

function wireResult(id, accuracy, overrides = {}) {
  return {
    id,
    model_name: id,
    model_sha256: "model-fingerprint",
    configuration: { llm_rerank_count: 32, preceding_text_char_limit: 128 },
    mode: "full",
    config: { rime_schema: "rime_ice", llm_rerank_count: 32, preceding_text_char_limit: 128 },
    status: "completed",
    report: {
      complete: true,
      modes: ["full"],
      summaries: [
        { category: null, mode: "full", accuracy },
        { category: "zhihu", mode: "full", accuracy: 0.25 },
        { category: "classics", mode: "full", accuracy: 0.75 },
      ],
      observations: [{ case_id: "example", category: "zhihu", mode: "full", context: "你好，2026!", preedit: "shijie", expected: "世界", top1: "时节", correct: false }],
    },
    ...overrides,
  };
}

test("decodes corpus directory without transferring or inventing case text", () => {
  const corpora = [{ id: "zhihu", name: "知乎", characters: 42000, cases: 12000 }, { id: "classics", name: "经典文章", characters: 44000, cases: 16000 }];
  assert.deepEqual(decodeBenchmarkDataset({ id: "benchmark", name: "语料", version: 2, corpora }), { id: "benchmark", name: "语料", version: 2, corpora });
});

test("decodes every matrix result and keeps export metadata and error context", () => {
  const state = decodeBenchmarkRunState({ status: "running", dataset_id: "benchmark", dataset_version: 2, config_revision: 7, rime_snapshot_sha256: "rime-fingerprint", total: 20, completed: 10, results: [wireResult("first", 0.5), wireResult("second", null, { status: "idle", report: null })] });
  assert.equal(state.results.length, 2);
  assert.equal(state.results[1].status, "idle");
  assert.equal(state.rimeSnapshotSha256, "rime-fingerprint");
  assert.equal(state.configRevision, 7);
  assert.equal(state.results[0].modelSha256, "model-fingerprint");
  assert.equal(state.results[0].config.preceding_text_char_limit, 128);
  assert.deepEqual(state.results[0].configuration, { llm_rerank_count: 32, preceding_text_char_limit: 128 });
  assert.equal(state.results[0].report.observations[0].context, "你好，2026!");
  assert.equal(state.results[0].report.observations[0].correct, false);
});

test("orders complete results by total accuracy and keeps incomplete rows last", () => {
  const { results } = decodeBenchmarkRunState({ results: [wireResult("queued", null, { status: "idle", report: null }), wireResult("low", 0.1), wireResult("high", 0.9), wireResult("failed", 1, { status: "failed" }), wireResult("zero", 0), wireResult("high-tie", 0.9)] });
  const original = [...results];
  assert.deepEqual(sortBenchmarkResults(results).map((row) => row.id), ["high", "high-tie", "low", "zero", "queued", "failed"]);
  assert.deepEqual(results, original);
  assert.equal(benchmarkResultAccuracy(results[4]), 0);
  assert.equal(benchmarkResultAccuracy(results[3]), null);
});

test("uses the requested corpus and mode, withholds partial and invalid accuracy", () => {
  const result = decodeBenchmarkRunState({ results: [wireResult("result", 0.5)] }).results[0];
  assert.equal(benchmarkResultAccuracy(result, "zhihu"), 0.25);
  assert.equal(benchmarkResultAccuracy(result, "classics"), 0.75);
  assert.equal(benchmarkResultAccuracy(result, "unknown"), null);
  assert.equal(benchmarkResultAccuracy({ ...result, mode: "initials" }), null);
  assert.equal(benchmarkResultAccuracy({ ...result, report: { ...result.report, complete: false } }), null);
  assert.equal(benchmarkResultAccuracy({ ...result, report: { ...result.report, summaries: [{ category: null, mode: "full", accuracy: NaN }] } }), null);
});
