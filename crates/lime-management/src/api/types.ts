export type ServiceState = "ready" | "rime_only" | "reloading" | "unavailable";

export interface Config {
  rime_schema: string;
  preceding_text_char_limit: number;
  context_preview_char_limit: number;
  page_size: number;
  llm_rerank_count: number;
  llm_effective_count: number;
  llm_context_token_limit: number;
  llm_inference_count_limit: number;
  llm_ignore_emoji: boolean;
  llm_backend: "cuda" | "cpu";
}

export interface ConfigSnapshot { revision: number; config: Config; }

export interface ModelInfo {
  path: string | null;
  sizeBytes: number | null;
  sha256: string | null;
  loaded: boolean;
  scoringPath: "attention" | "recurrent" | null;
  memory: Record<string, number>;
}

export interface ModelPreset {
  name: string;
  path: string;
  sizeBytes: number | null;
  sha256: string | null;
  loaded: boolean;
}

export interface Candidate { displayText: string; commitText: string; }

export interface CandidateDiagnostic {
  index: number;
  rimeCandidate: Candidate | null;
  llmCandidate: Candidate | null;
  logprob: number | null;
  logprobs: number[];
  mismatch: boolean | null;
  displayCandidate: Candidate | null;
  hasDisplayCandidate: boolean;
}

export interface LlmPerformance {
  totalMs: number;
  tokenizeMs: number;
  decodeMs: number;
  rimeMs: number | null;
  candidateCount: number;
  scoredCount: number;
  targetTokenCount: number;
  batchCount: number;
  mismatchCount: number;
  contextTokenCount: number;
  decodeInputTokenCount: number;
  logprobOutputCount: number;
  inferenceCountLimit: number | null;
  omittedCandidateCount: number;
}

export interface InputData {
  requestId?: number;
  timestampMs: number | null;
  endToEndMs: number | null;
  precedingText: string;
  preedit: string;
  rimeCandidates: Candidate[];
  finalCandidates: Candidate[];
  diagnostics: CandidateDiagnostic[];
  llmPerformance: LlmPerformance | null;
  rimeMs: number | null;
  model: string | null;
  contextUsed: boolean | null;
  serviceState: ServiceState;
}

export interface HistoryPage { items: InputData[]; total: number; page: number; pageSize: number; }
export interface ServiceStatus { state: ServiceState; config: ConfigSnapshot; model: ModelInfo; }
export interface DictionaryEntry { pinyin: string; text: string; weight: number; }
export interface DictionaryPage { items: DictionaryEntry[]; total: number; page: number; pageSize: number; }

export type BenchmarkMode = "full" | "initials";
export type BenchmarkRunStatus = "idle" | "running" | "stopping" | "cancelled" | "completed" | "failed";

export interface BenchmarkRunRequest {
  modes: BenchmarkMode[];
  categories: string[];
}

export interface BenchmarkCase {
  id: string;
  category: string | null;
  context: string;
  expected: string;
  syllables: string[];
}

export interface BenchmarkDatasetView {
  id: string | null;
  name: string | null;
  version: number | null;
  sha256: string | null;
  cases: BenchmarkCase[];
}

export interface BenchmarkSummary {
  category: string | null;
  mode: BenchmarkMode;
  total: number;
  completed: number;
  correct: number;
  noPrediction: number;
  errors: number;
  accuracy: number | null;
}

export interface BenchmarkObservation {
  caseId: string;
  category: string | null;
  context: string;
  expected: string;
  preedit: string;
  mode: BenchmarkMode;
  top1: string | null;
  correct: boolean | null;
  error: string | null;
  elapsedMs: number | null;
}

export interface BenchmarkReportView {
  datasetId: string | null;
  datasetName: string | null;
  datasetVersion: number | null;
  sha256: string | null;
  modes: BenchmarkMode[];
  summaries: BenchmarkSummary[];
  observations: BenchmarkObservation[];
  complete: boolean;
}

export interface BenchmarkRunState {
  status: BenchmarkRunStatus;
  runId: string | null;
  datasetId: string | null;
  datasetName: string | null;
  datasetVersion: number | null;
  configRevision: number | null;
  modelName: string | null;
  modelSha256: string | null;
  config: Config | null;
  total: number;
  completed: number;
  report: BenchmarkReportView | null;
  error: string | null;
}

export const DICTIONARY_PAGE_SIZE = 100;
export const HISTORY_PAGE_SIZE = 100;
