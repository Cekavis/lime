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
