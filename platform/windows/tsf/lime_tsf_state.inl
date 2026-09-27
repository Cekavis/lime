namespace {

bool IsMpcPlaybackWindow(HWND window) {
  if (!window) return false;
  wchar_t class_name[256]{};
  const int length = GetClassNameW(window, class_name, ARRAYSIZE(class_name));
  if (length == 0) return false;
  const std::wstring_view name(class_name, length);
  if (name == L"MPC-BE") return true;

  // MPC-BE's CMainFrame::OnSetFocus forwards to its CChildView, a direct
  // Afx child with AFX_IDW_PANE_FIRST. Its default IME context can still be
  // writable despite the view having no editor. Match that exact surface,
  // not the process/ancestor: dialogs and nested edits must keep working.
  constexpr int kMfcFirstPane = 0xe900;
  if (!name.starts_with(L"Afx:") || GetDlgCtrlID(window) != kMfcFirstPane ||
      (GetWindowLongPtrW(window, GWL_STYLE) & WS_CHILD) == 0) {
    return false;
  }
  HWND parent = GetAncestor(window, GA_PARENT);
  const int parent_length = GetClassNameW(parent, class_name, ARRAYSIZE(class_name));
  return parent_length > 0 &&
         std::wstring_view(class_name, parent_length) == L"MPC-BE";
}

}  // namespace

bool TextService::IsKeyboardDisabled(ITfContext* context) const {
  // TSF may still invoke the key sink while the focused application has no
  // editable document.  Treat a missing callback context as host-owned input
  // immediately; this check must happen before the service or Rime sees the
  // key.
  if (!context) return true;

  // Use the current thread's actual focus, not the context view, which may
  // still describe the previous editor or Windows' floating IME window.
  // This also runs before IPC, edit sessions and mode-switch handling.
  if (IsMpcPlaybackWindow(GetFocus())) return true;

  // Empty browser text stores can remain focused but explicitly become
  // read-only. GetStatus does not request an edit lock.
  TF_STATUS status{};
  if (SUCCEEDED(context->GetStatus(&status)) &&
      (status.dwDynamicFlags & TF_SD_READONLY) != 0) return true;

  // Unit contracts call the sink directly without activating a thread manager.
  // They still exercise the native-focus and read-only gates above;
  // production callbacks also use the manager checks below.
  if (!thread_manager_) return false;

  Microsoft::WRL::ComPtr<ITfDocumentMgr> focused_document_manager;
  if (FAILED(thread_manager_->GetFocus(&focused_document_manager)) ||
      !focused_document_manager) {
    return true;
  }
  Microsoft::WRL::ComPtr<ITfContext> focused_context;
  if (FAILED(focused_document_manager->GetTop(&focused_context)) ||
      !focused_context) {
    return true;
  }
  // The key sink context must be the current top context. A browser can keep
  // invoking a sink for the previous text store after focus moved to a blank
  // document; accepting that stale pointer is what buffers shortcut letters.
  if (focused_context.Get() != context) return true;
  Microsoft::WRL::ComPtr<ITfDocumentMgr> context_document_manager;
  if (FAILED(context->GetDocumentMgr(&context_document_manager)) ||
      !context_document_manager ||
      context_document_manager.Get() != focused_document_manager.Get()) {
    return true;
  }

  // Keep Weasel's compartment checks as the authoritative disabled signal,
  // then reject dummy TSF contexts that cannot create a composition at all.
  // A default IME context can still support composition without an editor.
  // Do not infer editability from TF_SS_TRANSITORY either: browsers use it.
  Microsoft::WRL::ComPtr<ITfContextComposition> composition;
  if (FAILED(context->QueryInterface(IID_PPV_ARGS(&composition))) ||
      !composition) {
    return true;
  }

  Microsoft::WRL::ComPtr<ITfCompartmentMgr> compartments;
  if (SUCCEEDED(focused_context.As(&compartments)) && compartments) {
    const auto compartment_is_set = [&](REFGUID guid) {
      Microsoft::WRL::ComPtr<ITfCompartment> compartment;
      if (FAILED(compartments->GetCompartment(guid, &compartment)) ||
          !compartment) {
        return false;
      }
      VARIANT value{};
      const HRESULT hr = compartment->GetValue(&value);
      const bool set = SUCCEEDED(hr) && value.vt == VT_I4 && value.lVal != 0;
      VariantClear(&value);
      return set;
    };
    // Match Weasel: these compartments are the host's authoritative signal
    // that the focused context cannot receive keyboard input.
    if (compartment_is_set(GUID_COMPARTMENT_KEYBOARD_DISABLED) ||
        compartment_is_set(GUID_COMPARTMENT_EMPTYCONTEXT)) {
      return true;
    }
  }

  return false;
}

void TextService::DropUnavailableContext() {
  // Detach before calling the host. Old writes become stale, while cleanup
  // retains its own references and cannot cancel the next editor's input.
  ComPtr<ITfContext> old_context = composition_context_;
  ComPtr<ITfComposition> old_composition = composition_;
  ++edit_generation_;
  UnadviseLayoutSink();
  composition_.Reset();
  composition_context_.Reset();
  active_context_.Reset();
  ClearCompositionState();
  shift_down_mask_ = 0;
  shift_pending_mask_ = 0;
  left_shift_down_tick_ = 0;
  right_shift_down_tick_ = 0;
  last_chinese_input_was_digit_ = false;
  space_keyup_pending_ = false;
  single_quote_open_ = false;
  double_quote_open_ = false;
  passthrough_notified_ = false;
  last_edit_error_ = S_OK;
  if (old_context && old_composition) {
    QueueDetachedCompositionEnd(old_context.Get(), old_composition.Get());
  } else {
    RetryDetachedCompositionEnds();
  }
}

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
