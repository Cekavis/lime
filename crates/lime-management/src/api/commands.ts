import { invoke } from "@tauri-apps/api/core";
import { decodeBenchmarkDataset, decodeBenchmarkRunState, decodeConfigSnapshot, decodeDictionaryPage, decodeHistoryPage, decodeInputData, decodeModel, decodePreset, decodePresets } from "./decode";
import { DICTIONARY_PAGE_SIZE, HISTORY_PAGE_SIZE, type BenchmarkDatasetView, type BenchmarkRunRequest, type BenchmarkRunState, type Config, type ConfigSnapshot, type DictionaryEntry, type DictionaryPage, type HistoryPage, type InputData, type ModelInfo, type ModelPreset, type ServiceStatus } from "./types";

export async function getConfig(): Promise<ConfigSnapshot> {
  return decodeConfigSnapshot(await invoke<unknown>("get_config"));
}

export async function setConfig(config: Config): Promise<ConfigSnapshot> {
  return decodeConfigSnapshot(await invoke<unknown>("set_config", { config }));
}

export async function getStatus(): Promise<ServiceStatus> {
  const value = await invoke<unknown>("get_status");
  const record = value as { state?: unknown; config?: unknown; model?: unknown };
  return {
    state: record.state === "ready" || record.state === "rime_only" || record.state === "reloading" || record.state === "unavailable" ? record.state : "unavailable",
    config: decodeConfigSnapshot(record.config),
    model: decodeModel(record.model),
  };
}

export async function loadModel(path: string): Promise<ModelInfo> {
  return decodeModel(await invoke<unknown>("load_model", { path }));
}

export async function unloadModel(): Promise<ModelInfo> {
  return decodeModel(await invoke<unknown>("unload_model"));
}

export async function listModelPresets(): Promise<ModelPreset[]> {
  return decodePresets(await invoke<unknown>("list_model_presets"));
}

function requirePreset(value: unknown): ModelPreset {
  const preset = decodePreset(value);
  if (!preset) throw new Error("管理服务返回了无效的模型预设");
  return preset;
}

export async function saveModelPreset(name: string, path: string): Promise<ModelPreset> {
  return requirePreset(await invoke<unknown>("save_model_preset", { name, path }));
}

export async function renameModelPreset(name: string, newName: string): Promise<ModelPreset> {
  return requirePreset(await invoke<unknown>("rename_model_preset", { name, newName }));
}

export async function deleteModelPreset(name: string): Promise<void> {
  await invoke("delete_model_preset", { name });
}

export async function selectModelPreset(name: string): Promise<ModelPreset> {
  return requirePreset(await invoke<unknown>("select_model_preset", { name }));
}

export async function getDictionaryPage(page: number): Promise<DictionaryPage> {
  return decodeDictionaryPage(await invoke<unknown>("get_dictionary_page", { page, pageSize: DICTIONARY_PAGE_SIZE }), page);
}

export async function importDictionary(entries: DictionaryEntry[]): Promise<void> {
  await invoke("import_dictionary", { entries });
}

export async function clearDictionary(): Promise<void> {
  await invoke("clear_dictionary");
}

export async function testInput(precedingText: string, preedit: string): Promise<InputData> {
  return decodeInputData(await invoke<unknown>("test_input", { precedingText, preedit }));
}

export async function getHistoryPage(page: number): Promise<HistoryPage> {
  return decodeHistoryPage(await invoke<unknown>("get_input_history_page", { page, pageSize: HISTORY_PAGE_SIZE }), page);
}

export async function waitForHistory(revision: number): Promise<number> {
  return Number(await invoke<unknown>("wait_for_input_history", { revision }));
}

export async function clearHistory(): Promise<void> {
  await invoke("clear_input_history");
}

export async function getBenchmarkDataset(): Promise<BenchmarkDatasetView> {
  return decodeBenchmarkDataset(await invoke<unknown>("get_benchmark_dataset"));
}

export async function startBenchmark(request: BenchmarkRunRequest): Promise<BenchmarkRunState> {
  return decodeBenchmarkRunState(await invoke<unknown>("start_benchmark", { request }));
}

// Keep the command-layer name aligned with the original benchmark design while
// using the asynchronous start/status protocol exposed by the management service.
export async function runBenchmark(request: BenchmarkRunRequest): Promise<BenchmarkRunState> {
  return startBenchmark(request);
}

export async function stopBenchmark(): Promise<void> {
  await invoke("stop_benchmark");
}

export async function getBenchmarkStatus(): Promise<BenchmarkRunState> {
  return decodeBenchmarkRunState(await invoke<unknown>("get_benchmark_status"));
}
