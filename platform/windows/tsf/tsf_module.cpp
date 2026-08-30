#include "tsf_module.h"

#include <windows.h>
#include <objbase.h>

#include <atomic>

namespace {
std::atomic<unsigned long> g_module_references{0};
thread_local bool g_com_initialized = false;
}

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
  UNREFERENCED_PARAMETER(instance);
  UNREFERENCED_PARAMETER(reserved);

  if (reason == DLL_PROCESS_ATTACH) {
    // COM/TSF initialization must happen on the owning thread, never in DllMain.
    DisableThreadLibraryCalls(instance);
  }
  return TRUE;
}

extern "C" LIME_TSF_API HRESULT LimeTsfInitialize() {
  if (g_com_initialized) {
    return S_FALSE;
  }

  const HRESULT hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  if (FAILED(hr)) {
    return hr;
  }

  // Phase 0 placeholder: Phase 2 will create the ITfThreadMgr/TSF profile objects
  // on this same owning thread.
  g_com_initialized = true;
  g_module_references.fetch_add(1, std::memory_order_relaxed);
  return S_OK;
}

extern "C" LIME_TSF_API HRESULT LimeTsfShutdown() {
  if (!g_com_initialized) {
    return S_FALSE;
  }

  auto current = g_module_references.load(std::memory_order_relaxed);
  while (current != 0 && !g_module_references.compare_exchange_weak(
      current, current - 1, std::memory_order_relaxed)) {
  }
  g_com_initialized = false;
  CoUninitialize();
  return S_OK;
}

extern "C" HRESULT __declspec(dllexport) STDAPICALLTYPE DllRegisterServer() {
  // Registration is intentionally deferred until the TSF implementation lands.
  return E_NOTIMPL;
}

extern "C" HRESULT __declspec(dllexport) STDAPICALLTYPE DllUnregisterServer() {
  return E_NOTIMPL;
}
