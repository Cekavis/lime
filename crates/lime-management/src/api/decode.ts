import type {
  Candidate,
  CandidateDiagnostic,
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
