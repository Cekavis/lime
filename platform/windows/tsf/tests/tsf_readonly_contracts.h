#pragma once

#include <functional>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

namespace {

class ScopedEnvironment final {
 public:
  ScopedEnvironment(const wchar_t* name, const std::wstring& value) : name_(name) {
    SetLastError(ERROR_SUCCESS);
    const DWORD size = GetEnvironmentVariableW(name_, nullptr, 0);
    present_ = size != 0 || GetLastError() != ERROR_ENVVAR_NOT_FOUND;
    if (size != 0) {
      previous_.resize(size);
      const DWORD length = GetEnvironmentVariableW(name_, previous_.data(), size);
      CHECK(length < size);
      previous_.resize(length);
    }
    CHECK(SetEnvironmentVariableW(name_, value.c_str()));
  }
  ~ScopedEnvironment() {
    CHECK(SetEnvironmentVariableW(name_, present_ ? previous_.c_str() : nullptr));
  }

 private:
  const wchar_t* name_;
  bool present_ = false;
  std::wstring previous_;
};

class ScopedKeyboardState final {
 public:
  ScopedKeyboardState() {
    CHECK(GetKeyboardState(previous_));
    BYTE neutral[256]{};
    // This changes only the test thread's keyboard state, without SendInput
    // or any synthetic input to the user's focused application.
    CHECK(SetKeyboardState(neutral));
  }
  ~ScopedKeyboardState() { CHECK(SetKeyboardState(previous_)); }

 private:
  BYTE previous_[256]{};
};

class InputPipeStub final {
 public:
  InputPipeStub()
      : name_(L"\\\\.\\pipe\\lime-tsf-contracts-" +
              std::to_wstring(GetCurrentProcessId()) + L"-" +
              std::to_wstring(++sequence_)),
        pipe_environment_(L"LIME_PIPE", name_),
        // A failed fixture connection must never start a real Lime service.
        service_environment_(L"LIME_SERVICE_PATH", L"\\\\.\\NUL") {
    const HANDLE first_pipe = CreatePipe();
    worker_ = std::thread([this, first_pipe] { Serve(first_pipe); });
  }
  ~InputPipeStub() {
    stopping_ = true;
    // Wake a pending ConnectNamedPipe without leaving a server thread or
    // named pipe behind. Every real client has already finished OnKeyDown.
    const ULONGLONG deadline = GetTickCount64() + 5000;
    while (!finished_ && GetTickCount64() < deadline) {
      HANDLE wake = CreateFileW(name_.c_str(), GENERIC_READ | GENERIC_WRITE,
                                0, nullptr, OPEN_EXISTING, 0, nullptr);
      if (wake != INVALID_HANDLE_VALUE) CloseHandle(wake);
      Sleep(1);
    }
    CHECK(finished_);
    worker_.join();
  }

  void FailNextInput() {
    std::lock_guard lock(mutex_);
    fail_next_input_ = true;
  }
  void ExpectPreedits(const std::vector<std::string>& expected) {
    std::lock_guard lock(mutex_);
    CHECK(preedits_ == expected);
  }
  size_t StatusRequests() {
    std::lock_guard lock(mutex_);
    return status_requests_;
  }

 private:
  HANDLE CreatePipe() const {
    HANDLE pipe = CreateNamedPipeW(name_.c_str(), PIPE_ACCESS_DUPLEX,
                                   PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT |
                                       PIPE_REJECT_REMOTE_CLIENTS,
                                   2, 4096, 4096, 0, nullptr);
    CHECK(pipe != INVALID_HANDLE_VALUE);
    return pipe;
  }
  static bool Transfer(HANDLE pipe, void* data, DWORD size, bool write) {
    auto* cursor = static_cast<BYTE*>(data);
    while (size != 0) {
      DWORD transferred = 0;
      const BOOL ok = write ? WriteFile(pipe, cursor, size, &transferred, nullptr)
                            : ReadFile(pipe, cursor, size, &transferred, nullptr);
      if (!ok || transferred == 0) return false;
      cursor += transferred;
      size -= transferred;
    }
    return true;
  }
  static bool ReadFrame(HANDLE pipe, std::string& body) {
    BYTE header[4]{};
    if (!Transfer(pipe, header, sizeof(header), false)) return false;
    const DWORD size = static_cast<DWORD>(header[0]) |
                       (static_cast<DWORD>(header[1]) << 8) |
                       (static_cast<DWORD>(header[2]) << 16) |
                       (static_cast<DWORD>(header[3]) << 24);
    if (size == 0 || size > 65536) return false;
    body.resize(size);
    return Transfer(pipe, body.data(), size, false);
  }
  static bool WriteFrame(HANDLE pipe, std::string body) {
    const DWORD size = static_cast<DWORD>(body.size());
    BYTE header[4] = {static_cast<BYTE>(size), static_cast<BYTE>(size >> 8),
                      static_cast<BYTE>(size >> 16), static_cast<BYTE>(size >> 24)};
    return Transfer(pipe, header, sizeof(header), true) &&
           Transfer(pipe, body.data(), size, true);
  }
  std::string Reply(const std::string& request) {
    std::lock_guard lock(mutex_);
    if (request == R"({"kind":"get_status"})") {
      ++status_requests_;
      return R"({"kind":"status","payload":{"state":"rime_only","rime_schema":"rime_ice","revision":0}})";
    }
    CHECK(request.find(R"("kind":"input")") != std::string::npos);
    const std::string marker = R"("preedit":")";
    const size_t start = request.find(marker);
    CHECK(start != std::string::npos);
    const size_t first = start + marker.size();
    const size_t end = request.find('"', first);
    CHECK(end != std::string::npos);
    // These contracts send only ASCII letters, so no JSON unescaping is
    // required to observe the actual production request's preedit.
    preedits_.push_back(request.substr(first, end - first));
    if (fail_next_input_) {
      fail_next_input_ = false;
      return R"({"kind":"error","payload":{"code":"request_cancelled"}})";
    }
    return R"({"kind":"input","payload":{"service_state":"rime_only","candidates":[]}})";
  }
  void Serve(HANDLE pipe) {
    while (!stopping_) {
      const BOOL connected = ConnectNamedPipe(pipe, nullptr);
      CHECK(connected || GetLastError() == ERROR_PIPE_CONNECTED);
      if (stopping_) break;
      // Make the next instance connectable before replying, including an
      // input error immediately followed by get_status on the same key.
      // The production client must never depend on its PIPE_BUSY retries.
      const HANDLE next_pipe = CreatePipe();
      std::string request;
      CHECK(ReadFrame(pipe, request));
      CHECK(request == R"({"kind":"handshake","payload":{"protocol_version":1}})");
      CHECK(WriteFrame(pipe, R"({"kind":"handshake","payload":{"accepted":true}})"));
      CHECK(ReadFrame(pipe, request));
      CHECK(WriteFrame(pipe, Reply(request)));
      // Keep the reply available until the synchronous production client has
      // consumed it before recycling this pipe instance for the next request.
      const BOOL flushed = FlushFileBuffers(pipe);
      CHECK(flushed || GetLastError() == ERROR_BROKEN_PIPE);
      CHECK(DisconnectNamedPipe(pipe));
      CHECK(CloseHandle(pipe));
      pipe = next_pipe;
    }
    CHECK(CloseHandle(pipe));
    finished_ = true;
  }

  inline static unsigned sequence_ = 0;
  std::wstring name_;
  ScopedEnvironment pipe_environment_;
  ScopedEnvironment service_environment_;
  std::thread worker_;
  std::atomic<bool> stopping_{false};
  std::atomic<bool> finished_{false};
  std::mutex mutex_;
  std::vector<std::string> preedits_;
  bool fail_next_input_ = false;
  size_t status_requests_ = 0;
};

// This models TSF accepting a write for later execution, not host rendering.
// We keep the real edit-session references until teardown and observe the
// following production IPC requests to test HandleKey's retained input.
class EditContextStub final : public ITfContext {
 public:
  HRESULT next_error = S_OK;
  bool error_in_session = false;
  std::function<void()> during_next_write;
  size_t writes = 0;

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (iid != IID_IUnknown && iid != IID_ITfContext) return E_NOINTERFACE;
    *object = static_cast<ITfContext*>(this);
    AddRef();
    return S_OK;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references_;
    if (!value) delete this;
    return value;
  }
  HRESULT STDMETHODCALLTYPE RequestEditSession(TfClientId, ITfEditSession* session,
                                               DWORD flags, HRESULT* result) override {
    if (!session || !result) return E_POINTER;
    if ((flags & TF_ES_READWRITE) != TF_ES_READWRITE) {
      // Preceding-text reads are optional and do not set last_edit_error_.
      *result = TF_E_NOLOCK;
      return S_OK;
    }
    ++writes;
    CHECK(flags == TF_ES_READWRITE);
    const HRESULT error = next_error;
    next_error = S_OK;
    if (during_next_write) {
      auto callback = std::move(during_next_write);
      during_next_write = {};
      callback();
    }
    if (FAILED(error)) {
      *result = error_in_session ? error : E_FAIL;
      return error_in_session ? S_OK : error;
    }
    queued_.emplace_back(session);
    *result = TF_S_ASYNC;
    return S_OK;
  }
  void DiscardQueuedSessions() { queued_.clear(); }
  void RunNextQueuedSession() {
    CHECK(!queued_.empty());
    ComPtr<ITfEditSession> session = queued_.front();
    queued_.erase(queued_.begin());
    const HRESULT result = session->DoEditSession(1);
    CHECK(SUCCEEDED(result) || result == E_FAIL);
  }

  HRESULT STDMETHODCALLTYPE InWriteSession(TfClientId, BOOL* value) override {
    *value = FALSE; return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetSelection(TfEditCookie, ULONG, ULONG,
                                         TF_SELECTION*, ULONG* fetched) override {
    *fetched = 0; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE SetSelection(TfEditCookie, ULONG,
                                         const TF_SELECTION*) override { return E_NOTIMPL; }
  HRESULT STDMETHODCALLTYPE GetStart(TfEditCookie, ITfRange** range) override {
    *range = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetEnd(TfEditCookie, ITfRange** range) override {
    *range = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetActiveView(ITfContextView** view) override {
    *view = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE EnumViews(IEnumTfContextViews** views) override {
    *views = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetStatus(TF_STATUS* status) override {
    *status = {}; return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetProperty(REFGUID, ITfProperty** property) override {
    *property = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetAppProperty(REFGUID, ITfReadOnlyProperty** property) override {
    *property = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE TrackProperties(const GUID**, ULONG, const GUID**, ULONG,
                                            ITfReadOnlyProperty** property) override {
    *property = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE EnumProperties(IEnumTfProperties** properties) override {
    *properties = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE GetDocumentMgr(ITfDocumentMgr** manager) override {
    *manager = nullptr; return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE CreateRangeBackup(TfEditCookie, ITfRange*,
                                              ITfRangeBackup** backup) override {
    *backup = nullptr; return E_NOTIMPL;
  }

 private:
  std::atomic<ULONG> references_{1};
  std::vector<ComPtr<ITfEditSession>> queued_;
};

class InputContract final {
 public:
  InputContract() { context.Attach(new EditContextStub()); }
  ~InputContract() {
    // Break queued-session -> service -> context references while the stack
    // service is still alive, then invalidate its accepted callbacks.
    context->DiscardQueuedSessions();
    CHECK(SUCCEEDED(service.Deactivate()));
  }
  void Press(WPARAM key, HRESULT error = S_OK, bool error_in_session = false) {
    context->next_error = error;
    context->error_in_session = error_in_session;
    BOOL eaten = FALSE;
    CHECK(SUCCEEDED(service.OnKeyDown(context.Get(), key, 0, &eaten)));
    CHECK(eaten == TRUE);
  }

  InputPipeStub pipe;
  TextService service;
  ComPtr<EditContextStub> context;
};

bool DispatchKeyDown(TextService& service, ITfContext* context, WPARAM key,
                     LPARAM lparam = 0) {
  BOOL probe_eaten = FALSE;
  CHECK(SUCCEEDED(service.OnTestKeyDown(context, key, lparam, &probe_eaten)));
  if (!probe_eaten) return false;
  BOOL eaten = FALSE;
  CHECK(SUCCEEDED(service.OnKeyDown(context, key, lparam, &eaten)));
  CHECK(eaten == TRUE);
  return true;
}

bool DispatchKeyUp(TextService& service, ITfContext* context, WPARAM key,
                   LPARAM lparam = 0) {
  BOOL probe_eaten = FALSE;
  CHECK(SUCCEEDED(service.OnTestKeyUp(context, key, lparam, &probe_eaten)));
  if (!probe_eaten) return false;
  BOOL eaten = FALSE;
  CHECK(SUCCEEDED(service.OnKeyUp(context, key, lparam, &eaten)));
  CHECK(eaten == TRUE);
  return true;
}

void TestBareLeftShiftToggles() {
  EditContextStub context;
  TextService service;
  CHECK(DispatchKeyDown(service, &context, VK_LSHIFT));
  CHECK(service.ShiftDownMaskForTest() != 0);
  CHECK(service.ShiftPendingMaskForTest() != 0);
  CHECK(DispatchKeyUp(service, &context, VK_LSHIFT));
  CHECK(service.ShiftDownMaskForTest() == 0);
  CHECK(service.ShiftPendingMaskForTest() == 0);
  CHECK(service.AsciiModeForTest());

  CHECK(DispatchKeyDown(service, &context, VK_LSHIFT));
  CHECK(DispatchKeyUp(service, &context, VK_LSHIFT));
  CHECK(!service.AsciiModeForTest());
}

void TestLeftShiftChordDoesNotPoisonNextTap() {
  EditContextStub context;
  TextService service;
  CHECK(DispatchKeyDown(service, &context, VK_LSHIFT));
  CHECK(service.ShiftPendingMaskForTest() != 0);
  CHECK(!DispatchKeyDown(service, &context, VK_F1));
  CHECK(service.ShiftPendingMaskForTest() == 0);

  // The pass-through release is not followed by OnKeyUp in TSF's two-stage
  // dispatch, so OnTestKeyUp must clear the physical Shift state itself.
  CHECK(!DispatchKeyUp(service, &context, VK_LSHIFT));
  CHECK(service.ShiftDownMaskForTest() == 0);
  CHECK(!service.AsciiModeForTest());

  CHECK(DispatchKeyDown(service, &context, VK_LSHIFT));
  CHECK(DispatchKeyUp(service, &context, VK_LSHIFT));
  CHECK(service.AsciiModeForTest());
}

void TestShiftStateResetsOnFocusAndDeactivate() {
  EditContextStub context;
  TextService service;
  CHECK(DispatchKeyDown(service, &context, VK_LSHIFT));
  CHECK(service.ShiftDownMaskForTest() != 0);
  CHECK(SUCCEEDED(service.OnSetFocus(FALSE)));
  CHECK(service.ShiftDownMaskForTest() == 0);
  CHECK(service.ShiftPendingMaskForTest() == 0);

  CHECK(DispatchKeyDown(service, &context, VK_LSHIFT));
  CHECK(DispatchKeyUp(service, &context, VK_LSHIFT));
  CHECK(service.AsciiModeForTest());
  CHECK(SUCCEEDED(service.Deactivate()));
  CHECK(service.ShiftDownMaskForTest() == 0);
  CHECK(service.ShiftPendingMaskForTest() == 0);
  CHECK(!service.AsciiModeForTest());
}

void TestUnavailableContextPassesThrough() {
  TextService service;
  BOOL eaten = TRUE;
  CHECK(SUCCEEDED(service.OnTestKeyDown(nullptr, 'F', 0, &eaten)));
  CHECK(eaten == FALSE);
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnKeyDown(nullptr, 'F', 0, &eaten)));
  CHECK(eaten == FALSE);
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnTestKeyDown(nullptr, VK_LSHIFT, 0, &eaten)));
  CHECK(eaten == FALSE);
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnKeyDown(nullptr, VK_LSHIFT, 0, &eaten)));
  CHECK(eaten == FALSE);
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnTestKeyUp(nullptr, VK_LSHIFT, 0, &eaten)));
  CHECK(eaten == FALSE);
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnKeyUp(nullptr, VK_LSHIFT, 0, &eaten)));
  CHECK(eaten == FALSE);
  CHECK(service.PreeditForTest().empty());
  CHECK(!service.CompositionActiveForTest());
}

void TestShiftInputContracts() {
  ScopedKeyboardState keyboard;
  TestUnavailableContextPassesThrough();
  TestBareLeftShiftToggles();
  TestLeftShiftChordDoesNotPoisonNextTap();
  TestShiftStateResetsOnFocusAndDeactivate();
}

void TestReadonlyFirstKey() {
  // Both RequestEditSession HRESULT channels can report a read-only context.
  for (const bool error_in_session : {false, true}) {
    InputContract input;
    input.Press('N', TF_E_READONLY, error_in_session);
    input.Press('I');
    input.Press('A');
    input.pipe.ExpectPreedits({"n", "i", "ia"});
    CHECK(input.context->writes == 3);
  }
}

void TestConsecutiveReadonlyKeys() {
  InputContract input;
  input.Press('N', TF_E_READONLY);
  input.Press('I', TF_E_READONLY);
  input.Press('A');
  input.pipe.ExpectPreedits({"n", "i", "a"});
  CHECK(input.context->writes == 3);
}

void TestReadonlyPreservesAcceptedPrefix() {
  InputContract input;
  input.Press('N');
  input.Press('I', TF_E_READONLY);
  input.Press('A');
  input.pipe.ExpectPreedits({"n", "ni", "na"});
  CHECK(input.context->writes == 3);
}

void TestLockedPreservesAttemptedKey() {
  InputContract input;
  input.Press('N', TF_E_LOCKED);
  input.Press('I');
  input.pipe.ExpectPreedits({"n", "ni"});
  CHECK(input.context->writes == 2);
}

void TestFetchFailureDoesNotReuseReadonly() {
  InputContract input;
  input.Press('N', TF_E_READONLY);
  input.pipe.FailNextInput();
  input.Press('I');
  // FetchCandidates's error response refreshes status but never enters a
  // write session. Its stale READONLY must not discard this different key.
  CHECK(input.context->writes == 1);
  CHECK(input.pipe.StatusRequests() == 1);
  input.Press('A');
  input.pipe.ExpectPreedits({"n", "i", "ia"});
  CHECK(input.context->writes == 2);
}

void TestReadonlyAfterFocusCleared() {
  InputContract input;
  input.context->during_next_write = [&input] {
    CHECK(SUCCEEDED(input.service.OnSetFocus(FALSE)));
  };
  input.Press('N', TF_E_READONLY);
  input.Press('I');
  input.Press('A');
  input.pipe.ExpectPreedits({"n", "i", "ia"});
  CHECK(input.context->writes == 3);
}

void TestReadonlyAfterReentrantNewInput() {
  for (const bool same_letter : {false, true}) {
    InputContract input;
    input.context->during_next_write = [&input, same_letter] {
      CHECK(SUCCEEDED(input.service.OnSetFocus(FALSE)));
      input.Press(same_letter ? 'N' : 'I');
    };
    input.Press('N', TF_E_READONLY);
    input.Press('A');
    // The rejected outer N must not remove the new input accepted during
    // reentry, even when its text happens to match the rejected input.
    if (same_letter) input.pipe.ExpectPreedits({"n", "n", "na"});
    else input.pipe.ExpectPreedits({"n", "i", "ia"});
    CHECK(input.context->writes == 3);
  }
}

void TestUnavailableEditFailureDoesNotBuffer() {
  InputContract input;
  input.context->next_error = E_FAIL;
  input.context->error_in_session = false;
  BOOL eaten = TRUE;
  CHECK(SUCCEEDED(input.service.OnKeyDown(input.context.Get(), 'N', 0, &eaten)));
  CHECK(eaten == FALSE);
  CHECK(input.service.PreeditForTest().empty());
  CHECK(!input.service.CompositionActiveForTest());
  CHECK(!input.service.TerminalEditPendingForTest());

  input.Press('I');
  input.pipe.ExpectPreedits({"n", "i"});
}

void TestAsyncEditFailureDoesNotBuffer() {
  InputContract input;
  input.Press('N');
  CHECK(input.service.PreeditForTest() == L"n");
  input.context->RunNextQueuedSession();
  CHECK(input.service.PreeditForTest().empty());
  CHECK(!input.service.CompositionActiveForTest());
  CHECK(!input.service.TerminalEditPendingForTest());

  input.Press('I');
  input.pipe.ExpectPreedits({"n", "i"});
}

void TestReadonlyInputContracts() {
  ScopedKeyboardState keyboard;
  const long references_before = g_module_references.load();
  TestReadonlyFirstKey();
  TestConsecutiveReadonlyKeys();
  TestReadonlyPreservesAcceptedPrefix();
  TestLockedPreservesAttemptedKey();
  TestFetchFailureDoesNotReuseReadonly();
  TestReadonlyAfterFocusCleared();
  TestReadonlyAfterReentrantNewInput();
  TestUnavailableEditFailureDoesNotBuffer();
  TestAsyncEditFailureDoesNotBuffer();
  CHECK(g_module_references.load() == references_before);
}

}  // namespace
