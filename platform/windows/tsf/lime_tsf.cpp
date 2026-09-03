#include "lime_tsf.h"

#include <windows.h>
#include <inputscope.h>
#include <objbase.h>
#include <shlwapi.h>

#include <algorithm>
#include <cctype>
#include <condition_variable>
#include <mutex>
#include <sstream>
#include <thread>

#if defined(LIME_WITH_WEASEL_UI)
#include "weasel_ui_adapter.h"
#endif

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
constexpr UINT kDefaultContextPreviewLimit = 32;

std::wstring EditErrorText(HRESULT hr) {
  switch (hr) {
    case TF_E_COMPOSITION_REJECTED:
      return L"当前编辑器拒绝 TSF 组合串";
    case TF_E_LOCKED:
    case TF_E_NOLOCK:
    case TF_E_SYNCHRONOUS:
      return L"编辑器当前占用 TSF 编辑锁";
    case TF_E_DISCONNECTED:
      return L"编辑器上下文已断开";
    case TF_E_READONLY:
      return L"编辑器上下文只读";
    default: {
      wchar_t value[32]{};
      swprintf_s(value, L"TSF 编辑失败 (0x%08lX)",
                 static_cast<unsigned long>(hr));
      return value;
    }
  }
}

std::wstring PreviewText(std::wstring_view text, uint32_t limit) {
  if (limit == 0 || text.empty()) return {};
  if (text.size() <= limit) return std::wstring(text);
  size_t start = text.size() - limit;
  // Keep a UTF-16 surrogate pair together when the configured preview limit
  // happens to split one. The service still receives the full context range.
  const auto is_high = [](wchar_t value) {
    return value >= 0xd800 && value <= 0xdbff;
  };
  const auto is_low = [](wchar_t value) {
    return value >= 0xdc00 && value <= 0xdfff;
  };
  if (start > 0 && is_low(text[start]) && is_high(text[start - 1])) --start;
  return std::wstring(text.substr(start));
}

bool UppercaseLetterActive() {
  const bool shift = (GetKeyState(VK_SHIFT) & 0x8000) != 0;
  const bool caps_lock = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;
  return shift != caps_lock;
}

std::wstring PipeName() {
  wchar_t value[256]{};
  const DWORD n = GetEnvironmentVariableW(L"LIME_PIPE", value, ARRAYSIZE(value));
  if (n > 0 && n < ARRAYSIZE(value)) return value;
  return LR"(\\.\pipe\lime-core-v1)";
}

bool HasNonTextModifier() {
  return (GetKeyState(VK_CONTROL) & 0x8000) != 0 ||
         (GetKeyState(VK_MENU) & 0x8000) != 0 ||
         (GetKeyState(VK_LWIN) & 0x8000) != 0 ||
         (GetKeyState(VK_RWIN) & 0x8000) != 0;
}

constexpr uint8_t kLeftShiftBit = 0x01;
constexpr uint8_t kRightShiftBit = 0x02;
constexpr ULONGLONG kShiftTapTimeoutMs = 500;

bool IsShiftKey(WPARAM key) {
  return key == VK_SHIFT || key == VK_LSHIFT || key == VK_RSHIFT;
}

uint8_t ShiftKeyBit(WPARAM key, LPARAM lparam = 0) {
  if (key == VK_LSHIFT) return kLeftShiftBit;
  if (key == VK_RSHIFT) return kRightShiftBit;
  if (key != VK_SHIFT) return 0;
  // TSF normally forwards the same key-data LPARAM as WM_KEYDOWN.  The right
  // Shift key has scan code 0x36; generic/unknown data conservatively maps to
  // the left side so synthetic test events remain deterministic.
  const UINT scan = (static_cast<UINT_PTR>(lparam) >> 16) & 0xffu;
  return scan == 0x36u ? kRightShiftBit : kLeftShiftBit;
}

bool IsKeyRepeat(LPARAM lparam) {
  // Bit 30 is the previous-key-state flag in WM_KEYDOWN/TSF key data.
  return (static_cast<UINT_PTR>(lparam) & (static_cast<UINT_PTR>(1) << 30)) != 0;
}

// Convert a virtual key to the character that a standard Windows keyboard
// layout would produce.  Keeping this as a side-effect-free function makes
// the mode logic testable without a TSF context; the caller supplies the
// modifier state captured by GetKeyState().
constexpr wchar_t AsciiCharForVirtualKey(WPARAM key, bool shift, bool caps_lock,
                                         bool num_lock = true) {
  if (key >= 'A' && key <= 'Z') {
    const bool upper = shift != caps_lock;
    const wchar_t letter = static_cast<wchar_t>(key);
    return upper ? letter : static_cast<wchar_t>(letter + (L'a' - L'A'));
  }
  if (key >= 'a' && key <= 'z') {
    const wchar_t upper = static_cast<wchar_t>(key - (L'a' - L'A'));
    return (shift != caps_lock) ? upper : static_cast<wchar_t>(key);
  }
  if (key >= '0' && key <= '9') {
    constexpr wchar_t shifted[] = L")!@#$%^&*(";
    return shift ? shifted[key - '0'] : static_cast<wchar_t>(key);
  }
  if (key >= VK_NUMPAD0 && key <= VK_NUMPAD9) {
    return num_lock ? static_cast<wchar_t>(L'0' + (key - VK_NUMPAD0)) : 0;
  }

  switch (key) {
    case VK_SPACE: return L' ';
    case VK_DECIMAL: return num_lock ? L'.' : 0;
    case VK_SEPARATOR: return L',';
    case VK_ADD: return L'+';
    case VK_SUBTRACT: return L'-';
    case VK_MULTIPLY: return L'*';
    case VK_DIVIDE: return L'/';
    case VK_OEM_1: return shift ? L':' : L';';
    case VK_OEM_PLUS: return shift ? L'+' : L'=';
    case VK_OEM_COMMA: return shift ? L'<' : L',';
    case VK_OEM_MINUS: return shift ? L'_' : L'-';
    case VK_OEM_PERIOD: return shift ? L'>' : L'.';
    case VK_OEM_2: return shift ? L'?' : L'/';
    case VK_OEM_3: return shift ? L'~' : L'`';
    case VK_OEM_4: return shift ? L'{' : L'[';
    case VK_OEM_5: return shift ? L'|' : L'\\';
    case VK_OEM_6: return shift ? L'}' : L']';
    case VK_OEM_7: return shift ? L'"' : L'\'';
    case VK_OEM_102: return shift ? L'>' : L'<';
    default: return 0;
  }
}

constexpr std::wstring_view FullShapeForAscii(wchar_t value) {
  // These are the first/default choices from the bundled Rime punctuator
  // table.  Letters and digits intentionally return an empty view: Chinese mode must
  // still let those keys follow the normal preedit/host paths.
  switch (value) {
    case L' ': return L"　";
    case L',': return L"，";
    case L'.': return L"。";
    case L'<': return L"《";
    case L'>': return L"》";
    case L'/': return L"／";
    case L'?': return L"？";
    case L';': return L"；";
    case L':': return L"：";
    case L'\\': return L"、";
    case L'|': return L"·";
    case L'`': return L"｀";
    case L'~': return L"～";
    case L'!': return L"！";
    case L'@': return L"＠";
    case L'#': return L"＃";
    case L'%': return L"％";
    case L'$': return L"￥";
    case L'^': return L"……";
    case L'&': return L"＆";
    case L'*': return L"＊";
    case L'(': return L"（";
    case L')': return L"）";
    case L'-': return L"－";
    case L'_': return L"——";
    case L'+': return L"＋";
    case L'=': return L"＝";
    case L'[': return L"「";
    case L']': return L"」";
    case L'{': return L"『";
    case L'}': return L"』";
    default: return {};
  }
}

bool IsPunctuationCharacter(wchar_t value) {
  return value == L' ' || (value >= L'!' && value <= L'/') ||
         (value >= L':' && value <= L'@') ||
         (value >= L'[' && value <= L'`') ||
         (value >= L'{' && value <= L'~');
}

static_assert(AsciiCharForVirtualKey('A', false, false) == L'a');
static_assert(AsciiCharForVirtualKey('A', true, false) == L'A');
static_assert(AsciiCharForVirtualKey('1', true, false) == L'!');
static_assert(FullShapeForAscii(L',') == std::wstring_view(L"，"));
static_assert(FullShapeForAscii(L'^') == std::wstring_view(L"……"));

// Rime's stock Windows key bindings use PageUp/PageDown as well as the
// unshifted -/= keys.  Keep those aliases in the TSF sink so a key that the
// candidate window (or the user) uses for paging is not passed to the host
// as ordinary punctuation.  The comma/period/bracket bindings in the stock
// config are commented out, so they must remain ordinary host keys here.
bool IsPreviousPageKey(WPARAM key) {
  if (HasNonTextModifier()) return false;
  const bool shift = (GetKeyState(VK_SHIFT) & 0x8000) != 0;
  return key == VK_PRIOR || key == VK_SUBTRACT ||
         (key == VK_OEM_MINUS && !shift);
}

bool IsNextPageKey(WPARAM key) {
  if (HasNonTextModifier()) return false;
  const bool shift = (GetKeyState(VK_SHIFT) & 0x8000) != 0;
  return key == VK_NEXT || key == VK_ADD || (key == VK_OEM_PLUS && !shift);
}

void InjectNavigationKey(WORD key) {
  INPUT inputs[2]{};
  inputs[0].type = INPUT_KEYBOARD;
  inputs[0].ki.wVk = key;
  const bool extended = key == VK_PRIOR || key == VK_NEXT || key == VK_UP ||
                        key == VK_DOWN;
  inputs[0].ki.dwFlags = extended ? KEYEVENTF_EXTENDEDKEY : 0;
  inputs[1] = inputs[0];
  inputs[1].ki.dwFlags = KEYEVENTF_KEYUP |
                         (extended ? KEYEVENTF_EXTENDEDKEY : 0);
  SendInput(ARRAYSIZE(inputs), inputs, sizeof(INPUT));
}

std::wstring ServicePath() {
  wchar_t value[32768]{};
  const DWORD n = GetEnvironmentVariableW(L"LIME_SERVICE_PATH", value, ARRAYSIZE(value));
  if (n > 0 && n < ARRAYSIZE(value)) return value;

  // Installed bundles place the service beside the TSF DLL in the install root.
  wchar_t module[MAX_PATH]{};
  const DWORD module_length = GetModuleFileNameW(g_instance, module, ARRAYSIZE(module));
  if (module_length == 0 || module_length >= ARRAYSIZE(module)) return {};
  std::wstring path(module, module_length);
  const size_t separator = path.find_last_of(L"\\/");
  if (separator == std::wstring::npos) return {};
  path.resize(separator + 1);
  path.append(L"lime-service.exe");
  return path;
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

// Return the exclusive end of a JSON array beginning at `start`.  The TSF client only needs
// the top-level `candidates` array, but input diagnostics also contain nested candidate objects;
// bounding the scan prevents those diagnostic copies from being presented as real candidates.
size_t JsonArrayEnd(std::string_view value, size_t start) {
  if (start >= value.size() || value[start] != '[') return std::string_view::npos;
  size_t depth = 0;
  bool in_string = false;
  bool escaped = false;
  for (size_t i = start; i < value.size(); ++i) {
    const char ch = value[i];
    if (in_string) {
      if (escaped) escaped = false;
      else if (ch == '\\') escaped = true;
      else if (ch == '"') in_string = false;
      continue;
    }
    if (ch == '"') {
      in_string = true;
    } else if (ch == '[') {
      ++depth;
    } else if (ch == ']') {
      if (depth == 0) return std::string_view::npos;
      if (--depth == 0) return i + 1;
    }
  }
  return std::string_view::npos;
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
        const std::wstring service_path = ServicePath();
        if (!service_path.empty()) {
          STARTUPINFOW startup{}; startup.cb = sizeof(startup); PROCESS_INFORMATION process{};
          std::wstring command = L"\"" + service_path + L"\"";
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
  uint64_t generation;
  CompositionSession(TextService* o, ITfContext* c, TextService::Action a,
                     std::wstring t, uint64_t g)
      : owner(o), context(c), action(a), text(std::move(t)), generation(g) {
    owner->AddRef();
  }
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

bool ReadPrecedingRange(ITfContext* context, TfEditCookie cookie, uint32_t limit,
                        std::wstring& text, ITfRange* composition_range = nullptr) {
  text.clear();
  ComPtr<ITfRange> range;
  if (composition_range) {
    if (FAILED(composition_range->Clone(&range)) || !range) return false;
  } else {
    TF_SELECTION selection{};
    ULONG fetched = 0;
    const HRESULT hr = context->GetSelection(cookie, TF_DEFAULT_SELECTION, 1,
                                              &selection, &fetched);
    if (FAILED(hr) || fetched != 1 || !selection.range) return false;
    range.Attach(selection.range);
  }
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
    if (read) text.assign(buffer.data(), read);
    return true;
  }
  LONG moved = 0;
  if (FAILED(before->ShiftStart(cookie, -static_cast<LONG>(limit), &moved, nullptr))) return false;
  const ULONG count = static_cast<ULONG>(std::max<LONG>(0, -moved));
  std::vector<wchar_t> buffer(count); ULONG read = 0;
  if (count && FAILED(before->GetText(cookie, 0, buffer.data(), count, &read))) return false;
  if (read) text.assign(buffer.data(), read);
  return true;
}

class CandidateWindow {
 public:
  void Show(ITfContext* context, const std::vector<TextService::Candidate>& candidates,
            size_t page, size_t selected, size_t page_size,
            std::wstring_view preedit = {}, std::wstring_view preview = {}) {
#if defined(LIME_WITH_WEASEL_UI)
    weasel_ui_.Show(context, candidates, page, selected, page_size, preedit, preview);
    return;
#else
    Snapshot snapshot;
    snapshot.visible = true;
    snapshot.anchor = Anchor(context);
    snapshot.preview = std::wstring(preview);
    const size_t begin = page * page_size;
    for (size_t i = begin; i < std::min(candidates.size(), begin + page_size); ++i) {
      Row row;
      row.number = std::to_wstring(i - begin + 1);
      row.text = candidates[i].display;
      row.selected = i == selected;
      snapshot.rows.push_back(std::move(row));
    }
    Update(std::move(snapshot));
#endif
  }
  void ShowStatus(ITfContext* context, std::wstring_view message) {
#if defined(LIME_WITH_WEASEL_UI)
    weasel_ui_.ShowStatus(context, message);
    return;
#else
    Snapshot snapshot;
    snapshot.visible = true;
    snapshot.status = std::wstring(message);
    snapshot.is_status = true;
    snapshot.anchor = Anchor(context);
    Update(std::move(snapshot));
#endif
  }
  void Hide() {
#if defined(LIME_WITH_WEASEL_UI)
    weasel_ui_.Hide();
    return;
#else
    if (!thread_.joinable()) return;
    Snapshot snapshot;
    snapshot.visible = false;
    Update(std::move(snapshot));
#endif
  }

  ~CandidateWindow() { Stop(); }

 private:
  struct Row {
    std::wstring number;
    std::wstring text;
    bool selected = false;
  };
  struct Snapshot {
    RECT anchor{200, 200, 200, 220};
    std::vector<Row> rows;
    std::wstring preview;
    std::wstring status;
    bool visible = false;
    bool is_status = false;
  };

  static constexpr UINT kUpdateMessage = WM_APP + 41;

  RECT Anchor(ITfContext* context) const {
    GUITHREADINFO info{};
    info.cbSize = sizeof(info);
    HWND foreground = GetForegroundWindow();
    const DWORD thread_id = foreground ? GetWindowThreadProcessId(foreground, nullptr) : 0;
    if (thread_id && GetGUIThreadInfo(thread_id, &info) && info.hwndCaret &&
        info.rcCaret.bottom > info.rcCaret.top) {
      // rcCaret is relative to hwndCaret.  The popup renderer consumes screen
      // coordinates, so translate both corners before handing it the anchor.
      POINT top_left{info.rcCaret.left, info.rcCaret.top};
      POINT bottom_right{info.rcCaret.right, info.rcCaret.bottom};
      if (ClientToScreen(info.hwndCaret, &top_left) &&
          ClientToScreen(info.hwndCaret, &bottom_right)) {
        return RECT{top_left.x, top_left.y, bottom_right.x, bottom_right.y};
      }
    }
    ComPtr<ITfContextView> view;
    HWND hwnd = nullptr;
    if (context && SUCCEEDED(context->GetActiveView(&view)) && view) view->GetWnd(&hwnd);
    RECT rect{200, 200, 200, 220};
    if (hwnd && GetWindowRect(hwnd, &rect)) {
      rect.left += 16;
      rect.right = rect.left + 1;
      rect.top += 32;
      rect.bottom = rect.top + 20;
    }
    return rect;
  }

  void Update(Snapshot snapshot) {
    Ensure();
    HWND hwnd = nullptr;
    {
      std::lock_guard lock(state_mutex_);
      snapshot_ = std::move(snapshot);
      hwnd = window_;
    }
    if (hwnd) PostMessageW(hwnd, kUpdateMessage, 0, 0);
  }

  void Ensure() {
    std::call_once(start_once_, [this] {
      thread_ = std::thread([this] { UiThread(); });
      std::unique_lock lock(ready_mutex_);
      ready_cv_.wait(lock, [this] { return ready_; });
    });
  }

  void Stop() {
    if (!thread_.joinable()) return;
    HWND hwnd = nullptr;
    {
      std::lock_guard lock(state_mutex_);
      hwnd = window_;
    }
    if (hwnd) PostMessageW(hwnd, WM_CLOSE, 0, 0);
    thread_.join();
  }

  void UiThread() {
    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    WNDCLASSW wc{};
    wc.style = CS_DROPSHADOW;
    wc.lpfnWndProc = &CandidateWindow::Proc;
    wc.hInstance = g_instance;
    wc.lpszClassName = L"LimeCandidateWindowV2";
    wc.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    wc.hbrBackground = nullptr;
    RegisterClassW(&wc);
    HWND hwnd = CreateWindowExW(WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
                                wc.lpszClassName, L"Lime", WS_POPUP,
                                0, 0, 460, 120, nullptr, nullptr, g_instance, this);
    {
      std::lock_guard lock(state_mutex_);
      window_ = hwnd;
    }
    {
      std::lock_guard lock(ready_mutex_);
      ready_ = true;
    }
    ready_cv_.notify_all();
    if (!hwnd) return;
    MSG msg{};
    while (GetMessageW(&msg, nullptr, 0, 0) > 0) {
      TranslateMessage(&msg);
      DispatchMessageW(&msg);
    }
    {
      std::lock_guard lock(state_mutex_);
      window_ = nullptr;
    }
  }

  void Render(HWND hwnd) {
    Snapshot snapshot;
    {
      std::lock_guard lock(state_mutex_);
      snapshot = snapshot_;
    }
    if (!snapshot.visible) {
      KillTimer(hwnd, 1);
      ShowWindow(hwnd, SW_HIDE);
      return;
    }
    const UINT dpi = std::max<UINT>(96, GetDpiForWindow(hwnd));
    const int scale = static_cast<int>(dpi) / 96;
    const int width = 460 * scale;
    const int row_height = 36 * scale;
    const int header_height = snapshot.preview.empty() ? 12 * scale : 40 * scale;
    const int content_rows = snapshot.is_status ? 1 : static_cast<int>(snapshot.rows.size());
    const int height = std::max(54 * scale, header_height + content_rows * row_height + 12 * scale);
    const int x = snapshot.anchor.left;
    const int y = snapshot.anchor.bottom + 4 * scale;
    SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    HRGN region = CreateRoundRectRgn(0, 0, width + 1, height + 1, 14 * scale, 14 * scale);
    if (region) SetWindowRgn(hwnd, region, TRUE);
    if (snapshot.is_status) SetTimer(hwnd, 1, 1800, nullptr); else KillTimer(hwnd, 1);
    InvalidateRect(hwnd, nullptr, TRUE);
  }

  void Paint(HWND hwnd, HDC dc) {
    Snapshot snapshot;
    {
      std::lock_guard lock(state_mutex_);
      snapshot = snapshot_;
    }
    RECT client{};
    GetClientRect(hwnd, &client);
    const UINT dpi = std::max<UINT>(96, GetDpiForWindow(hwnd));
    const int scale = static_cast<int>(dpi) / 96;
    HBRUSH background = CreateSolidBrush(RGB(255, 255, 255));
    FillRect(dc, &client, background);
    DeleteObject(background);
    SetBkMode(dc, TRANSPARENT);
    HFONT font = CreateFontW(-16 * scale, 0, 0, 0, FW_NORMAL, FALSE, FALSE, FALSE,
                             DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS,
                             CLEARTYPE_QUALITY, DEFAULT_PITCH | FF_DONTCARE, L"Segoe UI");
    HFONT old_font = static_cast<HFONT>(SelectObject(dc, font));
    int y = 8 * scale;
    if (snapshot.is_status) {
      SetTextColor(dc, RGB(180, 55, 55));
      RECT text_rect{16 * scale, y, client.right - 16 * scale, client.bottom - 8 * scale};
      DrawTextW(dc, snapshot.status.c_str(), -1, &text_rect, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
    } else {
      if (!snapshot.preview.empty()) {
        SetTextColor(dc, RGB(110, 118, 130));
        RECT preview_rect{16 * scale, y, client.right - 16 * scale, y + 24 * scale};
        DrawTextW(dc, snapshot.preview.c_str(), -1, &preview_rect, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        y += 34 * scale;
      }
      for (const Row& row : snapshot.rows) {
        RECT row_rect{8 * scale, y, client.right - 8 * scale, y + 32 * scale};
        if (row.selected) {
          HBRUSH selected = CreateSolidBrush(RGB(37, 99, 235));
          FillRect(dc, &row_rect, selected);
          DeleteObject(selected);
          SetTextColor(dc, RGB(255, 255, 255));
        } else {
          SetTextColor(dc, RGB(30, 35, 45));
        }
        RECT number_rect{18 * scale, y, 46 * scale, y + 32 * scale};
        DrawTextW(dc, row.number.c_str(), -1, &number_rect, DT_SINGLELINE | DT_VCENTER | DT_CENTER);
        RECT text_rect{58 * scale, y, client.right - 18 * scale, y + 32 * scale};
        DrawTextW(dc, row.text.c_str(), -1, &text_rect, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        y += 36 * scale;
      }
    }
    SelectObject(dc, old_font);
    DeleteObject(font);
    HPEN pen = CreatePen(PS_SOLID, std::max(1, scale), RGB(224, 228, 235));
    HGDIOBJ old_pen = SelectObject(dc, pen);
    HGDIOBJ old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    Rectangle(dc, 0, 0, client.right, client.bottom);
    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    DeleteObject(pen);
  }

  static LRESULT CALLBACK Proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp) {
    auto* self = reinterpret_cast<CandidateWindow*>(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
    if (msg == WM_NCCREATE) {
      self = static_cast<CandidateWindow*>(reinterpret_cast<CREATESTRUCTW*>(lp)->lpCreateParams);
      SetWindowLongPtrW(hwnd, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(self));
    }
    if (msg == kUpdateMessage && self) { self->Render(hwnd); return 0; }
    if (msg == WM_PAINT && self) {
      PAINTSTRUCT ps{};
      HDC dc = BeginPaint(hwnd, &ps);
      self->Paint(hwnd, dc);
      EndPaint(hwnd, &ps);
      return 0;
    }
    if (msg == WM_TIMER && self) { KillTimer(hwnd, 1); ShowWindow(hwnd, SW_HIDE); return 0; }
    if (msg == WM_MOUSEWHEEL && self) {
      Snapshot snapshot;
      {
        std::lock_guard lock(self->state_mutex_);
        snapshot = self->snapshot_;
      }
      if (snapshot.visible && !snapshot.is_status && !snapshot.rows.empty()) {
        const bool next = GET_WHEEL_DELTA_WPARAM(wp) < 0;
        InjectNavigationKey(next ? VK_NEXT : VK_PRIOR);
      }
      return 0;
    }
    if (msg == WM_MOUSEACTIVATE) return MA_NOACTIVATE;
    if (msg == WM_ERASEBKGND) return 1;
    if (msg == WM_CLOSE) { DestroyWindow(hwnd); return 0; }
    if (msg == WM_DESTROY) { PostQuitMessage(0); return 0; }
    return DefWindowProcW(hwnd, msg, wp, lp);
  }
  HWND window_ = nullptr;
  Snapshot snapshot_;
  std::mutex state_mutex_;
  std::once_flag start_once_;
  std::thread thread_;
  std::mutex ready_mutex_;
  std::condition_variable ready_cv_;
  bool ready_ = false;
#if defined(LIME_WITH_WEASEL_UI)
  WeaselUiAdapter weasel_ui_;
#endif
};

CandidateWindow g_candidates;
PipeClient g_pipe;

}  // namespace

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
  // A Commit request can represent either the end of a live composition or a
  // standalone direct insertion (ASCII mode/full-shape punctuation).  Avoid
  // creating a transient TSF composition for the latter; InsertTextAtSelection
  // follows the host's normal insertion path and keeps ASCII mode invisible.
  if (action == TextService::Action::Commit && !owner->HasComposition()) {
    succeeded = owner->InsertTextAtSelection(context.Get(), cookie, text);
    owner->CompleteEditSession(action, generation, succeeded);
    return succeeded ? S_OK : E_FAIL;
  }
  if (!owner->EnsureComposition(context.Get(), cookie)) {
    owner->CompleteEditSession(action, generation, false);
    return E_FAIL;
  }
  if (action == TextService::Action::Update) {
    succeeded = owner->SetCompositionText(cookie, text);
  } else {
    succeeded = owner->CommitComposition(cookie, text);
  }
  owner->CompleteEditSession(action, generation, succeeded);
  return succeeded ? S_OK : E_FAIL;
}

TextService::TextService() { ++g_module_references; }
TextService::~TextService() { Deactivate(); --g_module_references; }

HRESULT TextService::QueryInterface(REFIID iid, void** object) {
  if (!object) return E_POINTER; *object = nullptr;
  if (iid == IID_IUnknown || iid == IID_ITfTextInputProcessor || iid == IID_ITfTextInputProcessorEx) *object = static_cast<ITfTextInputProcessorEx*>(this);
  else if (iid == IID_ITfKeyEventSink) *object = static_cast<ITfKeyEventSink*>(this);
  else if (iid == IID_ITfCompositionSink) *object = static_cast<ITfCompositionSink*>(this);
  else return E_NOINTERFACE;
  AddRef(); return S_OK;
}
ULONG TextService::AddRef() { return ++references_; }
ULONG TextService::Release() { const ULONG v = --references_; if (!v) delete this; return v; }

HRESULT TextService::Activate(ITfThreadMgr* manager, TfClientId client_id) { return ActivateEx(manager, client_id, 0); }
HRESULT TextService::ActivateEx(ITfThreadMgr* manager, TfClientId client_id, DWORD flags) {
  if (!manager) return E_INVALIDARG;
  const HRESULT deactivated = Deactivate();
  if (FAILED(deactivated)) return deactivated;
  thread_manager_ = manager; client_id_ = client_id; activation_flags_ = flags;
  HRESULT hr = manager->QueryInterface(IID_PPV_ARGS(&keystroke_manager_)); if (FAILED(hr)) return hr;
  hr = keystroke_manager_->AdviseKeyEventSink(client_id_, this, TRUE); if (FAILED(hr)) { keystroke_manager_.Reset(); return hr; }
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
  single_quote_open_ = false;
  double_quote_open_ = false;
  if (keystroke_manager_ && client_id_ != TF_CLIENTID_NULL) keystroke_manager_->UnadviseKeyEventSink(client_id_);
  keystroke_manager_.Reset(); thread_manager_.Reset(); client_id_ = TF_CLIENTID_NULL; activation_flags_ = 0; return S_OK;
}
HRESULT TextService::OnSetFocus(BOOL foreground) {
  if (foreground) return S_OK;
  shift_down_mask_ = 0;
  shift_pending_mask_ = 0;
  left_shift_down_tick_ = 0;
  right_shift_down_tick_ = 0;
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
  composition_.Reset();
  composition_context_.Reset();
  ClearCompositionState();
  active_context_.Reset();
  return S_OK;
}

void TextService::CompleteEditSession(Action action, uint64_t generation,
                                      bool succeeded) {
  if (!IsEditCurrent(generation)) return;
  last_edit_pending_ = false;
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
wchar_t TextService::PreeditChar(WPARAM key) const {
  // Ctrl/Alt/Win combinations belong to the host (copy, paste, shortcuts,
  // AltGr, shell commands, ...).  Only an unmodified or Shift-modified
  // letter may enter the Rime snapshot path.
  if (HasNonTextModifier()) return 0;
  if (key >= 'A' && key <= 'Z') {
    const wchar_t letter = static_cast<wchar_t>(key);
    return UppercaseLetterActive() ? letter : static_cast<wchar_t>(letter + ('a' - 'A'));
  }
  if (key >= 'a' && key <= 'z') {
    const wchar_t letter = static_cast<wchar_t>(key);
    return UppercaseLetterActive() ? static_cast<wchar_t>(letter - ('a' - 'A')) : letter;
  }
  if (key == VK_OEM_3) return L'`';
  if (key == VK_OEM_7) return L'\'';
  return 0;
}
bool TextService::IsPrintable(WPARAM key) const {
  return IsPreeditKey(key);
}
bool TextService::IsPreeditKey(WPARAM key) const {
  if (HasNonTextModifier()) return false;
  if (key == VK_OEM_3 || key == VK_OEM_7) {
    // The unshifted OEM keys are Rime preedit delimiters.  Shifted variants
    // produce `~`/`\"` and must continue through the Chinese punctuator path.
    if ((GetKeyState(VK_SHIFT) & 0x8000) != 0) return false;
    return !preedit_.empty();
  }
  return (key >= 'A' && key <= 'Z') || (key >= 'a' && key <= 'z');
}
std::wstring TextService::AsciiText(WPARAM key) const {
  if (HasNonTextModifier()) return {};
  const bool shift = (GetKeyState(VK_SHIFT) & 0x8000) != 0;
  const bool caps_lock = (GetKeyState(VK_CAPITAL) & 0x0001) != 0;
  const bool num_lock = (GetKeyState(VK_NUMLOCK) & 0x0001) != 0;
  const wchar_t value = AsciiCharForVirtualKey(key, shift, caps_lock, num_lock);
  return value ? std::wstring(1, value) : std::wstring();
}
bool TextService::IsChinesePunctuationKey(WPARAM key) const {
  if (HasNonTextModifier()) return false;
  const std::wstring text = AsciiText(key);
  return text.size() == 1 && IsPunctuationCharacter(text.front());
}
std::wstring TextService::ChinesePunctuationText(WPARAM key) {
  const std::wstring ascii = AsciiText(key);
  if (ascii.size() != 1 || !IsPunctuationCharacter(ascii.front())) return {};
  const wchar_t value = ascii.front();
  if (value == L'\'') {
    const wchar_t quote = single_quote_open_ ? L'’' : L'‘';
    single_quote_open_ = !single_quote_open_;
    return std::wstring(1, quote);
  }
  if (value == L'"') {
    const wchar_t quote = double_quote_open_ ? L'”' : L'“';
    double_quote_open_ = !double_quote_open_;
    return std::wstring(1, quote);
  }
  const std::wstring_view mapped = FullShapeForAscii(value);
  return mapped.empty() ? std::wstring() : std::wstring(mapped);
}
bool TextService::IsImeKey(WPARAM key) const {
  return IsPreeditKey(key) || key == VK_BACK || key == VK_RETURN ||
         (key >= '1' && key <= '9') || key == VK_SPACE || key == VK_ESCAPE ||
         IsPreviousPageKey(key) || IsNextPageKey(key) || key == VK_UP ||
         key == VK_DOWN;
}
HRESULT TextService::OnTestKeyDown(ITfContext* context, WPARAM key, LPARAM lparam,
                                   BOOL* eaten) {
  if (!eaten) return E_POINTER;
  try {
    // A non-Shift key turns a possible bare-Shift tap into a chord.  This is
    // deliberately done in the probe so OnKeyDown sees the same state even
    // when the host invokes both callbacks under different TSF locks.
    if (!IsShiftKey(key)) shift_pending_mask_ = 0;
    if (IsShiftKey(key) && ShiftKeyBit(key, lparam) == kRightShiftBit) {
      // Shift_R is configured as a no-op by the bundled Rime schema.  It is
      // still a chord if pressed while Shift_L is pending, so cancel the
      // pending left-toggle before letting the host receive the key.
      shift_pending_mask_ = 0;
      *eaten = FALSE;
      return S_OK;
    }
    if (cancel_pending_) {
      if (!ResolvePendingCancellation()) {
        if (!passthrough_notified_) {
          g_candidates.ShowStatus(context, L"Lime：正在结束上一个输入状态");
          passthrough_notified_ = true;
        }
        // Do not let a new host context receive a key while the old
        // composition is still alive.  That would mix two TSF contexts.
        *eaten = TRUE;
        return S_OK;
      }
    }
    if (terminal_edit_pending_ && key != VK_ESCAPE) {
      // A commit/cancel accepted asynchronously owns the composition until
      // its edit session callback runs.  Dropping another key into the host
      // here would race the pending range mutation.
      *eaten = TRUE;
      return S_OK;
    }
    if (IsShiftKey(key)) {
      const uint8_t bit = ShiftKeyBit(key, lparam);
      // Shift is a switch key only when no Ctrl/Alt/Win modifier participates
      // in the chord.  The key itself is consumed so the host cannot treat a
      // bare Shift as an ordinary accelerator.
      *eaten = HasNonTextModifier() ? FALSE : TRUE;
      return S_OK;
    }
    if (context) active_context_ = context;
    // OnTestKeyDown is only a probe.  Do not open a read edit session or call
    // the service here: some hosts keep the probe inside their own TSF lock,
    // which makes the real write session in OnKeyDown return TF_E_LOCKED.
    // Candidate fetching and context reads happen exactly once in OnKeyDown.
    // In ASCII mode librime rejects ordinary text keys when no composition is
    // active, letting the host insert them with its own keyboard layout.  Do
    // the same here instead of synthesizing a second TSF edit session.
    if (ascii_mode_ &&
        !(key == VK_ESCAPE &&
          (composition_ || !preedit_.empty() || terminal_edit_pending_))) {
      *eaten = FALSE;
      return S_OK;
    }
    if ((GetKeyState(VK_SHIFT) & 0x8000) != 0 && key == VK_SPACE) {
      // Shift+Space is explicitly not an ascii-composer switch gesture.
      *eaten = FALSE;
      return S_OK;
    }
    if (IsPreeditKey(key) || IsChinesePunctuationKey(key)) {
      if (!connected_) RefreshConfigRevision();
      if (!connected_) {
        if (!passthrough_notified_) {
          g_candidates.ShowStatus(context, L"Lime：服务不可用，英文透传");
          passthrough_notified_ = true;
        }
        *eaten = FALSE;
        return S_OK;
      }
    }
  } catch (...) {
    connected_ = false;
    if (!passthrough_notified_) {
      g_candidates.ShowStatus(context, L"Lime：服务异常，英文透传");
      passthrough_notified_ = true;
    }
  }
  if (ascii_mode_) {
    // No composition is kept in persistent ASCII mode; reject the key so the
    // host commits letters, digits and half-width punctuation directly.
    *eaten = FALSE;
  } else if ((GetKeyState(VK_SHIFT) & 0x8000) != 0 && key == VK_SPACE) {
    *eaten = FALSE;
  } else if (IsPreeditKey(key)) {
    *eaten = connected_ ? TRUE : FALSE;
  } else if (IsChinesePunctuationKey(key)) {
    *eaten = connected_ ? TRUE : FALSE;
  } else if (key == VK_ESCAPE &&
             (composition_ || !preedit_.empty() || terminal_edit_pending_)) {
    // The host must never see Esc while a TSF composition is still alive,
    // even if the service connection has dropped or a prior commit was
    // accepted asynchronously.  OnKeyDown will clear the range (or keep the
    // key consumed until the queued cancellation completes).
    *eaten = TRUE;
  } else if (IsPreviousPageKey(key) || IsNextPageKey(key) || key == VK_UP ||
             key == VK_DOWN) {
    // Navigation belongs to Lime only while a candidate page is visible;
    // otherwise the host must retain its normal scrolling/caret behavior.
    *eaten = !candidates_.empty() && connected_ ? TRUE : FALSE;
  } else {
    *eaten = IsImeKey(key) && !preedit_.empty() && connected_ ? TRUE : FALSE;
  }
  return S_OK;
}
HRESULT TextService::OnTestKeyUp(ITfContext*, WPARAM key, LPARAM lparam, BOOL* eaten) {
  if (!eaten) return E_POINTER;
  if (!IsShiftKey(key)) {
    *eaten = FALSE;
    return S_OK;
  }
  const uint8_t bit = ShiftKeyBit(key, lparam);
  if (bit == kRightShiftBit) {
    *eaten = FALSE;
    return S_OK;
  }
  *eaten = (bit != 0 && (shift_pending_mask_ & bit) != 0) ? TRUE : FALSE;
  return S_OK;
}
HRESULT TextService::OnKeyDown(ITfContext* context, WPARAM key, LPARAM lparam,
                               BOOL* eaten) {
  if (!eaten) return E_POINTER;
  try {
    if (IsShiftKey(key)) {
      const uint8_t bit = ShiftKeyBit(key, lparam);
      if (bit == kRightShiftBit) {
        shift_pending_mask_ = 0;
        *eaten = FALSE;
        return S_OK;
      }
      const bool first_press = bit != 0 && (shift_down_mask_ & bit) == 0;
      if (bit != 0) {
        if (first_press) {
          const bool first_shift = shift_down_mask_ == 0;
          shift_down_mask_ |= bit;
          const ULONGLONG now = GetTickCount64();
          if (bit == kLeftShiftBit) {
            left_shift_down_tick_ = now;
          } else {
            right_shift_down_tick_ = now;
          }
          // Do not make Ctrl/Alt/Win chords look like a bare switch.  A
          // second Shift held alongside the first also shares the original
          // tap, preventing a double toggle when both keys are released.
          if (first_shift && !HasNonTextModifier() && !cancel_pending_ &&
              !terminal_edit_pending_ && !IsKeyRepeat(lparam)) {
            shift_pending_mask_ |= bit;
          } else {
            // A second Shift key is a chord, not a second tap.  Clear the
            // original pending bit as well so releasing either key cannot
            // toggle the mode after both keys were held together.
            shift_pending_mask_ = 0;
          }
        }
      }
      if (cancel_pending_ && !ResolvePendingCancellation()) {
        *eaten = TRUE;
        return S_OK;
      }
      if (terminal_edit_pending_) {
        *eaten = TRUE;
        return S_OK;
      }
      *eaten = HasNonTextModifier() ? FALSE : TRUE;
      return S_OK;
    }
    // Any non-Shift key cancels a pending bare-Shift tap, including Ctrl/Alt/
    // Win themselves.  This preserves host shortcut handling.
    shift_pending_mask_ = 0;
    if (cancel_pending_ && !ResolvePendingCancellation()) {
      *eaten = TRUE;
      return S_OK;
    }
    if (terminal_edit_pending_ && key != VK_ESCAPE) {
      *eaten = TRUE;
      return S_OK;
    }
    if (context) active_context_ = context;
    *eaten = context && HandleKey(context, key) ? TRUE : FALSE;
  } catch (...) {
    *eaten = FALSE;
  }
  return S_OK;
}
HRESULT TextService::OnKeyUp(ITfContext* context, WPARAM key, LPARAM lparam,
                             BOOL* eaten) {
  if (!eaten) return E_POINTER;
  try {
    if (!IsShiftKey(key)) {
      *eaten = FALSE;
      return S_OK;
    }
    const uint8_t bit = ShiftKeyBit(key, lparam);
    if (bit == kRightShiftBit) {
      *eaten = FALSE;
      return S_OK;
    }
    const bool pending = bit != 0 && (shift_pending_mask_ & bit) != 0;
    const ULONGLONG down_tick = bit == kRightShiftBit ? right_shift_down_tick_
                                                      : left_shift_down_tick_;
    const ULONGLONG now = GetTickCount64();
    const bool short_tap = pending && down_tick != 0 &&
                           now >= down_tick && now - down_tick <= kShiftTapTimeoutMs;
    if (bit != 0) {
      shift_down_mask_ &= static_cast<uint8_t>(~bit);
      shift_pending_mask_ &= static_cast<uint8_t>(~bit);
    }
    if (short_tap && !HasNonTextModifier()) {
      if (context) active_context_ = context;
      if (cancel_pending_ && !ResolvePendingCancellation()) {
        *eaten = TRUE;
        return S_OK;
      }
      // ToggleAsciiMode reports edit failures through the native status popup;
      // either way Shift itself remains consumed and must not reach the host.
      ToggleAsciiMode(context);
      *eaten = TRUE;
      return S_OK;
    }
    // A long hold or a Shift chord has no mode-switch side effect.  The keydown
    // was consumed for an eligible bare Shift, so consume its keyup as well.
    *eaten = pending ? TRUE : FALSE;
    return S_OK;
  } catch (...) {
    *eaten = FALSE;
    return S_OK;
  }
}
HRESULT TextService::OnPreservedKey(ITfContext*, REFGUID, BOOL* eaten) { if (!eaten) return E_POINTER; *eaten = FALSE; return S_OK; }

bool TextService::ToggleAsciiMode(ITfContext* context) {
  const bool target_ascii = !ascii_mode_;
  if (!preedit_.empty() || composition_) {
    // The bundled Weasel/Rime configuration uses Shift_L: commit_code, so a
    // pending composition is committed exactly as the user typed it.
    ITfContext* composition_owner = composition_context_ ? composition_context_.Get()
                                                         : context;
    // Mode switching is a user-visible state transition.  Require the edit to
    // complete synchronously so a queued terminal edit cannot swallow the
    // next ASCII key before the mode has actually changed.
    if (!RequestEdit(composition_owner, Action::Commit, preedit_, true)) {
      const std::wstring reason = last_edit_error_ == S_OK
                                      ? L"切换模式时无法提交组合串"
                                      : EditErrorText(last_edit_error_);
      g_candidates.ShowStatus(context, std::wstring(L"Lime：") + reason);
      return false;
    }
    if (terminal_edit_pending_) {
      HideCandidates();
    } else {
      ClearCompositionState();
    }
  }
  ascii_mode_ = target_ascii;
  // Quote pairing is scoped to Chinese punctuation mode.  Resetting it at a
  // mode boundary mirrors a fresh Rime punctuation processor and avoids a
  // stale opening quote after a long ASCII session.
  single_quote_open_ = false;
  double_quote_open_ = false;
  g_candidates.ShowStatus(context, ascii_mode_ ? L"Lime：英文模式" : L"Lime：中文模式");
  return true;
}

bool TextService::HandleKey(ITfContext* context, WPARAM key) {
  // Persistent ASCII mode deliberately leaves ordinary keys to the host, as
  // Weasel does when its ascii_composer has no active composition.  This
  // preserves the user's Windows keyboard layout and all half-width symbols.
  if (ascii_mode_ &&
      !(key == VK_ESCAPE &&
        (composition_ || !preedit_.empty() || terminal_edit_pending_))) {
    return false;
  }

  // Esc must remain a local cancellation even while a schema reset is waiting
  // on a rejected edit lock; otherwise the host could receive it with a live
  // composition still attached.
  if (schema_reset_pending_ && key != VK_ESCAPE &&
      !ResetCompositionForSchemaChange(context)) {
    return false;
  }
  if ((GetKeyState(VK_SHIFT) & 0x8000) != 0 && key == VK_SPACE) {
    // Match ascii_composer's explicit Shift+Space no-op.  In particular, do
    // not accidentally turn it into a persistent mode switch or a full-width
    // space while the user is holding Shift for another host gesture.
    return false;
  }
  if (IsPreeditKey(key)) {
    wchar_t value[2] = {PreeditChar(key), 0};
    preedit_ += value;
    if (!UpdateCandidates(context)) {
      // A live service plus a rejected TSF write lock is a transient editor
      // condition, not a reason to leak the key to the host. Keep the
      // attempted preedit so a following key/retry can complete it, and make
      // the failure visible. Only fall back to English when the service path
      // itself is unavailable.
      if (connected_) {
        const std::wstring reason = last_edit_error_ == S_OK
                                        ? L"编辑器拒绝组合串"
                                        : EditErrorText(last_edit_error_);
        g_candidates.ShowStatus(context, std::wstring(L"Lime：") + reason);
        return true;
      }
      preedit_.pop_back();
      return false;
    }
    if (candidates_.empty()) {
      // Rime has no match for this snapshot. Cancel the native composition so
      // the host can receive the original printable key unchanged.
      const bool canceled = CancelComposition(context);
      if (canceled) {
        ClearCompositionState();
      } else {
        // Do not forward the key while an asynchronous/rejected cancel can
        // still leave the old composition range alive.  Resolve it before
        // accepting the next key instead.
        cancel_pending_ = true;
        HideCandidates();
      }
      if (!passthrough_notified_) {
        g_candidates.ShowStatus(context, L"Lime：无候选，英文透传");
        passthrough_notified_ = true;
      }
      return canceled ? false : true;
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
    const std::wstring pinyin = preedit_;
    const std::wstring commit = candidates_[index].commit;
    if (!RequestEdit(context, Action::Commit, commit)) {
      // The probe already told the host that this key belongs to Lime.  Keep
      // it consumed when TSF rejects the edit; forwarding it would insert a
      // digit into the host while the old composition is still active.
      g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_,
                        page_size_, {}, preceding_preview_);
      return true;
    }
    LearnCandidate(pinyin, commit);
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
      const std::wstring pinyin = preedit_;
      const std::wstring commit = candidates_[index].commit;
      if (!RequestEdit(context, Action::Commit, commit)) {
        g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_,
                          page_size_, {}, preceding_preview_);
        return true;
      }
      LearnCandidate(pinyin, commit);
      if (terminal_edit_pending_) {
        HideCandidates();
      } else {
        ClearCompositionState();
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
      g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_,
                        page_size_, {}, preceding_preview_);
    } else if (next_page &&
               (candidate_page_ + 1) * page_size_ < candidates_.size()) {
      ++candidate_page_;
      selected_candidate_ = candidate_page_ * page_size_;
      g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_,
                        page_size_, {}, preceding_preview_);
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
    } else if ((candidate_page_ + 1) * page_size_ < candidates_.size()) {
      ++candidate_page_;
      selected_candidate_ = candidate_page_ * page_size_;
    }
    g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_,
                      page_size_, {}, preceding_preview_);
    return true;
  }
  if (IsChinesePunctuationKey(key)) {
    // Rime's punctuator commits a pending composition before emitting a
    // full-shape symbol.  Keep that behavior in the snapshot-based adapter;
    // when there is no composition, insert the symbol directly at the host
    // selection through the same TSF edit-session path.
    // Punctuation has no input request that could refresh the connection, so
    // re-check the service state before claiming the key.  This closes the
    // small window where the process went unavailable after the last letter.
    RefreshConfigRevision(context);
    if (!connected_) return false;
    const bool previous_single_quote = single_quote_open_;
    const bool previous_double_quote = double_quote_open_;
    const std::wstring punctuation = ChinesePunctuationText(key);
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
                                  std::wstring& preceding, bool& context_available) {
  if (!context) return false;
  result_candidates.clear();
  preceding.clear();
  context_available = false;
  // Read context in a read-only edit session, then synchronously ask the local service.
  class ReadSession final : public ITfEditSession {
   public: std::atomic<ULONG> refs{1}; TextService* owner; ComPtr<ITfContext> ctx; ComPtr<ITfComposition> composition; std::wstring* before; bool* available; ReadSession(TextService* o, ITfContext* c, ITfComposition* comp, std::wstring* b, bool* a):owner(o),ctx(c),composition(comp),before(b),available(a){owner->AddRef();} ~ReadSession(){owner->Release();}
   HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** p) override { if(!p)return E_POINTER;*p=nullptr;if(iid!=IID_IUnknown&&iid!=IID_ITfEditSession)return E_NOINTERFACE;*p=static_cast<ITfEditSession*>(this);AddRef();return S_OK; }
   ULONG STDMETHODCALLTYPE AddRef() override{return ++refs;} ULONG STDMETHODCALLTYPE Release() override{auto v=--refs;if(!v)delete this;return v;}
   HRESULT STDMETHODCALLTYPE DoEditSession(TfEditCookie c) override {
      ComPtr<ITfRange> composition_range;
      if (composition) composition->GetRange(&composition_range);
      *available = ReadPrecedingRange(ctx.Get(), c, owner->ContextLimit(), *before,
                                      composition_range.Get());
     if (!*available) before->clear();
     return S_OK;
   }
     } session(this, context, composition_.Get(), &preceding, &context_available);
  HRESULT result = E_FAIL, request = context->RequestEditSession(client_id_, &session, TF_ES_READ | TF_ES_SYNC, &result);
  if (FAILED(request) || FAILED(result)) { preceding.clear(); context_available = false; }
  std::string body; const std::string json = "{\"kind\":\"input\",\"payload\":{\"request_id\":" + std::to_string(++request_id_) + ",\"preedit\":\"" + JsonEscape(preedit) + "\",\"preceding_text\":\"" + JsonEscape(preceding) + "\",\"context_available\":" + (context_available ? "true" : "false") + ",\"config_revision\":" + std::to_string(config_revision_) + "}}";
   if (!g_pipe.Request(json, body)) { connected_ = false; return false; }
   if (body.find("\"kind\":\"error\"") != std::string::npos) { RefreshConfigRevision(context); return false; }
    std::string service_state;
    if (!JsonField(body, "service_state", service_state) ||
        (service_state != "ready" && service_state != "rime_only")) {
      connected_ = false;
      return false;
    }
    connected_ = true; passthrough_notified_ = false; const std::string candidates_needle = "\"candidates\":["; const size_t array = body.find(candidates_needle); if (array == std::string::npos) return false;
   const size_t array_end = JsonArrayEnd(body, array + candidates_needle.size() - 1); if (array_end == std::string::npos) return false;
   size_t pos = array + candidates_needle.size();
   while (pos < array_end) {
     const size_t d = body.find("\"display_text\":\"", pos); if (d == std::string::npos) break;
     const size_t c = body.find("\"commit_text\":\"", d); if (c == std::string::npos) break;
     if (d >= array_end || c >= array_end) break;
     Candidate candidate; candidate.display = Wide(JsonString(body, d + 16)); candidate.commit = Wide(JsonString(body, c + 15)); result_candidates.push_back(std::move(candidate)); pos = c + 15;
   }
  return true;
}

bool TextService::UpdateCandidates(ITfContext* context) {
  if (!context) return false;
  std::wstring preceding;
  bool context_available = false;
  if (!FetchCandidates(context, preedit_, candidates_, preceding, context_available)) {
    candidates_.clear();
    preceding_preview_.clear();
    HideCandidates();
    return false;
  }
  if (!RequestEdit(context, Action::Update, preedit_)) {
    candidates_.clear();
    preceding_preview_.clear();
    HideCandidates();
    return false;
  }
  preceding_preview_ = PreviewText(preceding, context_preview_limit_);
  candidate_page_ = 0; selected_candidate_ = 0; if (candidates_.empty()) HideCandidates(); else g_candidates.Show(context, candidates_, 0, 0, page_size_, {}, preceding_preview_); return true;
}

void TextService::RefreshConfigRevision(ITfContext* context) {
  std::string body;
  if (!g_pipe.Request(R"({"kind":"get_status"})", body)) { connected_ = false; return; }
  std::string kind;
  std::string state;
  if (!JsonField(body, "kind", kind) || kind != "status" ||
      !JsonField(body, "state", state) ||
      (state != "ready" && state != "rime_only")) {
    // Reloading/unavailable states intentionally fall back to the host.  The
    // TSF adapter must not eat letters or punctuation while the service cannot
    // produce a reliable Rime snapshot.
    connected_ = false;
    return;
  }
  connected_ = true;
  std::string schema;
  if (JsonField(body, "rime_schema", schema) && !schema.empty()) {
    const std::wstring next_schema = Wide(schema);
    if (!next_schema.empty() && next_schema != schema_id_) {
      schema_id_ = next_schema;
      schema_reset_pending_ = true;
      if (context) ResetCompositionForSchemaChange(context);
    }
  }
  uint64_t revision = 0;
  if (JsonNumber(body, "revision", revision)) config_revision_ = revision;
  uint64_t limit = 0;
  if (JsonNumber(body, "preceding_text_char_limit", limit)) context_limit_ = static_cast<uint32_t>(std::clamp<uint64_t>(limit, 1, 4096));
  uint64_t preview_limit = 0;
  if (JsonNumber(body, "context_preview_char_limit", preview_limit)) {
    context_preview_limit_ = static_cast<uint32_t>(std::clamp<uint64_t>(preview_limit, 0, 1024));
  }
  uint64_t page_size = 0;
  if (JsonNumber(body, "page_size", page_size)) {
    // The TSF key path intentionally exposes the standard 1-9 shortcuts.
    // Keep the visual page and the selectable range consistent until a
    // dedicated 10+ selection gesture is added.
    page_size_ = static_cast<uint32_t>(std::clamp<uint64_t>(page_size, 1, 9));
  }
}

bool TextService::ResetCompositionForSchemaChange(ITfContext* context) {
  if (!schema_reset_pending_) return true;
  if ((!preedit_.empty() || composition_) &&
      (!context || !CancelComposition(context))) {
    return false;
  }
  preedit_.clear();
  candidates_.clear();
  preceding_preview_.clear();
  candidate_page_ = 0;
  selected_candidate_ = 0;
  HideCandidates();
  schema_reset_pending_ = false;
  return true;
}

void TextService::LearnCandidate(std::wstring_view pinyin, std::wstring_view text) {
  if (!connected_ || pinyin.empty() || text.empty()) return;
  std::string body;
  const std::string json = "{\"kind\":\"learn\",\"payload\":{\"pinyin\":\"" +
                           JsonEscape(pinyin) + "\",\"text\":\"" + JsonEscape(text) + "\"}}";
  if (!g_pipe.Request(json, body)) connected_ = false;
}

bool TextService::ResolvePendingCancellation() {
  if (!cancel_pending_) return true;
  if (terminal_edit_pending_ && terminal_edit_action_ == Action::Cancel) {
    // The original cancellation is already queued.  Re-requesting it on
    // every key would advance the generation and invalidate the callback we
    // are waiting for.
    return false;
  }
  if (composition_) {
    if (!active_context_ || !CancelComposition(active_context_.Get())) return false;
    // An ASYNCDONTCARE edit session may have been accepted but not run yet.
    // Keep consuming keys until its composition callback has actually ended
    // the range; otherwise the host can append new text to the old pinyin.
    if (composition_) return false;
  }
  cancel_pending_ = false;
  ClearCompositionState();
  composition_.Reset();
  composition_context_.Reset();
  active_context_.Reset();
  passthrough_notified_ = false;
  return true;
}

bool TextService::RequestEdit(ITfContext* context, Action action, const std::wstring& text,
                              bool synchronous) {
  last_edit_pending_ = false;
  if (terminal_edit_pending_ && action == Action::Update) {
    // Do not enqueue a new composition update behind a pending commit/cancel;
    // doing so could resurrect text after the terminal edit has completed.
    last_edit_error_ = TF_E_LOCKED;
    return false;
  }
  const uint64_t generation = ++edit_generation_;
  // A newer terminal request (normally Esc after an accepted commit) makes an
  // older queued terminal callback stale through the generation check.
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  if (action == Action::Commit || action == Action::Cancel) {
    terminal_edit_pending_ = true;
    terminal_edit_action_ = action;
    terminal_edit_generation_ = generation;
  }
  if (!context) {
    last_edit_error_ = E_POINTER;
    if (action == Action::Cancel) cancel_pending_ = true;
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
    return false;
  }
  auto* session = new (std::nothrow)
      CompositionSession(this, context, action, text, generation);
  if (!session) {
    last_edit_error_ = E_OUTOFMEMORY;
    if (action == Action::Cancel) cancel_pending_ = true;
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
    return false;
  }
  last_edit_error_ = S_OK;
  HRESULT result = E_FAIL;
  // Ordinary composition updates use ASYNCDONTCARE so TSF can queue them when
  // the host is locked.  Mode switches opt into TF_ES_SYNC and therefore fail
  // rather than queueing: the next key must observe the new mode immediately.
  const DWORD edit_flags = TF_ES_READWRITE | (synchronous ? TF_ES_SYNC : 0);
  HRESULT request = context->RequestEditSession(client_id_, session, edit_flags, &result);
  session->Release();
  if (FAILED(request)) {
    last_edit_error_ = request;
    if (action == Action::Cancel) cancel_pending_ = true;
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
      terminal_edit_pending_ = false;
      terminal_edit_generation_ = 0;
      return false;
    }
    last_edit_pending_ = true;
    if (action == Action::Cancel) cancel_pending_ = true;
    return true;
  }
  if (SUCCEEDED(result)) return true;
  last_edit_error_ = result;
  if (action == Action::Cancel) cancel_pending_ = true;
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  return false;
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
  if (!SetSelectionToCompositionEnd(cookie)) {
    // Do not leave a live composition behind if the host cannot place the
    // caret at its end.  EndComposition is best-effort here; the original
    // edit error is retained for the caller.
    const HRESULT selection_error = last_edit_error_;
    composition_->EndComposition(cookie);
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
  hr = range->SetText(cookie, 0, text.c_str(), static_cast<LONG>(text.size()));
  if (FAILED(hr)) last_edit_error_ = hr;
  if (FAILED(hr)) return false;
  return SetSelectionToCompositionEnd(cookie);
}
bool TextService::CommitComposition(TfEditCookie cookie, const std::wstring& text) {
  if (!SetCompositionText(cookie, text)) return false;
  return EndComposition(cookie);
}
bool TextService::InsertTextAtSelection(ITfContext* context, TfEditCookie cookie,
                                        const std::wstring& text) {
  if (!context) {
    last_edit_error_ = E_POINTER;
    return false;
  }
  ComPtr<ITfInsertAtSelection> inserter;
  HRESULT hr = context->QueryInterface(IID_PPV_ARGS(&inserter));
  if (FAILED(hr) || !inserter) {
    last_edit_error_ = FAILED(hr) ? hr : E_NOINTERFACE;
    return false;
  }

  ComPtr<ITfRange> inserted;
  hr = inserter->InsertTextAtSelection(
      cookie, TF_IAS_NOQUERY, text.c_str(), static_cast<LONG>(text.size()),
      &inserted);
  if (FAILED(hr)) {
    last_edit_error_ = hr;
    return false;
  }

  // Most context owners move the selection for us.  Explicitly collapse the
  // returned range to its end as a compatibility measure for hosts that keep
  // the selection at the beginning of an insertion.
  if (inserted) {
    ComPtr<ITfRange> caret;
    if (SUCCEEDED(inserted->Clone(&caret)) && caret &&
        SUCCEEDED(caret->Collapse(cookie, TF_ANCHOR_END))) {
      TF_SELECTION selection{caret.Get(), {TF_AE_NONE, FALSE}};
      hr = context->SetSelection(cookie, 1, &selection);
      if (FAILED(hr)) {
        // The insertion itself already succeeded.  A few legacy context
        // owners reject an explicit selection update even though they leave
        // their caret at the insertion end; report the text operation as
        // successful and let the host retain its native caret behavior.
        last_edit_error_ = hr;
      }
    }
  }
  return true;
}
bool TextService::EndComposition(TfEditCookie cookie) {
  if (!composition_) return true;
  const HRESULT result = composition_->EndComposition(cookie);
  if (SUCCEEDED(result)) {
    composition_.Reset();
    composition_context_.Reset();
  }
  if (FAILED(result)) last_edit_error_ = result;
  return SUCCEEDED(result);
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
void TextService::ClearCompositionState() {
  cancel_pending_ = false;
  last_edit_pending_ = false;
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  preedit_.clear();
  preceding_preview_.clear();
  candidates_.clear();
  candidate_page_ = 0;
  selected_candidate_ = 0;
  HideCandidates();
}
void TextService::HideCandidates() { g_candidates.Hide(); }

class ClassFactory final : public IClassFactory {
 public:
  std::atomic<ULONG> refs{1}; ClassFactory(){++g_module_references;} ~ClassFactory(){--g_module_references;}
  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** p) override { if(!p)return E_POINTER;*p=nullptr;if(iid!=IID_IUnknown&&iid!=IID_IClassFactory)return E_NOINTERFACE;*p=static_cast<IClassFactory*>(this);AddRef();return S_OK; }
  ULONG STDMETHODCALLTYPE AddRef() override{return ++refs;} ULONG STDMETHODCALLTYPE Release() override{auto v=--refs;if(!v)delete this;return v;}
  HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer, REFIID iid, void** p) override { if(outer)return CLASS_E_NOAGGREGATION; auto* service=new(std::nothrow) TextService(); if(!service)return E_OUTOFMEMORY; const HRESULT hr=service->QueryInterface(iid,p); service->Release(); return hr; }
  HRESULT STDMETHODCALLTYPE LockServer(BOOL lock) override { if(lock)++g_module_references; else --g_module_references; return S_OK; }
};

std::wstring GuidString(REFGUID guid) {
  wchar_t value[64]{};
  StringFromGUID2(guid, value, ARRAYSIZE(value));
  return value;
}

HRESULT RegisterComServer() {
  wchar_t module[MAX_PATH]{};
  if (!GetModuleFileNameW(g_instance, module, ARRAYSIZE(module))) {
    return HRESULT_FROM_WIN32(GetLastError());
  }

  // The NSIS package is per-machine and regsvr32 runs elevated.  Registering
  // the in-proc server in HKCU makes activation depend on the administrator
  // account that accepted UAC, while TSF profile metadata is machine-scoped.
  // Keep the COM class in HKLM; profile enablement remains user-scoped below.
  HKEY key = nullptr;
  const std::wstring path = L"Software\\Classes\\CLSID\\" + GuidString(kClsid);
  LSTATUS status = RegCreateKeyExW(HKEY_LOCAL_MACHINE, path.c_str(), 0, nullptr,
                                  0, KEY_WRITE, nullptr, &key, nullptr);
  if (status != ERROR_SUCCESS) return HRESULT_FROM_WIN32(status);

  status = RegSetValueExW(key, nullptr, 0, REG_SZ,
                          reinterpret_cast<const BYTE*>(kDescription),
                          sizeof(kDescription));
  if (status == ERROR_SUCCESS) {
    HKEY inproc = nullptr;
    status = RegCreateKeyExW(key, L"InprocServer32", 0, nullptr, 0, KEY_WRITE,
                             nullptr, &inproc, nullptr);
    if (status == ERROR_SUCCESS) {
      status = RegSetValueExW(
          inproc, nullptr, 0, REG_SZ, reinterpret_cast<const BYTE*>(module),
          static_cast<DWORD>((wcslen(module) + 1) * sizeof(wchar_t)));
      if (status == ERROR_SUCCESS) {
        const wchar_t model[] = L"Apartment";
        status = RegSetValueExW(inproc, L"ThreadingModel", 0, REG_SZ,
                                reinterpret_cast<const BYTE*>(model),
                                sizeof(model));
      }
      RegCloseKey(inproc);
    }
  }
  RegCloseKey(key);
  return HRESULT_FROM_WIN32(status);
}

HRESULT RegisterTsfProfile() {
  const HRESULT init = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  if (FAILED(init) && init != RPC_E_CHANGED_MODE) return init;

  // The legacy profile APIs report E_FAIL for an already registered service or
  // profile.  Treat that result as idempotent, but continue with the enable and
  // category calls so repair/upgrade installs cannot leave Lime disabled.
  const auto duplicate_ok = [](HRESULT hr, bool legacy_e_fail) {
    return hr == TF_E_ALREADY_EXISTS ||
           hr == HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS) ||
           (legacy_e_fail && hr == E_FAIL);
  };
  HRESULT result = S_OK;
  const auto record = [&](HRESULT hr, bool allow_duplicate = false,
                          bool legacy_e_fail = false) {
    if (SUCCEEDED(hr)) return;
    if (allow_duplicate && duplicate_ok(hr, legacy_e_fail)) return;
    if (SUCCEEDED(result)) result = hr;
  };

  ComPtr<ITfInputProcessorProfiles> profiles;
  HRESULT hr = CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
                                CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&profiles));
  record(hr);
  wchar_t module[MAX_PATH]{};
  const DWORD module_length =
      GetModuleFileNameW(g_instance, module, ARRAYSIZE(module));
  if (SUCCEEDED(hr) && module_length != 0 &&
      module_length < ARRAYSIZE(module)) {
    record(profiles->Register(kClsid), true, true);
    record(profiles->AddLanguageProfile(
               kClsid, kLanguageId, kProfileGuid, kDescription,
               static_cast<ULONG>(wcslen(kDescription)), module, module_length,
               0),
           true, true);
    // Enable the profile for the current interactive user.  The installer
    // repeats this in its HKCU hive because elevated regsvr32 can be running
    // under a different account during a per-machine install.
    record(profiles->EnableLanguageProfile(kClsid, kLanguageId, kProfileGuid,
                                            TRUE));
    // Enable it by default for users created after installation.  This does
    // not change the current keyboard layout; users still select Lime in the
    // Windows language bar/settings.
    record(profiles->EnableLanguageProfileByDefault(
               kClsid, kLanguageId, kProfileGuid, TRUE),
           true, true);
  } else if (SUCCEEDED(hr)) {
    record(HRESULT_FROM_WIN32(module_length == 0 ? GetLastError()
                                                 : ERROR_INSUFFICIENT_BUFFER));
  }

  ComPtr<ITfCategoryMgr> categories;
  hr = CoCreateInstance(CLSID_TF_CategoryMgr, nullptr, CLSCTX_INPROC_SERVER,
                        IID_PPV_ARGS(&categories));
  record(hr);
  const GUID required[] = {GUID_TFCAT_TIP_KEYBOARD,
                           GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
                           GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT};
  if (SUCCEEDED(hr)) {
    for (const auto& category : required) {
      record(categories->RegisterCategory(kClsid, category, kClsid), true);
    }
  }
  if (SUCCEEDED(init)) CoUninitialize();
  return result;
}
HRESULT UnregisterTsfProfile() { const HRESULT init=CoInitializeEx(nullptr,COINIT_APARTMENTTHREADED); if(FAILED(init)&&init!=RPC_E_CHANGED_MODE)return init; ComPtr<ITfCategoryMgr> categories; if(SUCCEEDED(CoCreateInstance(CLSID_TF_CategoryMgr,nullptr,CLSCTX_INPROC_SERVER,IID_PPV_ARGS(&categories)))){const GUID required[]={GUID_TFCAT_TIP_KEYBOARD,GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT};for(const auto& category:required)categories->UnregisterCategory(kClsid,category,kClsid);} ComPtr<ITfInputProcessorProfiles> profiles; if(SUCCEEDED(CoCreateInstance(CLSID_TF_InputProcessorProfiles,nullptr,CLSCTX_INPROC_SERVER,IID_PPV_ARGS(&profiles)))){profiles->RemoveLanguageProfile(kClsid,kLanguageId,kProfileGuid);profiles->Unregister(kClsid);} if(SUCCEEDED(init))CoUninitialize(); return S_OK; }
HRESULT CreateClassFactory(REFIID iid, void** object) { auto* factory=new(std::nothrow) ClassFactory(); if(!factory)return E_OUTOFMEMORY; const HRESULT hr=factory->QueryInterface(iid,object);factory->Release();return hr; }

}  // namespace lime::tsf
