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

const { readBenchmarkSnapshot } = await loadTypeScript("./benchmark-resource.ts");
const { createRefreshQueue } = await loadTypeScript("../lib/refresh-queue.ts");
const tick = () => new Promise((resolve) => setImmediate(resolve));

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

test("a corpus failure waits for the outstanding status read before a queued retry", async () => {
  const firstStatus = deferred();
  const dataset = { id: "corpus", version: 2, corpora: [] };
  const state = { status: "completed", configRevision: 3, rimeSnapshotSha256: "snapshot", results: [] };
  const failure = new Error("读取语料失败");
  let datasetCalls = 0;
  let statusCalls = 0;
  let statusInFlight = 0;
  let maxStatusInFlight = 0;
  const errors = [];
  const snapshots = [];
  const queue = createRefreshQueue(async (isCurrent) => {
    try {
      const snapshot = await readBenchmarkSnapshot(
        async () => {
          datasetCalls += 1;
          if (datasetCalls === 1) throw failure;
          return dataset;
        },
        async () => {
          statusCalls += 1;
          statusInFlight += 1;
          maxStatusInFlight = Math.max(maxStatusInFlight, statusInFlight);
          try { return statusCalls === 1 ? await firstStatus.promise : state; }
          finally { statusInFlight -= 1; }
        },
      );
      if (isCurrent()) snapshots.push(snapshot);
    } catch (error) { if (isCurrent()) errors.push(error); }
  });
  const initial = queue.request();
  await tick();
  const retry = queue.request();
  await tick();
  assert.equal(datasetCalls, 1);
  assert.equal(statusCalls, 1);
  assert.deepEqual(errors, []);
  firstStatus.resolve(state);
  await Promise.all([initial, retry]);
  assert.equal(datasetCalls, 2);
  assert.equal(statusCalls, 2);
  assert.equal(maxStatusInFlight, 1);
  assert.deepEqual(errors, [failure]);
  assert.deepEqual(snapshots, [{ dataset, state }]);
  queue.dispose();
});

test("a status failure also waits for the corpus read and preserves its error", async () => {
  const corpus = deferred();
  const failure = new Error("读取评测失败");
  let settled = false;
  const snapshot = readBenchmarkSnapshot(() => corpus.promise, async () => { throw failure; });
  const completion = snapshot.catch((error) => { settled = true; return error; });
  await tick();
  assert.equal(settled, false);
  corpus.resolve({ id: "corpus", corpora: [] });
  assert.equal(await completion, failure);
});

test("a successful snapshot preserves all metadata without reshaping the report", async () => {
  const dataset = { id: "corpus", name: "语料", version: 2, corpora: [{ id: "zhihu", name: "知乎", characters: 42000, cases: 12000 }] };
  const state = { status: "completed", configRevision: 3, rimeSnapshotSha256: "snapshot", results: [{ modelSha256: "model", config: { llm_rerank_count: 32 } }] };
  const result = await readBenchmarkSnapshot(async () => dataset, async () => state);
  assert.equal(result.dataset, dataset);
  assert.equal(result.state, state);
});
