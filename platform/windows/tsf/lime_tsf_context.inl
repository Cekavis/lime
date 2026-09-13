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

bool RequestWeaselThemeFiles(std::string& base, std::string& custom) {
  base.clear();
  custom.clear();
  std::string body;
  if (!g_pipe.Request(R"({"kind":"get_weasel_theme"})", body)) return false;
  if (!JsonField(body, "base", base)) return false;
  JsonField(body, "custom", custom);
  return true;
}
