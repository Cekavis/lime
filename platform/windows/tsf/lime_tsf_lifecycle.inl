

HRESULT CompositionSession::DoEditSession(TfEditCookie cookie) {
  // Key callbacks can queue multiple ASYNCDONTCARE sessions.  Once a newer
  // key (especially Esc) advances the generation, an older callback must not
  // recreate or overwrite the current composition.
  if (!owner->IsEditCurrent(generation)) return S_OK;
  bool succeeded = false;
  if (action == TextService::Action::Cancel) {
    // EndComposition only changes the TSF state; it does not remove the
    // current composition text.  Clear the range first so Esc really cancels
    // the unconfirmed pinyin instead of committing it as Latin text.
    succeeded = owner->SetCompositionText(cookie, {}) &&
                owner->EndComposition(cookie);
    owner->CompleteEditSession(action, generation, succeeded);
    return succeeded ? S_OK : E_FAIL;
  }
  // Standalone punctuation still uses a short-lived TSF
  // composition.  Chromium/WebView2 context owners can re-enter the Windows
  // text-input framework when a key-event sink calls InsertTextAtSelection
  // directly, which has caused host hangs and process termination.  The same
  // StartComposition -> SetText -> EndComposition path used for candidates is
  // accepted by those hosts and leaves no visible unconfirmed state.
  if (!owner->EnsureComposition(context.Get(), cookie)) {
    owner->CompleteEditSession(action, generation, false);
    return E_FAIL;
  }
  if (action == TextService::Action::Update) {
    succeeded = owner->SetCompositionText(cookie, text);
  } else if (action == TextService::Action::CommitPartial) {
    succeeded = owner->CommitPartialComposition(cookie, text, remainder);
  } else {
    succeeded = owner->CommitComposition(cookie, text);
  }
  owner->CompleteEditSession(action, generation, succeeded);
  return succeeded ? S_OK : E_FAIL;
}

TextService::TextService() { ++g_module_references; }
TextService::~TextService() {
  Deactivate();
  if (candidate_ui_) candidate_ui_->DetachOwner();
  --g_module_references;
}

HRESULT TextService::QueryInterface(REFIID iid, void** object) {
  if (!object) return E_POINTER; *object = nullptr;
  if (iid == IID_IUnknown || iid == IID_ITfTextInputProcessor || iid == IID_ITfTextInputProcessorEx) *object = static_cast<ITfTextInputProcessorEx*>(this);
  else if (iid == IID_ITfThreadMgrEventSink) *object = static_cast<ITfThreadMgrEventSink*>(this);
  else if (iid == IID_ITfKeyEventSink) *object = static_cast<ITfKeyEventSink*>(this);
  else if (iid == IID_ITfThreadFocusSink) *object = static_cast<ITfThreadFocusSink*>(this);
  else if (iid == IID_ITfCompositionSink) *object = static_cast<ITfCompositionSink*>(this);
  else if (iid == IID_ITfTextEditSink) *object = static_cast<ITfTextEditSink*>(this);
  else if (iid == IID_ITfDisplayAttributeProvider) *object = static_cast<ITfDisplayAttributeProvider*>(this);
  else if (iid == IID_ITfTextLayoutSink) *object = static_cast<ITfTextLayoutSink*>(this);
  else return E_NOINTERFACE;
  AddRef(); return S_OK;
}
ULONG TextService::AddRef() { return ++references_; }
ULONG TextService::Release() { const ULONG v = --references_; if (!v) delete this; return v; }

HRESULT TextService::OnInitDocumentMgr(ITfDocumentMgr*) { return S_OK; }
HRESULT TextService::OnUninitDocumentMgr(ITfDocumentMgr*) { return S_OK; }
HRESULT TextService::OnPushContext(ITfContext*) { return S_OK; }
HRESULT TextService::OnPopContext(ITfContext*) { return S_OK; }

HRESULT TextService::OnSetFocus(ITfDocumentMgr* focused_document_manager,
                                ITfDocumentMgr*) {
  ComPtr<ITfContext> focused_context;
  if (focused_document_manager) {
    focused_document_manager->GetTop(&focused_context);
  }

  const bool context_changed =
      (composition_context_ && composition_context_.Get() != focused_context.Get()) ||
      (active_context_ && active_context_.Get() != focused_context.Get());
  if (!focused_context || context_changed) {
    DropUnavailableContext();
  }

  if (focused_context && IsKeyboardDisabled(focused_context.Get())) {
    DropUnavailableContext();
    UnadviseTextEditSink();
  } else {
    AdviseTextEditSink(focused_context.Get());
  }
  if (focused_context) PrimeFocusedUiAutomation();
  return S_OK;
}

HRESULT TextService::OnSetThreadFocus() {
  RetryDetachedCompositionEnds();
  PrimeFocusedUiAutomation();
  return S_OK;
}

HRESULT TextService::OnKillThreadFocus() {
  DropUnavailableContext();
  return S_OK;
}

void TextService::QueueDetachedCompositionEnd(ITfContext* context,
                                              ITfComposition* composition) {
  if (!context || !composition || client_id_ == TF_CLIENTID_NULL) return;
  ComPtr<DetachedCompositionEndSession> session;
  session.Attach(new (std::nothrow)
                     DetachedCompositionEndSession(context, composition));
  if (!session) return;
  detached_ends_.push_back(std::move(session));
  RetryDetachedCompositionEnds();
}

void TextService::RetryDetachedCompositionEnds() {
  // RequestEditSession can reenter a focus callback. Iterate a snapshot and
  // mark requests before entering COM so reentry cannot enqueue them twice.
  const auto sessions = detached_ends_;
  for (const auto& session : sessions) {
    if (session->finished || session->queued) continue;
    session->queued = true;
    HRESULT result = E_FAIL;
    const HRESULT request = session->context->RequestEditSession(
        client_id_, session.Get(), TF_ES_READWRITE | TF_ES_ASYNC, &result);
    if (FAILED(request) || FAILED(result)) session->queued = false;
  }
  std::erase_if(detached_ends_, [](const auto& session) {
    return session->finished;
  });
}

void TextService::UnadviseThreadSinks() {
  if (!thread_manager_) {
    thread_mgr_event_sink_cookie_ = TF_INVALID_COOKIE;
    thread_focus_sink_cookie_ = TF_INVALID_COOKIE;
    return;
  }
  ComPtr<ITfSource> source;
  if (SUCCEEDED(thread_manager_.As(&source)) && source) {
    if (thread_mgr_event_sink_cookie_ != TF_INVALID_COOKIE) {
      source->UnadviseSink(thread_mgr_event_sink_cookie_);
    }
    if (thread_focus_sink_cookie_ != TF_INVALID_COOKIE) {
      source->UnadviseSink(thread_focus_sink_cookie_);
    }
  }
  thread_mgr_event_sink_cookie_ = TF_INVALID_COOKIE;
  thread_focus_sink_cookie_ = TF_INVALID_COOKIE;
}

void TextService::AdviseTextEditSink(ITfContext* context) {
  UnadviseTextEditSink();
  if (!context) return;
  ComPtr<ITfSource> source;
  if (FAILED(context->QueryInterface(IID_PPV_ARGS(&source))) || !source) return;
  DWORD cookie = TF_INVALID_COOKIE;
  if (FAILED(source->AdviseSink(IID_ITfTextEditSink,
                                static_cast<ITfTextEditSink*>(this), &cookie))) {
    return;
  }
  text_edit_sink_source_ = std::move(source);
  text_edit_sink_context_ = context;
  text_edit_sink_cookie_ = cookie;
}

void TextService::UnadviseTextEditSink() {
  if (text_edit_sink_source_ && text_edit_sink_cookie_ != TF_INVALID_COOKIE) {
    text_edit_sink_source_->UnadviseSink(text_edit_sink_cookie_);
  }
  text_edit_sink_source_.Reset();
  text_edit_sink_context_.Reset();
  text_edit_sink_cookie_ = TF_INVALID_COOKIE;
}

HRESULT TextService::EnumDisplayAttributeInfo(
    IEnumTfDisplayAttributeInfo** enumerator) {
  if (!enumerator) return E_POINTER;
  *enumerator = new (std::nothrow) DisplayAttributeEnumerator();
  return *enumerator ? S_OK : E_OUTOFMEMORY;
}

HRESULT TextService::GetDisplayAttributeInfo(REFGUID guid,
                                             ITfDisplayAttributeInfo** info) {
  if (!info) return E_POINTER;
  *info = nullptr;
  if (!IsEqualGUID(guid, kDisplayAttributeInput)) return E_INVALIDARG;
  *info = new (std::nothrow) DisplayAttributeInfo();
  return *info ? S_OK : E_OUTOFMEMORY;
}

void TextService::InitializeDisplayAttribute() {
  if (display_attribute_atom_ != TF_INVALID_GUIDATOM) return;
  ComPtr<ITfCategoryMgr> categories;
  if (FAILED(CoCreateInstance(CLSID_TF_CategoryMgr, nullptr,
                              CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&categories))) ||
      !categories) {
    return;
  }
  TfGuidAtom atom = TF_INVALID_GUIDATOM;
  if (SUCCEEDED(categories->RegisterGUID(kDisplayAttributeInput, &atom))) {
    display_attribute_atom_ = atom;
  }
}

bool TextService::BeginCandidateUi() {
  if (!thread_manager_) return false;
  if (!candidate_ui_) candidate_ui_.Attach(new (std::nothrow) CandidateUiElement(this));
  if (!candidate_ui_) return false;
  if (candidate_ui_->started()) return true;
  // BeginUIElement may synchronously query the element.  Publish the current
  // page before entering the manager so those callbacks never observe the
  // previous composition's candidates.
  candidate_ui_->SetSnapshot(candidates_, candidate_page_, selected_candidate_,
                             page_size_);

  ComPtr<ITfUIElementMgr> manager;
  if (FAILED(thread_manager_->QueryInterface(IID_PPV_ARGS(&manager))) ||
      !manager) {
    return false;
  }
  BOOL show = TRUE;
  DWORD id = 0;
  const HRESULT hr = manager->BeginUIElement(candidate_ui_.Get(), &show, &id);
  if (FAILED(hr)) {
    return false;
  }
  candidate_ui_->set_started(true, id);
  candidate_ui_->set_external_show(show != FALSE);
  candidate_ui_external_ = show != FALSE;
  return true;
}

void TextService::UpdateCandidateUi() {
  ITfContext* context = active_context_.Get();
  if (composition_context_) context = composition_context_.Get();
  const bool should_show = context != nullptr && !candidates_.empty();
  if (!candidates_.empty() && (!candidate_ui_ || !candidate_ui_->started())) {
    BeginCandidateUi();
  }
  if (candidate_ui_ && candidate_ui_->started()) {
    candidate_ui_->SetSnapshot(candidates_, candidate_page_, selected_candidate_,
                               page_size_);
    // UIElement hosts query IsShown synchronously from UpdateUIElement.  Set
    // this before the manager call; publishing it afterwards leaves hosts
    // such as Windows Settings with a valid composition but no popup.
    candidate_ui_->SetShown(should_show);
    ComPtr<ITfUIElementMgr> manager;
    if (thread_manager_ &&
        SUCCEEDED(thread_manager_->QueryInterface(IID_PPV_ARGS(&manager))) &&
        manager) {
      manager->UpdateUIElement(candidate_ui_->ui_id());
    }
  }
  // A FALSE pbShow means the focused host owns the integrated candidate list.
  // Do not open a second Weasel popup over it; that would produce duplicate
  // candidates and different keyboard focus behavior.
  if (!should_show ||
      (candidate_ui_ && candidate_ui_->started() && !candidate_ui_external_)) {
    g_candidates.Hide();
    return;
  }
  g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_,
                    page_size_, {}, preceding_preview_, CandidateAnchor());
}

void TextService::ShowCandidates(ITfContext* context) {
  if (context) active_context_ = context;
  UpdateCandidateUi();
}

void TextService::EndCandidateUi() {
  if (candidate_ui_ && candidate_ui_->started() && thread_manager_) {
    ComPtr<ITfUIElementMgr> manager;
    if (SUCCEEDED(thread_manager_->QueryInterface(IID_PPV_ARGS(&manager))) &&
        manager) {
      manager->EndUIElement(candidate_ui_->ui_id());
    }
  }
  if (candidate_ui_) candidate_ui_->set_started(false);
  candidate_ui_external_ = true;
}

HRESULT TextService::Activate(ITfThreadMgr* manager, TfClientId client_id) { return ActivateEx(manager, client_id, 0); }
HRESULT TextService::ActivateEx(ITfThreadMgr* manager, TfClientId client_id, DWORD flags) {
  if (!manager) return E_INVALIDARG;
  const HRESULT deactivated = Deactivate();
  if (FAILED(deactivated)) return deactivated;
  thread_manager_ = manager; client_id_ = client_id; activation_flags_ = flags;
  HRESULT hr = manager->QueryInterface(IID_PPV_ARGS(&keystroke_manager_)); if (FAILED(hr)) return hr;
  hr = keystroke_manager_->AdviseKeyEventSink(client_id_, this, TRUE); if (FAILED(hr)) { keystroke_manager_.Reset(); return hr; }
  ComPtr<ITfSource> source;
  if (SUCCEEDED(manager->QueryInterface(IID_PPV_ARGS(&source))) && source) {
    source->AdviseSink(IID_ITfThreadMgrEventSink,
                       static_cast<ITfThreadMgrEventSink*>(this),
                       &thread_mgr_event_sink_cookie_);
    source->AdviseSink(IID_ITfThreadFocusSink,
                       static_cast<ITfThreadFocusSink*>(this),
                       &thread_focus_sink_cookie_);
  }
  ComPtr<ITfDocumentMgr> focused_document_manager;
  manager->GetFocus(&focused_document_manager);
  OnSetFocus(focused_document_manager.Get(), nullptr);
  InitializeDisplayAttribute();
  RefreshConfigRevision();
  return S_OK;
}
HRESULT TextService::Deactivate() {
  DropUnavailableContext();
  // Accepted cleanup callbacks own their old context independently. A host
  // which rejects cleanup must not keep the deactivated key sink registered.
  detached_ends_.clear();
  UnadviseTextEditSink();
  connected_ = false;
  schema_id_ = L"rime_ice";
  context_limit_ = kDefaultContextLimit;
  context_preview_limit_ = kDefaultContextPreviewLimit;
  page_size_ = 9;
  schema_reset_pending_ = false;
  passthrough_notified_ = false;
  shift_down_mask_ = 0;
  shift_pending_mask_ = 0;
  left_shift_down_tick_ = 0;
  right_shift_down_tick_ = 0;
  ascii_mode_ = false;
  last_chinese_input_was_digit_ = false;
  space_keyup_pending_ = false;
  single_quote_open_ = false;
  double_quote_open_ = false;
  UnadviseThreadSinks();
  if (keystroke_manager_ && client_id_ != TF_CLIENTID_NULL) keystroke_manager_->UnadviseKeyEventSink(client_id_);
  keystroke_manager_.Reset(); thread_manager_.Reset(); client_id_ = TF_CLIENTID_NULL; activation_flags_ = 0; return S_OK;
}
HRESULT TextService::OnSetFocus(BOOL foreground) {
  if (foreground) {
    PrimeFocusedUiAutomation();
    return S_OK;
  }
  DropUnavailableContext();
  return S_OK;
}
HRESULT TextService::OnCompositionTerminated(TfEditCookie, ITfComposition* composition) {
  if (!composition || composition_.Get() != composition) return S_OK;
  ++edit_generation_;
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  UnadviseLayoutSink();
  composition_.Reset();
  composition_context_.Reset();
  ClearCompositionState();
  active_context_.Reset();
  return S_OK;
}

HRESULT TextService::OnEndEdit(ITfContext* context, TfEditCookie cookie,
                               ITfEditRecord* edit_record) {
  if (!composition_ || composition_context_.Get() != context || !edit_record) {
    return S_OK;
  }
  BOOL selection_changed = FALSE;
  if (FAILED(edit_record->GetSelectionStatus(&selection_changed)) ||
      !selection_changed) {
    return S_OK;
  }
  TF_SELECTION selection{};
  ULONG fetched = 0;
  if (FAILED(context->GetSelection(cookie, TF_DEFAULT_SELECTION, 1,
                                   &selection, &fetched)) ||
      fetched != 1 || !selection.range) {
    return S_OK;
  }
  ComPtr<ITfRange> selection_range;
  selection_range.Attach(selection.range);
  ComPtr<ITfRange> composition_range;
  if (FAILED(composition_->GetRange(&composition_range)) ||
      !composition_range) {
    return S_OK;
  }
  LONG comparison = 0;
  if (FAILED(composition_range->CompareStart(cookie, selection_range.Get(),
                                             TF_ANCHOR_START, &comparison)) ||
      comparison > 0 ||
      FAILED(composition_range->CompareEnd(cookie, selection_range.Get(),
                                            TF_ANCHOR_END, &comparison)) ||
      comparison < 0) {
    // Match Weasel: a caret moved out of the composition aborts the old
    // composition before the next editor can receive input.
    if (!CancelComposition(context)) DropUnavailableContext();
  }
  return S_OK;
}

HRESULT TextService::OnLayoutChange(ITfContext* context, TfLayoutCode code,
                                    ITfContextView*) {
  const bool matching = composition_ && composition_context_.Get() == context;
  if (!matching || code != TF_LC_CHANGE || candidates_.empty()) return S_OK;
  QueueCandidateAnchorRefresh(context, edit_generation_);
  return S_OK;
}

void TextService::CompleteEditSession(Action action, uint64_t generation,
                                      bool succeeded) {
  if (!IsEditCurrent(generation)) return;
  last_edit_pending_ = false;
  if (action == Action::Update) {
    ITfContext* context = composition_context_.Get();
    if (!succeeded) {
      HideCandidates();
      const bool host_unavailable = !composition_ ||
                                    IsKeyboardDisabled(context);
      if (IsUnavailableEditError(last_edit_error_) &&
          (host_unavailable || last_edit_error_ != E_FAIL)) {
        DropUnavailableContext();
      }
      return;
    }
    // SetText can leave GetTextExt temporarily without layout.  Like Weasel,
    // request a separate read session after the write so the first key uses
    // the new composition's position.  Never publish the pre-write caret.
    candidate_anchor_ = {};
    candidate_anchor_available_ = false;
    if (!context || candidates_.empty()) {
      HideCandidates();
    } else if (!QueueCandidateAnchorRefresh(context, generation)) {
      ShowCandidates(context);
    }
    return;
  }
  EndCandidateUi();
  if (action == Action::CommitPartial && partial_edit_generation_ == generation) {
    ITfContext* context = composition_context_.Get();
    if (!succeeded) {
      ClearPendingPartialSelection();
      if (context) {
        ShowCandidates(context);
      }
      return;
    }

    const std::wstring pinyin = pending_partial_pinyin_;
    const std::wstring commit = pending_partial_commit_;
    preedit_ = std::move(pending_partial_remainder_);
    candidates_ = std::move(pending_partial_candidates_);
    preceding_preview_ =
        PreviewText(pending_partial_preceding_, context_preview_limit_);
    candidate_anchor_ = pending_partial_anchor_;
    candidate_anchor_available_ = pending_partial_anchor_available_;
    candidate_fetch_complete_ = pending_partial_fetch_complete_;
    candidate_page_ = 0;
    selected_candidate_ = 0;
    accessible_context_.reset();
    ClearPendingPartialSelection();

    if (!pinyin.empty()) LearnCandidate(pinyin, commit);
    if (!context || candidates_.empty()) {
      HideCandidates();
    } else {
      ShowCandidates(context);
    }
    return;
  }
  if (terminal_edit_pending_ && terminal_edit_generation_ == generation) {
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
  }
  if (succeeded) {
    if (action == Action::Cancel || action == Action::Commit) {
      // For an asynchronous terminal edit, the key sink cannot clear its
      // local state until the TSF callback has actually changed the document.
      // Synchronous callbacks simply make this idempotent with HandleKey's
      // normal cleanup path.
      ClearCompositionState();
      composition_.Reset();
      composition_context_.Reset();
    }
    return;
  }
  if (action == Action::Cancel) {
    // Keep Esc/Backspace consumed until a later callback can retry the cancel;
    // forwarding either key while the composition survives can mutate host
    // text unexpectedly.
    cancel_pending_ = true;
  }
}
