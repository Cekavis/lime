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

std::wstring ConsumedPinyin(std::wstring_view preedit, std::wstring_view remainder) {
  if (remainder.size() > preedit.size() ||
      preedit.substr(preedit.size() - remainder.size()) != remainder) {
    return {};
  }
  return std::wstring(preedit.substr(0, preedit.size() - remainder.size()));
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

constexpr bool IsLetterVirtualKey(WPARAM key) {
  // Windows virtual-key codes for letters are VK_A..VK_Z (0x41..0x5A).
  // Do not accept the lowercase ASCII range here: 0x61..0x7A is also used
  // by numpad/operator keys and F1..F11.
  return key >= 'A' && key <= 'Z';
}

// Convert a virtual key to the character that a standard Windows keyboard
// layout would produce.  Keeping this as a side-effect-free function makes
// the mode logic testable without a TSF context; the caller supplies the
// modifier state captured by GetKeyState().
constexpr wchar_t AsciiCharForVirtualKey(WPARAM key, bool shift, bool caps_lock,
                                         bool num_lock = true) {
  if (IsLetterVirtualKey(key)) {
    const bool upper = shift != caps_lock;
    const wchar_t letter = static_cast<wchar_t>(key);
    return upper ? letter : static_cast<wchar_t>(letter + (L'a' - L'A'));
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

constexpr std::wstring_view HalfShapeForAscii(wchar_t value) {
  // These are the first/default choices from the bundled Rime punctuator
  // half_shape table.  Letters and digits intentionally return an empty view: Chinese mode must
  // still let those keys follow the normal preedit/host paths.
  switch (value) {
    // Rime's half_shape table does not define a space entry, but Lime keeps
    // the existing explicit half-width-space behavior in Chinese mode.
    case L' ': return L" ";
    case L',': return L"，";
    case L'.': return L"。";
    case L'<': return L"《";
    case L'>': return L"》";
    case L'/': return L"/";
    case L'?': return L"？";
    case L';': return L"；";
    case L':': return L"：";
    case L'\\': return L"、";
    case L'|': return L"|";
    case L'`': return L"·";
    case L'~': return L"~";
    case L'!': return L"！";
    case L'@': return L"@";
    case L'#': return L"#";
    case L'%': return L"%";
    case L'$': return L"\u00A5";
    case L'^': return L"……";
    case L'&': return L"&";
    case L'*': return L"*";
    case L'(': return L"（";
    case L')': return L"）";
    case L'-': return L"-";
    case L'_': return L"——";
    case L'+': return L"+";
    case L'=': return L"=";
    case L'[': return L"【";
    case L']': return L"】";
    case L'{': return L"「";
    case L'}': return L"」";
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
static_assert(AsciiCharForVirtualKey(VK_NUMPAD1, false, false) == L'1');
static_assert(AsciiCharForVirtualKey(VK_NUMPAD9, false, false) == L'9');
static_assert(AsciiCharForVirtualKey(VK_MULTIPLY, false, false) == L'*');
static_assert(AsciiCharForVirtualKey(VK_ADD, false, false) == L'+');
static_assert(AsciiCharForVirtualKey(VK_SEPARATOR, false, false) == L',');
static_assert(AsciiCharForVirtualKey(VK_SUBTRACT, false, false) == L'-');
static_assert(AsciiCharForVirtualKey(VK_DECIMAL, false, false) == L'.');
static_assert(AsciiCharForVirtualKey(VK_DIVIDE, false, false) == L'/');
static_assert(AsciiCharForVirtualKey(VK_F1, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F2, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F3, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F4, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F5, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F6, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F7, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F8, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F9, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F10, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F11, false, false) == 0);
static_assert(AsciiCharForVirtualKey(VK_F12, false, false) == 0);
static_assert(HalfShapeForAscii(L' ') == std::wstring_view(L" "));
static_assert(HalfShapeForAscii(L'/') == std::wstring_view(L"/"));
static_assert(HalfShapeForAscii(L'`') == std::wstring_view(L"·"));
static_assert(HalfShapeForAscii(L'$') == std::wstring_view(L"\u00A5"));
static_assert(HalfShapeForAscii(L'[') == std::wstring_view(L"【"));
static_assert(HalfShapeForAscii(L'^') == std::wstring_view(L"……"));

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

size_t JsonStringEnd(std::string_view value, size_t start) {
  bool escaped = false;
  for (size_t i = start; i < value.size(); ++i) {
    const char ch = value[i];
    if (escaped) {
      escaped = false;
    } else if (ch == '\\') {
      escaped = true;
    } else if (ch == '"') {
      return i + 1;
    }
  }
  return std::string_view::npos;
}

std::vector<std::optional<std::string>> JsonOptionalStringArray(
    std::string_view value, std::string_view key) {
  const std::string needle = "\"" + std::string(key) + "\":[";
  const size_t array = value.find(needle);
  if (array == std::string_view::npos) return {};
  const size_t array_end = JsonArrayEnd(value, array + needle.size() - 1);
  if (array_end == std::string_view::npos) return {};

  std::vector<std::optional<std::string>> result;
  size_t pos = array + needle.size();
  while (pos < array_end - 1) {
    while (pos < array_end - 1 &&
           (value[pos] == ',' || std::isspace(static_cast<unsigned char>(value[pos])))) {
      ++pos;
    }
    if (pos >= array_end - 1) break;
    if (value.compare(pos, 4, "null") == 0) {
      result.emplace_back(std::nullopt);
      pos += 4;
    } else if (value[pos] == '"') {
      const size_t string_end = JsonStringEnd(value, pos + 1);
      if (string_end == std::string_view::npos || string_end > array_end) return {};
      result.emplace_back(JsonString(value, pos + 1));
      pos = string_end;
    } else {
      return {};
    }
    while (pos < array_end - 1 && value[pos] != ',') ++pos;
  }
  return result;
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
  std::wstring remainder;
  uint64_t generation;
  CompositionSession(TextService* o, ITfContext* c, TextService::Action a,
                     std::wstring t, uint64_t g, std::wstring r)
      : owner(o),
        context(c),
        action(a),
        text(std::move(t)),
        remainder(std::move(r)),
        generation(g) {
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

}  // namespace