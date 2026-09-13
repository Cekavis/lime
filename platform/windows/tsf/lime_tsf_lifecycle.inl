

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
  else if (iid == IID_ITfKeyEventSink) *object = static_cast<ITfKeyEventSink*>(this);
  else if (iid == IID_ITfCompositionSink) *object = static_cast<ITfCompositionSink*>(this);
  else if (iid == IID_ITfDisplayAttributeProvider) *object = static_cast<ITfDisplayAttributeProvider*>(this);
  else if (iid == IID_ITfTextLayoutSink) *object = static_cast<ITfTextLayoutSink*>(this);
  else return E_NOINTERFACE;
  AddRef(); return S_OK;
}
ULONG TextService::AddRef() { return ++references_; }
ULONG TextService::Release() { const ULONG v = --references_; if (!v) delete this; return v; }

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
  InitializeDisplayAttribute();
  RefreshConfigRevision();
  return S_OK;
}
HRESULT TextService::Deactivate() {
  if (composition_) {
    if (!active_context_ || !CancelComposition(active_context_.Get())) {
      // Keep the composition handle and its context.  Dropping either one
      // after a rejected edit session would leave the host with an orphaned
      // TSF composition that Lime can no longer close safely.
      cancel_pending_ = true;
      HideCandidates();
      return E_FAIL;
    }
  }
  // Invalidate any ASYNCDONTCARE session that has been accepted but has not
  // reached DoEditSession before the service is deactivated.
  ++edit_generation_;
  cancel_pending_ = false;
  ClearCompositionState();
  composition_.Reset();
  composition_context_.Reset();
  active_context_.Reset();
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
  if (keystroke_manager_ && client_id_ != TF_CLIENTID_NULL) keystroke_manager_->UnadviseKeyEventSink(client_id_);
  keystroke_manager_.Reset(); thread_manager_.Reset(); client_id_ = TF_CLIENTID_NULL; activation_flags_ = 0; return S_OK;
}
HRESULT TextService::OnSetFocus(BOOL foreground) {
  if (foreground) {
    PrimeFocusedUiAutomation();
    return S_OK;
  }
  shift_down_mask_ = 0;
  shift_pending_mask_ = 0;
  left_shift_down_tick_ = 0;
  right_shift_down_tick_ = 0;
  last_chinese_input_was_digit_ = false;
  space_keyup_pending_ = false;
  // Punctuation pairing belongs to the focused document.  Do not carry an
  // opening quote from one application/window into the next one.
  single_quote_open_ = false;
  double_quote_open_ = false;
  if (!composition_) {
    // An Update/Commit session may already be queued even though StartComposition
    // has not run yet.  Focus loss invalidates that callback before local state
    // is cleared; otherwise the old context could receive a composition after
    // the user has moved to another window.
    ++edit_generation_;
    last_edit_pending_ = false;
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
  }
  const bool canceled = !composition_ || CancelComposition(active_context_.Get());
  if (canceled) {
    cancel_pending_ = false;
    ClearCompositionState();
    composition_.Reset();
    composition_context_.Reset();
    // Contexts are apartment-bound and should not be retained across focus
    // changes. The next key callback supplies the new active context.
    active_context_.Reset();
  } else {
    // Do not drop the composition handle when the host rejects the cancel
    // edit session; otherwise the host can retain an orphaned composition.
    cancel_pending_ = true;
    HideCandidates();
  }
  return S_OK;
}
HRESULT TextService::OnCompositionTerminated(TfEditCookie, ITfComposition* composition) {
  if (composition && composition_.Get() != composition) return S_OK;
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
