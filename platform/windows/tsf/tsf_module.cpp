#include "tsf_module.h"
#include "lime_tsf.h"

#include <objbase.h>

using namespace lime::tsf;

#ifdef _WIN64
#pragma comment(linker, "/EXPORT:DllCanUnloadNow,PRIVATE")
#pragma comment(linker, "/EXPORT:DllGetClassObject,PRIVATE")
#pragma comment(linker, "/EXPORT:DllRegisterServer,PRIVATE")
#pragma comment(linker, "/EXPORT:DllUnregisterServer,PRIVATE")
#else
#pragma comment(linker, "/EXPORT:DllCanUnloadNow=_DllCanUnloadNow@0,PRIVATE")
#pragma comment(linker, "/EXPORT:DllGetClassObject=_DllGetClassObject@12,PRIVATE")
#pragma comment(linker, "/EXPORT:DllRegisterServer=_DllRegisterServer@0,PRIVATE")
#pragma comment(linker, "/EXPORT:DllUnregisterServer=_DllUnregisterServer@0,PRIVATE")
#endif

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {
  UNREFERENCED_PARAMETER(reserved);
  if (reason == DLL_PROCESS_ATTACH) {
    g_instance = instance;
    DisableThreadLibraryCalls(instance);
  }
  return TRUE;
}

extern "C" LIME_TSF_API HRESULT LimeTsfInitialize() {
  const HRESULT hr = CoInitializeEx(nullptr, COINIT_APARTMENTTHREADED);
  return FAILED(hr) ? hr : S_OK;
}

extern "C" LIME_TSF_API HRESULT LimeTsfShutdown() {
  CoUninitialize();
  return S_OK;
}

STDAPI DllCanUnloadNow() {
  return g_module_references.load(std::memory_order_relaxed) == 0 ? S_OK : S_FALSE;
}

STDAPI DllGetClassObject(REFCLSID clsid, REFIID iid, void** object) {
  if (clsid != kClsid) return CLASS_E_CLASSNOTAVAILABLE;
  return CreateClassFactory(iid, object);
}

STDAPI DllRegisterServer() {
  HRESULT hr = RegisterComServer();
  if (SUCCEEDED(hr)) hr = RegisterTsfProfile();
  return hr;
}

STDAPI DllUnregisterServer() {
  UnregisterTsfProfile();
  wchar_t guid[64]{};
  StringFromGUID2(kClsid, guid, ARRAYSIZE(guid));
  const std::wstring key = std::wstring(L"CLSID\\") + guid;
  const LSTATUS status = RegDeleteTreeW(HKEY_CLASSES_ROOT, key.c_str());
  return status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND ? S_OK : HRESULT_FROM_WIN32(status);
}
