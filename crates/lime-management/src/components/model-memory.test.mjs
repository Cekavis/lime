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

const { decodeModel } = loadSource(resolve(sourceRoot, "api/decode.ts"));
const { buildModelMemoryTable } = loadSource(resolve(sourceRoot, "components/model-memory-data.ts"));
const { ModelMemory } = loadSource(resolve(sourceRoot, "components/model-memory.tsx"));
const loaded = (initialization_memory) => decodeModel({ loaded: true, size_bytes: 8 * 1024 ** 3, initialization_memory });
const allocation = (table, kind, device) => table.rows.find((row) => row.id === kind)?.values[table.devices.findIndex((column) => column.id === device)];
const render = (model, connected = true) => renderToStaticMarkup(createElement(ModelMemory, { model, connected }));

test("decodes optional summaries and rejects invalid byte counts without inventing old data", () => {
  const model = loaded({ model_bytes: 0, context_bytes: 2048, compute_bytes: -1, total_bytes: 4096, backend: "cuda", breakdown: {
    "cuda0.model": 0, "cuda0.kv": 1024, "cpu.compute": -1, unknown: null, string: "4", fractional: 1.5, infinite: Infinity, nan: NaN, unsafe: Number.MAX_SAFE_INTEGER + 1,
  } });
  assert.deepEqual(model.memory, { "cuda0.model": 0, "cuda0.kv": 1024 });
  assert.deepEqual(model.memorySummary, { modelBytes: 0, contextBytes: 2048, computeBytes: null, totalBytes: 4096, backend: "cuda" });
  const legacy = decodeModel({ loaded: true, size_bytes: 1024 });
  assert.deepEqual(legacy.memory, {});
  assert.equal(legacy.memorySummary, null);
  assert.equal(buildModelMemoryTable(legacy).hasData, false);
  assert.deepEqual(loaded({ total_bytes: "1024", backend: 123 }).memorySummary, { modelBytes: null, contextBytes: null, computeBytes: null, totalBytes: null, backend: null });
});

test("retains native devices, extended buffers, missing cells and zero allocations", () => {
  const table = buildModelMemoryTable(loaded({ breakdown: {
    "cuda10.model": 10, "cuda2.model": 20, "cuda0.model": 100, "cuda0.kv": 30, "cuda0.compute": 40, "cuda0.output": 0,
    "cuda0.rs": 2, "cuda0.lora": 3, "cpu.model": 4, "cpu.state": 5, "cuda_host.output": 6,
  } }));
  assert.deepEqual(table.devices.map((device) => device.label), ["CUDA0", "CUDA2", "CUDA10", "CPU", "CUDA_HOST"]);
  assert.equal(allocation(table, "model", "cuda0"), 100);
  assert.equal(allocation(table, "output", "cuda0"), 0);
  assert.equal(allocation(table, "kv", "cpu"), null);
  assert.equal(allocation(table, "state", "cpu"), 5);
  assert.equal(allocation(table, "output", "cuda_host"), 6);
  assert.equal(allocation(table, "rs", "cuda0"), 2);
  assert.equal(allocation(table, "lora", "cuda0"), 3);
  assert.equal(table.totalBytes, null);
  assert.equal(table.hasData, true);
});

test("keeps unfamiliar reported devices and buffers without treating them as a GPU", () => {
  const table = buildModelMemoryTable(loaded({ backend: "cuda", breakdown: { "accelerator7.scratch": 123, "cpu.model": 1024 } }));
  assert.deepEqual(table.devices.map((device) => device.label), ["ACCELERATOR7", "CPU"]);
  assert.equal(allocation(table, "scratch", "accelerator7"), 123);
  assert.ok(!table.devices.some((device) => device.id === "cuda"));
  const malformed = buildModelMemoryTable({ ...loaded({}), memory: { "cuda0.model": -1, "cuda0.kv": NaN, "cuda0.output": null, ".model": 20, "cuda0.": 30 } });
  assert.equal(malformed.hasData, false);
  assert.ok(malformed.rows.every((row) => row.values.every((value) => value === null)));
  const prototypeNames = buildModelMemoryTable(loaded({ breakdown: JSON.parse('{"cpu.__proto__": 10, "cpu.constructor": 20}') }));
  assert.equal(prototypeNames.rows.find((row) => row.id === "__proto__").label, "__proto__");
  assert.equal(prototypeNames.rows.find((row) => row.id === "constructor").label, "constructor");
});

test("summary-only responses stay unassigned and context is not relabeled as KV", () => {
  const table = buildModelMemoryTable(loaded({ backend: "cuda", model_bytes: 1024, context_bytes: 2048, compute_bytes: 512, total_bytes: 3000 }));
  assert.deepEqual(table.devices, [{ id: "", label: "未区分设备" }]);
  assert.equal(allocation(table, "model", ""), 1024);
  assert.equal(allocation(table, "context", ""), 2048);
  assert.equal(allocation(table, "compute", ""), 512);
  assert.equal(allocation(table, "kv", ""), null);
  assert.equal(allocation(table, "output", ""), null);
  assert.equal(table.totalBytes, 3000);
});

test("native breakdowns win over duplicate summaries and totals are never summed", () => {
  const model = loaded({ model_bytes: 100, context_bytes: 35, compute_bytes: 20, total_bytes: 140, breakdown: {
    "cuda0.model": 100, "cuda0.model_bytes": 100, "cuda0.kv": 30, "cuda0.output": 5, "cuda0.compute": 20,
    "cuda0.total": 150, total: 155, total_bytes: 155,
  } });
  const table = buildModelMemoryTable(model);
  assert.deepEqual(table.devices, [{ id: "cuda0", label: "CUDA0" }]);
  assert.equal(allocation(table, "model", "cuda0"), 100);
  assert.equal(table.rows.filter((row) => row.id === "model").length, 1);
  assert.ok(!table.rows.some((row) => row.id === "context"));
  assert.equal(allocation(table, "total", "cuda0"), 150);
  assert.equal(table.totalBytes, 140);
  assert.equal(buildModelMemoryTable(loaded({ breakdown: { "cpu.model": 100, total: 90 } })).totalBytes, 90);
  assert.equal(buildModelMemoryTable(loaded({ breakdown: { "cpu.model": 100 } })).totalBytes, null);
});

test("nonoverlapping summary fields fill gaps while duplicate context aggregates stay hidden", () => {
  const table = buildModelMemoryTable(loaded({ model_bytes: 1024, context_bytes: 300, compute_bytes: 200, breakdown: { "cuda0.kv": 256 } }));
  assert.equal(allocation(table, "model", ""), 1024);
  assert.equal(allocation(table, "compute", ""), 200);
  assert.equal(allocation(table, "kv", "cuda0"), 256);
  assert.ok(!table.rows.some((row) => row.id === "context"));
});

test("zero-byte summaries and totals stay present rather than becoming unavailable", () => {
  const model = loaded({ model_bytes: 0, context_bytes: 0, compute_bytes: 0, total_bytes: 0 });
  const table = buildModelMemoryTable(model);
  assert.equal(table.hasData, true);
  assert.equal(table.totalBytes, 0);
  assert.equal(allocation(table, "model", ""), 0);
  assert.equal(allocation(table, "context", ""), 0);
  assert.equal(allocation(table, "compute", ""), 0);
  const html = render(model);
  assert.ok(!html.includes("用量暂不可用"));
  assert.equal((html.match(/>0 B</g) ?? []).length, 4);
});

test("the SSR table is always visible with explicit unavailable states and no file-size estimate", () => {
  for (const [model, connected, status] of [
    [null, false, "服务未连接"],
    [decodeModel({ loaded: false }), true, "未加载模型"],
    [loaded(undefined), true, "用量暂不可用"],
  ]) {
    const html = render(model, connected);
    assert.ok(html.includes(status));
    assert.ok(html.includes('aria-label="模型内存占用"'));
    for (const label of ["模型权重", "KV 缓存", "计算缓冲", "输出缓冲", "已报告合计"]) assert.ok(html.includes(label));
    assert.equal((html.match(/>—</g) ?? []).length, 5);
    assert.ok(!html.includes("8.00 GiB"));
    assert.doesNotMatch(html, /<(details|dialog|button)\b|data-state="closed"|\shidden(?:[=\s>])/);
    for (const className of html.matchAll(/class="([^"]*)"/g)) {
      assert.ok(!className[1].split(/\s+/).some((token) => token === "hidden" || token.endsWith(":hidden")));
    }
  }
});

test("SSR keeps device memory separate, escapes names, and suppresses stale disconnected values", () => {
  const model = loaded({ backend: "cuda", total_bytes: 4096, breakdown: { "cpu.model": 1024, "cuda_host.output": 0, "custom.<script>": 512 } });
  const html = render(model);
  assert.ok(html.includes("CPU"));
  assert.ok(html.includes("CUDA_HOST"));
  assert.ok(html.includes("1.0 KiB"));
  assert.ok(html.includes("0 B"));
  assert.ok(html.includes("&lt;script&gt;"));
  assert.doesNotMatch(html, /<script>/);
  assert.ok(html.includes("overflow-x-auto"));
  assert.equal((html.match(/已报告合计/g) ?? []).length, 1);
  assert.ok(!html.includes("GPU 显存"));
  const offline = render(model, false);
  assert.ok(offline.includes("服务未连接"));
  assert.ok(!offline.includes("1.0 KiB"));
  assert.ok(!offline.includes("4.0 KiB"));
});
