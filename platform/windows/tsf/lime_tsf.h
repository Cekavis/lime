#pragma once

#include <windows.h>
#include <msctf.h>
#include <wrl/client.h>

#include <atomic>
#include <cstdint>
#include <memory>
#include <string>
#include <string_view>
#include <vector>

namespace lime::tsf {

extern const CLSID kClsid;
extern const GUID kProfileGuid;
extern const LANGID kLanguageId;
extern HINSTANCE g_instance;
extern std::atomic<long> g_module_references;

class CandidateUiElement;
struct CandidateAnchorSession;
struct DetachedCompositionEndSession;
struct AccessibleContextState;

class TextService final : public ITfTextInputProcessorEx,
                          public ITfThreadMgrEventSink,
                          public ITfKeyEventSink,
                          public ITfThreadFocusSink,
                          public ITfCompositionSink,
                          public ITfTextEditSink,
                          public ITfDisplayAttributeProvider,
                          public ITfTextLayoutSink {
 public:
  struct Candidate {
    std::wstring display;
    std::wstring commit;
    std::wstring remainder;
    bool remainder_available = false;
  };
  enum class Action { Update, Commit, CommitPartial, Cancel };

  TextService();
  ~TextService();

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override;
  ULONG STDMETHODCALLTYPE AddRef() override;
  ULONG STDMETHODCALLTYPE Release() override;
  HRESULT STDMETHODCALLTYPE Activate(ITfThreadMgr* thread_manager, TfClientId client_id) override;
  HRESULT STDMETHODCALLTYPE Deactivate() override;
  HRESULT STDMETHODCALLTYPE ActivateEx(ITfThreadMgr* thread_manager, TfClientId client_id,
                                        DWORD flags) override;

  // ITfThreadMgrEventSink
  HRESULT STDMETHODCALLTYPE OnInitDocumentMgr(ITfDocumentMgr* document_manager) override;
  HRESULT STDMETHODCALLTYPE OnUninitDocumentMgr(ITfDocumentMgr* document_manager) override;
  HRESULT STDMETHODCALLTYPE OnSetFocus(ITfDocumentMgr* focused_document_manager,
                                        ITfDocumentMgr* previous_document_manager) override;
  HRESULT STDMETHODCALLTYPE OnPushContext(ITfContext* context) override;
  HRESULT STDMETHODCALLTYPE OnPopContext(ITfContext* context) override;

  HRESULT STDMETHODCALLTYPE OnSetFocus(BOOL foreground) override;
  HRESULT STDMETHODCALLTYPE OnTestKeyDown(ITfContext* context, WPARAM key, LPARAM lparam,
                                          BOOL* eaten) override;
  HRESULT STDMETHODCALLTYPE OnTestKeyUp(ITfContext* context, WPARAM key, LPARAM lparam,
                                        BOOL* eaten) override;
  HRESULT STDMETHODCALLTYPE OnKeyDown(ITfContext* context, WPARAM key, LPARAM lparam,
                                      BOOL* eaten) override;
  HRESULT STDMETHODCALLTYPE OnKeyUp(ITfContext* context, WPARAM key, LPARAM lparam,
                                    BOOL* eaten) override;
  HRESULT STDMETHODCALLTYPE OnPreservedKey(ITfContext* context, REFGUID guid, BOOL* eaten) override;

  // ITfThreadFocusSink
  HRESULT STDMETHODCALLTYPE OnSetThreadFocus() override;
  HRESULT STDMETHODCALLTYPE OnKillThreadFocus() override;

  HRESULT STDMETHODCALLTYPE OnCompositionTerminated(TfEditCookie cookie,
                                                     ITfComposition* composition) override;
  HRESULT STDMETHODCALLTYPE OnEndEdit(ITfContext* context,
                                      TfEditCookie read_only_cookie,
                                      ITfEditRecord* edit_record) override;
  HRESULT STDMETHODCALLTYPE OnLayoutChange(ITfContext* context,
                                            TfLayoutCode code,
                                            ITfContextView* view) override;
  HRESULT STDMETHODCALLTYPE EnumDisplayAttributeInfo(
      IEnumTfDisplayAttributeInfo** enumerator) override;
  HRESULT STDMETHODCALLTYPE GetDisplayAttributeInfo(
      REFGUID guid, ITfDisplayAttributeInfo** info) override;

  bool EnsureComposition(ITfContext* context, TfEditCookie cookie);
  bool SetCompositionText(TfEditCookie cookie, const std::wstring& text);
  bool CommitComposition(TfEditCookie cookie, const std::wstring& text);
  bool CommitPartialComposition(TfEditCookie cookie, const std::wstring& commit,
                                const std::wstring& remainder);
  bool EndComposition(TfEditCookie cookie);
  bool IsEditCurrent(uint64_t generation) const { return generation == edit_generation_; }
  void CompleteEditSession(Action action, uint64_t generation, bool succeeded);
  uint32_t ContextLimit() const { return context_limit_; }
#ifdef LIME_TSF_TESTS
  uint8_t ShiftDownMaskForTest() const { return shift_down_mask_; }
  uint8_t ShiftPendingMaskForTest() const { return shift_pending_mask_; }
  bool AsciiModeForTest() const { return ascii_mode_; }
  const std::wstring& PreeditForTest() const { return preedit_; }
  bool CompositionActiveForTest() const { return composition_ != nullptr; }
  bool TerminalEditPendingForTest() const { return terminal_edit_pending_; }
#endif

 private:
  friend class CandidateUiElement;
  friend struct CandidateAnchorSession;

  bool HandleKey(ITfContext* context, WPARAM key);
  bool IsKeyboardDisabled(ITfContext* context) const;
  bool ToggleAsciiMode(ITfContext* context);
  bool IsImeKey(WPARAM key) const;
  bool IsPrintable(WPARAM key) const;
  bool IsPreeditKey(WPARAM key) const;
  bool IsChinesePunctuationKey(WPARAM key) const;
  bool IsDigitKey(WPARAM key) const;
  std::wstring AsciiText(WPARAM key) const;
  std::wstring ChinesePunctuationText(WPARAM key, bool previous_digit = false);
  wchar_t PreeditChar(WPARAM key) const;
  bool FetchCandidates(ITfContext* context, const std::wstring& preedit,
                       std::vector<Candidate>& candidates, std::wstring& preceding,
                       bool& context_available, RECT& anchor,
                       bool& anchor_available, size_t candidate_limit,
                       std::wstring_view preceding_suffix = {},
                       uint64_t candidate_extension_of = 0);
  bool LoadMoreCandidates(ITfContext* context, size_t required_count);
  bool UpdateCandidates(ITfContext* context);
  bool SelectCandidate(ITfContext* context, size_t index);
  void RefreshConfigRevision(ITfContext* context = nullptr);
  void PrimeFocusedUiAutomation();
  bool ResetCompositionForSchemaChange(ITfContext* context);
  void LearnCandidate(std::wstring_view pinyin, std::wstring_view text);
  void ClearPendingPartialSelection();
  bool ResolvePendingCancellation();
  void DropUnavailableContext();
  void QueueDetachedCompositionEnd(ITfContext* context,
                                   ITfComposition* composition);
  void RetryDetachedCompositionEnds();
  void UnadviseThreadSinks();
  void AdviseTextEditSink(ITfContext* context);
  void UnadviseTextEditSink();
  bool RequestEdit(ITfContext* context, Action action, const std::wstring& text,
                   bool synchronous = false,
                   const std::wstring& remainder = {});
  bool CancelComposition(ITfContext* context);
  bool SetSelectionToCompositionEnd(TfEditCookie cookie);
  bool SetCompositionDisplayAttribute(TfEditCookie cookie);
  void ClearCompositionDisplayAttribute(TfEditCookie cookie);
  bool RefreshCandidateAnchor(TfEditCookie cookie);
  bool QueueCandidateAnchorRefresh(ITfContext* context, uint64_t generation,
                                   uint8_t attempt = 0);
  void AdviseLayoutSink(ITfContext* context);
  void UnadviseLayoutSink();
  void InitializeDisplayAttribute();
  bool BeginCandidateUi();
  void UpdateCandidateUi();
  void EndCandidateUi();
  void ShowCandidates(ITfContext* context);
  void ClearCompositionState();
  void HideCandidates();
  const RECT* CandidateAnchor() const {
    return candidate_anchor_available_ ? &candidate_anchor_ : nullptr;
  }

  std::atomic<ULONG> references_{1};
  Microsoft::WRL::ComPtr<ITfThreadMgr> thread_manager_;
  Microsoft::WRL::ComPtr<ITfKeystrokeMgr> keystroke_manager_;
  TfClientId client_id_ = TF_CLIENTID_NULL;
  DWORD activation_flags_ = 0;
  DWORD thread_mgr_event_sink_cookie_ = TF_INVALID_COOKIE;
  DWORD thread_focus_sink_cookie_ = TF_INVALID_COOKIE;
  DWORD text_edit_sink_cookie_ = TF_INVALID_COOKIE;
  Microsoft::WRL::ComPtr<ITfComposition> composition_;
  std::vector<Microsoft::WRL::ComPtr<DetachedCompositionEndSession>> detached_ends_;
  TfGuidAtom display_attribute_atom_ = TF_INVALID_GUIDATOM;
  Microsoft::WRL::ComPtr<CandidateUiElement> candidate_ui_;
  bool candidate_ui_external_ = true;
  // The context that owns composition_.  Keeping it alongside the
  // composition lets every text update restore the host caret to the end of
  // the unconfirmed range instead of leaving it at the range start.
  Microsoft::WRL::ComPtr<ITfContext> composition_context_;
  Microsoft::WRL::ComPtr<ITfSource> layout_source_;
  DWORD layout_sink_cookie_ = TF_INVALID_COOKIE;
  Microsoft::WRL::ComPtr<ITfSource> text_edit_sink_source_;
  Microsoft::WRL::ComPtr<ITfContext> text_edit_sink_context_;
  std::wstring preedit_;
  std::wstring preceding_preview_;
  // A result belongs to one composition.  Clearing the state drops our
  // reference so an older UIA worker cannot supply a later editor's context.
  std::shared_ptr<AccessibleContextState> accessible_context_;
  RECT candidate_anchor_{};
  bool candidate_anchor_available_ = false;
  std::vector<Candidate> candidates_;
  size_t candidate_page_ = 0;
  size_t selected_candidate_ = 0;
  bool connected_ = false;
  std::wstring schema_id_ = L"rime_ice";
  uint64_t config_revision_ = 0;
  uint64_t request_id_ = 0;
  uint64_t active_input_request_id_ = 0;
  uint32_t context_limit_ = 128;
  uint32_t context_preview_limit_ = 32;
  uint32_t page_size_ = 9;
  bool candidate_fetch_complete_ = false;
  bool last_fetch_failed_ = false;
  bool schema_reset_pending_ = false;
  bool cancel_pending_ = false;
  bool last_edit_pending_ = false;
  bool partial_edit_pending_ = false;
  uint64_t partial_edit_generation_ = 0;
  std::wstring pending_partial_pinyin_;
  std::wstring pending_partial_commit_;
  std::wstring pending_partial_remainder_;
  std::vector<Candidate> pending_partial_candidates_;
  std::wstring pending_partial_preceding_;
  RECT pending_partial_anchor_{};
  bool pending_partial_anchor_available_ = false;
  bool pending_partial_fetch_complete_ = false;
  bool terminal_edit_pending_ = false;
  Action terminal_edit_action_ = Action::Update;
  uint64_t terminal_edit_generation_ = 0;
  // Monotonically invalidates queued TSF edit sessions.  In particular, an
  // Esc cancellation must make an older asynchronous Update a no-op instead
  // of allowing it to recreate the composition after state was cleared.
  uint64_t edit_generation_ = 0;
  HRESULT last_edit_error_ = S_OK;
  bool passthrough_notified_ = false;
  Microsoft::WRL::ComPtr<ITfContext> active_context_;
  // Mirrors the bundled Rime ascii_composer configuration: left Shift uses
  // commit_code semantics, while right Shift is a no-op.  A left Shift key
  // toggles on a short, unmodified press/release; pressing any other key
  // clears the pending toggle.  The mode itself survives focus changes while
  // this TSF instance remains active.
  uint8_t shift_down_mask_ = 0;
  uint8_t shift_pending_mask_ = 0;
  ULONGLONG left_shift_down_tick_ = 0;
  ULONGLONG right_shift_down_tick_ = 0;
  bool ascii_mode_ = false;
  // Chinese punctuation keeps one bit of keyboard history: an unmodified
  // digit that was just passed through to the host makes the next period
  // half-width, matching Weasel/Rime's numeric punctuation behavior.  This
  // is deliberately independent of the host context text.
  bool last_chinese_input_was_digit_ = false;
  bool space_keyup_pending_ = false;
  bool single_quote_open_ = false;
  bool double_quote_open_ = false;
};

HRESULT RegisterComServer();
HRESULT RegisterTsfProfile();
HRESULT UnregisterTsfProfile();
HRESULT CreateClassFactory(REFIID iid, void** object);

}  // namespace lime::tsf
