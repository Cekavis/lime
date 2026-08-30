#pragma once

#if defined(_WIN32)
#  include <windows.h>
#endif

#if defined(_WIN32)
#  define LIME_TSF_API __declspec(dllexport)
#else
#  define LIME_TSF_API
#endif

extern "C" {
LIME_TSF_API HRESULT LimeTsfInitialize();
LIME_TSF_API HRESULT LimeTsfShutdown();
}
