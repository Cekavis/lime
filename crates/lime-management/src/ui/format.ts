export function escapeHtml(value: unknown): string {
  return String(value ?? "").replace(/[&<>'"]/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", "'": "&#39;", '"': "&quot;" })[character] ?? character);
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return bytes + " B";
  if (bytes < 1024 * 1024) return (bytes / 1024).toFixed(1) + " KiB";
  return (bytes / (1024 * 1024)).toFixed(1) + " MiB";
}

export function formatDecimal(value: number): string {
  return Number.isFinite(value) ? value.toFixed(2) : "—";
}

export function formatMilliseconds(value: number | null | undefined): string {
  if (value == null || !Number.isFinite(value)) return "—";
  return (Number.isInteger(value) ? String(value) : value.toFixed(2)) + " ms";
}

export function formatLogprobs(values: number[], aggregate: number | null): string {
  if (!values.length) return "—";
  const rawSum = values.reduce((sum, value) => sum + value, 0);
  const targetCents = Math.round((aggregate == null ? rawSum : aggregate) * 100);
  const cents = values.map((value) => Math.round(value * 100));
  const prefixSum = cents.slice(0, -1).reduce((sum, value) => sum + value, 0);
  cents[cents.length - 1] = targetCents - prefixSum;
  return cents.map((value) => formatDecimal(value / 100)).join(", ");
}

export function formatTimestamp(value: number | null): string {
  if (value == null || !Number.isFinite(value)) return "时间未知";
  const milliseconds = value < 1_000_000_000_000 ? value * 1000 : value;
  const date = new Date(milliseconds);
  return Number.isNaN(date.getTime()) ? "时间未知" : date.toLocaleString("zh-CN");
}

export function errorMessage(error: unknown): string {
  const message = error instanceof Error ? error.message : String(error);
  if (message.includes("model_unsupported") || message.includes("LIME-0011")) {
    return "模型不支持：仅支持 causal decoder；encoder、embedding 或 diffusion 模型不能用于候选排序";
  }
  return message;
}
