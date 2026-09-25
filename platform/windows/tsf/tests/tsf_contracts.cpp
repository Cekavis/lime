#include "../lime_tsf.h"
#include "../weasel_ui_adapter.h"

#include <windows.h>

#include <cstdio>
#include <cstdlib>

using namespace lime::tsf;
using Microsoft::WRL::ComPtr;

#define CHECK(expression) do { \
  if (!(expression)) { \
    std::fprintf(stderr, "line %d: %s\n", __LINE__, #expression); \
    std::abort(); \
  } \
} while (false)

#include "tsf_readonly_contracts.h"

int wmain() {
  const HRESULT init = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  CHECK(SUCCEEDED(init));

  {
    TextService service;
    ComPtr<ITfThreadMgrEventSink> thread_sink;
    CHECK(SUCCEEDED(service.QueryInterface(IID_PPV_ARGS(&thread_sink))));
    ComPtr<ITfThreadFocusSink> focus_sink;
    CHECK(SUCCEEDED(service.QueryInterface(IID_PPV_ARGS(&focus_sink))));
    ComPtr<ITfTextEditSink> edit_sink;
    CHECK(SUCCEEDED(service.QueryInterface(IID_PPV_ARGS(&edit_sink))));
    ComPtr<ITfDisplayAttributeProvider> provider;
    CHECK(SUCCEEDED(service.QueryInterface(IID_PPV_ARGS(&provider))));

    ComPtr<IEnumTfDisplayAttributeInfo> enumerator;
    CHECK(SUCCEEDED(provider->EnumDisplayAttributeInfo(&enumerator)));
    ITfDisplayAttributeInfo* values[2]{};
    ULONG fetched = 0;
    CHECK(enumerator->Next(2, values, &fetched) == S_FALSE);
    CHECK(fetched == 1 && values[0] != nullptr);
    TF_DISPLAYATTRIBUTE attribute{};
    CHECK(SUCCEEDED(values[0]->GetAttributeInfo(&attribute)));
    CHECK(attribute.lsStyle == TF_LS_DOT);
    values[0]->Release();
    CHECK(enumerator->Next(1, values, &fetched) == S_FALSE);
    CHECK(fetched == 0);

    CHECK(SUCCEEDED(enumerator->Reset()));
    CHECK(SUCCEEDED(enumerator->Skip(1)));
    CHECK(enumerator->Skip(1) == S_FALSE);

    ComPtr<ITfDisplayAttributeInfo> info;
    const GUID unknown = {0x8f9e6b31, 0x4f38, 0x4a75,
                          {0x98, 0x0f, 0xf8, 0x6b, 0x41, 0x50, 0x2d, 0x11}};
    CHECK(provider->GetDisplayAttributeInfo(unknown, &info) == E_INVALIDARG);

    CHECK(testing::WeaselColorSchemeFallbacksMatchUpstream());

    // With no active composition or candidates, a bare Chinese-mode Space
    // must remain host-owned so media controls (for example, video
    // play/pause) still receive it.  The key-up probe follows the same
    // pass-through contract.
    BOOL eaten = TRUE;
    CHECK(SUCCEEDED(service.OnTestKeyDown(nullptr, VK_SPACE, 0, &eaten)));
    CHECK(eaten == FALSE);
    eaten = TRUE;
    CHECK(SUCCEEDED(service.OnKeyDown(nullptr, VK_SPACE, 0, &eaten)));
    CHECK(eaten == FALSE);
    eaten = TRUE;
    CHECK(SUCCEEDED(service.OnTestKeyUp(nullptr, VK_SPACE, 0, &eaten)));
    CHECK(eaten == FALSE);
    eaten = TRUE;
    CHECK(SUCCEEDED(service.OnKeyUp(nullptr, VK_SPACE, 0, &eaten)));
    CHECK(eaten == FALSE);
  }

  TestShiftInputContracts();
  TestReadonlyInputContracts();

  if (SUCCEEDED(init)) CoUninitialize();
  return 0;
}
