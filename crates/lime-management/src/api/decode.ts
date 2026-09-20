import type {
  BoundaryRollback,
  Candidate,
  CandidateDiagnostic,
  BenchmarkCase,
  BenchmarkDatasetView,
  BenchmarkMode,
  BenchmarkObservation,
  BenchmarkReportView,
  BenchmarkRunState,
  BenchmarkRunStatus,
  BenchmarkSummary,
  Config,
  ConfigSnapshot,
  DictionaryEntry,
  DictionaryPage,
  HistoryPage,
  InputData,
  LlmPerformance,
  ModelInfo,
  ModelPreset,
  ServiceState,
} from "./types";

type RecordValue = Record<string, unknown>;

export function asRecord(value: unknown): RecordValue | null {
  return value && typeof value === "object" && !Array.isArray(value)
    ? value as RecordValue
    : null;
}

export function asNumber(value: unknown): number | null {
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function integer(value: unknown, fallback = 0): number {
  return Math.max(0, Math.trunc(asNumber(value) ?? fallback));
}

function string(value: unknown): string | null {
  return typeof value === "string" ? value : null;
}

export function decodeState(value: unknown): ServiceState {
  return value === "ready" || value === "rime_only" || value === "reloading" || value === "unavailable"
    ? value
    : "unavailable";
}

export function decodeCandidate(value: unknown): Candidate | null {
  const item = asRecord(value);
  const display = string(item?.display_text);
  const commit = string(item?.commit_text);
  return display !== null && commit !== null ? { displayText: display, commitText: commit } : null;
}

function candidates(value: unknown): Candidate[] {
  return Array.isArray(value) ? value.map(decodeCandidate).filter((item): item is Candidate => item !== null) : [];
}

function logprobs(value: unknown): number[] {
  return Array.isArray(value) ? value.map(asNumber).filter((item): item is number => item !== null) : [];
}

function decodeBoundaryRollback(value: unknown): BoundaryRollback | null {
  const item = asRecord(value);
  const prefixTokenCount = asNumber(item?.prefix_token_count);
  const replayedTokenCount = asNumber(item?.replayed_token_count);
  if (prefixTokenCount === null || !Number.isInteger(prefixTokenCount) || prefixTokenCount < 0 || prefixTokenCount > 0xffff_ffff
    || replayedTokenCount === null || !Number.isInteger(replayedTokenCount) || replayedTokenCount <= 0 || replayedTokenCount > 0xffff_ffff) return null;
  const prefixText = string(item?.prefix_text);
  const replayedText = string(item?.replayed_text);
  const hasText = prefixText !== null && replayedText !== null;
  return {
    prefixTokenCount,
    replayedTokenCount,
    prefixText: hasText ? prefixText : null,
    replayedText: hasText ? replayedText : null,
  };
}

export function decodePerformance(value: unknown): LlmPerformance | null {
  const item = asRecord(value);
  if (!item) return null;
  const total = asNumber(item.total_ms);
  if (total === null) return null;
  const optionalNumber = (key: string) => {
    const value = asNumber(item[key]);
    return value === null ? null : Math.max(0, value);
  };
  return {
    totalMs: Math.max(0, total),
    tokenizeMs: optionalNumber("tokenize_ms") ?? 0,
    decodeMs: optionalNumber("decode_ms") ?? 0,
    rimeMs: optionalNumber("rime_duration_ms"),
    candidateCount: integer(item.candidate_count),
    scoredCount: integer(item.scored_count),
    targetTokenCount: integer(item.target_token_count),
    batchCount: integer(item.batch_count),
    mismatchCount: integer(item.mismatch_count),
    boundaryRollback: decodeBoundaryRollback(item.boundary_rollback),
    contextTokenCount: integer(item.context_token_count),
    decodeInputTokenCount: integer(item.decode_input_token_count),
    logprobOutputCount: integer(item.logits_output_count),
    inferenceCountLimit: item.inference_count_limit === undefined
      ? null
      : integer(item.inference_count_limit),
    omittedCandidateCount: integer(item.omitted_candidate_count),
  };
}

function diagnostic(value: unknown, index: number): CandidateDiagnostic {
  const item = asRecord(value);
  const logprob = asNumber(item?.logprob);
  return {
    index,
    rimeCandidate: decodeCandidate(item?.rime_candidate),
    llmCandidate: decodeCandidate(item?.llm_candidate),
    logprob,
    logprobs: logprobs(item?.logprobs),
    mismatch: typeof item?.mismatch === "boolean" ? item.mismatch : null,
    displayCandidate: decodeCandidate(item?.display_candidate),
    hasDisplayCandidate: item?.display_candidate !== null && item?.display_candidate !== undefined,
  };
}

export function decodeInputData(value: unknown): InputData {
  const item = asRecord(value);
  const performance = decodePerformance(item?.llm_performance);
  const rimeMs = asNumber(item?.rime_duration_ms) ?? performance?.rimeMs ?? null;
  const rawDiagnostics = Array.isArray(item?.diagnostics) ? item.diagnostics : [];
  return {
    requestId: asNumber(item?.request_id) ?? undefined,
    timestampMs: asNumber(item?.timestamp_ms),
    endToEndMs: asNumber(item?.end_to_end_duration_ms),
    precedingText: string(item?.preceding_text) ?? "",
    preedit: string(item?.preedit) ?? "",
    rimeCandidates: candidates(item?.rime_candidates),
    finalCandidates: candidates(item?.final_candidates),
    diagnostics: rawDiagnostics.map((entry, index) => diagnostic(entry, index)),
    llmPerformance: performance,
    rimeMs,
    model: string(item?.model_name),
    contextUsed: typeof item?.context_used === "boolean" ? item.context_used : null,
    serviceState: decodeState(item?.service_state),
  };
}

const defaultConfig: Config = {
  rime_schema: "rime_ice",
  preceding_text_char_limit: 128,
  context_preview_char_limit: 32,
  page_size: 9,
  llm_rerank_count: 32,
  llm_effective_count: 3,
  llm_context_token_limit: 1024,
  llm_inference_count_limit: 1,
  llm_ignore_emoji: true,
  llm_backend: "cuda",
};

export function decodeConfigSnapshot(value: unknown): ConfigSnapshot {
  const item = asRecord(value);
  const source = asRecord(item?.config) ?? {};
  const config: Config = {
    rime_schema: string(source.rime_schema) ?? defaultConfig.rime_schema,
    preceding_text_char_limit: integer(source.preceding_text_char_limit, defaultConfig.preceding_text_char_limit),
    context_preview_char_limit: integer(source.context_preview_char_limit, defaultConfig.context_preview_char_limit),
    page_size: integer(source.page_size, defaultConfig.page_size),
    llm_rerank_count: integer(source.llm_rerank_count, defaultConfig.llm_rerank_count),
    llm_effective_count: integer(source.llm_effective_count, defaultConfig.llm_effective_count),
    llm_context_token_limit: integer(source.llm_context_token_limit, defaultConfig.llm_context_token_limit),
    llm_inference_count_limit: integer(source.llm_inference_count_limit, defaultConfig.llm_inference_count_limit),
    llm_ignore_emoji: typeof source.llm_ignore_emoji === "boolean" ? source.llm_ignore_emoji : defaultConfig.llm_ignore_emoji,
    llm_backend: source.llm_backend === "cpu" ? "cpu" : "cuda",
  };
  return { revision: integer(item?.revision), config };
}

export function decodeModel(value: unknown): ModelInfo {
  const item = asRecord(value);
  const memory = asRecord(item?.initialization_memory);
  const breakdown = asRecord(memory?.breakdown);
  const flattened: Record<string, number> = {};
  for (const [key, value] of Object.entries(breakdown ?? {})) {
    const parsed = asNumber(value);
    if (parsed !== null) flattened[key] = parsed;
  }
  return {
    path: string(item?.path),
    sizeBytes: asNumber(item?.size_bytes),
    sha256: string(item?.sha256),
    loaded: item?.loaded === true,
    scoringPath: item?.scoring_path === "attention" || item?.scoring_path === "recurrent" ? item.scoring_path : null,
    memory: flattened,
  };
}

export function decodePreset(value: unknown): ModelPreset | null {
  const item = asRecord(value);
  const name = string(item?.name);
  const path = string(item?.path);
  return name !== null && path !== null
    ? { name, path, sizeBytes: asNumber(item?.size_bytes), sha256: string(item?.sha256), loaded: item?.loaded === true }
    : null;
}

export function decodePresets(value: unknown): ModelPreset[] {
  return Array.isArray(value) ? value.map(decodePreset).filter((item): item is ModelPreset => item !== null) : [];
}

export function decodeHistoryPage(value: unknown, page: number): HistoryPage {
  const item = asRecord(value);
  const items = Array.isArray(item?.items) ? item.items.map(decodeInputData) : [];
  return { items, total: integer(item?.total), page: integer(item?.page, page), pageSize: integer(item?.page_size, 100) };
}

export function decodeDictionaryEntry(value: unknown): DictionaryEntry | null {
  const item = asRecord(value);
  const pinyin = string(item?.pinyin);
  const text = string(item?.text);
  const weight = asNumber(item?.weight);
  return pinyin !== null && text !== null && weight !== null ? { pinyin, text, weight } : null;
}

export function decodeDictionaryPage(value: unknown, page: number): DictionaryPage {
  const item = asRecord(value);
  const items = Array.isArray(item?.items)
    ? item.items.map(decodeDictionaryEntry).filter((entry): entry is DictionaryEntry => entry !== null)
    : [];
  return { items, total: integer(item?.total, items.length), page: integer(item?.page, page), pageSize: integer(item?.page_size, 100) };
}

function nullableString(value: unknown): string | null {
  return value == null ? null : string(value);
}

function nullableNumber(value: unknown): number | null {
  return value == null ? null : asNumber(value);
}

export function decodeBenchmarkMode(value: unknown): BenchmarkMode {
  if (value === "initials" || value === "jianpin" || value === "first_letters") return "initials";
  return "full";
}

function decodeBenchmarkModes(value: unknown): BenchmarkMode[] {
  if (!Array.isArray(value)) return [];
  return [...new Set(value.map(decodeBenchmarkMode))];
}

function decodeBenchmarkCase(value: unknown): BenchmarkCase | null {
  const item = asRecord(value);
  const id = string(item?.id);
  if (id === null) return null;
  const rawSyllables = item?.syllables;
  const syllables = Array.isArray(rawSyllables)
    ? rawSyllables.map(string).filter((part): part is string => part !== null)
    : typeof rawSyllables === "string" ? rawSyllables.trim().split(/\s+/).filter(Boolean) : [];
  return {
    id,
    category: nullableString(item?.category),
    context: string(item?.context) ?? string(item?.preceding_text) ?? "",
    expected: string(item?.expected) ?? string(item?.target) ?? "",
    syllables,
  };
}

export function decodeBenchmarkDataset(value: unknown): BenchmarkDatasetView {
  const item = asRecord(value);
  const rawCases = Array.isArray(item?.cases) ? item.cases : Array.isArray(item?.items) ? item.items : [];
  return {
    id: nullableString(item?.id) ?? nullableString(item?.dataset_id),
    name: nullableString(item?.name) ?? nullableString(item?.dataset_name),
    version: nullableNumber(item?.version) ?? nullableNumber(item?.dataset_version),
    sha256: nullableString(item?.sha256) ?? nullableString(item?.dataset_sha256),
    cases: rawCases.map(decodeBenchmarkCase).filter((entry): entry is BenchmarkCase => entry !== null),
  };
}

function benchmarkTop1(value: unknown): string | null {
  const direct = string(value);
  if (direct !== null) return direct;
  const item = asRecord(value);
  return string(item?.display_text) ?? string(item?.commit_text) ?? string(item?.text);
}

function decodeBenchmarkSummary(value: unknown): BenchmarkSummary | null {
  const item = asRecord(value);
  if (!item) return null;
  const modeValue = item.mode ?? item.input_mode;
  const total = integer(item.total);
  const completed = integer(item.completed, total);
  const accuracy = asNumber(item.accuracy);
  return {
    category: nullableString(item.category),
    mode: decodeBenchmarkMode(modeValue),
    total,
    completed,
    correct: integer(item.correct),
    noPrediction: integer(item.no_prediction ?? item.noPrediction),
    errors: integer(item.errors ?? item.error_count),
    accuracy,
  };
}

function decodeBenchmarkObservation(value: unknown): BenchmarkObservation | null {
  const item = asRecord(value);
  const caseId = string(item?.case_id) ?? string(item?.caseId);
  if (caseId === null) return null;
  return {
    caseId,
    category: nullableString(item?.category),
    context: string(item?.context) ?? "",
    expected: string(item?.expected) ?? "",
    preedit: string(item?.preedit) ?? string(item?.pinyin) ?? "",
    mode: decodeBenchmarkMode(item?.mode ?? item?.input_mode),
    top1: benchmarkTop1(item?.top1 ?? item?.top_1 ?? item?.prediction),
    correct: typeof item?.correct === "boolean" ? item.correct : null,
    error: nullableString(item?.error),
    elapsedMs: asNumber(item?.elapsed_ms ?? item?.elapsedMs),
  };
}

export function decodeBenchmarkReport(value: unknown): BenchmarkReportView | null {
  const item = asRecord(value);
  if (!item) return null;
  const rawSummaries = Array.isArray(item.summaries) ? item.summaries : [];
  const rawObservations = Array.isArray(item.observations) ? item.observations : [];
  return {
    datasetId: nullableString(item.dataset_id) ?? nullableString(item.datasetId),
    datasetName: nullableString(item.dataset_name) ?? nullableString(item.datasetName),
    datasetVersion: nullableNumber(item.dataset_version) ?? nullableNumber(item.datasetVersion),
    sha256: nullableString(item.sha256) ?? nullableString(item.dataset_sha256),
    modes: decodeBenchmarkModes(item.modes),
    summaries: rawSummaries.map(decodeBenchmarkSummary).filter((entry): entry is BenchmarkSummary => entry !== null),
    observations: rawObservations.map(decodeBenchmarkObservation).filter((entry): entry is BenchmarkObservation => entry !== null),
    complete: item.complete !== false,
  };
}

export function decodeBenchmarkRunState(value: unknown): BenchmarkRunState {
  const item = asRecord(value);
  const rawStatus = item?.status;
  const status: BenchmarkRunStatus = rawStatus === "running" || rawStatus === "stopping" || rawStatus === "cancelled" || rawStatus === "completed" || rawStatus === "failed"
    ? rawStatus
    : "idle";
  return {
    status,
    runId: nullableString(item?.run_id) ?? nullableString(item?.runId),
    datasetId: nullableString(item?.dataset_id) ?? nullableString(item?.datasetId),
    datasetName: nullableString(item?.dataset_name) ?? nullableString(item?.datasetName),
    datasetVersion: nullableNumber(item?.dataset_version) ?? nullableNumber(item?.datasetVersion),
    configRevision: asNumber(item?.config_revision ?? item?.configRevision),
    modelName: nullableString(item?.model_name) ?? nullableString(item?.modelName),
    modelSha256: nullableString(item?.model_sha256) ?? nullableString(item?.modelSha256),
    config: item?.config == null ? null : decodeConfigSnapshot({ revision: item?.config_revision ?? 0, config: item.config }).config,
    total: integer(item?.total),
    completed: integer(item?.completed),
    report: decodeBenchmarkReport(item?.report),
    error: nullableString(item?.error),
  };
}

export function diagnosticsFrom(_value: unknown, rime: Candidate[], final: Candidate[]): CandidateDiagnostic[] {
  const count = Math.max(rime.length, final.length);
  return Array.from({ length: count }, (_, index) => ({
    index,
    rimeCandidate: rime[index] ?? null,
    llmCandidate: final[index] ?? null,
    logprob: null,
    logprobs: [],
    mismatch: null,
    displayCandidate: final[index] ?? rime[index] ?? null,
    hasDisplayCandidate: Boolean(final[index] ?? rime[index]),
  }));
}
