import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("./history-state.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } });
const { sortHistory, keyedHistoryEntries, createHistoryWatcher } = await import("data:text/javascript;base64," + Buffer.from(outputText).toString("base64"));

function entry(overrides = {}) {
  return { timestampMs: null, requestId: undefined, precedingText: "", preedit: "nihao", model: null, rimeCandidates: [], finalCandidates: [], ...overrides };
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((accept, decline) => { resolve = accept; reject = decline; });
  return { promise, resolve, reject };
}

const tick = () => new Promise((resolve) => setImmediate(resolve));

test("history uses timestamps, preserving server order when timestamps are absent", () => {
  const timestamped = [entry({ timestampMs: 100, requestId: 999 }), entry({ timestampMs: 300, requestId: 1 }), entry({ timestampMs: 200 })];
  const page = { items: timestamped, total: 3, page: 1, pageSize: 100 };
  assert.deepEqual(sortHistory(page).items.map((item) => item.timestampMs), [300, 200, 100]);
  assert.equal(page.items[0].timestampMs, 100);
  const legacy = [entry({ requestId: 1 }), entry({ requestId: 9 }), entry({ requestId: 3 })];
  assert.deepEqual(sortHistory({ ...page, items: legacy }).items, legacy);
});

test("history keys keep existing rows stable when new records arrive", () => {
  const original = [entry({ timestampMs: 100 }), entry({ requestId: 5 }), entry({ preedit: "shijie" })];
  const keys = keyedHistoryEntries(original).map(({ key }) => key);
  assert.deepEqual(keyedHistoryEntries([entry({ timestampMs: 200 }), ...original]).slice(1).map(({ key }) => key), keys);
  const duplicateKeys = keyedHistoryEntries([entry(), entry()]).map(({ key }) => key);
  assert.equal(new Set(duplicateKeys).size, 2);
});

test("history remount reuses the pending native wait and advances the server revision", async () => {
  const calls = [];
  const subscribe = createHistoryWatcher((revision) => {
    const pending = deferred();
    calls.push({ revision, ...pending });
    return pending.promise;
  });
  let changes = 0;
  const first = subscribe({ onChange: () => { throw new Error("unmounted observer called"); }, onError: (error) => { throw error; } });
  assert.equal(calls.length, 1);
  first();
  const second = subscribe({ onChange: () => { changes += 1; }, onError: (error) => { throw error; } });
  assert.equal(calls.length, 1);
  calls[0].resolve(7);
  await tick();
  assert.equal(changes, 1);
  assert.equal(calls[1].revision, 7);
  calls[1].resolve(7);
  await tick();
  assert.equal(changes, 1);
  assert.equal(calls[2].revision, 7);
  second();
  calls[2].resolve(8);
  await tick();
  assert.equal(changes, 1);
  assert.equal(calls.length, 3);
});

test("a restarted service can reset its revision and invalid revisions retry", async () => {
  const calls = [];
  let changes = 0;
  let errors = 0;
  const subscribe = createHistoryWatcher((revision) => {
    const pending = deferred();
    calls.push({ revision, ...pending });
    return pending.promise;
  }, 0);
  const unsubscribe = subscribe({ onChange: () => { changes += 1; }, onError: () => { errors += 1; } });
  calls[0].resolve(10);
  await tick();
  calls[1].resolve(0);
  await tick();
  assert.equal(changes, 2);
  assert.equal(calls[2].revision, 0);
  calls[2].resolve(NaN);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(errors, 1);
  assert.equal(calls[3].revision, 0);
  unsubscribe();
  calls[3].resolve(0);
  await tick();
});
