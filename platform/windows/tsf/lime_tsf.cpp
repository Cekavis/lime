#include "lime_tsf.h"

#include <windows.h>
#include <UIAutomation.h>
#include <inputscope.h>
#include <ctffunc.h>
#include <objbase.h>
#include <shlwapi.h>

#include <algorithm>
#include <cctype>
#include <limits>
#include <mutex>
#include <optional>
#include <sstream>
#include <thread>

#include "weasel_ui_adapter.h"

using Microsoft::WRL::ComPtr;

namespace lime::tsf {

const CLSID kClsid = {0x2f7a6c4c, 0x2a0b, 0x4ef0, {0x9d, 0x7e, 0xd4, 0x14, 0x3d, 0x64, 0x80, 0x12}};
const GUID kProfileGuid = {0x6f7d47b0, 0x5a33, 0x4d61, {0x95, 0x95, 0x90, 0x0e, 0xd3, 0x95, 0xb6, 0x1a}};
const LANGID kLanguageId = MAKELANGID(LANG_CHINESE, SUBLANG_CHINESE_SIMPLIFIED);
HINSTANCE g_instance = nullptr;
std::atomic<long> g_module_references{0};

struct AccessibleContextState {
  std::mutex mutex;
  HWND window = nullptr;
  bool pending = false;
  bool ready = false;
  std::wstring text;
};

constexpr GUID kDisplayAttributeInput = {
    0xc2adb175, 0x7e45, 0x4477, {0x9c, 0xf9, 0x6e, 0x89, 0xec, 0x19, 0xb6, 0xcd}};

// Keep the same display-attribute contract as Weasel.  Some TSF hosts render
// a composition only when its GUID_PROP_ATTRIBUTE value can be resolved through
// ITfDisplayAttributeProvider; without it the text is accepted but appears as
// ordinary host text with no composition underline.
class DisplayAttributeInfo final : public ITfDisplayAttributeInfo {
 public:
  DisplayAttributeInfo() : references_(1) { ++g_module_references; }
  ~DisplayAttributeInfo() { --g_module_references; }

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (iid == IID_IUnknown || iid == IID_ITfDisplayAttributeInfo) {
      *object = static_cast<ITfDisplayAttributeInfo*>(this);
      AddRef();
      return S_OK;
    }
    return E_NOINTERFACE;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references_;
    if (!value) delete this;
    return value;
  }
  HRESULT STDMETHODCALLTYPE GetGUID(GUID* guid) override {
    if (!guid) return E_POINTER;
    *guid = kDisplayAttributeInput;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE GetDescription(BSTR* description) override {
    if (!description) return E_POINTER;
    *description = SysAllocString(L"Lime Display Attribute Input");
    return *description ? S_OK : E_OUTOFMEMORY;
  }
  HRESULT STDMETHODCALLTYPE GetAttributeInfo(TF_DISPLAYATTRIBUTE* attribute) override {
    if (!attribute) return E_POINTER;
    *attribute = TF_DISPLAYATTRIBUTE{
        {TF_CT_NONE, 0},  // text color: use the host's color
        {TF_CT_NONE, 0},  // background color: use the host's color
        TF_LS_DOT,        // match Weasel's dotted composition underline
        FALSE,
        {TF_CT_NONE, 0},
        TF_ATTR_INPUT};
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE SetAttributeInfo(
      const TF_DISPLAYATTRIBUTE*) override {
    return E_NOTIMPL;
  }
  HRESULT STDMETHODCALLTYPE Reset() override { return S_OK; }

 private:
  std::atomic<ULONG> references_;
};

class DisplayAttributeEnumerator final : public IEnumTfDisplayAttributeInfo {
 public:
  DisplayAttributeEnumerator() : references_(1) { ++g_module_references; }
  ~DisplayAttributeEnumerator() { --g_module_references; }

  HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** object) override {
    if (!object) return E_POINTER;
    *object = nullptr;
    if (iid == IID_IUnknown || iid == IID_IEnumTfDisplayAttributeInfo) {
      *object = static_cast<IEnumTfDisplayAttributeInfo*>(this);
      AddRef();
      return S_OK;
    }
    return E_NOINTERFACE;
  }
  ULONG STDMETHODCALLTYPE AddRef() override { return ++references_; }
  ULONG STDMETHODCALLTYPE Release() override {
    const ULONG value = --references_;
    if (!value) delete this;
    return value;
  }
  HRESULT STDMETHODCALLTYPE Clone(IEnumTfDisplayAttributeInfo** enumerator) override {
    if (!enumerator) return E_POINTER;
    *enumerator = nullptr;
    auto* clone = new (std::nothrow) DisplayAttributeEnumerator();
    if (!clone) return E_OUTOFMEMORY;
    clone->index_ = index_;
    *enumerator = clone;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE Next(ULONG count, ITfDisplayAttributeInfo** values,
                                  ULONG* fetched) override {
    if (!values || !fetched) return E_POINTER;
    *fetched = 0;
    if (count == 0) return S_OK;
    if (index_ != 0) return S_FALSE;
    auto* info = new (std::nothrow) DisplayAttributeInfo();
    if (!info) return E_OUTOFMEMORY;
    values[0] = info;
    *fetched = 1;
    index_ = 1;
    return count == 1 ? S_OK : S_FALSE;
  }
  HRESULT STDMETHODCALLTYPE Reset() override {
    index_ = 0;
    return S_OK;
  }
  HRESULT STDMETHODCALLTYPE Skip(ULONG count) override {
    if (count == 0) return S_OK;
    if (index_ == 0 && count == 1) {
      index_ = 1;
      return S_OK;
    }
    index_ = 1;
    return S_FALSE;
  }

 private:
  std::atomic<ULONG> references_;
  ULONG index_ = 0;
};

// UWP/immersive text controls consume candidate lists through TSF's UI
// element manager instead of allowing an out-of-process popup to cover their
// surface.  Keep a small data-only element alongside WeaselUI so those hosts
// can render the same candidates natively.  Desktop hosts normally request
// pbShow=TRUE, in which case TextService continues to use the Weasel popup.
// These files are included into this translation unit deliberately: the TSF
// implementation shares private COM helpers and anonymous-namespace state,
// while the split keeps each responsibility navigable without changing linkage.
#include "lime_tsf_ui_element.inl"
#include "lime_tsf_support.inl"
#include "lime_tsf_sessions.inl"
#include "lime_tsf_context.inl"
#include "lime_tsf_lifecycle.inl"
#include "lime_tsf_input.inl"
#include "lime_tsf_candidates.inl"
#include "lime_tsf_state.inl"
#include "lime_tsf_composition.inl"
#include "lime_tsf_registration.inl"
