#include "lime_tsf.h"

#include <windows.h>
#include <UIAutomation.h>
#include <inputscope.h>
#include <ctffunc.h>
#include <objbase.h>
#include <shlwapi.h>

#include <algorithm>
#include <cctype>
#include <limits>
#include <mutex>
#include <optional>
#include <sstream>
#include <thread>

#include "weasel_ui_adapter.h"

using Microsoft::WRL::ComPtr;

namespace lime::tsf {

const CLSID kClsid = {0x2f7a6c4c, 0x2a0b, 0x4ef0, {0x9d, 0x7e, 0xd4, 0x14, 0x3d, 0x64, 0x80, 0x12}};
const GUID kProfileGuid = {0x6f7d47b0, 0x5a33, 0x4d61, {0x95, 0x95, 0x90, 0x0e, 0xd3, 0x95, 0xb6, 0x1a}};
const LANGID kLanguageId = MAKELANGID(LANG_CHINESE, SUBLANG_CHINESE_SIMPLIFIED);
HINSTANCE g_instance = nullptr;
std::atomic<long> g_module_references{0};

struct AccessibleContextState {
  std::mutex mutex;
  HWND window = nullptr;
  bool pending = false;
  bool ready = false;
  std::wstring text;
};

constexpr GUID kDisplayAttributeInput = {
    0xc2adb175, 0x7e45, 0x4477, {0x9c, 0xf9, 0x6e, 0x89, 0xec, 0x19, 0xb6, 0xcd}};

// Keep the same display-attribute contract as Weasel.  Some TSF hosts render
// a composition only when its GUID_PROP_ATTRIBUTE value can be resolved through
// ITfDisplayAttributeProvider; without it the text is accepted but appears as
// ordinary host text with no composition underline.
class DisplayAttributeInfo final : public ITfDisplayAttributeInfo {
 public:
  DisplayAttributeInfo() : references_(1) { ++g_module_references; }
  ~DisplayAttributeInfo() { --g_module_references; }

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (iid == IID_IUnknown || iid == IID_ITfDisplayAttributeInfo) {
      *object = static_cast<ITfDisplayAttributeInfo*>(this);
      AddRef();
      return S_OK;
    }
    return E_NOINTERFACE;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references_;
    if (!value) delete this;
    return value;
  }
  HRESULT STDMETHODCALLTYPE GetGUID(GUID* guid) override {
    if (!guid) return E_POINTER;
    *guid = kDisplayAttributeInput;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetDescription(BSTR* description) override {
    if (!description) return E_POINTER;
    *description = SysAllocString(L"Lime Display Attribute Input");
    return *description ? S_OK : E_OUTOFMEMORY;
  }
  HRESULT STDMETHODCALLTYPE GetAttributeInfo(TF_DISPLAYATTRIBUTE* attribute) override {
    if (!attribute) return E_POINTER;
    *attribute = TF_DISPLAYATTRIBUTE{
        {TF_CT_NONE, 0},  // text color: use the host's color
        {TF_CT_NONE, 0},  // background color: use the host's color
        TF_LS_DOT,        // match Weasel's dotted composition underline
        FALSE,
        {TF_CT_NONE, 0},
        TF_ATTR_INPUT};
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE SetAttributeInfo(
      const TF_DISPLAYATTRIBUTE*) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Reset() override { return S_OK; }

 private:
  std::atomic<ULONG> references_;
};

class DisplayAttributeEnumerator final : public IEnumTfDisplayAttributeInfo {
 public:
  DisplayAttributeEnumerator() : references_(1) { ++g_module_references; }
  ~DisplayAttributeEnumerator() { --g_module_references; }

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (iid == IID_IUnknown || iid == IID_IEnumTfDisplayAttributeInfo) {
      *object = static_cast<IEnumTfDisplayAttributeInfo*>(this);
      AddRef();
      return S_OK;
    }
    return E_NOINTERFACE;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references_;
    if (!value) delete this;
    return value;
  }
  HRESULT STDMETHODCALLTYPE Clone(IEnumTfDisplayAttributeInfo** enumerator) override {
    if (!enumerator) return E_POINTER;
    *enumerator = nullptr;
    auto* clone = new (std::nothrow) DisplayAttributeEnumerator();
    if (!clone) return E_OUTOFMEMORY;
    clone->index_ = index_;
    *enumerator = clone;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE Next(ULONG count, ITfDisplayAttributeInfo** values,
                                  ULONG* fetched) override {
    if (!values || !fetched) return E_POINTER;
    *fetched = 0;
    if (count == 0) return S_OK;
    if (index_ != 0) return S_FALSE;
    auto* info = new (std::nothrow) DisplayAttributeInfo();
    if (!info) return E_OUTOFMEMORY;
    values[0] = info;
    *fetched = 1;
    index_ = 1;
    return count == 1 ? S_OK : S_FALSE;
  }
  HRESULT STDMETHODCALLTYPE Reset() override {
    index_ = 0;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE Skip(ULONG count) override {
    if (count == 0) return S_OK;
    if (index_ == 0 && count == 1) {
      index_ = 1;
      return S_OK;
    }
    index_ = 1;
    return S_FALSE;
  }

 private:
  std::atomic<ULONG> references_;
  ULONG index_ = 0;
};

// UWP/immersive text controls consume candidate lists through TSF's UI
// element manager instead of allowing an out-of-process popup to cover their
// surface.  Keep a small data-only element alongside WeaselUI so those hosts
// can render the same candidates natively.  Desktop hosts normally request
// pbShow=TRUE, in which case TextService continues to use the Weasel popup.
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
    *page_count = candidates_.empty() ? 0 : 1;
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

namespace {

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

  // Use the provider-neutral TSF range operation first.  It preserves the
  // host's document model and is the path used by the stock TSF
  // implementations.  ACP is retained only for older controls that do not
  // implement ShiftStart.
  LONG moved = 0;
  if (SUCCEEDED(before->ShiftStart(cookie, -static_cast<LONG>(limit), &moved,
                                   nullptr))) {
    const ULONG count = static_cast<ULONG>(std::max<LONG>(0, -moved));
    if (count != 0 || limit == 0) {
      std::vector<wchar_t> buffer(count);
      ULONG read = 0;
      if (count &&
          FAILED(before->GetText(cookie, 0, buffer.data(), count, &read))) {
        return false;
      }
      if (read) text.assign(buffer.data(), read);
      return true;
    }
  }

  // Some legacy controls do not implement ShiftStart but do expose ACP
  // ranges.  Keep that compatibility fallback after the generic path.
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
  return false;
}

// Qt's Windows TSF bridge can expose an empty ITfRange while its accessibility
// provider still exposes the focused QTextEdit's bounded prefix.  Query the
// provider asynchronously on a COM MTA after the edit session returns.  This
// avoids IMR_RECONVERTSTRING, which changes the selection, and never requests
// a document range or any UI outside the focused editor.
bool ReadUiAutomationPrecedingOnWorker(HWND view_window, uint32_t limit,
                                       std::wstring& text) {
  text.clear();
  if (!view_window || !IsWindow(view_window)) return false;
  DWORD view_process = 0;
  GetWindowThreadProcessId(view_window, &view_process);
  if (!view_process) return false;

  ComPtr<IUIAutomation> automation;
  if (FAILED(CoCreateInstance(CLSID_CUIAutomation, nullptr,
                              CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&automation))) ||
      !automation) {
    return false;
  }
  ComPtr<IUIAutomation2> automation2;
  if (SUCCEEDED(automation.As(&automation2)) && automation2) {
    automation2->put_ConnectionTimeout(75);
    automation2->put_TransactionTimeout(75);
  }
  // GetFocusedElement is substantially cheaper than walking every descendant
  // of Telegram's top-level Qt window.  The focused element is still checked
  // below for process, root-window, framework, control-type and text-pattern
  // identity before it is trusted.
  ComPtr<IUIAutomationElement> focused;
  automation->GetFocusedElement(&focused);
  if (!focused) {
    ComPtr<IUIAutomationElement> root;
    if (SUCCEEDED(automation->ElementFromHandle(view_window, &root)) && root) {
      VARIANT focus_value{};
      focus_value.vt = VT_BOOL;
      focus_value.boolVal = VARIANT_TRUE;
      ComPtr<IUIAutomationCondition> focus_condition;
      if (SUCCEEDED(automation->CreatePropertyCondition(
              UIA_HasKeyboardFocusPropertyId, focus_value,
              &focus_condition)) &&
          focus_condition) {
        VARIANT pattern_value{};
        pattern_value.vt = VT_BOOL;
        pattern_value.boolVal = VARIANT_TRUE;
        ComPtr<IUIAutomationCondition> pattern_condition;
        ComPtr<IUIAutomationCondition> combined_condition;
        if (SUCCEEDED(automation->CreatePropertyCondition(
                UIA_IsTextPattern2AvailablePropertyId, pattern_value,
                &pattern_condition)) &&
            pattern_condition &&
            SUCCEEDED(automation->CreateAndCondition(
                focus_condition.Get(), pattern_condition.Get(),
                &combined_condition)) &&
            combined_condition) {
          root->FindFirst(
              static_cast<TreeScope>(TreeScope_Element | TreeScope_Descendants),
              combined_condition.Get(), &focused);
        }
        VariantClear(&pattern_value);
      }
      VariantClear(&focus_value);
    }
  }
  if (!focused) return false;

  VARIANT value{};
  if (FAILED(focused->GetCurrentPropertyValue(UIA_ProcessIdPropertyId, &value)) ||
      value.vt != VT_I4 || static_cast<DWORD>(value.lVal) != view_process) {
    VariantClear(&value);
    return false;
  }
  VariantClear(&value);
  if (SUCCEEDED(focused->GetCurrentPropertyValue(
          UIA_NativeWindowHandlePropertyId, &value)) &&
      value.vt == VT_I4 && value.lVal != 0) {
    HWND focused_window = reinterpret_cast<HWND>(static_cast<INT_PTR>(value.lVal));
    if (!focused_window ||
        GetAncestor(focused_window, GA_ROOT) != GetAncestor(view_window, GA_ROOT)) {
      VariantClear(&value);
      return false;
    }
  }
  VariantClear(&value);
  if (FAILED(focused->GetCurrentPropertyValue(UIA_HasKeyboardFocusPropertyId,
                                              &value)) ||
      value.vt != VT_BOOL || value.boolVal == VARIANT_FALSE) {
    VariantClear(&value);
    return false;
  }
  VariantClear(&value);
  if (FAILED(focused->GetCurrentPropertyValue(UIA_IsPasswordPropertyId,
                                              &value)) ||
      value.vt != VT_BOOL || value.boolVal != VARIANT_FALSE) {
    VariantClear(&value);
    return false;
  }
  VariantClear(&value);

  if (SUCCEEDED(focused->GetCurrentPropertyValue(UIA_FrameworkIdPropertyId,
                                                 &value)) &&
      value.vt == VT_BSTR && value.bstrVal) {
    const std::wstring framework(value.bstrVal, SysStringLen(value.bstrVal));
    const bool is_qt = framework.size() >= 2 &&
                       (framework[0] == L'Q' || framework[0] == L'q') &&
                       (framework[1] == L'T' || framework[1] == L't');
    VariantClear(&value);
    if (!is_qt) return false;
  } else {
    VariantClear(&value);
    return false;
  }

  if (FAILED(focused->GetCurrentPropertyValue(UIA_ControlTypePropertyId,
                                              &value)) ||
      value.vt != VT_I4 ||
      (value.lVal != UIA_EditControlTypeId &&
       value.lVal != UIA_DocumentControlTypeId)) {
    VariantClear(&value);
    return false;
  }
  VariantClear(&value);

  ComPtr<IUIAutomationTextPattern2> pattern;
  if (FAILED(focused->GetCurrentPatternAs(UIA_TextPattern2Id,
                                          IID_PPV_ARGS(&pattern))) ||
      !pattern) {
    return false;
  }
  BOOL active = FALSE;
  ComPtr<IUIAutomationTextRange> caret;
  if (FAILED(pattern->GetCaretRange(&active, &caret)) || !active || !caret) {
    return false;
  }
  const int move_limit = static_cast<int>((std::min)(
      limit, static_cast<uint32_t>(std::numeric_limits<int>::max())));
  int moved = 0;
  if (move_limit > 0 &&
      FAILED(caret->MoveEndpointByUnit(TextPatternRangeEndpoint_Start,
                                        TextUnit_Character, -move_limit,
                                        &moved))) {
    return false;
  }
  BSTR value_text = nullptr;
  if (FAILED(caret->GetText(-1, &value_text)) || !value_text) return false;
  text.assign(value_text, SysStringLen(value_text));
  SysFreeString(value_text);
  return true;
}

std::atomic_flag g_ui_automation_busy = ATOMIC_FLAG_INIT;

bool ReadUiAutomationPreceding(std::shared_ptr<AccessibleContextState> state,
                               HWND view_window, uint32_t limit,
                               std::wstring& text) {
  text.clear();
  if (!view_window || !IsWindow(view_window)) return false;
  wchar_t class_name[64]{};
  const int class_length = GetClassNameW(view_window, class_name,
                                         ARRAYSIZE(class_name));
  if (class_length < 2 ||
      (class_name[0] != L'Q' && class_name[0] != L'q') ||
      (class_name[1] != L'T' && class_name[1] != L't')) {
    return false;
  }
  {
    std::lock_guard lock(state->mutex);
    if (state->window == view_window) {
      if (state->ready) {
        text = state->text;
        return true;
      }
      if (state->pending) return false;
    }
  }
  if (g_ui_automation_busy.test_and_set(std::memory_order_acquire)) return false;

  {
    std::lock_guard lock(state->mutex);
    state->window = view_window;
    state->pending = true;
    state->ready = false;
    state->text.clear();
  }

  ++g_module_references;
  try {
    std::thread([state, view_window, limit] {
      const HRESULT init = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
      std::wstring value;
      bool success = false;
      try {
        if (SUCCEEDED(init)) {
          success = ReadUiAutomationPrecedingOnWorker(view_window, limit, value);
        }
      } catch (...) {
        success = false;
      }
      {
        std::lock_guard lock(state->mutex);
        state->window = view_window;
        state->pending = false;
        state->ready = success;
        if (success) {
          state->text = std::move(value);
        } else {
          state->text.clear();
        }
      }
      g_ui_automation_busy.clear(std::memory_order_release);
      if (SUCCEEDED(init)) CoUninitialize();
      --g_module_references;
    }).detach();
  } catch (...) {
    {
      std::lock_guard lock(state->mutex);
      state->pending = false;
      state->ready = false;
      state->text.clear();
    }
    g_ui_automation_busy.clear(std::memory_order_release);
    --g_module_references;
    return false;
  }
  return false;
}

void PrimeUiAutomationPreceding(std::shared_ptr<AccessibleContextState> state,
                                HWND view_window, uint32_t limit) {
  std::wstring ignored;
  (void)ReadUiAutomationPreceding(std::move(state), view_window, limit, ignored);
}

// Match Weasel's TSF positioning path: resolve the composition/selection
// start inside the edit session and ask the active view for its screen-space
// text extent.  A GUI-thread caret rectangle can lag behind an asynchronous
// composition update, so it is only used by the UI adapter as a fallback.
bool ReadInputPosition(ITfContext* context, TfEditCookie cookie,
                       ITfRange* composition_range, RECT& rect) {
  rect = {};
  if (!context) return false;

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
  ComPtr<ITfContextView> view;
  if (FAILED(context->GetActiveView(&view)) || !view) return false;
  auto read_extent = [&](TfAnchor anchor, RECT& result) {
    ComPtr<ITfRange> collapsed;
    if (FAILED(range->Clone(&collapsed)) || !collapsed) return false;
    if (FAILED(collapsed->Collapse(cookie, anchor))) return false;
    BOOL clipped = FALSE;
    const HRESULT hr = view->GetTextExt(cookie, collapsed.Get(), &result, &clipped);
    return SUCCEEDED(hr) && !(result.left == 0 && result.top == 0);
  };

  RECT start_rect{};
  if (!read_extent(TF_ANCHOR_START, start_rect)) return false;

  // Qt's TSF bridge can return the whole first composition glyph, or the
  // editor window origin, when the newly-created range has not reached its
  // final layout yet.  A collapsed end range is the caret position in that
  // case and is stable once the host has accepted the composition.
  const LONG start_width = start_rect.right - start_rect.left;
  const LONG start_height = start_rect.bottom - start_rect.top;
  const bool start_looks_like_caret = start_height > 0 && start_width >= 0 &&
                                      start_width <= 4;
  rect = start_rect;
  if (!start_looks_like_caret && composition_range) {
    RECT end_rect{};
    if (read_extent(TF_ANCHOR_END, end_rect)) {
      const LONG end_width = end_rect.right - end_rect.left;
      const LONG end_height = end_rect.bottom - end_rect.top;
      if (end_height > 0 && end_width >= 0 && end_width <= 4) rect = end_rect;
    }
  }
  if (rect.bottom <= rect.top) return false;

  // Match Weasel's enhanced-position correction.  A few controls return a
  // valid rectangle in their own client coordinate space during the first
  // composition layout.  When it falls outside the foreground window, use
  // the current caret origin to translate it into screen coordinates.
  HWND foreground = GetForegroundWindow();
  RECT foreground_rect{};
  if (foreground && GetWindowRect(foreground, &foreground_rect) &&
      (rect.left < foreground_rect.left || rect.left > foreground_rect.right ||
       rect.top < foreground_rect.top || rect.top > foreground_rect.bottom)) {
    POINT caret{};
    const bool has_caret = GetCaretPos(&caret) != FALSE;
    const LONG offset_x = foreground_rect.left - rect.left +
                          (has_caret ? caret.x : 0);
    const LONG offset_y = foreground_rect.top - rect.top +
                          (has_caret ? caret.y : 0);
    rect.left += offset_x;
    rect.right += offset_x;
    rect.top += offset_y;
    rect.bottom += offset_y;
  }
  return true;
}

class CandidateWindow {
 public:
  void Show(ITfContext* context, const std::vector<TextService::Candidate>& candidates,
            size_t page, size_t selected, size_t page_size,
            std::wstring_view preedit = {}, std::wstring_view preview = {},
            const RECT* anchor = nullptr) {
    weasel_ui_.Show(context, candidates, page, selected, page_size, preedit,
                    preview, anchor);
  }
  void ShowStatus(ITfContext* context, std::wstring_view message) {
    weasel_ui_.ShowStatus(context, message);
  }
  void Hide() {
    weasel_ui_.Hide();
  }

  ~CandidateWindow() = default;

 private:
  WeaselUiAdapter weasel_ui_;
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
  // Standalone punctuation still uses a short-lived TSF
  // composition.  Chromium/WebView2 context owners can re-enter the Windows
  // text-input framework when a key-event sink calls InsertTextAtSelection
  // directly, which has caused host hangs and process termination.  The same
  // StartComposition -> SetText -> EndComposition path used for candidates is
  // accepted by those hosts and leaves no visible unconfirmed state.
  if (!owner->EnsureComposition(context.Get(), cookie)) {
    owner->CompleteEditSession(action, generation, false);
    return E_FAIL;
  }
  if (action == TextService::Action::Update) {
    succeeded = owner->SetCompositionText(cookie, text);
  } else if (action == TextService::Action::CommitPartial) {
    succeeded = owner->CommitPartialComposition(cookie, text, remainder);
  } else {
    succeeded = owner->CommitComposition(cookie, text);
  }
  owner->CompleteEditSession(action, generation, succeeded);
  return succeeded ? S_OK : E_FAIL;
}

TextService::TextService() { ++g_module_references; }
TextService::~TextService() {
  Deactivate();
  if (candidate_ui_) candidate_ui_->DetachOwner();
  --g_module_references;
}

HRESULT TextService::QueryInterface(REFIID iid, void** object) {
  if (!object) return E_POINTER; *object = nullptr;
  if (iid == IID_IUnknown || iid == IID_ITfTextInputProcessor || iid == IID_ITfTextInputProcessorEx) *object = static_cast<ITfTextInputProcessorEx*>(this);
  else if (iid == IID_ITfKeyEventSink) *object = static_cast<ITfKeyEventSink*>(this);
  else if (iid == IID_ITfCompositionSink) *object = static_cast<ITfCompositionSink*>(this);
  else if (iid == IID_ITfDisplayAttributeProvider) *object = static_cast<ITfDisplayAttributeProvider*>(this);
  else if (iid == IID_ITfTextLayoutSink) *object = static_cast<ITfTextLayoutSink*>(this);
  else return E_NOINTERFACE;
  AddRef(); return S_OK;
}
ULONG TextService::AddRef() { return ++references_; }
ULONG TextService::Release() { const ULONG v = --references_; if (!v) delete this; return v; }

HRESULT TextService::EnumDisplayAttributeInfo(
    IEnumTfDisplayAttributeInfo** enumerator) {
  if (!enumerator) return E_POINTER;
  *enumerator = new (std::nothrow) DisplayAttributeEnumerator();
  return *enumerator ? S_OK : E_OUTOFMEMORY;
}

HRESULT TextService::GetDisplayAttributeInfo(REFGUID guid,
                                             ITfDisplayAttributeInfo** info) {
  if (!info) return E_POINTER;
  *info = nullptr;
  if (!IsEqualGUID(guid, kDisplayAttributeInput)) return E_INVALIDARG;
  *info = new (std::nothrow) DisplayAttributeInfo();
  return *info ? S_OK : E_OUTOFMEMORY;
}

void TextService::InitializeDisplayAttribute() {
  if (display_attribute_atom_ != TF_INVALID_GUIDATOM) return;
  ComPtr<ITfCategoryMgr> categories;
  if (FAILED(CoCreateInstance(CLSID_TF_CategoryMgr, nullptr,
                              CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&categories))) ||
      !categories) {
    return;
  }
  TfGuidAtom atom = TF_INVALID_GUIDATOM;
  if (SUCCEEDED(categories->RegisterGUID(kDisplayAttributeInput, &atom))) {
    display_attribute_atom_ = atom;
  }
}

bool TextService::BeginCandidateUi() {
  if (!thread_manager_) return false;
  if (!candidate_ui_) candidate_ui_.Attach(new (std::nothrow) CandidateUiElement(this));
  if (!candidate_ui_) return false;
  if (candidate_ui_->started()) return true;
  // BeginUIElement may synchronously query the element.  Publish the current
  // page before entering the manager so those callbacks never observe the
  // previous composition's candidates.
  candidate_ui_->SetSnapshot(candidates_, candidate_page_, selected_candidate_,
                             page_size_);

  ComPtr<ITfUIElementMgr> manager;
  if (FAILED(thread_manager_->QueryInterface(IID_PPV_ARGS(&manager))) ||
      !manager) {
    return false;
  }
  BOOL show = TRUE;
  DWORD id = 0;
  const HRESULT hr = manager->BeginUIElement(candidate_ui_.Get(), &show, &id);
  if (FAILED(hr)) {
    return false;
  }
  candidate_ui_->set_started(true, id);
  candidate_ui_->set_external_show(show != FALSE);
  candidate_ui_external_ = show != FALSE;
  return true;
}

void TextService::UpdateCandidateUi() {
  ITfContext* context = active_context_.Get();
  if (composition_context_) context = composition_context_.Get();
  const bool should_show = context != nullptr && !candidates_.empty();
  if (!candidates_.empty() && (!candidate_ui_ || !candidate_ui_->started())) {
    BeginCandidateUi();
  }
  if (candidate_ui_ && candidate_ui_->started()) {
    candidate_ui_->SetSnapshot(candidates_, candidate_page_, selected_candidate_,
                               page_size_);
    // UIElement hosts query IsShown synchronously from UpdateUIElement.  Set
    // this before the manager call; publishing it afterwards leaves hosts
    // such as Windows Settings with a valid composition but no popup.
    candidate_ui_->SetShown(should_show);
    ComPtr<ITfUIElementMgr> manager;
    if (thread_manager_ &&
        SUCCEEDED(thread_manager_->QueryInterface(IID_PPV_ARGS(&manager))) &&
        manager) {
      manager->UpdateUIElement(candidate_ui_->ui_id());
    }
  }
  if (!should_show ||
      (candidate_ui_ && candidate_ui_->started() && !candidate_ui_external_)) {
    g_candidates.Hide();
    return;
  }
  g_candidates.Show(context, candidates_, candidate_page_, selected_candidate_,
                    page_size_, {}, preceding_preview_, CandidateAnchor());
}

void TextService::ShowCandidates(ITfContext* context) {
  if (context) active_context_ = context;
  UpdateCandidateUi();
}

void TextService::EndCandidateUi() {
  if (candidate_ui_ && candidate_ui_->started() && thread_manager_) {
    ComPtr<ITfUIElementMgr> manager;
    if (SUCCEEDED(thread_manager_->QueryInterface(IID_PPV_ARGS(&manager))) &&
        manager) {
      manager->EndUIElement(candidate_ui_->ui_id());
    }
  }
  if (candidate_ui_) candidate_ui_->set_started(false);
  candidate_ui_external_ = true;
}

HRESULT TextService::Activate(ITfThreadMgr* manager, TfClientId client_id) { return ActivateEx(manager, client_id, 0); }
HRESULT TextService::ActivateEx(ITfThreadMgr* manager, TfClientId client_id, DWORD flags) {
  if (!manager) return E_INVALIDARG;
  const HRESULT deactivated = Deactivate();
  if (FAILED(deactivated)) return deactivated;
  thread_manager_ = manager; client_id_ = client_id; activation_flags_ = flags;
  HRESULT hr = manager->QueryInterface(IID_PPV_ARGS(&keystroke_manager_)); if (FAILED(hr)) return hr;
  hr = keystroke_manager_->AdviseKeyEventSink(client_id_, this, TRUE); if (FAILED(hr)) { keystroke_manager_.Reset(); return hr; }
  InitializeDisplayAttribute();
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
  if (foreground) {
    PrimeFocusedUiAutomation();
    return S_OK;
  }
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
  UnadviseLayoutSink();
  composition_.Reset();
  composition_context_.Reset();
  ClearCompositionState();
  active_context_.Reset();
  return S_OK;
}

HRESULT TextService::OnLayoutChange(ITfContext* context, TfLayoutCode code,
                                    ITfContextView*) {
  const bool matching = composition_ && composition_context_.Get() == context;
  if (!matching || code != TF_LC_CHANGE || candidates_.empty()) return S_OK;
  QueueCandidateAnchorRefresh(context, edit_generation_);
  return S_OK;
}

void TextService::CompleteEditSession(Action action, uint64_t generation,
                                      bool succeeded) {
  if (!IsEditCurrent(generation)) return;
  last_edit_pending_ = false;
  if (action == Action::Update) {
    ITfContext* context = composition_context_.Get();
    if (!succeeded) {
      HideCandidates();
      return;
    }
    // SetText can leave GetTextExt temporarily without layout.  Like Weasel,
    // request a separate read session after the write so the first key uses
    // the new composition's position.  Never publish the pre-write caret.
    candidate_anchor_ = {};
    candidate_anchor_available_ = false;
    if (!context || candidates_.empty()) {
      HideCandidates();
    } else if (!QueueCandidateAnchorRefresh(context, generation)) {
      ShowCandidates(context);
    }
    return;
  }
  EndCandidateUi();
  if (action == Action::CommitPartial && partial_edit_generation_ == generation) {
    ITfContext* context = composition_context_.Get();
    if (!succeeded) {
      ClearPendingPartialSelection();
      if (context) {
        ShowCandidates(context);
      }
      return;
    }

    const std::wstring pinyin = pending_partial_pinyin_;
    const std::wstring commit = pending_partial_commit_;
    preedit_ = std::move(pending_partial_remainder_);
    candidates_ = std::move(pending_partial_candidates_);
    preceding_preview_ =
        PreviewText(pending_partial_preceding_, context_preview_limit_);
    candidate_anchor_ = pending_partial_anchor_;
    candidate_anchor_available_ = pending_partial_anchor_available_;
    candidate_fetch_complete_ = pending_partial_fetch_complete_;
    candidate_page_ = 0;
    selected_candidate_ = 0;
    accessible_context_.reset();
    ClearPendingPartialSelection();

    if (!pinyin.empty()) LearnCandidate(pinyin, commit);
    if (!context || candidates_.empty()) {
      HideCandidates();
    } else {
      ShowCandidates(context);
    }
    return;
  }
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
  if (IsLetterVirtualKey(key)) {
    const wchar_t letter = static_cast<wchar_t>(key);
    return UppercaseLetterActive() ? letter : static_cast<wchar_t>(letter + ('a' - 'A'));
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
  return IsLetterVirtualKey(key);
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
  const std::wstring_view mapped = HalfShapeForAscii(value);
  return mapped.empty() ? std::wstring() : std::wstring(mapped);
}
bool TextService::IsImeKey(WPARAM key) const {
  return IsPreeditKey(key) || key == VK_BACK || key == VK_RETURN ||
         ((key >= '1' && key <= '9') && !candidates_.empty()) ||
         key == VK_SPACE || key == VK_ESCAPE ||
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
      shift_down_mask_ |= kRightShiftBit;
      if (right_shift_down_tick_ == 0) right_shift_down_tick_ = GetTickCount64();
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
    if (partial_edit_pending_ && key != VK_ESCAPE) {
      // A partial candidate selection also owns the composition until its
      // callback has shifted the range and installed the recomposed suffix.
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
    // Telegram's Qt TSF bridge exposes the preceding text through UIA.  Start
    // that MTA query during the key probe so a completed result can be consumed
    // by a later real input request without blocking this key.
    if (context && IsPreeditKey(key)) {
      ComPtr<ITfContextView> active_view;
      HWND view_window = nullptr;
      if (SUCCEEDED(context->GetActiveView(&active_view)) && active_view)
        active_view->GetWnd(&view_window);
      if (view_window) {
        if (!accessible_context_)
          accessible_context_ = std::make_shared<AccessibleContextState>();
        PrimeUiAutomationPreceding(accessible_context_, view_window,
                                    ContextLimit());
      }
    }
    // OnTestKeyDown is only a probe.  Do not open a read edit session or call
    // the service here: some hosts keep the probe inside their own TSF lock,
    // which makes the real write session in OnKeyDown return TF_E_LOCKED.
    // Candidate fetching and context reads happen exactly once in OnKeyDown.
    // In ASCII mode librime rejects ordinary text keys when no composition is
    // active, letting the host insert them with its own keyboard layout.  Do
    // the same here instead of synthesizing a second TSF edit session.
    if (ascii_mode_ &&
        !(key == VK_ESCAPE &&
          (composition_ || !preedit_.empty() || terminal_edit_pending_ ||
           partial_edit_pending_))) {
      *eaten = FALSE;
      return S_OK;
    }
    const bool previous_page = IsPreviousPageKey(key);
    const bool next_page = IsNextPageKey(key);
    if ((GetKeyState(VK_SHIFT) & 0x8000) != 0 && key == VK_SPACE) {
      // Shift+Space is explicitly not an ascii-composer switch gesture.
      *eaten = FALSE;
      return S_OK;
    }
    if (IsPreeditKey(key) ||
        (IsChinesePunctuationKey(key) && !previous_page && !next_page)) {
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
  } else if (key == VK_ESCAPE &&
             (composition_ || !preedit_.empty() || terminal_edit_pending_ ||
              partial_edit_pending_)) {
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
  } else if (IsChinesePunctuationKey(key)) {
    *eaten = connected_ ? TRUE : FALSE;
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
    shift_down_mask_ &= static_cast<uint8_t>(~kRightShiftBit);
    right_shift_down_tick_ = 0;
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
        shift_down_mask_ |= kRightShiftBit;
        if (right_shift_down_tick_ == 0) right_shift_down_tick_ = GetTickCount64();
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
              !terminal_edit_pending_ && !partial_edit_pending_ &&
              !IsKeyRepeat(lparam)) {
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
      if (partial_edit_pending_) {
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
    if (partial_edit_pending_ && key != VK_ESCAPE) {
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
      shift_down_mask_ &= static_cast<uint8_t>(~kRightShiftBit);
      right_shift_down_tick_ = 0;
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

  // Esc must remain a local cancellation even while a schema reset is waiting
  // on a rejected edit lock; otherwise the host could receive it with a live
  // composition still attached.
  if (schema_reset_pending_ && key != VK_ESCAPE &&
      !ResetCompositionForSchemaChange(context)) {
    return false;
  }
  if ((GetKeyState(VK_SHIFT) & 0x8000) != 0 && key == VK_SPACE) {
    // Match ascii_composer's explicit Shift+Space no-op.  In particular, do
    // not accidentally turn it into a persistent mode switch or an IME-managed
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

void TextService::PrimeFocusedUiAutomation() {
  if (!thread_manager_) return;
  ComPtr<ITfDocumentMgr> document_manager;
  ComPtr<ITfContext> context;
  ComPtr<ITfContextView> view;
  HWND view_window = nullptr;
  if (FAILED(thread_manager_->GetFocus(&document_manager)) ||
      !document_manager ||
      FAILED(document_manager->GetTop(&context)) || !context ||
      FAILED(context->GetActiveView(&view)) || !view ||
      FAILED(view->GetWnd(&view_window)) || !view_window) {
    return;
  }
  if (!accessible_context_)
    accessible_context_ = std::make_shared<AccessibleContextState>();
  PrimeUiAutomationPreceding(accessible_context_, view_window, ContextLimit());
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
  candidate_anchor_ = {};
  candidate_anchor_available_ = false;
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

void TextService::ClearPendingPartialSelection() {
  partial_edit_pending_ = false;
  partial_edit_generation_ = 0;
  pending_partial_pinyin_.clear();
  pending_partial_commit_.clear();
  pending_partial_remainder_.clear();
  pending_partial_candidates_.clear();
  pending_partial_preceding_.clear();
  pending_partial_anchor_ = {};
  pending_partial_anchor_available_ = false;
  pending_partial_fetch_complete_ = false;
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
                              bool synchronous, const std::wstring& remainder) {
  last_edit_pending_ = false;
  if (partial_edit_pending_ && action != Action::CommitPartial &&
      action != Action::Cancel) {
    last_edit_error_ = TF_E_LOCKED;
    return false;
  }
  if (partial_edit_pending_ && action == Action::CommitPartial) {
    last_edit_error_ = TF_E_LOCKED;
    return false;
  }
  if (terminal_edit_pending_ &&
      (action == Action::Update || action == Action::CommitPartial)) {
    // Do not enqueue a new composition update behind a pending commit/cancel;
    // doing so could resurrect text after the terminal edit has completed.
    last_edit_error_ = TF_E_LOCKED;
    return false;
  }
  const uint64_t generation = ++edit_generation_;
  if (action == Action::Cancel) ClearPendingPartialSelection();
  // A newer terminal request (normally Esc after an accepted commit) makes an
  // older queued terminal callback stale through the generation check.
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  if (action == Action::Commit || action == Action::Cancel) {
    terminal_edit_pending_ = true;
    terminal_edit_action_ = action;
    terminal_edit_generation_ = generation;
  } else if (action == Action::CommitPartial) {
    partial_edit_pending_ = true;
    partial_edit_generation_ = generation;
  }
  if (!context) {
    last_edit_error_ = E_POINTER;
    if (action == Action::Cancel) cancel_pending_ = true;
    if (action == Action::CommitPartial) ClearPendingPartialSelection();
    terminal_edit_pending_ = false;
    terminal_edit_generation_ = 0;
    return false;
  }
  auto* session = new (std::nothrow)
      CompositionSession(this, context, action, text, generation, remainder);
  if (!session) {
    last_edit_error_ = E_OUTOFMEMORY;
    if (action == Action::Cancel) cancel_pending_ = true;
    if (action == Action::CommitPartial) ClearPendingPartialSelection();
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
    if (action == Action::CommitPartial) ClearPendingPartialSelection();
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
      if (action == Action::CommitPartial) ClearPendingPartialSelection();
      return false;
    }
    last_edit_pending_ = true;
    if (action == Action::Cancel) cancel_pending_ = true;
    return true;
  }
  if (SUCCEEDED(result)) {
    return true;
  }
  last_edit_error_ = result;
  if (action == Action::Cancel) cancel_pending_ = true;
  if (action == Action::CommitPartial) ClearPendingPartialSelection();
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  return false;
}

void TextService::AdviseLayoutSink(ITfContext* context) {
  UnadviseLayoutSink();
  if (!context) return;
  ComPtr<ITfSource> source;
  if (FAILED(context->QueryInterface(IID_PPV_ARGS(&source))) || !source) {
    return;
  }
  DWORD cookie = TF_INVALID_COOKIE;
  const HRESULT hr = source->AdviseSink(IID_ITfTextLayoutSink,
                                        static_cast<ITfTextLayoutSink*>(this),
                                        &cookie);
  if (SUCCEEDED(hr)) {
    layout_source_ = std::move(source);
    layout_sink_cookie_ = cookie;
  }
}

void TextService::UnadviseLayoutSink() {
  if (layout_source_ && layout_sink_cookie_ != TF_INVALID_COOKIE) {
    layout_source_->UnadviseSink(layout_sink_cookie_);
  }
  layout_source_.Reset();
  layout_sink_cookie_ = TF_INVALID_COOKIE;
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
  AdviseLayoutSink(context);
  if (!SetSelectionToCompositionEnd(cookie)) {
    // Do not leave a live composition behind if the host cannot place the
    // caret at its end.  EndComposition is best-effort here; the original
    // edit error is retained for the caller.
    const HRESULT selection_error = last_edit_error_;
    composition_->EndComposition(cookie);
    UnadviseLayoutSink();
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
  ClearCompositionDisplayAttribute(cookie);
  hr = range->SetText(cookie, 0, text.c_str(), static_cast<LONG>(text.size()));
  if (FAILED(hr)) {
    last_edit_error_ = hr;
    return false;
  }
  if (!SetCompositionDisplayAttribute(cookie)) {
    return false;
  }
  return SetSelectionToCompositionEnd(cookie);
}
bool TextService::CommitComposition(TfEditCookie cookie, const std::wstring& text) {
  if (!SetCompositionText(cookie, text)) return false;
  return EndComposition(cookie);
}
bool TextService::CommitPartialComposition(TfEditCookie cookie,
                                            const std::wstring& commit,
                                            const std::wstring& remainder) {
  if (!composition_) {
    last_edit_error_ = TF_E_COMPOSITION_REJECTED;
    return false;
  }
  if (commit.size() > static_cast<size_t>(std::numeric_limits<LONG>::max()) ||
      remainder.size() > static_cast<size_t>(std::numeric_limits<LONG>::max()) -
                              commit.size()) {
    last_edit_error_ = E_INVALIDARG;
    return false;
  }

  ComPtr<ITfRange> range;
  HRESULT hr = composition_->GetRange(&range);
  if (FAILED(hr) || !range) {
    last_edit_error_ = FAILED(hr) ? hr : TF_E_NOOBJECT;
    return false;
  }
  ComPtr<ITfRange> original_start;
  hr = range->Clone(&original_start);
  if (FAILED(hr) || !original_start ||
      FAILED(hr = original_start->Collapse(cookie, TF_ANCHOR_START))) {
    last_edit_error_ = FAILED(hr) ? hr : TF_E_NOOBJECT;
    return false;
  }

  const std::wstring original = preedit_;
  const std::wstring combined = commit + remainder;
  // The composition range is about to shrink past the committed prefix.
  // Clear the old property first so the committed text cannot retain Lime's
  // input underline.
  ClearCompositionDisplayAttribute(cookie);
  hr = range->SetText(cookie, 0, combined.c_str(), static_cast<LONG>(combined.size()));
  if (FAILED(hr)) {
    last_edit_error_ = hr;
    return false;
  }

  ComPtr<ITfRange> new_start;
  hr = range->Clone(&new_start);
  if (FAILED(hr) || !new_start ||
      FAILED(hr = new_start->Collapse(cookie, TF_ANCHOR_START))) {
    const HRESULT error = FAILED(hr) ? hr : TF_E_NOOBJECT;
    SetCompositionText(cookie, original);
    last_edit_error_ = error;
    return false;
  }
  LONG moved = 0;
  hr = new_start->ShiftStart(cookie, static_cast<LONG>(commit.size()), &moved, nullptr);
  if (FAILED(hr) || moved != static_cast<LONG>(commit.size())) {
    const HRESULT error = FAILED(hr) ? hr : E_FAIL;
    SetCompositionText(cookie, original);
    last_edit_error_ = error;
    return false;
  }

  hr = composition_->ShiftStart(cookie, new_start.Get());
  if (FAILED(hr)) {
    SetCompositionText(cookie, original);
    last_edit_error_ = hr;
    return false;
  }
  if (!SetSelectionToCompositionEnd(cookie)) {
    const HRESULT error = last_edit_error_;
    // Restore the old composition when the host rejects the caret update so
    // the caller can safely keep the old local preedit and retry.
    composition_->ShiftStart(cookie, original_start.Get());
    SetCompositionText(cookie, original);
    last_edit_error_ = error;
    return false;
  }
  if (!SetCompositionDisplayAttribute(cookie)) return false;
  return true;
}
bool TextService::EndComposition(TfEditCookie cookie) {
  if (!composition_) return true;
  ClearCompositionDisplayAttribute(cookie);
  const HRESULT result = composition_->EndComposition(cookie);
  if (SUCCEEDED(result)) {
    UnadviseLayoutSink();
    composition_.Reset();
    composition_context_.Reset();
  }
  if (FAILED(result)) last_edit_error_ = result;
  return SUCCEEDED(result);
}

bool TextService::SetCompositionDisplayAttribute(TfEditCookie cookie) {
  if (!composition_ || display_attribute_atom_ == TF_INVALID_GUIDATOM ||
      !composition_context_) {
    // A host may not expose GUID_PROP_ATTRIBUTE.  Composition text itself is
    // still valid, so treat the optional visual hint as best effort.
    return true;
  }
  ComPtr<ITfRange> range;
  if (FAILED(composition_->GetRange(&range)) || !range) return false;
  ComPtr<ITfProperty> property;
  if (FAILED(composition_context_->GetProperty(GUID_PROP_ATTRIBUTE, &property)) ||
      !property) {
    return true;
  }
  VARIANT value{};
  value.vt = VT_I4;
  value.lVal = static_cast<LONG>(display_attribute_atom_);
  const HRESULT hr = property->SetValue(cookie, range.Get(), &value);
  if (FAILED(hr)) {
    // Display attributes are a host-side decoration.  Do not turn a valid
    // composition update into a rejected key merely because this context does
    // not accept the optional property.
    return true;
  }
  return true;
}

void TextService::ClearCompositionDisplayAttribute(TfEditCookie cookie) {
  if (!composition_ || !composition_context_) return;
  ComPtr<ITfRange> range;
  if (FAILED(composition_->GetRange(&range)) || !range) return;
  ComPtr<ITfProperty> property;
  if (SUCCEEDED(composition_context_->GetProperty(GUID_PROP_ATTRIBUTE,
                                                  &property)) &&
      property) {
    property->Clear(cookie, range.Get());
  }
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

bool TextService::RefreshCandidateAnchor(TfEditCookie cookie) {
  if (!composition_ || !composition_context_) return false;
  ComPtr<ITfRange> range;
  if (FAILED(composition_->GetRange(&range)) || !range) return false;
  RECT anchor{};
  if (!ReadInputPosition(composition_context_.Get(), cookie, range.Get(), anchor)) {
    return false;
  }
  candidate_anchor_ = anchor;
  candidate_anchor_available_ = true;
  return true;
}

bool TextService::QueueCandidateAnchorRefresh(ITfContext* context,
                                              uint64_t generation,
                                              uint8_t attempt) {
  auto* session = new (std::nothrow)
      CandidateAnchorSession(this, context, generation, attempt);
  if (!session) return false;
  HRESULT result = E_FAIL;
  const HRESULT request = context->RequestEditSession(
      client_id_, session, TF_ES_READ | TF_ES_ASYNCDONTCARE, &result);
  session->Release();
  return SUCCEEDED(request) && SUCCEEDED(result);
}

void TextService::ClearCompositionState() {
  accessible_context_.reset();
  cancel_pending_ = false;
  last_edit_pending_ = false;
  ClearPendingPartialSelection();
  terminal_edit_pending_ = false;
  terminal_edit_generation_ = 0;
  preedit_.clear();
  preceding_preview_.clear();
  candidate_anchor_ = {};
  candidate_anchor_available_ = false;
  candidates_.clear();
  candidate_fetch_complete_ = false;
  active_input_request_id_ = 0;
  candidate_page_ = 0;
  selected_candidate_ = 0;
  EndCandidateUi();
  HideCandidates();
}
void TextService::HideCandidates() {
  if (candidate_ui_) candidate_ui_->SetShown(false);
  g_candidates.Hide();
}

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
                           GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
                           GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
                           GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
                           GUID_TFCAT_DISPLAYATTRIBUTEPROPERTY};
  if (SUCCEEDED(hr)) {
    for (const auto& category : required) {
      record(categories->RegisterCategory(kClsid, category, kClsid), true);
    }
  }
  if (SUCCEEDED(init)) CoUninitialize();
  return result;
}
HRESULT UnregisterTsfProfile() {
  const HRESULT init = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  if (FAILED(init) && init != RPC_E_CHANGED_MODE) return init;
  ComPtr<ITfCategoryMgr> categories;
  if (SUCCEEDED(CoCreateInstance(CLSID_TF_CategoryMgr, nullptr,
                                 CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&categories))) &&
      categories) {
    const GUID required[] = {GUID_TFCAT_TIP_KEYBOARD,
                             GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT,
                             GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT,
                             GUID_TFCAT_TIPCAP_UIELEMENTENABLED,
                             GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER,
                             GUID_TFCAT_DISPLAYATTRIBUTEPROPERTY};
    for (const auto& category : required)
      categories->UnregisterCategory(kClsid, category, kClsid);
  }
  ComPtr<ITfInputProcessorProfiles> profiles;
  if (SUCCEEDED(CoCreateInstance(CLSID_TF_InputProcessorProfiles, nullptr,
                                 CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&profiles))) &&
      profiles) {
    profiles->RemoveLanguageProfile(kClsid, kLanguageId, kProfileGuid);
    profiles->Unregister(kClsid);
  }
  if (SUCCEEDED(init)) CoUninitialize();
  return S_OK;
}
HRESULT CreateClassFactory(REFIID iid, void** object) { auto* factory=new(std::nothrow) ClassFactory(); if(!factory)return E_OUTOFMEMORY; const HRESULT hr=factory->QueryInterface(iid,object);factory->Release();return hr; }

}  // namespace lime::tsf
