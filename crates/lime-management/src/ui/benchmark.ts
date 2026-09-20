import type { BenchmarkMode, BenchmarkResult } from "../api/types";

export function benchmarkModeLabel(mode: BenchmarkMode): string {
  return mode === "full" ? "全拼" : "首拼";
}

export function benchmarkResultAccuracy(result: BenchmarkResult, category: string | null = null): number | null {
  if (result.status !== "completed" || !result.report?.complete) return null;
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
