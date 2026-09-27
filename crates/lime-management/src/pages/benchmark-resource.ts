import type { BenchmarkDatasetView, BenchmarkRunState } from "@/api/types";

export async function readBenchmarkSnapshot(
  getDataset: () => Promise<BenchmarkDatasetView>,
  getState: () => Promise<BenchmarkRunState>,
): Promise<{ dataset: BenchmarkDatasetView; state: BenchmarkRunState }> {
  // A failed read must still wait for its sibling before the polling queue
  // starts another request, otherwise retries can overlap outstanding IPC.
  const [dataset, state] = await Promise.allSettled([getDataset(), getState()]);
  if (dataset.status === "rejected") throw dataset.reason;
  if (state.status === "rejected") throw state.reason;
  return { dataset: dataset.value, state: state.value };
}
