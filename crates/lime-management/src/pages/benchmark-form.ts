import type { BenchmarkConfiguration, BenchmarkMode, BenchmarkModelSelection, BenchmarkObservation, BenchmarkResult, BenchmarkRunRequest, BenchmarkRunState } from "@/api/types";

export interface BenchmarkConfigurationDraft {
  id: string;
  rerankCount: string;
  contextLimit: string;
}

export function buildBenchmarkRequest(
  modes: BenchmarkMode[],
  models: BenchmarkModelSelection[],
  drafts: BenchmarkConfigurationDraft[],
  corpora: string[] = ["all"],
): BenchmarkRunRequest {
  const uniqueModes = [...new Set(modes)];
  const uniqueModels = [...new Map(models.map((model) => [JSON.stringify(model), model])).values()];
  const uniqueCorpora = [...new Set(corpora)];
  const presetCount = uniqueModels.filter((model) => model.kind === "preset").length;
  const rimeOnlyCount = uniqueModels.filter((model) => model.kind === "rime_only").length;
  if (!uniqueModes.length) throw new Error("至少选择一种拼音模式");
  if (!uniqueModels.length) throw new Error("至少选择一个模型");
  if (!uniqueCorpora.length) throw new Error("至少选择一种语料");
  const configurations: BenchmarkConfiguration[] = drafts.map((draft) => ({
    llm_rerank_count: Number(draft.rerankCount),
    preceding_text_char_limit: Number(draft.contextLimit),
  }));
  if (presetCount > 0 && configurations.some((configuration) =>
    !Number.isInteger(configuration.llm_rerank_count)
    || configuration.llm_rerank_count < 1
    || configuration.llm_rerank_count > 128
    || !Number.isInteger(configuration.preceding_text_char_limit)
    || configuration.preceding_text_char_limit < 1
    || configuration.preceding_text_char_limit > 4096)) {
    throw new Error("重排候选词数需为 1–128 的整数，前文字符数需为 1–4096 的整数");
  }
  const uniqueConfigurations = presetCount > 0
    ? [...new Map(configurations.map((configuration) => [JSON.stringify(configuration), configuration])).values()]
    : [];
  if (presetCount > 0 && !uniqueConfigurations.length) throw new Error("至少添加一种配置");
  const matrixSize = (presetCount * uniqueConfigurations.length + rimeOnlyCount) * uniqueModes.length;
  if (matrixSize > 32) {
    throw new Error("每次最多评测 32 种模型、配置和拼音模式组合");
  }
  return { modes: uniqueModes, models: uniqueModels, configurations: uniqueConfigurations, corpora: uniqueCorpora };
}

export function benchmarkProgress(state: Pick<BenchmarkRunState, "total" | "completed">): number {
  if (!Number.isFinite(state.total) || !Number.isFinite(state.completed) || state.total <= 0) return 0;
  return Math.max(0, Math.min(100, state.completed / state.total * 100));
}

export function benchmarkCurrentProgress(progress: BenchmarkRunState["current"]): number {
  if (!progress || !Number.isFinite(progress.total) || !Number.isFinite(progress.processed) || progress.total <= 0) return 0;
  return Math.max(0, Math.min(100, progress.processed / progress.total * 100));
}

export function benchmarkErrorExamples(result: BenchmarkResult): BenchmarkObservation[] {
  return (result.report?.observations ?? []).filter((observation) => observation.error || observation.correct === false).slice(0, 40);
}
