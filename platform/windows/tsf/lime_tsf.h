#pragma once

#include <windows.h>
#include <msctf.h>
#include <wrl/client.h>

#include <atomic>
#include <string>
#include <vector>

namespace lime::tsf {

extern const CLSID kClsid;
extern const GUID kProfileGuid;
extern const LANGID kLanguageId;
extern HINSTANCE g_instance;
extern std::atomic<long> g_module_references;

class TextService final : public ITfTextInputProcessorEx, public ITfKeyEventSink {
 public:
  struct Candidate { std::wstring display; std::wstring commit; };
  enum class Action { Update, Commit, Cancel };

  TextService();
  ~TextService();

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override;
  ULONG STDMETHODCALLTYPE AddRef() override;
  ULONG STDMETHODCALLTYPE Release() override;
  HRESULT STDMETHODCALLTYPE Activate(ITfThreadMgr* thread_manager, TfClientId client_id) override;
  HRESULT STDMETHODCALLTYPE Deactivate() override;
  HRESULT STDMETHODCALLTYPE ActivateEx(ITfThreadMgr* thread_manager, TfClientId client_id,
                                        DWORD flags) override;
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

  bool EnsureComposition(ITfContext* context, TfEditCookie cookie);
  bool SetCompositionText(TfEditCookie cookie, const std::wstring& text);
  bool CommitComposition(TfEditCookie cookie, const std::wstring& text);
  void EndComposition(TfEditCookie cookie);
  uint32_t ContextLimit() const { return context_limit_; }

 private:
  bool HandleKey(ITfContext* context, WPARAM key);
  bool IsImeKey(WPARAM key) const;
  bool IsPrintable(WPARAM key) const;
  bool UpdateCandidates(ITfContext* context);
  void RefreshConfigRevision();
  bool RequestEdit(ITfContext* context, Action action, const std::wstring& text);
  bool ReadPreceding(ITfContext* context, TfEditCookie cookie, std::wstring& text) const;
  void HideCandidates();

  std::atomic<ULONG> references_{1};
  Microsoft::WRL::ComPtr<ITfThreadMgr> thread_manager_;
  Microsoft::WRL::ComPtr<ITfKeystrokeMgr> keystroke_manager_;
  TfClientId client_id_ = TF_CLIENTID_NULL;
  DWORD activation_flags_ = 0;
  Microsoft::WRL::ComPtr<ITfComposition> composition_;
  std::wstring preedit_;
  std::vector<Candidate> candidates_;
  size_t candidate_page_ = 0;
  size_t selected_candidate_ = 0;
  bool connected_ = false;
  uint64_t config_revision_ = 0;
  uint64_t request_id_ = 0;
  uint32_t context_limit_ = 128;
};

HRESULT RegisterComServer();
HRESULT RegisterTsfProfile();
HRESULT UnregisterTsfProfile();
HRESULT CreateClassFactory(REFIID iid, void** object);

}  // namespace lime::tsf
