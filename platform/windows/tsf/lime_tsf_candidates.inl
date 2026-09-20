bool TextService::SelectCandidate(ITfContext* context, size_t index) {
  if (!context || index >= candidates_.size()) return false;

  const Candidate candidate = candidates_[index];
  const std::wstring pinyin = preedit_;
  if (candidate.remainder_available && !candidate.remainder.empty()) {
    std::vector<Candidate> next_candidates;
    std::wstring next_preceding;
    bool next_context_available = false;
    RECT next_anchor{};
    bool next_anchor_available = false;
    const bool fetched = FetchCandidates(
        context, candidate.remainder, next_candidates, next_preceding,
        next_context_available, next_anchor, next_anchor_available, page_size_,
        candidate.commit);

    pending_partial_pinyin_ = ConsumedPinyin(pinyin, candidate.remainder);
    pending_partial_commit_ = candidate.commit;
    pending_partial_remainder_ = candidate.remainder;
    pending_partial_candidates_ = std::move(next_candidates);
    pending_partial_preceding_ = std::move(next_preceding);
    pending_partial_anchor_ = next_anchor;
    pending_partial_anchor_available_ = next_anchor_available;
    pending_partial_fetch_complete_ =
        fetched && pending_partial_candidates_.size() < page_size_;

    // Keep the unconsumed Rime input in the same TSF composition.  The
    // prefix is made ordinary text by moving the composition start forward;
    // this is the TSF equivalent of Weasel committing a selected segment and
    // recomposing the remaining raw pinyin.
    if (!RequestEdit(context, Action::CommitPartial, candidate.commit, false,
                     candidate.remainder)) {
      return false;
    }
    active_input_request_id_ = request_id_;
    if (partial_edit_pending_) HideCandidates();
    return true;
  }

  if (!RequestEdit(context, Action::Commit, candidate.commit)) return false;
  LearnCandidate(pinyin, candidate.commit);
  if (terminal_edit_pending_) {
    // Keep the local snapshot until the asynchronous commit callback has
    // actually ended the TSF composition, but remove the stale popup and
    // consume subsequent keys in OnTestKeyDown.
    HideCandidates();
  } else {
    ClearCompositionState();
  }
  return true;
}

bool TextService::HandleKey(ITfContext* context, WPARAM key) {
  // Persistent ASCII mode deliberately leaves ordinary keys to the host, as
  // Weasel does when its ascii_composer has no active composition.  This
  // preserves the user's Windows keyboard layout and all half-width symbols.
  if (ascii_mode_ &&
      !(key == VK_ESCAPE &&
        (composition_ || !preedit_.empty() || terminal_edit_pending_ ||
         partial_edit_pending_))) {
    return false;
  }

  // Rime's numeric punctuation rule depends on the immediately preceding
  // key event, rather than the text before the caret.  Remember only an
  // unmodified digit that is going to the host; candidate-number shortcuts
  // are consumed by Lime and therefore clear the flag.  Every other key
  // clears it, while a period keeps it alive until punctuation conversion
  // below has observed the value.
  const bool period_key = !HasNonTextModifier() && AsciiText(key) == L".";
  const bool period_after_digit = period_key && last_chinese_input_was_digit_;
  if (period_key) last_chinese_input_was_digit_ = false;
  if (IsDigitKey(key) && candidates_.empty()) {
    last_chinese_input_was_digit_ = true;
  } else if (!period_key) {
    last_chinese_input_was_digit_ = false;
  }

  // Esc must remain a local cancellation even while a schema reset is waiting
  // on a rejected edit lock; otherwise the host could receive it with a live
  // composition still attached.
  if (schema_reset_pending_ && key != VK_ESCAPE &&
      !ResetCompositionForSchemaChange(context)) {
    if (period_key) last_chinese_input_was_digit_ = false;
    return false;
  }
  if ((GetKeyState(VK_SHIFT) & 0x8000) != 0 && key == VK_SPACE) {
    // Match ascii_composer's explicit Shift+Space no-op.  In particular, do
    // not accidentally turn it into a persistent mode switch or an IME-managed
    // space while the user is holding Shift for another host gesture.
    return false;
  }
  // A bare Space with no active composition is left to the host, matching
  // Weasel's key sink so media controls (such as video play/pause) continue
  // to work in Chinese mode.  Space remains Lime-owned for candidate
  // selection or raw-preedit commit below.
  if (key == VK_SPACE && !composition_ && preedit_.empty() &&
      candidates_.empty()) {
    return false;
  }
  if (IsPreeditKey(key)) {
    wchar_t value[2] = {PreeditChar(key), 0};
    preedit_ += value;
    const std::wstring attempted_preedit = preedit_;
    // RequestEdit advances the generation before entering host callbacks.
    const uint64_t expected_edit_generation = edit_generation_ + 1;
    if (!UpdateCandidates(context)) {
      // Keep the key consumed while the service is available. Retain failed
      // edits for retry, except input rejected by a read-only context.
      if (connected_) {
        if (!last_fetch_failed_ && last_edit_error_ == TF_E_READONLY &&
            edit_generation_ == expected_edit_generation &&
            preedit_ == attempted_preedit && !preedit_.empty()) {
          // Discard only this character. A reentrant focus callback may have
          // already cleared or replaced it, even with the same text.
          preedit_.pop_back();
        }
        const std::wstring reason =
            last_fetch_failed_
                ? L"候选获取失败"
                : (last_edit_error_ == S_OK ? L"编辑器拒绝组合串"
                                            : EditErrorText(last_edit_error_));
        g_candidates.ShowStatus(context, std::wstring(L"Lime：") + reason);
        return true;
      }
      preedit_.pop_back();
      return false;
    }
    if (candidates_.empty()) {
      // An empty Rime result is still a valid composition state. Keep the raw
      // pinyin in the TSF composition, hide the candidate UI, and let Enter or
      // Space commit it explicitly. Cancelling here would forward only the
      // latest key to the host (for example, the final `a` in `uia`).
      HideCandidates();
    }
    return true;
  }
  if (key == VK_BACK && !preedit_.empty()) {
    const wchar_t removed = preedit_.back();
    preedit_.pop_back();
    if (preedit_.empty()) {
      if (CancelComposition(context)) {
        ClearCompositionState();
        composition_context_.Reset();
        return true;
      }
      // The key has already been claimed by OnTestKeyDown.  Keep it consumed
      // while the queued cancellation finishes instead of letting Backspace
      // delete confirmed host text; restore the in-flight composition state
      // until ResolvePendingCancellation can clear it safely.
      preedit_.push_back(removed);
      cancel_pending_ = true;
      HideCandidates();
      return true;
    }
    if (!UpdateCandidates(context)) preedit_.push_back(removed);
    return true;
  }
  if ((key >= '1' && key <= '9') && !candidates_.empty()) {
    const size_t index = candidate_page_ * page_size_ + (key - '1');
    if (index >= candidates_.size()) return true;
    selected_candidate_ = index;
    if (!SelectCandidate(context, index)) {
      // The probe already told the host that this key belongs to Lime.  Keep
      // it consumed when TSF rejects the edit; forwarding it would insert a
      // digit into the host while the old composition is still active.
      ShowCandidates(context);
      return true;
    }
    return true;
  }
  if (key == VK_RETURN) {
    if (preedit_.empty()) return false;
    // Enter confirms exactly what the user typed.  Candidate selection is
    // intentionally reserved for Space and the numbered shortcuts.
    if (!RequestEdit(context, Action::Commit, preedit_)) {
      const std::wstring reason = last_edit_error_ == S_OK
                                      ? L"提交失败"
                                      : EditErrorText(last_edit_error_);
      g_candidates.ShowStatus(context, std::wstring(L"Lime：") + reason);
      return true;
    }
    if (terminal_edit_pending_) {
      HideCandidates();
    } else {
      ClearCompositionState();
    }
    return true;
  }
  if (key == VK_SPACE) {
    if (!candidates_.empty()) {
      const size_t index = std::min(selected_candidate_, candidates_.size() - 1);
      selected_candidate_ = index;
      if (!SelectCandidate(context, index)) {
        ShowCandidates(context);
        return true;
      }
      return true;
    }
    if (!preedit_.empty()) {
      if (!RequestEdit(context, Action::Commit, preedit_)) {
        const std::wstring reason = last_edit_error_ == S_OK
                                        ? L"提交失败"
                                        : EditErrorText(last_edit_error_);
        g_candidates.ShowStatus(context, std::wstring(L"Lime：") + reason);
        return true;
      }
      if (terminal_edit_pending_) {
        HideCandidates();
      } else {
        ClearCompositionState();
      }
      return true;
    }
  }
  if (key == VK_ESCAPE &&
      (!preedit_.empty() || composition_ || terminal_edit_pending_)) {
    if (!CancelComposition(context)) {
      // Esc is a cancellation request, not text input.  Keep it consumed even
      // when TSF queues/rejects the edit session, and block subsequent keys
      // until ResolvePendingCancellation observes the completed callback.
      cancel_pending_ = true;
      HideCandidates();
      return true;
    }
    ClearCompositionState();
    composition_context_.Reset();
    return true;
  }
  const bool previous_page = IsPreviousPageKey(key);
  const bool next_page = IsNextPageKey(key);
  if (previous_page || next_page) {
    if (candidates_.empty()) return false;
    if (previous_page && candidate_page_ > 0) {
      --candidate_page_;
      selected_candidate_ = candidate_page_ * page_size_;
      ShowCandidates(context);
    } else if (next_page) {
      const size_t next_begin = (candidate_page_ + 1) * page_size_;
      if (next_begin >= candidates_.size()) {
        LoadMoreCandidates(context, next_begin + page_size_);
      }
      if (next_begin < candidates_.size()) {
        ++candidate_page_;
        selected_candidate_ = candidate_page_ * page_size_;
        ShowCandidates(context);
      }
    }
    // Page keys are owned by the candidate window even at the ends; treating
    // the boundary as a no-op prevents the host from scrolling unexpectedly.
    return true;
  }
  if (key == VK_UP || key == VK_DOWN) {
    if (candidates_.empty()) return false;
    const size_t begin = candidate_page_ * page_size_;
    const size_t end = (std::min)(candidates_.size(), begin + page_size_);
    if (begin >= end) return true;
    if (selected_candidate_ < begin || selected_candidate_ >= end) {
      selected_candidate_ = begin;
    }
    if (key == VK_UP) {
      if (selected_candidate_ > begin) {
        --selected_candidate_;
      } else if (candidate_page_ > 0) {
        --candidate_page_;
        const size_t previous_begin = candidate_page_ * page_size_;
        const size_t previous_end =
            (std::min)(candidates_.size(), previous_begin + page_size_);
        selected_candidate_ = previous_end ? previous_end - 1 : previous_begin;
      }
    } else if (selected_candidate_ + 1 < end) {
      ++selected_candidate_;
    } else {
      const size_t next_begin = (candidate_page_ + 1) * page_size_;
      if (next_begin >= candidates_.size()) {
        LoadMoreCandidates(context, next_begin + page_size_);
      }
      if (next_begin < candidates_.size()) {
        ++candidate_page_;
        selected_candidate_ = candidate_page_ * page_size_;
      }
    }
    ShowCandidates(context);
    return true;
  }
  if (IsChinesePunctuationKey(key)) {
    // Rime's punctuator commits a pending composition before emitting a
    // punctuation symbol.  Keep that behavior in the snapshot-based adapter;
    // when there is no composition, create and immediately finalize a TSF
    // composition instead of directly mutating the host selection.
    // Punctuation has no input request that could refresh the connection, so
    // re-check the service state before claiming the key.  This closes the
    // small window where the process went unavailable after the last letter.
    RefreshConfigRevision(context);
    if (!connected_) {
      if (period_key) last_chinese_input_was_digit_ = false;
      return false;
    }
    const bool previous_single_quote = single_quote_open_;
    const bool previous_double_quote = double_quote_open_;
    const std::wstring punctuation =
        ChinesePunctuationText(key, period_after_digit);
    // The numeric-period exception applies to one punctuation event only,
    // including when the edit later fails or the service becomes unavailable.
    last_chinese_input_was_digit_ = false;
    if (punctuation.empty()) return false;

    std::wstring pinyin;
    std::wstring commit = punctuation;
    if (!preedit_.empty() || composition_) {
      pinyin = preedit_;
      if (!candidates_.empty()) {
        const size_t index = std::min(selected_candidate_, candidates_.size() - 1);
        commit = candidates_[index].commit + punctuation;
      } else if (!preedit_.empty()) {
        commit = preedit_ + punctuation;
      }
    }
    if (!RequestEdit(context, Action::Commit, commit)) {
      single_quote_open_ = previous_single_quote;
      double_quote_open_ = previous_double_quote;
      g_candidates.ShowStatus(context, L"Lime：符号输入失败");
      return true;
    }
    if (!pinyin.empty() && !candidates_.empty()) {
      LearnCandidate(pinyin, commit.substr(0, commit.size() - punctuation.size()));
    }
    if (terminal_edit_pending_) {
      HideCandidates();
    } else {
      ClearCompositionState();
    }
    return true;
  }
  return false;
}

bool TextService::FetchCandidates(ITfContext* context, const std::wstring& preedit,
                                   std::vector<Candidate>& result_candidates,
                                   std::wstring& preceding, bool& context_available,
                                   RECT& anchor, bool& anchor_available,
                                   size_t candidate_limit,
                                   std::wstring_view preceding_suffix,
                                   uint64_t candidate_extension_of) {
  if (!context) return false;
  result_candidates.clear();
  preceding.clear();
  context_available = false;
  anchor = {};
  anchor_available = false;
  if ((composition_context_ && composition_context_.Get() != context) ||
      (!composition_context_ && active_context_ && active_context_.Get() != context)) {
    accessible_context_.reset();
  }
  HWND view_window = nullptr;
  ComPtr<ITfContextView> active_view;
  if (SUCCEEDED(context->GetActiveView(&active_view)) && active_view)
    active_view->GetWnd(&view_window);
  // Read context in a read-only edit session, then synchronously ask the local service.
  class ReadSession final : public ITfEditSession {
   public:
    std::atomic<ULONG> refs{1};
    TextService* owner;
    ComPtr<ITfContext> ctx;
    ComPtr<ITfComposition> composition;
    std::wstring* before;
    bool* available;
    RECT* anchor;
    bool* anchor_available;

    ReadSession(TextService* o, ITfContext* c, ITfComposition* comp,
                std::wstring* b, bool* a, RECT* r, bool* r_available)
        : owner(o),
          ctx(c),
          composition(comp),
          before(b),
          available(a),
          anchor(r),
          anchor_available(r_available) {
      owner->AddRef();
    }
    ~ReadSession() { owner->Release(); }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** p) override {
      if (!p) return E_POINTER;
      *p = nullptr;
      if (iid != IID_IUnknown && iid != IID_ITfEditSession)
        return E_NOINTERFACE;
      *p = static_cast<ITfEditSession*>(this);
      AddRef();
      return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return ++refs; }
    ULONG STDMETHODCALLTYPE Release() override {
      const ULONG value = --refs;
      if (!value) delete this;
      return value;
    }
    HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie c) override {
      ComPtr<ITfRange> composition_range;
      if (composition) composition->GetRange(&composition_range);
      *available = ReadPrecedingRange(ctx.Get(), c, owner->ContextLimit(),
                                      *before, composition_range.Get());
      if (!*available) before->clear();
      *anchor_available = ReadInputPosition(ctx.Get(), c,
                                             composition_range.Get(), *anchor);
      return S_OK;
    }
  } session(this, context, composition_.Get(), &preceding, &context_available,
            &anchor, &anchor_available);
  HRESULT result = E_FAIL, request = context->RequestEditSession(client_id_, &session, TF_ES_READ | TF_ES_SYNC, &result);
  if (FAILED(request) || FAILED(result)) {
    preceding.clear();
    context_available = false;
    anchor = {};
    anchor_available = false;
  }
  if (preceding.empty() || !context_available) {
    if (!accessible_context_)
      accessible_context_ = std::make_shared<AccessibleContextState>();
    if (ReadUiAutomationPreceding(accessible_context_, view_window,
                                  ContextLimit(), preceding)) {
      context_available = true;
    }
  }
  if (context_available && !preceding_suffix.empty()) {
    preceding.append(preceding_suffix);
    if (preceding.size() > ContextLimit()) {
      preceding.erase(0, preceding.size() - ContextLimit());
    }
  }
  const uint64_t requested_limit = (std::min)(
      static_cast<uint64_t>(candidate_limit),
      static_cast<uint64_t>(std::numeric_limits<uint32_t>::max()));
  const uint64_t request_id = ++request_id_;
  const std::string extension_field =
      candidate_extension_of == 0
          ? std::string()
          : ",\"candidate_extension_of\":" + std::to_string(candidate_extension_of);
  std::string body;
  const std::string json = "{\"kind\":\"input\",\"payload\":{\"request_id\":" +
                           std::to_string(request_id) +
                           ",\"preedit\":\"" + JsonEscape(preedit) +
                           "\",\"preceding_text\":\"" + JsonEscape(preceding) +
                           "\",\"context_available\":" +
                           (context_available ? "true" : "false") +
                           ",\"config_revision\":" + std::to_string(config_revision_) +
                           ",\"candidate_limit\":" + std::to_string(requested_limit) +
                           extension_field +
                           "}}";
  if (!g_pipe.Request(json, body)) {
    connected_ = false;
    return false;
  }
  if (body.find("\"kind\":\"error\"") != std::string::npos) {
    RefreshConfigRevision(context);
    return false;
  }
  std::string service_state;
  if (!JsonField(body, "service_state", service_state) ||
      (service_state != "ready" && service_state != "rime_only")) {
    connected_ = false;
    return false;
  }
  connected_ = true;
  passthrough_notified_ = false;
  const std::string candidates_needle = "\"candidates\":[";
  const size_t array = body.find(candidates_needle);
  if (array == std::string::npos) return false;
  const size_t array_end = JsonArrayEnd(body, array + candidates_needle.size() - 1);
  if (array_end == std::string::npos) return false;
  const std::vector<std::optional<std::string>> remainders =
      JsonOptionalStringArray(body, "candidate_remainders");
  size_t pos = array + candidates_needle.size();
  size_t candidate_index = 0;
  while (pos < array_end) {
    const size_t d = body.find("\"display_text\":\"", pos);
    if (d == std::string::npos) break;
    const size_t c = body.find("\"commit_text\":\"", d);
    if (c == std::string::npos) break;
    if (d >= array_end || c >= array_end) break;
    Candidate candidate;
    candidate.display = Wide(JsonString(body, d + 16));
    candidate.commit = Wide(JsonString(body, c + 15));
    if (candidate_index < remainders.size() && remainders[candidate_index]) {
      candidate.remainder = Wide(*remainders[candidate_index]);
      candidate.remainder_available = true;
    }
    result_candidates.push_back(std::move(candidate));
    ++candidate_index;
    pos = c + 15;
  }
  return true;
}

bool TextService::LoadMoreCandidates(ITfContext* context, size_t required_count) {
  if (candidate_fetch_complete_ || active_input_request_id_ == 0) return false;

  std::vector<Candidate> fetched;
  std::wstring preceding;
  bool context_available = false;
  RECT anchor{};
  bool anchor_available = false;
  if (!FetchCandidates(context, preedit_, fetched, preceding, context_available, anchor,
                       anchor_available, required_count, {}, active_input_request_id_)) {
    return false;
  }
  if (fetched.size() <= candidates_.size()) {
    candidate_fetch_complete_ = true;
    return false;
  }

  candidates_ = std::move(fetched);
  preceding_preview_ = PreviewText(preceding, context_preview_limit_);
  candidate_anchor_ = anchor;
  candidate_anchor_available_ = anchor_available;
  if (candidates_.size() < required_count) candidate_fetch_complete_ = true;
  return true;
}

bool TextService::UpdateCandidates(ITfContext* context) {
  if (!context) return false;
  candidate_fetch_complete_ = false;
  last_fetch_failed_ = false;
  std::wstring preceding;
  bool context_available = false;
  RECT anchor{};
  bool anchor_available = false;
  if (!FetchCandidates(context, preedit_, candidates_, preceding, context_available,
                       anchor, anchor_available, page_size_)) {
    last_fetch_failed_ = true;
    candidates_.clear();
    preceding_preview_.clear();
    candidate_anchor_ = {};
    candidate_anchor_available_ = false;
    HideCandidates();
    return false;
  }
  candidate_anchor_ = anchor;
  candidate_anchor_available_ = anchor_available;
  preceding_preview_ = PreviewText(preceding, context_preview_limit_);
  candidate_page_ = 0;
  selected_candidate_ = 0;
  if (!RequestEdit(context, Action::Update, preedit_)) {
    candidates_.clear();
    preceding_preview_.clear();
    candidate_anchor_ = {};
    candidate_anchor_available_ = false;
    HideCandidates();
    return false;
  }
  active_input_request_id_ = request_id_;
  // The popup is published from CompleteEditSession after the composition
  // range has been created and its screen extent can be read.  Showing it
  // here would position the first-key popup from the previous caret.
  return true;
}
