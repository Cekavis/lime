#pragma once

#include "lime_tsf.h"

#include <windows.h>

#include <condition_variable>
#include <mutex>
#include <string>
#include <string_view>
#include <thread>
#include <vector>

#include <WeaselUI.h>

namespace lime::tsf {

// A small, thread-owned bridge between Lime's snapshot candidate model and
// WeaselUI's rendering API.  The TSF host owns the unconfirmed composition;
// this window renders candidates and the preceding-text auxiliary row only.
// Key handling, paging and commits remain in TextService.
class WeaselUiAdapter final {
 public:
  WeaselUiAdapter() = default;
  ~WeaselUiAdapter();

  void Show(ITfContext* context,
            const std::vector<TextService::Candidate>& candidates,
            size_t page,
            size_t selected,
            size_t page_size,
            std::wstring_view preedit,
            std::wstring_view preceding,
            const RECT* anchor = nullptr);
  void ShowStatus(ITfContext* context, std::wstring_view message);
  void Hide();

 private:
  struct Row {
    std::wstring display;
    bool selected = false;
  };

  struct Snapshot {
    RECT anchor{200, 200, 200, 220};
    HWND target_window = nullptr;
    std::vector<Row> rows;
    // Kept in the snapshot for API compatibility with callers.  The adapter
    // deliberately never forwards it to WeaselUI; the host editor is the
    // source of truth for the unconfirmed composition.
    std::wstring preedit;
    std::wstring preceding;
    std::wstring status;
    size_t page = 0;
    size_t total_pages = 0;
    size_t selected = 0;
    bool visible = false;
    bool is_status = false;
  };

  static constexpr UINT kUpdateMessage = WM_APP + 43;

  RECT Anchor(ITfContext* context) const;
  void Update(Snapshot snapshot);
  void Ensure();
  void Stop();
  void UiThread();
  void Render();
  bool EnsureUiCreated(HWND parent);
  static LRESULT CALLBACK HostProc(HWND hwnd, UINT message, WPARAM wparam,
                                   LPARAM lparam);

  void ConfigureStyle();

  HWND host_window_ = nullptr;
  DWORD ui_thread_id_ = 0;
  Snapshot snapshot_;
  std::mutex state_mutex_;
  std::once_flag start_once_;
  std::thread thread_;
  std::mutex ready_mutex_;
  std::condition_variable ready_cv_;
  bool ready_ = false;
  bool stopped_ = false;
  bool ui_created_ = false;
  HWND ui_parent_ = nullptr;

  weasel::UI ui_;
};

}  // namespace lime::tsf
