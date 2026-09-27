// Browser QA only. Inject into a local test tab before app startup; never bundled.
(() => {
  let config = {
    rime_schema: "rime_ice", preceding_text_char_limit: 128, context_preview_char_limit: 32,
    page_size: 9, llm_rerank_count: 32, llm_effective_count: 3, llm_context_token_limit: 1024,
    llm_inference_count_limit: 1, llm_ignore_emoji: true, llm_backend: "cuda",
  };
  let revision = 1;
  let historyRevision = 1;
  let presets = [
    { name: "本地模型 A", path: "C:\\Models\\local-a.gguf", size_bytes: 640 * 1024 ** 2, loaded: true },
    { name: "本地模型 B", path: "C:\\Models\\local-b.gguf", size_bytes: 1280 * 1024 ** 2, loaded: false },
  ];
  const initializedModel = (preset) => {
    const modelBytes = preset.size_bytes ?? 512 * 1024 ** 2;
    const kvBytes = config.llm_context_token_limit * config.llm_rerank_count * 512;
    const computeBytes = config.llm_inference_count_limit * 48 * 1024 ** 2;
    const outputBytes = 4 * 1024 ** 2;
    const hostComputeBytes = config.llm_backend === "cpu" ? 0 : Math.round(4.1 * 1024 ** 2);
    const sharedModelBytes = Math.floor(modelBytes / 5);
    // Include the separate CUDA / CUDA0 / CUDA_HOST columns seen in native reports.
    const breakdown = config.llm_backend === "cpu"
      ? { "cpu.model": modelBytes, "cpu.kv": kvBytes, "cpu.compute": computeBytes, "cpu.output": outputBytes }
      : { "cuda.model": sharedModelBytes, "cuda0.model": modelBytes - sharedModelBytes, "cuda0.kv": kvBytes,
        "cuda0.compute": computeBytes, "cuda_host.compute": hostComputeBytes, "cuda_host.output": outputBytes };
    const totalBytes = modelBytes + kvBytes + computeBytes + outputBytes + hostComputeBytes;
    return { ...preset, loaded: true, scoring_path: "attention", initialization_memory: {
      model_bytes: modelBytes, context_bytes: kvBytes + outputBytes, compute_bytes: computeBytes + hostComputeBytes,
      total_bytes: totalBytes, backend: config.llm_backend,
      breakdown: { ...breakdown, total: totalBytes },
    } };
  };
  let model = initializedModel(presets[0]);
  let dictionary = Array.from({ length: 102 }, (_, index) => ({ text: ["世界", "输入法", "青柠"][index % 3], pinyin: ["shi jie", "shu ru fa", "qing ning"][index % 3], weight: 100 + index }));
  const candidate = (text) => ({ display_text: text, commit_text: text });
  const response = (preedit, context, index = 0) => ({
    request_id: index + 1, timestamp_ms: 1790480000000 - index * 1000,
    preedit, preceding_text: context, model_name: "local-a.gguf", service_state: "ready", context_used: true,
    end_to_end_duration_ms: 24.18, rime_duration_ms: 2.5,
    rime_candidates: [candidate("时节"), candidate("世界"), candidate("视界")],
    final_candidates: [candidate("世界"), candidate("时节"), candidate("视界")],
    diagnostics: ["世界", "时节", "视界"].map((text, i) => ({ rime_candidate: candidate(["时节", "世界", "视界"][i]), llm_candidate: candidate(text), display_candidate: candidate(text), logprob: -0.6 - i, logprobs: [-0.3, -0.3 - i], mismatch: i === 2 })),
    llm_performance: { total_ms: 20, tokenize_ms: 1, decode_ms: 19, candidate_count: 3, scored_count: 3, target_token_count: 6, batch_count: 1, mismatch_count: 1, context_token_count: 8, decode_input_token_count: 14, logits_output_count: 6, inference_count_limit: 1, omitted_candidate_count: 0, boundary_rollback: { prefix_token_count: 3, replayed_token_count: 1, prefix_text: "你好，", replayed_text: "这个" } },
  });
  let history = Array.from({ length: 102 }, (_, index) => response("shijie", "你好，这个", index));
  const dataset = { id: "qa-corpus", name: "测试语料", version: 1, corpora: [{ id: "zhihu", name: "知乎", characters: 42000, cases: 12000 }, { id: "classics", name: "经典文章", characters: 44000, cases: 16000 }] };
  const benchmarkResult = (name, index = 0) => ({ id: String(index), model_name: name, mode: "full", status: "completed", configuration: { llm_rerank_count: 32, preceding_text_char_limit: 128 }, config, model_sha256: "qa-model", report: { complete: true, modes: ["full"], summaries: [null, "zhihu", "classics"].map((category) => ({ category, mode: "full", accuracy: 0.83 - index * 0.04 })), observations: [{ case_id: "qa-1", category: "zhihu", mode: "full", context: "你好，这个", preedit: "shijie", expected: "世界", top1: "时节", correct: false }] } });
  let benchmark = { status: "completed", dataset_id: dataset.id, total: 100, completed: 100, results: presets.map((preset, i) => benchmarkResult(preset.name, i)) };
  const page = (items, args) => ({ items: items.slice((args.page - 1) * args.pageSize, args.page * args.pageSize), total: items.length, page: args.page, page_size: args.pageSize });
  window.__TAURI_INTERNALS__ = {
    invoke: async (command, args = {}) => {
      await new Promise((resolve) => setTimeout(resolve, 80));
      switch (command) {
        case "get_status": return { state: model.loaded ? "ready" : "rime_only", config: { revision, config: { ...config } }, model: { ...model } };
        case "get_config": return { revision, config: { ...config } };
        case "set_config": config = { ...args.config }; revision++; return { revision, config: { ...config } };
        case "list_model_presets": return presets.map((preset) => ({ ...preset }));
        case "save_model_preset": { const preset = { name: args.name, path: args.path, loaded: false }; presets.push(preset); return preset; }
        case "rename_model_preset": { const preset = presets.find((item) => item.name === args.name); preset.name = args.newName; return { ...preset }; }
        case "delete_model_preset": presets = presets.filter((item) => item.name !== args.name); return;
        case "select_model_preset": presets.forEach((preset) => { preset.loaded = preset.name === args.name; }); model = initializedModel(presets.find((preset) => preset.loaded)); return { ...model };
        case "load_model": model = initializedModel({ ...model, path: args.path }); return { ...model };
        case "unload_model": model = { ...model, loaded: false, initialization_memory: null }; presets.forEach((preset) => { preset.loaded = false; }); return { ...model };
        case "get_dictionary_page": return page(dictionary, args);
        case "import_dictionary": dictionary = [...dictionary, ...args.entries]; return;
        case "clear_dictionary": dictionary = []; return;
        case "get_input_history_page": return page(history, args);
        case "wait_for_input_history": await new Promise((resolve) => setTimeout(resolve, 600)); return historyRevision;
        case "clear_input_history": history = []; historyRevision++; return;
        case "test_input": { const result = response(args.preedit, args.precedingText); history.unshift(result); historyRevision++; return result; }
        case "get_benchmark_dataset": return dataset;
        case "get_benchmark_status": return structuredClone(benchmark);
        case "start_benchmark": benchmark = { ...benchmark, status: "running", completed: 25, results: args.request.models.map((name, index) => ({ ...benchmarkResult(name, index), status: "running" })) }; return structuredClone(benchmark);
        case "stop_benchmark": benchmark = { ...benchmark, status: "cancelled" }; return;
        default: throw new Error("Unsupported QA command: " + command);
      }
    },
  };
})();
