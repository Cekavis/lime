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
