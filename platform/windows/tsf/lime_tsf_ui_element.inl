class CandidateUiElement final : public ITfIntegratableCandidateListUIElement,
                                 public ITfCandidateListUIElementBehavior {
 public:
  explicit CandidateUiElement(TextService* owner) : owner_(owner) {
    ++g_module_references;
  }
  ~CandidateUiElement() { --g_module_references; }
  void DetachOwner() { owner_ = nullptr; }

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (iid == IID_IUnknown || iid == IID_ITfIntegratableCandidateListUIElement) {
      *object = static_cast<ITfIntegratableCandidateListUIElement*>(this);
    } else if (iid == IID_ITfUIElement || iid == IID_ITfCandidateListUIElement ||
               iid == IID_ITfCandidateListUIElementBehavior) {
      *object = static_cast<ITfCandidateListUIElementBehavior*>(this);
    } else {
      return E_NOINTERFACE;
    }
    AddRef();
    return S_OK;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references_;
    if (!value) delete this;
    return value;
  }

  HRESULT STDMETHODCALLTYPE GetDescription(BSTR* description) override {
    if (!description) return E_POINTER;
    *description = SysAllocString(L"Lime Candidate List");
    return *description ? S_OK : E_OUTOFMEMORY;
  }
  HRESULT STDMETHODCALLTYPE GetGUID(GUID* guid) override {
    if (!guid) return E_POINTER;
    *guid = {0x05c2b076, 0x35ed, 0x4401,
             {0xb4, 0x1c, 0x38, 0x96, 0x54, 0x9e, 0x9a, 0x5f}};
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE Show(BOOL show) override {
    shown_ = show != FALSE;
    // The UI element manager may call Show while it is synchronizing the
    // element.  This is only the element's visibility state; it must not
    // overwrite the pbShow policy returned by BeginUIElement.  Weasel keeps
    // those two states separate as well.
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE IsShown(BOOL* show) override {
    if (!show) return E_POINTER;
    *show = shown_ ? TRUE : FALSE;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetUpdatedFlags(DWORD* flags) override {
    if (!flags) return E_POINTER;
    *flags = TF_CLUIE_DOCUMENTMGR | TF_CLUIE_COUNT | TF_CLUIE_SELECTION |
             TF_CLUIE_STRING | TF_CLUIE_CURRENTPAGE;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetDocumentMgr(ITfDocumentMgr** manager) override {
    if (!manager) return E_POINTER;
    *manager = nullptr;
    if (!owner_ || !owner_->thread_manager_) return E_FAIL;
    return owner_->thread_manager_->GetFocus(manager);
  }
  HRESULT STDMETHODCALLTYPE GetCount(UINT* count) override {
    if (!count) return E_POINTER;
    *count = static_cast<UINT>(candidates_.size());
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetSelection(UINT* index) override {
    if (!index) return E_POINTER;
    *index = static_cast<UINT>(selected_);
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetString(UINT index, BSTR* value) override {
    if (!value) return E_POINTER;
    *value = nullptr;
    if (index >= candidates_.size()) return E_INVALIDARG;
    *value = SysAllocStringLen(candidates_[index].c_str(),
                               static_cast<UINT>(candidates_[index].size()));
    return *value ? S_OK : E_OUTOFMEMORY;
  }
  HRESULT STDMETHODCALLTYPE GetPageIndex(UINT* indices, UINT size,
                                          UINT* page_count) override {
    if (!page_count) return E_POINTER;
    // Keep one logical page even while the snapshot is empty, matching
    // Weasel's candidate-list contract and avoiding a special empty-list
    // shape during the initial UI-element handshake.
    *page_count = 1;
    if (!indices) return S_OK;
    if (size < 1 || *page_count != 1) return E_INVALIDARG;
    indices[0] = 0;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE SetPageIndex(UINT* indices, UINT page_count) override {
    if (!indices && page_count != 0) return E_POINTER;
    if (page_count == 0) return S_OK;
    return page_count == 1 && indices[0] == 0 ? S_OK : E_INVALIDARG;
  }
  HRESULT STDMETHODCALLTYPE GetCurrentPage(UINT* page) override {
    if (!page) return E_POINTER;
    // Like Weasel, expose the current page as a single native page.  The
    // popup itself owns paging; integrated hosts see only this page snapshot.
    *page = 0;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE SetSelection(UINT index) override {
    if (!owner_ || candidates_.empty()) return E_INVALIDARG;
    if (index >= candidates_.size()) return E_INVALIDARG;
    const size_t absolute = candidate_begin_ + index;
    owner_->selected_candidate_ = absolute;
    selected_ = index;
    owner_->UpdateCandidateUi();
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE Finalize() override {
    if (!owner_ || candidates_.empty()) return E_FAIL;
    ITfContext* context = owner_->active_context_.Get();
    return owner_->SelectCandidate(context, candidate_begin_ + selected_) ? S_OK : E_FAIL;
  }
  HRESULT STDMETHODCALLTYPE Abort() override {
    if (!owner_) return E_FAIL;
    return owner_->CancelComposition(owner_->active_context_.Get()) ? S_OK : E_FAIL;
  }

  HRESULT STDMETHODCALLTYPE SetIntegrationStyle(GUID) override { return S_OK; }
  HRESULT STDMETHODCALLTYPE GetSelectionStyle(
      TfIntegratableCandidateListSelectionStyle* style) override {
    if (!style) return E_POINTER;
    *style = STYLE_ACTIVE_SELECTION;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE OnKeyDown(WPARAM wParam, LPARAM, BOOL* eaten) override {
    if (!eaten) return E_POINTER;
    *eaten = FALSE;
    if (owner_ && owner_->active_context_) {
      // Integrated hosts send navigation through the UI element instead of
      // the key sink.  Reuse the same key path so number selection, paging,
      // commit and cancel retain identical behavior.
      *eaten = owner_->HandleKey(owner_->active_context_.Get(), wParam) ? TRUE : FALSE;
    }
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE ShowCandidateNumbers(BOOL* show) override {
    if (!show) return E_POINTER;
    *show = TRUE;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE FinalizeExactCompositionString() override {
    return E_NOTIMPL;
  }

  void SetSnapshot(const std::vector<TextService::Candidate>& candidates,
                   size_t page, size_t selected, size_t page_size) {
    candidates_.clear();
    page_size_ = std::max<size_t>(1, page_size);
    candidate_begin_ = candidates.empty()
                           ? 0
                           : (std::min)(page * page_size_, candidates.size() - 1);
    const size_t end = (std::min)(candidates.size(), candidate_begin_ + page_size_);
    candidates_.reserve(end - candidate_begin_);
    for (size_t index = candidate_begin_; index < end; ++index)
      candidates_.push_back(candidates[index].display);
    selected_ = selected >= candidate_begin_ && selected < end
                    ? selected - candidate_begin_
                    : 0;
  }

  bool started() const { return started_; }
  void SetShown(bool shown) { shown_ = shown; }
  void set_started(bool value, DWORD id = 0) {
    started_ = value;
    ui_id_ = id;
  }
  DWORD ui_id() const { return ui_id_; }
  bool external_show() const { return external_show_; }
  void set_external_show(bool value) { external_show_ = value; }

 private:
  std::atomic<ULONG> references_{1};
  TextService* owner_ = nullptr;
  std::vector<std::wstring> candidates_;
  size_t candidate_begin_ = 0;
  size_t selected_ = 0;
  size_t page_size_ = 9;
  DWORD ui_id_ = 0;
  bool started_ = false;
  bool shown_ = false;
  bool external_show_ = true;
};
