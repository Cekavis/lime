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
  const corpora = [{ id: "zhihu", name: "知乎", characters: 42000, cases: 12000, articles: 3 }, { id: "classics", name: "经典文章", characters: 44000, cases: 16000, articles: 4 }];
  assert.deepEqual(decodeBenchmarkDataset({ id: "benchmark", name: "语料", version: 2, sha256: "dataset", directory: "C:/corpora", corpora }), { id: "benchmark", name: "语料", version: 2, sha256: "dataset", directory: "C:/corpora", corpora: corpora.map(({ articles, ...corpus }) => ({ ...corpus, documents: articles })) });
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

test("decodes Rime-only rows, corpus cells, current progress and queue", () => {
  const { results, current, queue } = decodeBenchmarkRunState({
    status: "running",
    results: [{
      id: "row",
      model: { kind: "rime_only" },
      model_name: "仅 Rime",
      configuration: { llm_rerank_count: 32, preceding_text_char_limit: 128 },
      mode: "full",
      status: "completed",
      cells: [{ key: "cell-key", corpus_id: "zhihu", status: "completed", total: 10, completed: 10, correct: 8, no_prediction: 0, errors: 2, accuracy: 0.8 }],
    }],
    current: { key: "next", model_name: "仅 Rime", corpus_id: "classics", completed: 3, total: 10, rate_per_second: 12.5, eta_seconds: 1 },
    queue: [{ key: "queued", model_name: "local", corpus_id: "zhihu", status: "pending" }],
  });
  assert.deepEqual(results[0].model, { kind: "rime_only" });
  assert.equal(results[0].corpusCells[0].id, "cell-key");
  assert.equal(results[0].corpusCells[0].summary.accuracy, 0.8);
  assert.equal(current.itemId, "next");
  assert.equal(current.processed, 3);
  assert.equal(current.rate, 12.5);
  assert.equal(queue[0].id, "queued");
  assert.equal(queue[0].status, "waiting");
});

test("does not count unprocessed partial cases as errors", () => {
  const { results } = decodeBenchmarkRunState({ results: [{
    id: "row",
    model: { kind: "rime_only" },
    configuration: { llm_rerank_count: 1, preceding_text_char_limit: 128 },
    mode: "full",
    status: "running",
    cells: [{ key: "cell", corpus_id: "zhihu", status: "running", total: 100, completed: 10, correct: 7, no_prediction: 1, errors: 1, accuracy: null }],
  }] });
  assert.equal(results[0].corpusCells[0].errorCount, 3);
  assert.equal(results[0].corpusCells[0].summary.errors, 3);
});

test("derives cell error count from incorrect cases instead of inference errors", () => {
  const { results } = decodeBenchmarkRunState({ results: [{
    id: "row",
    model: { kind: "rime_only" },
    configuration: { llm_rerank_count: 1, preceding_text_char_limit: 128 },
    mode: "full",
    status: "completed",
    cells: [{ key: "cell", corpus_id: "zhihu", status: "completed", total: 10, completed: 10, correct: 7, no_prediction: 1, errors: 1, accuracy: 0.7 }],
  }] });
  const cell = results[0].corpusCells[0];
  assert.equal(cell.errorCount, 3);
  assert.equal(cell.summary.errors, 3);
});

test("keeps an empty user corpus directory and loader error visible", () => {
  assert.deepEqual(decodeBenchmarkDataset({ id: "benchmark", name: "用户语料", version: 1, directory: "", error: "未发现 txt 文件", corpora: [] }), {
    id: "benchmark", name: "用户语料", version: 1, directory: "", error: "未发现 txt 文件", corpora: [],
  });
});

test("decodes paginated error pages without requiring report snapshots", async () => {
  const { decodeBenchmarkErrorPage } = await loadTypeScript("../api/decode.ts");
  const page = decodeBenchmarkErrorPage({ page: 2, page_size: 50, total: 75, items: [{ case_id: "case-2", context: "上文", preedit: "shijie", expected: "世界", top1: "时节", correct: false, error: "首位不匹配" }] }, 1);
  assert.equal(page.page, 2);
  assert.equal(page.total, 75);
  assert.equal(page.items[0].caseId, "case-2");
});
