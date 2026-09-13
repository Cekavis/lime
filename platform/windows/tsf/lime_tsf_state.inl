void TextService::RefreshConfigRevision(ITfContext* context) {
  std::string body;
  if (!g_pipe.Request(R"({"kind":"get_status"})", body)) { connected_ = false; return; }
  std::string kind;
  std::string state;
  if (!JsonField(body, "kind", kind) || kind != "status" ||
      !JsonField(body, "state", state) ||
      (state != "ready" && state != "rime_only")) {
    // Reloading/unavailable states intentionally fall back to the host.  The
    // TSF adapter must not eat letters or punctuation while the service cannot
    // produce a reliable Rime snapshot.
    connected_ = false;
    return;
  }
  connected_ = true;
  std::string schema;
  if (JsonField(body, "rime_schema", schema) && !schema.empty()) {
    const std::wstring next_schema = Wide(schema);
    if (!next_schema.empty() && next_schema != schema_id_) {
      schema_id_ = next_schema;
      schema_reset_pending_ = true;
      if (context) ResetCompositionForSchemaChange(context);
    }
  }
  uint64_t revision = 0;
  if (JsonNumber(body, "revision", revision)) config_revision_ = revision;
  uint64_t limit = 0;
  if (JsonNumber(body, "preceding_text_char_limit", limit)) context_limit_ = static_cast<uint32_t>(std::clamp<uint64_t>(limit, 1, 4096));
  uint64_t preview_limit = 0;
  if (JsonNumber(body, "context_preview_char_limit", preview_limit)) {
    context_preview_limit_ = static_cast<uint32_t>(std::clamp<uint64_t>(preview_limit, 0, 1024));
  }
  uint64_t page_size = 0;
  if (JsonNumber(body, "page_size", page_size)) {
    // The TSF key path intentionally exposes the standard 1-9 shortcuts.
    // Keep the visual page and the selectable range consistent until a
    // dedicated 10+ selection gesture is added.
    page_size_ = static_cast<uint32_t>(std::clamp<uint64_t>(page_size, 1, 9));
  }
}

void TextService::PrimeFocusedUiAutomation() {
  if (!thread_manager_) return;
  ComPtr<ITfDocumentMgr> document_manager;
  ComPtr<ITfContext> context;
  ComPtr<ITfContextView> view;
  HWND view_window = nullptr;
  if (FAILED(thread_manager_->GetFocus(&document_manager)) ||
      !document_manager ||
      FAILED(document_manager->GetTop(&context)) || !context ||
      FAILED(context->GetActiveView(&view)) || !view ||
      FAILED(view->GetWnd(&view_window)) || !view_window) {
    return;
  }
  if (!accessible_context_)
    accessible_context_ = std::make_shared<AccessibleContextState>();
  PrimeUiAutomationPreceding(accessible_context_, view_window, ContextLimit());
}

bool TextService::ResetCompositionForSchemaChange(ITfContext* context) {
  if (!schema_reset_pending_) return true;
  if ((!preedit_.empty() || composition_) &&
      (!context || !CancelComposition(context))) {
    return false;
  }
  preedit_.clear();
  candidates_.clear();
  preceding_preview_.clear();
  candidate_anchor_ = {};
  candidate_anchor_available_ = false;
  candidate_page_ = 0;
  selected_candidate_ = 0;
  HideCandidates();
  schema_reset_pending_ = false;
  return true;
}

void TextService::LearnCandidate(std::wstring_view pinyin, std::wstring_view text) {
  if (!connected_ || pinyin.empty() || text.empty()) return;
  std::string body;
  const std::string json = "{\"kind\":\"learn\",\"payload\":{\"pinyin\":\"" +
                           JsonEscape(pinyin) + "\",\"text\":\"" + JsonEscape(text) + "\"}}";
  if (!g_pipe.Request(json, body)) connected_ = false;
}

void TextService::ClearPendingPartialSelection() {
  partial_edit_pending_ = false;
  partial_edit_generation_ = 0;
  pending_partial_pinyin_.clear();
  pending_partial_commit_.clear();
  pending_partial_remainder_.clear();
  pending_partial_candidates_.clear();
  pending_partial_preceding_.clear();
  pending_partial_anchor_ = {};
  pending_partial_anchor_available_ = false;
  pending_partial_fetch_complete_ = false;
}

bool TextService::ResolvePendingCancellation() {
  if (!cancel_pending_) return true;
  if (terminal_edit_pending_ && terminal_edit_action_ == Action::Cancel) {
    // The original cancellation is already queued.  Re-requesting it on
    // every key would advance the generation and invalidate the callback we
    // are waiting for.
    return false;
  }
  if (composition_) {
    if (!active_context_ || !CancelComposition(active_context_.Get())) return false;
    // An ASYNCDONTCARE edit session may have been accepted but not run yet.
    // Keep consuming keys until its composition callback has actually ended
    // the range; otherwise the host can append new text to the old pinyin.
    if (composition_) return false;
  }
  cancel_pending_ = false;
  ClearCompositionState();
  composition_.Reset();
  composition_context_.Reset();
  active_context_.Reset();
  passthrough_notified_ = false;
  return true;
}
