// Focus-loss cleanup owns the old objects, just as Weasel's end-composition
// session does. It must not depend on the next editor's input generation.
struct DetachedCompositionEndSession final : ITfEditSession {
  std::atomic<ULONG> references{1};
  ComPtr<ITfContext> context;
  ComPtr<ITfComposition> composition;
  bool queued = false;
  bool finished = false;
  DetachedCompositionEndSession(ITfContext* ctx, ITfComposition* value)
      : context(ctx), composition(value) { ++g_module_references; }
  ~DetachedCompositionEndSession() { --g_module_references; }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** out) override {
    if (!out) return E_POINTER;
    *out = nullptr;
    if (iid != IID_IUnknown && iid != IID_ITfEditSession) return E_NOINTERFACE;
    *out = static_cast<ITfEditSession*>(this);
    AddRef();
    return S_OK;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references;
    if (!value) delete this;
    return value;
  }
  HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie cookie) override {
    queued = false;
    if (finished) return S_OK;
    ComPtr<ITfRange> range;
    HRESULT result = composition->GetRange(&range);
    if (SUCCEEDED(result) && range) {
      ComPtr<ITfProperty> property;
      if (SUCCEEDED(context->GetProperty(GUID_PROP_ATTRIBUTE, &property)) &&
          property) property->Clear(cookie, range.Get());
      result = range->SetText(cookie, 0, L"", 0);
    }
    const HRESULT ended = composition->EndComposition(cookie);
    finished = SUCCEEDED(ended);
    return FAILED(ended) ? ended : result;
  }
};

struct CandidateAnchorSession final : ITfEditSession {
  std::atomic<ULONG> references{1};
  TextService* owner;
  ComPtr<ITfContext> context;
  uint64_t generation;
  uint8_t attempt;
  CandidateAnchorSession(TextService* service, ITfContext* ctx, uint64_t gen,
                         uint8_t refresh_attempt)
      : owner(service), context(ctx), generation(gen), attempt(refresh_attempt) {
    owner->AddRef();
  }
  ~CandidateAnchorSession() { owner->Release(); }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** out) override {
    if (!out) return E_POINTER;
    *out = nullptr;
    if (iid != IID_IUnknown && iid != IID_ITfEditSession) return E_NOINTERFACE;
    *out = static_cast<ITfEditSession*>(this);
    AddRef();
    return S_OK;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references;
    if (!value) delete this;
    return value;
  }
  HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie cookie) override {
    if (!owner->IsEditCurrent(generation) ||
        owner->composition_context_.Get() != context.Get()) return S_OK;
    const bool refreshed = owner->RefreshCandidateAnchor(cookie);
    // The first read can run before the host has laid out the newly extended
    // composition.  Give the view one more read-only edit session before
    // publishing the popup.  If the retry cannot be queued, retain a valid
    // first result and fall back only when both reads failed.
    if (attempt == 0 &&
        owner->QueueCandidateAnchorRefresh(context.Get(), generation, 1)) {
      return S_OK;
    }
    if (!refreshed) {
      owner->candidate_anchor_ = {};
      owner->candidate_anchor_available_ = false;
    }
    owner->ShowCandidates(context.Get());
    return S_OK;
  }
};
