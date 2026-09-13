bool TextService::RequestEdit(ITfContext* context, Action action, const std::wstring& text,
                              bool synchronous, const std::wstring& remainder) {
  last_edit_pending_ = false;
  if (partial_edit_pending_ && action != Action::CommitPartial &&
      action != Action::Cancel) {
    last_edit_error_ = TF_E_LOCKED;
    return false;
  }
  if (partial_edit_pending_ && action == Action::CommitPartial) {
    last_edit_error_ = TF_E_LOCKED;
    return false;
  }
  if (terminal_edit_pending_ &&
      (action == Action::Update || action == Action::CommitPartial)) {
    // Do not enqueue a new composition update behind a pending commit/cancel;
    // doing so could resurrect text after the terminal edit has completed.
    last_edit_error_ = TF_E_LOCKED;
    return false;
  }
  const uint64_t generation = ++edit_generation_;
  if (action == Action::Cancel) ClearPendingPartialSelection();
  // A newer terminal request (normally Esc after an accepted commit) makes an
  // older queued terminal callback stale through the generation check.
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  if (action == Action::Commit || action == Action::Cancel) {
    terminal_edit_pending_ = true;
    terminal_edit_action_ = action;
    terminal_edit_generation_ = generation;
  } else if (action == Action::CommitPartial) {
    partial_edit_pending_ = true;
    partial_edit_generation_ = generation;
  }
  if (!context) {
    last_edit_error_ = E_POINTER;
    if (action == Action::Cancel) cancel_pending_ = true;
    if (action == Action::CommitPartial) ClearPendingPartialSelection();
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
    return false;
  }

  auto* session = new (std::nothrow)
      CompositionSession(this, context, action, text, generation, remainder);
  if (!session) {
    last_edit_error_ = E_OUTOFMEMORY;
    if (action == Action::Cancel) cancel_pending_ = true;
    if (action == Action::CommitPartial) ClearPendingPartialSelection();
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
    return false;
  }
  // Match Weasel's TSF lifecycle: register the candidate UI before the first
  // composition is created.  Search-box hosts make their rendering choice
  // during BeginUIElement, so this keeps that initial handshake in the same
  // position as Weasel's implementation.
  const bool candidate_ui_pending = action == Action::Update && !composition_;
  if (candidate_ui_pending) BeginCandidateUi();

  last_edit_error_ = S_OK;
  HRESULT result = E_FAIL;
  // Ordinary composition updates use ASYNCDONTCARE so TSF can queue them when
  // the host is locked.  Mode switches opt into TF_ES_SYNC and therefore fail
  // rather than queueing: the next key must observe the new mode immediately.
  const DWORD edit_flags = TF_ES_READWRITE | (synchronous ? TF_ES_SYNC : 0);
  HRESULT request = context->RequestEditSession(client_id_, session, edit_flags, &result);
  session->Release();
  if (FAILED(request)) {
    if (candidate_ui_pending && !composition_) EndCandidateUi();
    last_edit_error_ = request;
    if (action == Action::Cancel) cancel_pending_ = true;
    if (action == Action::CommitPartial) ClearPendingPartialSelection();
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
    return false;
  }
  // TF_S_ASYNC means the session was accepted and will invoke DoEditSession
  // later; it is a successful request even though there is no edit result yet.
  if (request == TF_S_ASYNC || result == TF_S_ASYNC) {
    if (synchronous) {
      // A synchronous mode switch must never leave a terminal edit queued: it
      // would make the following host key appear to disappear. Invalidate any
      // accepted callback and let the user retry after the editor releases its
      // lock.
      ++edit_generation_;
      last_edit_error_ = TF_E_SYNCHRONOUS;
      if (candidate_ui_pending && !composition_) EndCandidateUi();
      terminal_edit_pending_ = false;
      terminal_edit_generation_ = 0;
      if (action == Action::CommitPartial) ClearPendingPartialSelection();
      return false;
    }
    last_edit_pending_ = true;
    if (action == Action::Cancel) cancel_pending_ = true;
    return true;
  }
  if (SUCCEEDED(result)) {
    return true;
  }
  last_edit_error_ = result;
  if (candidate_ui_pending && !composition_) EndCandidateUi();
  if (action == Action::Cancel) cancel_pending_ = true;
  if (action == Action::CommitPartial) ClearPendingPartialSelection();
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  return false;
}

void TextService::AdviseLayoutSink(ITfContext* context) {
  UnadviseLayoutSink();
  if (!context) return;
  ComPtr<ITfSource> source;
  if (FAILED(context->QueryInterface(IID_PPV_ARGS(&source))) || !source) {
    return;
  }
  DWORD cookie = TF_INVALID_COOKIE;
  const HRESULT hr = source->AdviseSink(IID_ITfTextLayoutSink,
                                        static_cast<ITfTextLayoutSink*>(this),
                                        &cookie);
  if (SUCCEEDED(hr)) {
    layout_source_ = std::move(source);
    layout_sink_cookie_ = cookie;
  }
}

void TextService::UnadviseLayoutSink() {
  if (layout_source_ && layout_sink_cookie_ != TF_INVALID_COOKIE) {
    layout_source_->UnadviseSink(layout_sink_cookie_);
  }
  layout_source_.Reset();
  layout_sink_cookie_ = TF_INVALID_COOKIE;
}

bool TextService::EnsureComposition(ITfContext* context, TfEditCookie cookie) {
  if (composition_) return true;
  ComPtr<ITfContextComposition> composition_context;
  HRESULT hr = context->QueryInterface(IID_PPV_ARGS(&composition_context));
  if (FAILED(hr)) {
    last_edit_error_ = hr;
    return false;
  }
  ComPtr<ITfRange> range;
  // Query the insertion range through the context owner first.  This is the
  // path used by the Windows TSF samples and is more compatible with rich
  // editors than assuming that GetSelection() is a writable composition
  // range.  It does not modify document text.
  ComPtr<ITfInsertAtSelection> inserter;
  if (SUCCEEDED(context->QueryInterface(IID_PPV_ARGS(&inserter)))) {
    inserter->InsertTextAtSelection(cookie, TF_IAS_QUERYONLY, L"", 0, &range);
  }
  if (!range) {
    TF_SELECTION selection{};
    ULONG fetched = 0;
    hr = context->GetSelection(cookie, TF_DEFAULT_SELECTION, 1, &selection, &fetched);
    if (FAILED(hr) || fetched != 1 || !selection.range) {
      last_edit_error_ = FAILED(hr) ? hr : TF_E_NOSELECTION;
      return false;
    }
    range.Attach(selection.range);
  }

  composition_.Reset();
  const HRESULT started = composition_context->StartComposition(
      cookie, range.Get(), static_cast<ITfCompositionSink*>(this), &composition_);
  // A context owner is allowed to return S_OK while rejecting the
  // composition, in which case ppComposition is NULL.  Treat that as a real
  // failure instead of letting SetCompositionText fail later with a generic
  // error.
  if (FAILED(started) || !composition_) {
    composition_.Reset();
    composition_context_.Reset();
    last_edit_error_ = FAILED(started) ? started : TF_E_COMPOSITION_REJECTED;
    return false;
  }
  composition_context_ = context;
  AdviseLayoutSink(context);
  if (!SetSelectionToCompositionEnd(cookie)) {
    // Do not leave a live composition behind if the host cannot place the
    // caret at its end.  EndComposition is best-effort here; the original
    // edit error is retained for the caller.
    const HRESULT selection_error = last_edit_error_;
    composition_->EndComposition(cookie);
    UnadviseLayoutSink();
    composition_.Reset();
    composition_context_.Reset();
    last_edit_error_ = selection_error;
    return false;
  }
  return true;
}
bool TextService::SetCompositionText(TfEditCookie cookie, const std::wstring& text) {
  if (!composition_) {
    last_edit_error_ = TF_E_COMPOSITION_REJECTED;
    return false;
  }
  ComPtr<ITfRange> range;
  HRESULT hr = composition_->GetRange(&range);
  if (FAILED(hr) || !range) {
    last_edit_error_ = FAILED(hr) ? hr : TF_E_NOOBJECT;
    return false;
  }
  ClearCompositionDisplayAttribute(cookie);
  hr = range->SetText(cookie, 0, text.c_str(), static_cast<LONG>(text.size()));
  if (FAILED(hr)) {
    last_edit_error_ = hr;
    return false;
  }
  if (!SetCompositionDisplayAttribute(cookie)) {
    return false;
  }
  return SetSelectionToCompositionEnd(cookie);
}
bool TextService::CommitComposition(TfEditCookie cookie, const std::wstring& text) {
  if (!SetCompositionText(cookie, text)) return false;
  return EndComposition(cookie);
}
bool TextService::CommitPartialComposition(TfEditCookie cookie,
                                            const std::wstring& commit,
                                            const std::wstring& remainder) {
  if (!composition_) {
    last_edit_error_ = TF_E_COMPOSITION_REJECTED;
    return false;
  }
  if (commit.size() > static_cast<size_t>(std::numeric_limits<LONG>::max()) ||
      remainder.size() > static_cast<size_t>(std::numeric_limits<LONG>::max()) -
                              commit.size()) {
    last_edit_error_ = E_INVALIDARG;
    return false;
  }

  ComPtr<ITfRange> range;
  HRESULT hr = composition_->GetRange(&range);
  if (FAILED(hr) || !range) {
    last_edit_error_ = FAILED(hr) ? hr : TF_E_NOOBJECT;
    return false;
  }
  ComPtr<ITfRange> original_start;
  hr = range->Clone(&original_start);
  if (FAILED(hr) || !original_start ||
      FAILED(hr = original_start->Collapse(cookie, TF_ANCHOR_START))) {
    last_edit_error_ = FAILED(hr) ? hr : TF_E_NOOBJECT;
    return false;
  }

  const std::wstring original = preedit_;
  const std::wstring combined = commit + remainder;
  // The composition range is about to shrink past the committed prefix.
  // Clear the old property first so the committed text cannot retain Lime's
  // input underline.
  ClearCompositionDisplayAttribute(cookie);
  hr = range->SetText(cookie, 0, combined.c_str(), static_cast<LONG>(combined.size()));
  if (FAILED(hr)) {
    last_edit_error_ = hr;
    return false;
  }

  ComPtr<ITfRange> new_start;
  hr = range->Clone(&new_start);
  if (FAILED(hr) || !new_start ||
      FAILED(hr = new_start->Collapse(cookie, TF_ANCHOR_START))) {
    const HRESULT error = FAILED(hr) ? hr : TF_E_NOOBJECT;
    SetCompositionText(cookie, original);
    last_edit_error_ = error;
    return false;
  }
  LONG moved = 0;
  hr = new_start->ShiftStart(cookie, static_cast<LONG>(commit.size()), &moved, nullptr);
  if (FAILED(hr) || moved != static_cast<LONG>(commit.size())) {
    const HRESULT error = FAILED(hr) ? hr : E_FAIL;
    SetCompositionText(cookie, original);
    last_edit_error_ = error;
    return false;
  }

  hr = composition_->ShiftStart(cookie, new_start.Get());
  if (FAILED(hr)) {
    SetCompositionText(cookie, original);
    last_edit_error_ = hr;
    return false;
  }
  if (!SetSelectionToCompositionEnd(cookie)) {
    const HRESULT error = last_edit_error_;
    // Restore the old composition when the host rejects the caret update so
    // the caller can safely keep the old local preedit and retry.
    composition_->ShiftStart(cookie, original_start.Get());
    SetCompositionText(cookie, original);
    last_edit_error_ = error;
    return false;
  }
  if (!SetCompositionDisplayAttribute(cookie)) return false;
  return true;
}
bool TextService::EndComposition(TfEditCookie cookie) {
  if (!composition_) return true;
  ClearCompositionDisplayAttribute(cookie);
  const HRESULT result = composition_->EndComposition(cookie);
  if (SUCCEEDED(result)) {
    UnadviseLayoutSink();
    composition_.Reset();
    composition_context_.Reset();
  }
  if (FAILED(result)) last_edit_error_ = result;
  return SUCCEEDED(result);
}

bool TextService::SetCompositionDisplayAttribute(TfEditCookie cookie) {
  if (!composition_ || display_attribute_atom_ == TF_INVALID_GUIDATOM ||
      !composition_context_) {
    // A host may not expose GUID_PROP_ATTRIBUTE.  Composition text itself is
    // still valid, so treat the optional visual hint as best effort.
    return true;
  }
  ComPtr<ITfRange> range;
  if (FAILED(composition_->GetRange(&range)) || !range) return false;
  ComPtr<ITfProperty> property;
  if (FAILED(composition_context_->GetProperty(GUID_PROP_ATTRIBUTE, &property)) ||
      !property) {
    return true;
  }
  VARIANT value{};
  value.vt = VT_I4;
  value.lVal = static_cast<LONG>(display_attribute_atom_);
  const HRESULT hr = property->SetValue(cookie, range.Get(), &value);
  if (FAILED(hr)) {
    // Display attributes are a host-side decoration.  Do not turn a valid
    // composition update into a rejected key merely because this context does
    // not accept the optional property.
    return true;
  }
  return true;
}

void TextService::ClearCompositionDisplayAttribute(TfEditCookie cookie) {
  if (!composition_ || !composition_context_) return;
  ComPtr<ITfRange> range;
  if (FAILED(composition_->GetRange(&range)) || !range) return;
  ComPtr<ITfProperty> property;
  if (SUCCEEDED(composition_context_->GetProperty(GUID_PROP_ATTRIBUTE,
                                                  &property)) &&
      property) {
    property->Clear(cookie, range.Get());
  }
}
bool TextService::CancelComposition(ITfContext* context) {
  if (!composition_) {
    // There may still be a queued Update that has not created the composition
    // object yet.  Advance the generation so that callback becomes a no-op.
    ++edit_generation_;
    last_edit_pending_ = false;
    return true;
  }
  ITfContext* composition_owner = composition_context_ ? composition_context_.Get()
                                                        : context;
  if (!composition_owner || !RequestEdit(composition_owner, Action::Cancel, {})) {
    return false;
  }
  // A queued cancellation is not complete from the key sink's point of view.
  // The caller must keep the key consumed until the edit session has run.
  return !composition_;
}
bool TextService::SetSelectionToCompositionEnd(TfEditCookie cookie) {
  if (!composition_ || !composition_context_) return true;
  ComPtr<ITfRange> range;
  HRESULT hr = composition_->GetRange(&range);
  if (FAILED(hr) || !range) {
    last_edit_error_ = FAILED(hr) ? hr : TF_E_NOOBJECT;
    return false;
  }
  ComPtr<ITfRange> caret;
  hr = range->Clone(&caret);
  if (FAILED(hr) || !caret) {
    last_edit_error_ = FAILED(hr) ? hr : TF_E_NOOBJECT;
    return false;
  }
  hr = caret->Collapse(cookie, TF_ANCHOR_END);
  if (FAILED(hr)) {
    last_edit_error_ = hr;
    return false;
  }
  TF_SELECTION selection{caret.Get(), {TF_AE_NONE, FALSE}};
  hr = composition_context_->SetSelection(cookie, 1, &selection);
  if (FAILED(hr)) last_edit_error_ = hr;
  return SUCCEEDED(hr);
}

bool TextService::RefreshCandidateAnchor(TfEditCookie cookie) {
  if (!composition_ || !composition_context_) return false;
  ComPtr<ITfRange> range;
  if (FAILED(composition_->GetRange(&range)) || !range) return false;
  RECT anchor{};
  if (!ReadInputPosition(composition_context_.Get(), cookie, range.Get(), anchor)) {
    return false;
  }
  candidate_anchor_ = anchor;
  candidate_anchor_available_ = true;
  return true;
}

bool TextService::QueueCandidateAnchorRefresh(ITfContext* context,
                                              uint64_t generation,
                                              uint8_t attempt) {
  auto* session = new (std::nothrow)
      CandidateAnchorSession(this, context, generation, attempt);
  if (!session) return false;
  HRESULT result = E_FAIL;
  const HRESULT request = context->RequestEditSession(
      client_id_, session, TF_ES_READ | TF_ES_ASYNCDONTCARE, &result);
  session->Release();
  return SUCCEEDED(request) && SUCCEEDED(result);
}

void TextService::ClearCompositionState() {
  accessible_context_.reset();
  cancel_pending_ = false;
  last_edit_pending_ = false;
  ClearPendingPartialSelection();
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  preedit_.clear();
  preceding_preview_.clear();
  candidate_anchor_ = {};
  candidate_anchor_available_ = false;
  candidates_.clear();
  candidate_fetch_complete_ = false;
  active_input_request_id_ = 0;
  candidate_page_ = 0;
  selected_candidate_ = 0;
  EndCandidateUi();
  HideCandidates();
}
void TextService::HideCandidates() {
  if (candidate_ui_) candidate_ui_->SetShown(false);
  g_candidates.Hide();
}
