#pragma once

namespace {

// These windows stay hidden and belong only to the test thread. SetFocus
// exercises the native focus query without activating a user's application
// or injecting keyboard input into it.
class FocusWindows final {
 public:
  FocusWindows() : previous_focus_(GetFocus()), instance_(GetModuleHandleW(nullptr)) {
    for (const auto* name : {L"MPC-BE", L"Afx:LimeTsfFocusContract",
                             L"LimeTsfFocusContract"}) {
      WNDCLASSW window_class{};
      window_class.lpfnWndProc = DefWindowProcW;
      window_class.hInstance = instance_;
      window_class.lpszClassName = name;
      CHECK(RegisterClassW(&window_class) != 0);
    }
    player = Create(L"MPC-BE");
    playback = Create(L"Afx:LimeTsfFocusContract", player, 0xe900);
    editor = Create(L"EDIT", player, 100);
    nested_editor = Create(L"EDIT", playback, 101);
    // Even a native editor with the MFC pane ID must not be mistaken for
    // MPC-BE's Afx playback view.
    pane_editor = Create(L"EDIT", player, 0xe900);
    other_pane = Create(L"Afx:LimeTsfFocusContract", player, 102);
    other_frame = Create(L"LimeTsfFocusContract");
    other_playback = Create(L"Afx:LimeTsfFocusContract", other_frame, 0xe900);
    dialog = Create(L"LimeTsfFocusContract", player, 0, true);
    dialog_editor = Create(L"EDIT", dialog, 103);
  }
  ~FocusWindows() {
    SetFocus(previous_focus_);
    for (auto window = windows_.rbegin(); window != windows_.rend(); ++window)
      CHECK(DestroyWindow(*window));
    for (const auto* name : {L"MPC-BE", L"Afx:LimeTsfFocusContract",
                             L"LimeTsfFocusContract"})
      CHECK(UnregisterClassW(name, instance_));
  }
  void Focus(HWND window) {
    SetFocus(window);
    CHECK(GetFocus() == window);
  }

  HWND player = nullptr;
  HWND playback = nullptr;
  HWND editor = nullptr;
  HWND nested_editor = nullptr;
  HWND pane_editor = nullptr;
  HWND other_pane = nullptr;
  HWND other_frame = nullptr;
  HWND other_playback = nullptr;
  HWND dialog = nullptr;
  HWND dialog_editor = nullptr;

 private:
  HWND Create(const wchar_t* name, HWND parent = nullptr, int id = 0,
              bool owned_popup = false) {
    const DWORD style = parent && !owned_popup ? WS_CHILD : WS_POPUP;
    HWND window = CreateWindowExW(
        0, name, L"", style, 0, 0, 0, 0, parent,
        reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), instance_, nullptr);
    CHECK(window != nullptr);
    windows_.push_back(window);
    return window;
  }

  HWND previous_focus_;
  HINSTANCE instance_;
  std::vector<HWND> windows_;
};

void CheckHostOwnedKey(TextService& service, ITfContext* context, WPARAM key) {
  BOOL eaten = TRUE;
  CHECK(SUCCEEDED(service.OnTestKeyDown(context, key, 0, &eaten)));
  CHECK(eaten == FALSE);
  // Exercise the real callbacks too, including hosts that invoke them
  // despite the probe declining the key.
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnKeyDown(context, key, 0, &eaten)));
  CHECK(eaten == FALSE);
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnTestKeyUp(context, key, 0, &eaten)));
  CHECK(eaten == FALSE);
  eaten = TRUE;
  CHECK(SUCCEEDED(service.OnKeyUp(context, key, 0, &eaten)));
  CHECK(eaten == FALSE);
}

void TestMpcPlaybackFocusContracts() {
  ScopedKeyboardState keyboard;
  FocusWindows windows;
  {
    InputContract input;
    for (HWND focus : {windows.player, windows.playback}) {
      windows.Focus(focus);
      for (const WPARAM key : {WPARAM{'Y'}, WPARAM{VK_OEM_PERIOD},
                               WPARAM{VK_SPACE}, WPARAM{VK_LSHIFT}})
        CheckHostOwnedKey(input.service, input.context.Get(), key);
      CHECK(input.service.PreeditForTest().empty());
      CHECK(!input.service.CompositionActiveForTest());
      CHECK(!input.service.AsciiModeForTest());
      CHECK(input.service.ShiftPendingMaskForTest() == 0);
    }
    CHECK(input.context->writes == 0);
    CHECK(input.pipe.StatusRequests() == 0);
    input.pipe.ExpectPreedits({});
  }
  // Normal editors (including descendants of the playback pane), owned
  // dialogs, unrelated MFC windows and TSF-only/windowless hosts retain the
  // existing input path. Do not infer editability from a missing Win32 caret.
  for (HWND focus : {windows.editor, windows.nested_editor, windows.pane_editor,
                     windows.other_pane, windows.other_frame,
                     windows.other_playback, windows.dialog_editor, HWND{}}) {
    windows.Focus(focus);
    InputContract input;
    CHECK(DispatchKeyDown(input.service, input.context.Get(), 'N'));
    CHECK(input.service.PreeditForTest() == L"n");
    CHECK(input.context->writes == 1);
    input.pipe.ExpectPreedits({"n"});
  }
  {
    InputContract input;
    windows.Focus(windows.editor);
    CHECK(DispatchKeyDown(input.service, input.context.Get(), 'N'));
    const size_t status_requests = input.pipe.StatusRequests();
    windows.Focus(windows.playback);
    CheckHostOwnedKey(input.service, input.context.Get(), 'Y');
    CHECK(input.service.PreeditForTest().empty());
    CHECK(!input.service.CompositionActiveForTest());
    CHECK(input.context->writes == 1);
    CHECK(input.pipe.StatusRequests() == status_requests);
    windows.Focus(windows.nested_editor);
    CHECK(DispatchKeyDown(input.service, input.context.Get(), 'I'));
    // A delayed write from the old editor must neither resurrect its preedit
    // nor discard the new editor's input.
    input.context->RunNextQueuedSession();
    CHECK(input.service.PreeditForTest() == L"i");
    input.pipe.ExpectPreedits({"n", "i"});

    CHECK(SUCCEEDED(input.service.OnSetFocus(FALSE)));
    CHECK(DispatchKeyDown(input.service, input.context.Get(), VK_LSHIFT));
    CHECK(DispatchKeyUp(input.service, input.context.Get(), VK_LSHIFT));
    CHECK(input.service.AsciiModeForTest());
    windows.Focus(windows.playback);
    CheckHostOwnedKey(input.service, input.context.Get(), VK_LSHIFT);
    CHECK(input.service.AsciiModeForTest());
  }
}

}  // namespace
