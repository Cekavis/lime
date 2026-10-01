import type { BenchmarkMode, BenchmarkResult, BenchmarkModelSelection } from "../api/types";

export function benchmarkModeLabel(mode: BenchmarkMode): string {
  return mode === "full" ? "全拼" : "首拼";
}

export function benchmarkModelLabel(model: BenchmarkModelSelection | null, fallback = ""): string {
  if (model?.kind === "rime_only") return "仅 Rime";
  return model?.name ?? fallback;
}

export function benchmarkResultAccuracy(result: BenchmarkResult, category: string | null = null): number | null {
  if (result.status !== "completed") return null;
  const cell = category === null ? null : result.corpusCells.find((entry) => entry.corpusId === category);
  if (cell) return cell.summary?.accuracy ?? null;
  if (category === null && result.corpusCells.length) {
    if (result.corpusCells.some((entry) => entry.status !== "completed" || !entry.summary)) return null;
    const total = result.corpusCells.reduce((sum, entry) => sum + (entry.summary?.total ?? 0), 0);
    const correct = result.corpusCells.reduce((sum, entry) => sum + (entry.summary?.correct ?? 0), 0);
    return total > 0 ? correct / total : null;
  }
  if (!result.report?.complete) return null;
  const accuracy = result.report.summaries.find((summary) => summary.category === category && summary.mode === result.mode)?.accuracy;
  return accuracy != null && Number.isFinite(accuracy) ? accuracy : null;
}

export function sortBenchmarkResults(results: BenchmarkResult[]): BenchmarkResult[] {
  return [...results].sort((left, right) => {
    const leftAccuracy = benchmarkResultAccuracy(left);
    const rightAccuracy = benchmarkResultAccuracy(right);
    if (leftAccuracy === null) return rightAccuracy === null ? 0 : 1;
    if (rightAccuracy === null) return -1;
    return rightAccuracy - leftAccuracy;
  });
}
