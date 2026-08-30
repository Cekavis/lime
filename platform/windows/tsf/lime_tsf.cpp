#include "lime_tsf.h"

#include <windows.h>
#include <inputscope.h>
#include <objbase.h>
#include <shlwapi.h>

#include <algorithm>
#include <cctype>
#include <mutex>
#include <sstream>
#include <thread>

using Microsoft::WRL::ComPtr;

namespace lime::tsf {

const CLSID kClsid = {0x2f7a6c4c, 0x2a0b, 0x4ef0, {0x9d, 0x7e, 0xd4, 0x14, 0x3d, 0x64, 0x80, 0x12}};
const GUID kProfileGuid = {0x6f7d47b0, 0x5a33, 0x4d61, {0x95, 0x95, 0x90, 0x0e, 0xd3, 0x95, 0xb6, 0x1a}};
const LANGID kLanguageId = MAKELANGID(LANG_CHINESE, SUBLANG_CHINESE_SIMPLIFIED);
HINSTANCE g_instance = nullptr;
std::atomic<long> g_module_references{0};

namespace {
constexpr wchar_t kDescription[] = L"Lime Chinese Input";
constexpr UINT kMaxFrame = 16u * 1024u * 1024u;
constexpr UINT kDefaultContextLimit = 128;
constexpr size_t kPageSize = 9;

std::wstring PipeName() {
  wchar_t value[256]{};
  const DWORD n = GetEnvironmentVariableW(L"LIME_PIPE", value, ARRAYSIZE(value));
  if (n > 0 && n < ARRAYSIZE(value)) return value;
  return LR"(\\.\pipe\lime-core-v1)";
}

bool ReadAll(HANDLE handle, void* data, DWORD bytes) {
  auto* cursor = static_cast<BYTE*>(data);
  while (bytes) {
    DWORD read = 0;
    if (!ReadFile(handle, cursor, bytes, &read, nullptr) || read == 0) return false;
    cursor += read;
    bytes -= read;
  }
  return true;
}

bool WriteAll(HANDLE handle, const void* data, DWORD bytes) {
  auto* cursor = static_cast<const BYTE*>(data);
  while (bytes) {
    DWORD written = 0;
    if (!WriteFile(handle, cursor, bytes, &written, nullptr) || written == 0) return false;
    cursor += written;
    bytes -= written;
  }
  return true;
}

std::string Utf8(std::wstring_view value) {
  if (value.empty()) return {};
  const int size = WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(),
                                       static_cast<int>(value.size()), nullptr, 0, nullptr, nullptr);
  if (size <= 0) return {};
  std::string result(size, '\0');
  WideCharToMultiByte(CP_UTF8, WC_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
                      result.data(), size, nullptr, nullptr);
  return result;
}

std::wstring Wide(std::string_view value) {
  if (value.empty()) return {};
  const int size = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(),
                                       static_cast<int>(value.size()), nullptr, 0);
  if (size <= 0) return {};
  std::wstring result(size, L'\0');
  MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, value.data(), static_cast<int>(value.size()),
                      result.data(), size);
  return result;
}

std::string JsonEscape(std::wstring_view value) {
  std::ostringstream out;
  for (unsigned char ch : Utf8(value)) {
    if (ch == '"') out << "\\\"";
    else if (ch == '\\') out << "\\\\";
    else if (ch == '\n') out << "\\n";
    else if (ch == '\r') out << "\\r";
    else if (ch == '\t') out << "\\t";
    else if (ch < 0x20) out << "\\u00" << std::hex << static_cast<int>(ch) << std::dec;
    else out << static_cast<char>(ch);
  }
  return out.str();
}

std::string JsonString(std::string_view value, size_t start) {
  std::string out;
  bool escaped = false;
  for (size_t i = start; i < value.size(); ++i) {
    const char ch = value[i];
    if (escaped) {
      switch (ch) {
        case 'n': out.push_back('\n'); break;
        case 'r': out.push_back('\r'); break;
        case 't': out.push_back('\t'); break;
        case '"': out.push_back('"'); break;
        case '\\': out.push_back('\\'); break;
        default: out.push_back(ch); break;
      }
      escaped = false;
    } else if (ch == '\\') escaped = true;
    else if (ch == '"') break;
    else out.push_back(ch);
  }
  return out;
}

bool JsonNumber(std::string_view value, std::string_view key, uint64_t& result) {
  const std::string needle = "\"" + std::string(key) + "\":";
  const size_t p = value.find(needle);
  if (p == std::string_view::npos) return false;
  const char* begin = value.data() + p + needle.size();
  char* end = nullptr;
  result = _strtoui64(begin, &end, 10);
  return end != begin;
}

bool JsonBool(std::string_view value, std::string_view key, bool& result) {
  const std::string needle = "\"" + std::string(key) + "\":";
  const size_t p = value.find(needle);
  if (p == std::string_view::npos) return false;
  const auto tail = value.substr(p + needle.size());
  if (tail.rfind("true", 0) == 0) { result = true; return true; }
  if (tail.rfind("false", 0) == 0) { result = false; return true; }
  return false;
}

bool JsonField(std::string_view value, std::string_view key, std::string& result) {
  const std::string needle = "\"" + std::string(key) + "\":\"";
  const size_t p = value.find(needle);
  if (p == std::string_view::npos) return false;
  result = JsonString(value, p + needle.size());
  return true;
}

class PipeClient {
 public:
  bool Request(const std::string& request, std::string& response) {
    std::lock_guard lock(mutex_);
    HANDLE pipe = Connect();
    if (pipe == INVALID_HANDLE_VALUE) return false;
    const DWORD size = static_cast<DWORD>(request.size());
    const BYTE header[4] = {static_cast<BYTE>(size), static_cast<BYTE>(size >> 8),
                            static_cast<BYTE>(size >> 16), static_cast<BYTE>(size >> 24)};
    bool ok = WriteAll(pipe, header, sizeof(header)) && WriteAll(pipe, request.data(), size);
    BYTE reply_header[4]{};
    std::string body;
    if (ok && ReadAll(pipe, reply_header, sizeof(reply_header))) {
      const UINT reply_size = reply_header[0] | (reply_header[1] << 8) |
                              (reply_header[2] << 16) | (reply_header[3] << 24);
      if (reply_size == 0 || reply_size > kMaxFrame) ok = false;
      else { body.resize(reply_size); ok = ReadAll(pipe, body.data(), reply_size); }
    } else ok = false;
    CloseHandle(pipe);
    if (ok) response = std::move(body);
    return ok;
  }

 private:
  HANDLE Connect() {
    const std::wstring name = PipeName();
    for (int attempt = 0; attempt < 2; ++attempt) {
      HANDLE pipe = CreateFileW(name.c_str(), GENERIC_READ | GENERIC_WRITE, 0, nullptr,
                                OPEN_EXISTING, SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION, nullptr);
      if (pipe != INVALID_HANDLE_VALUE) {
        DWORD mode = PIPE_READMODE_BYTE;
        SetNamedPipeHandleState(pipe, &mode, nullptr, nullptr);
        std::string reply;
        const std::string handshake = R"({"kind":"handshake","payload":{"protocol_version":1}})";
        const DWORD size = static_cast<DWORD>(handshake.size());
        const BYTE header[4] = {static_cast<BYTE>(size), static_cast<BYTE>(size >> 8),
                                static_cast<BYTE>(size >> 16), static_cast<BYTE>(size >> 24)};
        if (WriteAll(pipe, header, sizeof(header)) && WriteAll(pipe, handshake.data(), size)) {
          BYTE rh[4]{};
          if (ReadAll(pipe, rh, sizeof(rh))) {
            const UINT n = rh[0] | (rh[1] << 8) | (rh[2] << 16) | (rh[3] << 24);
            if (n > 0 && n <= kMaxFrame) {
              reply.resize(n);
              if (ReadAll(pipe, reply.data(), n) && reply.find("\"accepted\":true") != std::string::npos)
                return pipe;
            }
          }
        }
        CloseHandle(pipe);
      }
      if (attempt == 0) {
        wchar_t service_path[32768]{};
        const DWORD n = GetEnvironmentVariableW(L"LIME_SERVICE_PATH", service_path, ARRAYSIZE(service_path));
        if (n > 0 && n < ARRAYSIZE(service_path)) {
          STARTUPINFOW startup{}; startup.cb = sizeof(startup); PROCESS_INFORMATION process{};
          std::wstring command(service_path);
          if (CreateProcessW(nullptr, command.data(), nullptr, nullptr, FALSE,
                              CREATE_NO_WINDOW | DETACHED_PROCESS, nullptr, nullptr, &startup, &process)) {
            CloseHandle(process.hThread); CloseHandle(process.hProcess);
          }
        }
      }
      WaitNamedPipeW(name.c_str(), 250);
    }
    return INVALID_HANDLE_VALUE;
  }
  std::mutex mutex_;
};

struct CompositionSession final : ITfEditSession {
  std::atomic<ULONG> references{1};
  TextService* owner;
  ComPtr<ITfContext> context;
  TextService::Action action;
  std::wstring text;
  CompositionSession(TextService* o, ITfContext* c, TextService::Action a, std::wstring t)
      : owner(o), context(c), action(a), text(std::move(t)) { owner->AddRef(); }
  ~CompositionSession() { owner->Release(); }
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** out) override {
    if (!out) return E_POINTER; *out = nullptr;
    if (iid != IID_IUnknown && iid != IID_ITfEditSession) return E_NOINTERFACE;
    *out = static_cast<ITfEditSession*>(this); AddRef(); return S_OK;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references; }
  ULONG STDMETHODCALLTYPE Release() override { const ULONG v = --references; if (!v) delete this; return v; }
  HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie cookie) override;
};

bool ReadPrecedingRange(ITfContext* context, TfEditCookie cookie, uint32_t limit, std::wstring& text) {
  text.clear();
  TF_SELECTION selection{}; ULONG fetched = 0;
  HRESULT hr = context->GetSelection(cookie, TF_DEFAULT_SELECTION, 1, &selection, &fetched);
  if (FAILED(hr) || fetched != 1 || !selection.range) return false;
  ComPtr<ITfRange> range; range.Attach(selection.range);
  ComPtr<ITfRange> before;
  if (FAILED(range->Clone(&before))) return false;
  if (FAILED(before->Collapse(cookie, TF_ANCHOR_START))) return false;
  ComPtr<ITfRangeACP> acp;
  LONG start = 0, length = 0;
  if (SUCCEEDED(before.As(&acp)) && SUCCEEDED(acp->GetExtent(&start, &length))) {
    const LONG count = std::min<LONG>(static_cast<LONG>(limit), std::max<LONG>(0, start));
    if (FAILED(acp->SetExtent(start - count, count))) return false;
    std::vector<wchar_t> buffer(static_cast<size_t>(count)); ULONG read = 0;
    if (count && FAILED(before->GetText(cookie, 0, buffer.data(), count, &read))) return false;
    text.assign(buffer.data(), read); return true;
  }
  LONG moved = 0;
  if (FAILED(before->ShiftStart(cookie, -static_cast<LONG>(limit), &moved, nullptr))) return false;
  const ULONG count = static_cast<ULONG>(std::max<LONG>(0, -moved));
  std::vector<wchar_t> buffer(count); ULONG read = 0;
  if (count && FAILED(before->GetText(cookie, 0, buffer.data(), count, &read))) return false;
  text.assign(buffer.data(), read); return true;
}

class CandidateWindow {
 public:
  void Show(ITfContext* context, const std::vector<TextService::Candidate>& candidates,
            size_t page, size_t selected, size_t page_size, std::wstring_view preview = {}) {
    Ensure(); if (!window_) return;
    lines_.clear();
    if (!preview.empty()) lines_.push_back(L"前文: " + std::wstring(preview));
    const size_t begin = page * page_size;
    for (size_t i = begin; i < std::min(candidates.size(), begin + page_size); ++i) {
      std::wstring line = std::to_wstring(i - begin + 1) + L"  " + candidates[i].display;
      if (i == selected) line = L"> " + line; else line = L"  " + line;
      lines_.push_back(std::move(line));
    }
    RECT rect{100, 100, 520, 100 + static_cast<LONG>(lines_.size() * 24 + 12)};
    ComPtr<ITfContextView> view; HWND hwnd = nullptr;
    if (context && SUCCEEDED(context->GetActiveView(&view)) && view) view->GetWnd(&hwnd);
    if (hwnd) { POINT p{0, 0}; ClientToScreen(hwnd, &p); rect.left = p.x + 12; rect.top = p.y + 24; rect.right = rect.left + 420; }
    SetWindowPos(window_, HWND_TOPMOST, rect.left, rect.top, rect.right - rect.left,
                 rect.bottom - rect.top, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    InvalidateRect(window_, nullptr, TRUE);
  }
  void Hide() { if (window_) ShowWindow(window_, SW_HIDE); }
 private:
  static LRESULT CALLBACK Proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    auto* self = reinterpret_cast<CandidateWindow*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    if (msg == WM_NCCREATE) { self = static_cast<CandidateWindow*>(reinterpret_cast<CREATESTRUCTW*>(lp)->lpCreateParams); SetWindowLongPtrW(hwnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(self)); }
    if (msg == WM_PAINT && self) {
      PAINTSTRUCT ps{}; HDC dc = BeginPaint(hwnd, &ps); FillRect(dc, &ps.rcPaint, reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1));
      SetBkMode(dc, TRANSPARENT); SetTextColor(dc, RGB(20,20,20)); LONG y = 6;
      for (const auto& line : self->lines_) { TextOutW(dc, 10, y, line.c_str(), static_cast<int>(line.size())); y += 24; }
      EndPaint(hwnd, &ps); return 0;
    }
    if (msg == WM_ERASEBKGND) return 1;
    return DefWindowProcW(hwnd, msg, wp, lp);
  }
  void Ensure() {
    if (window_) return;
    static std::once_flag once;
    std::call_once(once, [] { WNDCLASSW wc{}; wc.lpfnWndProc = &CandidateWindow::Proc; wc.hInstance = g_instance; wc.lpszClassName = L"LimeCandidateWindow"; wc.hCursor = LoadCursorW(nullptr, IDC_ARROW); wc.hbrBackground = reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1); RegisterClassW(&wc); });
    window_ = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST, L"LimeCandidateWindow", L"Lime", WS_POPUP | WS_BORDER,
                              0, 0, 420, 120, nullptr, nullptr, g_instance, this);
  }
  HWND window_ = nullptr;
  std::vector<std::wstring> lines_;
};

CandidateWindow g_candidates;
PipeClient g_pipe;

}  // namespace

HRESULT CompositionSession::DoEditSession(TfEditCookie cookie) {
  if (action == TextService::Action::Cancel) { owner->EndComposition(cookie); return S_OK; }
  if (!owner->EnsureComposition(context.Get(), cookie)) return S_OK;
  if (action == TextService::Action::Update) {
    owner->SetCompositionText(cookie, text);
  } else {
    owner->CommitComposition(cookie, text);
  }
  return S_OK;
}

TextService::TextService() { ++g_module_references; }
TextService::~TextService() { Deactivate(); --g_module_references; }

HRESULT TextService::QueryInterface(REFIID iid, void** object) {
  if (!object) return E_POINTER; *object = nullptr;
  if (iid == IID_IUnknown || iid == IID_ITfTextInputProcessor || iid == IID_ITfTextInputProcessorEx) *object = static_cast<ITfTextInputProcessorEx*>(this);
  else if (iid == IID_ITfKeyEventSink) *object = static_cast<ITfKeyEventSink*>(this);
  else return E_NOINTERFACE;
  AddRef(); return S_OK;
}
ULONG TextService::AddRef() { return ++references_; }
ULONG TextService::Release() { const ULONG v = --references_; if (!v) delete this; return v; }

HRESULT TextService::Activate(ITfThreadMgr* manager, TfClientId client_id) { return ActivateEx(manager, client_id, 0); }
HRESULT TextService::ActivateEx(ITfThreadMgr* manager, TfClientId client_id, DWORD flags) {
  if (!manager) return E_INVALIDARG; Deactivate(); thread_manager_ = manager; client_id_ = client_id; activation_flags_ = flags;
  HRESULT hr = manager->QueryInterface(IID_PPV_ARGS(&keystroke_manager_)); if (FAILED(hr)) return hr;
  hr = keystroke_manager_->AdviseKeyEventSink(client_id_, this, TRUE); if (FAILED(hr)) { keystroke_manager_.Reset(); return hr; }
  RefreshConfigRevision();
  return S_OK;
}
HRESULT TextService::Deactivate() {
  HideCandidates(); composition_.Reset(); preedit_.clear(); candidates_.clear(); connected_ = false;
  if (keystroke_manager_ && client_id_ != TF_CLIENTID_NULL) keystroke_manager_->UnadviseKeyEventSink(client_id_);
  keystroke_manager_.Reset(); thread_manager_.Reset(); client_id_ = TF_CLIENTID_NULL; activation_flags_ = 0; return S_OK;
}
HRESULT TextService::OnSetFocus(BOOL) { return S_OK; }
bool TextService::IsPrintable(WPARAM key) const { return (key >= 'A' && key <= 'Z') || (key >= 'a' && key <= 'z') || key == VK_OEM_3; }
bool TextService::IsImeKey(WPARAM key) const { return IsPrintable(key) || key == VK_BACK || key == VK_RETURN || (key >= '1' && key <= '9') || key == VK_SPACE || key == VK_ESCAPE || key == VK_PRIOR || key == VK_NEXT; }
HRESULT TextService::OnTestKeyDown(ITfContext*, WPARAM key, LPARAM, BOOL* eaten) { if (!eaten) return E_POINTER; *eaten = IsImeKey(key) && (!preedit_.empty() || IsPrintable(key)); return S_OK; }
HRESULT TextService::OnTestKeyUp(ITfContext*, WPARAM, LPARAM, BOOL* eaten) { if (!eaten) return E_POINTER; *eaten = FALSE; return S_OK; }
HRESULT TextService::OnKeyDown(ITfContext* context, WPARAM key, LPARAM, BOOL* eaten) { if (!eaten) return E_POINTER; *eaten = HandleKey(context, key) ? TRUE : FALSE; return S_OK; }
HRESULT TextService::OnKeyUp(ITfContext*, WPARAM, LPARAM, BOOL* eaten) { if (!eaten) return E_POINTER; *eaten = FALSE; return S_OK; }
HRESULT TextService::OnPreservedKey(ITfContext*, REFGUID, BOOL* eaten) { if (!eaten) return E_POINTER; *eaten = FALSE; return S_OK; }

bool TextService::HandleKey(ITfContext* context, WPARAM key) {
  if (IsPrintable(key)) {
    wchar_t value[2] = {static_cast<wchar_t>(key >= 'a' && key <= 'z' ? key : std::tolower(static_cast<int>(key))), 0};
    preedit_ += value;
    if (!UpdateCandidates(context)) { preedit_.pop_back(); return false; }
    return true;
  }
  if (key == VK_BACK && !preedit_.empty()) { preedit_.pop_back(); if (preedit_.empty()) RequestEdit(context, Action::Cancel, {}); else if (!UpdateCandidates(context)) return false; return true; }
  if ((key >= '1' && key <= '9') && !candidates_.empty()) { const size_t index = candidate_page_ * kPageSize + (key - '1'); if (index < candidates_.size()) { selected_candidate_ = index; RequestEdit(context, Action::Commit, candidates_[index].commit); preedit_.clear(); candidates_.clear(); HideCandidates(); } return true; }
  if (key == VK_RETURN || key == VK_SPACE) { if (!candidates_.empty()) { const size_t index = std::min(selected_candidate_, candidates_.size() - 1); RequestEdit(context, Action::Commit, candidates_[index].commit); preedit_.clear(); candidates_.clear(); HideCandidates(); return true; } if (!preedit_.empty()) { RequestEdit(context, Action::Commit, preedit_); preedit_.clear(); HideCandidates(); return true; } }
  if (key == VK_ESCAPE && !preedit_.empty()) { RequestEdit(context, Action::Cancel, {}); preedit_.clear(); candidates_.clear(); HideCandidates(); return true; }
  if (key == VK_PRIOR && candidate_page_ > 0) { --candidate_page_; g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_, kPageSize); return true; }
  if (key == VK_NEXT && (candidate_page_ + 1) * kPageSize < candidates_.size()) { ++candidate_page_; g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_, kPageSize); return true; }
  return false;
}

bool TextService::UpdateCandidates(ITfContext* context) {
  std::wstring preceding; bool context_available = false; RequestEdit(context, Action::Update, preedit_);
  // Read context in a read-only edit session, then synchronously ask the local service.
  class ReadSession final : public ITfEditSession {
   public: std::atomic<ULONG> refs{1}; TextService* owner; ComPtr<ITfContext> ctx; std::wstring* before; bool* available; ReadSession(TextService* o, ITfContext* c, std::wstring* b, bool* a):owner(o),ctx(c),before(b),available(a){owner->AddRef();} ~ReadSession(){owner->Release();}
   HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** p) override { if(!p)return E_POINTER;*p=nullptr;if(iid!=IID_IUnknown&&iid!=IID_ITfEditSession)return E_NOINTERFACE;*p=static_cast<ITfEditSession*>(this);AddRef();return S_OK; }
   ULONG STDMETHODCALLTYPE AddRef() override{return ++refs;} ULONG STDMETHODCALLTYPE Release() override{auto v=--refs;if(!v)delete this;return v;}
   HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie c) override {
     *available = ReadPrecedingRange(ctx.Get(), c, owner->ContextLimit(), *before);
     if (!*available) before->clear();
     return S_OK;
   }
    } session(this, context, &preceding, &context_available);
  HRESULT result = E_FAIL, request = context->RequestEditSession(client_id_, &session, TF_ES_READ | TF_ES_SYNC, &result);
  if (FAILED(request) || FAILED(result)) { preceding.clear(); context_available = false; }
  std::string body; const std::string json = "{\"kind\":\"input\",\"payload\":{\"request_id\":" + std::to_string(++request_id_) + ",\"preedit\":\"" + JsonEscape(preedit_) + "\",\"preceding_text\":\"" + JsonEscape(preceding) + "\",\"context_available\":" + (context_available ? "true" : "false") + ",\"config_revision\":" + std::to_string(config_revision_) + "}}";
  if (!g_pipe.Request(json, body)) { connected_ = false; candidates_.clear(); HideCandidates(); return false; }
  if (body.find("\"kind\":\"error\"") != std::string::npos) { RefreshConfigRevision(); return false; }
  connected_ = true; candidates_.clear(); const size_t array = body.find("\"candidates\":["); if (array == std::string::npos) return false;
  size_t pos = array + 14;
  while (pos < body.size()) {
    const size_t d = body.find("\"display_text\":\"", pos); if (d == std::string::npos) break;
    const size_t c = body.find("\"commit_text\":\"", d); if (c == std::string::npos) break;
    Candidate candidate; candidate.display = Wide(JsonString(body, d + 16)); candidate.commit = Wide(JsonString(body, c + 15)); candidates_.push_back(std::move(candidate)); pos = c + 15;
  }
  candidate_page_ = 0; selected_candidate_ = 0; if (candidates_.empty()) HideCandidates(); else g_candidates.Show(context, candidates_, 0, 0, kPageSize, preceding); return true;
}

void TextService::RefreshConfigRevision() {
  std::string body;
  if (!g_pipe.Request(R"({"kind":"get_status"})", body)) { connected_ = false; return; }
  uint64_t revision = 0;
  if (JsonNumber(body, "revision", revision)) config_revision_ = revision;
  uint64_t limit = 0;
  if (JsonNumber(body, "preceding_text_char_limit", limit)) context_limit_ = static_cast<uint32_t>(std::clamp<uint64_t>(limit, 1, 4096));
}

bool TextService::RequestEdit(ITfContext* context, Action action, const std::wstring& text) {
  auto* session = new (std::nothrow) CompositionSession(this, context, action, text); if (!session) return false;
  HRESULT result = E_FAIL; HRESULT request = context->RequestEditSession(client_id_, session, TF_ES_READWRITE | TF_ES_SYNC, &result);
  if (request == TF_E_SYNCHRONOUS) { result = E_FAIL; request = context->RequestEditSession(client_id_, session, TF_ES_READWRITE | TF_ES_ASYNC, &result); }
  session->Release();
  return SUCCEEDED(request) && SUCCEEDED(result);
}
bool TextService::EnsureComposition(ITfContext* context, TfEditCookie cookie) {
  if (composition_) return true;
  TF_SELECTION selection{}; ULONG fetched = 0;
  if (FAILED(context->GetSelection(cookie, TF_DEFAULT_SELECTION, 1, &selection, &fetched)) || fetched != 1 || !selection.range) return false;
  ComPtr<ITfRange> range; range.Attach(selection.range);
  ComPtr<ITfContextComposition> composition_context;
  if (FAILED(context->QueryInterface(IID_PPV_ARGS(&composition_context)))) return false;
  return SUCCEEDED(composition_context->StartComposition(cookie, range.Get(), nullptr, &composition_));
}
bool TextService::SetCompositionText(TfEditCookie cookie, const std::wstring& text) {
  if (!composition_) return false;
  ComPtr<ITfRange> range;
  if (FAILED(composition_->GetRange(&range)) || !range) return false;
  return SUCCEEDED(range->SetText(cookie, 0, text.c_str(), static_cast<LONG>(text.size())));
}
bool TextService::CommitComposition(TfEditCookie cookie, const std::wstring& text) {
  if (!SetCompositionText(cookie, text)) return false;
  EndComposition(cookie);
  return true;
}
void TextService::EndComposition(TfEditCookie cookie) { if (composition_) { composition_->EndComposition(cookie); composition_.Reset(); } }
void TextService::HideCandidates() { g_candidates.Hide(); }

class ClassFactory final : public IClassFactory {
 public:
  std::atomic<ULONG> refs{1}; ClassFactory(){++g_module_references;} ~ClassFactory(){--g_module_references;}
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** p) override { if(!p)return E_POINTER;*p=nullptr;if(iid!=IID_IUnknown&&iid!=IID_IClassFactory)return E_NOINTERFACE;*p=static_cast<IClassFactory*>(this);AddRef();return S_OK; }
  ULONG STDMETHODCALLTYPE AddRef() override{return ++refs;} ULONG STDMETHODCALLTYPE Release() override{auto v=--refs;if(!v)delete this;return v;}
  HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer, REFIID iid, void** p) override { if(outer)return CLASS_E_NOAGGREGATION; auto* service=new(std::nothrow) TextService(); if(!service)return E_OUTOFMEMORY; const HRESULT hr=service->QueryInterface(iid,p); service->Release(); return hr; }
  HRESULT STDMETHODCALLTYPE LockServer(BOOL lock) override { if(lock)++g_module_references; else --g_module_references; return S_OK; }
};

std::wstring GuidString(REFGUID guid) { wchar_t value[64]{}; StringFromGUID2(guid, value, ARRAYSIZE(value)); return value; }
HRESULT RegisterComServer() {
  wchar_t module[MAX_PATH]{}; if (!GetModuleFileNameW(g_instance, module, ARRAYSIZE(module))) return HRESULT_FROM_WIN32(GetLastError());
  HKEY key=nullptr; const std::wstring path=L"CLSID\\"+GuidString(kClsid); LSTATUS status=RegCreateKeyExW(HKEY_CLASSES_ROOT,path.c_str(),0,nullptr,0,KEY_WRITE,nullptr,&key,nullptr); if(status!=ERROR_SUCCESS)return HRESULT_FROM_WIN32(status);
  RegSetValueExW(key,nullptr,0,REG_SZ,reinterpret_cast<const BYTE*>(kDescription),sizeof(kDescription)); HKEY inproc=nullptr; status=RegCreateKeyExW(key,L"InprocServer32",0,nullptr,0,KEY_WRITE,nullptr,&inproc,nullptr); if(status==ERROR_SUCCESS){RegSetValueExW(inproc,nullptr,0,REG_SZ,reinterpret_cast<const BYTE*>(module),static_cast<DWORD>((wcslen(module)+1)*sizeof(wchar_t))); const wchar_t model[]=L"Apartment";RegSetValueExW(inproc,L"ThreadingModel",0,REG_SZ,reinterpret_cast<const BYTE*>(model),sizeof(model));RegCloseKey(inproc);} RegCloseKey(key); return HRESULT_FROM_WIN32(status);
}
HRESULT RegisterTsfProfile() {
  const HRESULT init=CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED); if(FAILED(init)&&init!=RPC_E_CHANGED_MODE)return init; HRESULT result=S_OK; ComPtr<ITfInputProcessorProfiles> profiles; result=CoCreateInstance(CLSID_TF_InputProcessorProfiles,nullptr,CLSCTX_INPROC_SERVER,IID_PPV_ARGS(&profiles)); if(SUCCEEDED(result))result=profiles->Register(kClsid); wchar_t module[MAX_PATH]{};GetModuleFileNameW(g_instance,module,ARRAYSIZE(module)); if(SUCCEEDED(result))result=profiles->AddLanguageProfile(kClsid,kLanguageId,kProfileGuid,kDescription,static_cast<ULONG>(wcslen(kDescription)),module,static_cast<ULONG>(wcslen(module)),0); ComPtr<ITfCategoryMgr> categories; if(SUCCEEDED(result))result=CoCreateInstance(CLSID_TF_CategoryMgr,nullptr,CLSCTX_INPROC_SERVER,IID_PPV_ARGS(&categories)); const GUID required[]={GUID_TFCAT_TIP_KEYBOARD,GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT}; for(const auto& category:required)if(SUCCEEDED(result))result=categories->RegisterCategory(kClsid,category,kClsid); if(SUCCEEDED(init))CoUninitialize(); return result;
}
HRESULT UnregisterTsfProfile() { const HRESULT init=CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED); if(FAILED(init)&&init!=RPC_E_CHANGED_MODE)return init; ComPtr<ITfCategoryMgr> categories; if(SUCCEEDED(CoCreateInstance(CLSID_TF_CategoryMgr,nullptr,CLSCTX_INPROC_SERVER,IID_PPV_ARGS(&categories)))){const GUID required[]={GUID_TFCAT_TIP_KEYBOARD,GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT};for(const auto& category:required)categories->UnregisterCategory(kClsid,category,kClsid);} ComPtr<ITfInputProcessorProfiles> profiles; if(SUCCEEDED(CoCreateInstance(CLSID_TF_InputProcessorProfiles,nullptr,CLSCTX_INPROC_SERVER,IID_PPV_ARGS(&profiles)))){profiles->RemoveLanguageProfile(kClsid,kLanguageId,kProfileGuid);profiles->Unregister(kClsid);} if(SUCCEEDED(init))CoUninitialize(); return S_OK; }
HRESULT CreateClassFactory(REFIID iid, void** object) { auto* factory=new(std::nothrow) ClassFactory(); if(!factory)return E_OUTOFMEMORY; const HRESULT hr=factory->QueryInterface(iid,object);factory->Release();return hr; }

}  // namespace lime::tsf
