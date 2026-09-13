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
bool TextService::IsDigitKey(WPARAM key) const {
  const std::wstring text = AsciiText(key);
  return text.size() == 1 && text.front() >= L'0' && text.front() <= L'9';
}
bool TextService::IsChinesePunctuationKey(WPARAM key) const {
  if (HasNonTextModifier()) return false;
  const std::wstring text = AsciiText(key);
  return text.size() == 1 && IsPunctuationCharacter(text.front());
}
std::wstring TextService::ChinesePunctuationText(WPARAM key, bool previous_digit) {
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
  const std::wstring_view mapped =
      HalfShapeForAsciiAfterDigit(value, previous_digit ||
                                           last_chinese_input_was_digit_);
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
    // Clear the one-key numeric punctuation history as soon as an actual
    // non-period key arrives, even when a pending TSF edit causes the handler
    // to return before HandleKey can inspect it.  Digits are re-established by
    // HandleKey only after they are confirmed to be host-bound text.
    const bool period_key = !HasNonTextModifier() && AsciiText(key) == L".";
    if (!period_key && !IsShiftKey(key)) last_chinese_input_was_digit_ = false;
    if (IsShiftKey(key)) {
      last_chinese_input_was_digit_ = false;
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
      if (period_key) last_chinese_input_was_digit_ = false;
      *eaten = TRUE;
      return S_OK;
    }
    if (terminal_edit_pending_ && key != VK_ESCAPE) {
      if (period_key) last_chinese_input_was_digit_ = false;
      *eaten = TRUE;
      return S_OK;
    }
    if (partial_edit_pending_ && key != VK_ESCAPE) {
      if (period_key) last_chinese_input_was_digit_ = false;
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
