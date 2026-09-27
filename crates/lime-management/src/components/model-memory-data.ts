import { asByteCount } from "@/api/decode";
import type { ModelInfo } from "@/api/types";
import { memoryLabel } from "@/ui/format";

export interface ModelMemoryTable {
  devices: { id: string; label: string }[];
  rows: { id: string; label: string; values: (number | null)[] }[];
  totalBytes: number | null;
  hasData: boolean;
}

const standardKinds = ["model", "kv", "compute", "output"];
const kindOrder = ["model", "context", "kv", "compute", "output", "rs", "lora", "state"];
const contextKinds = ["context", "kv", "output", "rs", "lora", "state"];
const kindAliases: Record<string, string> = {
  model_bytes: "model", context_bytes: "context", kv_bytes: "kv", kvcache: "kv",
  compute_bytes: "compute", output_bytes: "output", rs_bytes: "rs", lora_bytes: "lora",
  state_bytes: "state", total_bytes: "total",
};

function normalizeKind(kind: string): string {
  return Object.hasOwn(kindAliases, kind.toLowerCase()) ? kindAliases[kind.toLowerCase()] : kind;
}

function deviceOrder(device: string): number {
  if (!device) return 3;
  if (device === "cpu") return 1;
  if (device.endsWith("_host")) return 2;
  return 0;
}

function kindRank(kind: string): number {
  if (kind === "total") return kindOrder.length + 1;
  const index = kindOrder.indexOf(kind);
  return index < 0 ? kindOrder.length : index;
}

/** Keep allocations on their reported devices; summaries never imply a GPU. */
export function buildModelMemoryTable(model: ModelInfo | null): ModelMemoryTable {
  const allocations = new Map<string, Map<string, { bytes: number; canonical: boolean }>>();
  let breakdownTotal: number | null = null;
  let canonicalTotal = false;
  for (const [key, value] of Object.entries(model?.memory ?? {})) {
    const bytes = asByteCount(value);
    if (bytes === null) continue;
    const separator = key.lastIndexOf(".");
    const device = separator < 0 ? "" : key.slice(0, separator).trim().toLowerCase();
    const rawKind = key.slice(separator + 1).trim();
    if (!rawKind || (separator >= 0 && !device)) continue;
    const kind = normalizeKind(rawKind);
    const canonical = rawKind === kind;
    if (!device && kind === "total") {
      if (breakdownTotal === null || canonical || !canonicalTotal) {
        breakdownTotal = bytes;
        canonicalTotal = canonical;
      }
      continue;
    }
    let buffer = allocations.get(device);
    if (!buffer) { buffer = new Map(); allocations.set(device, buffer); }
    const existing = buffer.get(kind);
    // Native keys take precedence over aliases; aliases are never added twice.
    if (!existing || canonical || !existing.canonical) buffer.set(kind, { bytes, canonical });
  }

  const hasKind = (kinds: string[]) => [...allocations.values()].some((buffer) => kinds.some((kind) => buffer.has(kind)));
  function addSummary(kind: string, value: unknown, overlaps: string[] = [kind]) {
    const bytes = asByteCount(value);
    if (bytes === null || hasKind(overlaps)) return;
    let buffer = allocations.get("");
    if (!buffer) { buffer = new Map(); allocations.set("", buffer); }
    buffer.set(kind, { bytes, canonical: true });
  }
  addSummary("model", model?.memorySummary?.modelBytes);
  addSummary("context", model?.memorySummary?.contextBytes, contextKinds);
  addSummary("compute", model?.memorySummary?.computeBytes);

  const totalBytes = asByteCount(model?.memorySummary?.totalBytes) ?? breakdownTotal;
  const deviceIds = [...allocations.keys()].sort((left, right) => deviceOrder(left) - deviceOrder(right) || left.localeCompare(right, "en", { numeric: true }));
  const devices = deviceIds.length
    ? deviceIds.map((id) => ({ id, label: id ? id.toUpperCase() : "未区分设备" }))
    : [{ id: "", label: "占用" }];
  const kinds = new Set(standardKinds);
  for (const buffer of allocations.values()) for (const kind of buffer.keys()) kinds.add(kind);
  const rows = [...kinds].sort((left, right) => kindRank(left) - kindRank(right) || left.localeCompare(right)).map((id) => ({
    id,
    label: id === "model" ? memoryLabel("model_bytes") : id === "total" ? "设备合计" : kindOrder.includes(id) ? memoryLabel(id) : id,
    values: devices.map((device) => allocations.get(device.id)?.get(id)?.bytes ?? null),
  }));
  return { devices, rows, totalBytes, hasData: totalBytes !== null || allocations.size > 0 };
}
