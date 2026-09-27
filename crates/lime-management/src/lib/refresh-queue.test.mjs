import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

const source = await readFile(new URL("./refresh-queue.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, { compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ESNext } });
const { createRefreshQueue } = await import("data:text/javascript;base64," + Buffer.from(outputText).toString("base64"));
function deferred() { let resolve; const promise = new Promise((done) => { resolve = done; }); return { promise, resolve }; }
const tick = () => new Promise((resolve) => setImmediate(resolve));

test("poll bursts merge and never overlap the current read", async () => {
  const gates = [deferred(), deferred()];
  let calls = 0;
  let active = 0;
  let maximum = 0;
  const queue = createRefreshQueue(async () => {
    maximum = Math.max(maximum, ++active);
    await gates[calls++].promise;
    active--;
  });
  const first = queue.request();
  await tick();
  for (let i = 0; i < 20; i++) assert.equal(queue.request(), first);
  assert.equal(calls, 1);
  gates[0].resolve(); await tick();
  assert.equal(calls, 2);
  gates[1].resolve(); await first;
  assert.equal(maximum, 1);
});

test("a mutation rejects an old snapshot and drops already queued polling", async () => {
  const gate = deferred();
  const committed = [];
  let calls = 0;
  const queue = createRefreshQueue(async (isCurrent) => {
    const value = ++calls;
    if (value === 1) await gate.promise;
    if (isCurrent()) committed.push(value);
  });
  const pending = queue.request(); await tick();
  void queue.request(); queue.invalidate(); gate.resolve(); await pending;
  assert.deepEqual(committed, []);
  assert.equal(calls, 1);
  await queue.request(); assert.deepEqual(committed, [2]);
});

test("a newer page waits for the old request while excluding its response", async () => {
  const gate = deferred();
  let page = 1;
  const values = [];
  const queue = createRefreshQueue(async (isCurrent) => {
    const requestedPage = page;
    if (requestedPage === 1) await gate.promise;
    if (isCurrent()) values.push(requestedPage);
  });
  const first = queue.request(); await tick();
  page = 2; queue.invalidate(); void queue.request(); gate.resolve(); await first;
  assert.deepEqual(values, [2]);
});

test("disposed reads cannot commit, and failed reads can be retried", async () => {
  const gate = deferred(); let committed = false;
  const queue = createRefreshQueue(async (isCurrent) => { await gate.promise; committed = isCurrent(); });
  const pending = queue.request(); await tick(); queue.dispose(); gate.resolve(); await pending; await queue.request();
  assert.equal(committed, false);
  let attempts = 0;
  const retry = createRefreshQueue(async () => { if (++attempts === 1) throw new Error("offline"); });
  await assert.rejects(retry.request(), /offline/); await retry.request(); assert.equal(attempts, 2);
});
