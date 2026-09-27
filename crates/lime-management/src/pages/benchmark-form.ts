import type { BenchmarkConfiguration, BenchmarkMode, BenchmarkObservation, BenchmarkResult, BenchmarkRunRequest, BenchmarkRunState } from "@/api/types";

export interface BenchmarkConfigurationDraft {
  id: string;
  rerankCount: string;
  contextLimit: string;
}

export function buildBenchmarkRequest(
  modes: BenchmarkMode[],
  models: string[],
  drafts: BenchmarkConfigurationDraft[],
): BenchmarkRunRequest {
  const uniqueModes = [...new Set(modes)];
  const uniqueModels = [...new Set(models)];
  if (!uniqueModes.length) throw new Error("至少选择一种拼音模式");
  if (!uniqueModels.length) throw new Error("至少选择一个模型");
  const configurations: BenchmarkConfiguration[] = drafts.map((draft) => ({
    llm_rerank_count: Number(draft.rerankCount),
    preceding_text_char_limit: Number(draft.contextLimit),
  }));
  if (configurations.some((configuration) =>
    !Number.isInteger(configuration.llm_rerank_count)
    || configuration.llm_rerank_count < 1
    || configuration.llm_rerank_count > 128
    || !Number.isInteger(configuration.preceding_text_char_limit)
    || configuration.preceding_text_char_limit < 1
    || configuration.preceding_text_char_limit > 4096)) {
    throw new Error("重排候选词数需为 1–128 的整数，前文字符数需为 1–4096 的整数");
  }
  const uniqueConfigurations = [...new Map(configurations.map((configuration) => [JSON.stringify(configuration), configuration])).values()];
  if (!uniqueConfigurations.length) throw new Error("至少添加一种配置");
  if (uniqueModels.length * uniqueConfigurations.length * uniqueModes.length > 32) {
    throw new Error("每次最多评测 32 种模型、配置和拼音模式组合");
  }
  return { modes: uniqueModes, models: uniqueModels, configurations: uniqueConfigurations };
}

export function benchmarkProgress(state: Pick<BenchmarkRunState, "total" | "completed">): number {
  if (!Number.isFinite(state.total) || !Number.isFinite(state.completed) || state.total <= 0) return 0;
  return Math.max(0, Math.min(100, state.completed / state.total * 100));
}

export function benchmarkErrorExamples(result: BenchmarkResult): BenchmarkObservation[] {
  return (result.report?.observations ?? []).filter((observation) => observation.error || observation.correct === false).slice(0, 40);
}
