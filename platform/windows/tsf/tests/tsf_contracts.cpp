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

int wmain() {
  const HRESULT init = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  CHECK(SUCCEEDED(init));

  {
    TextService service;
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
  }

  if (SUCCEEDED(init)) CoUninitialize();
  return 0;
}
