// COM server plumbing: class factory, DllGetClassObject, (un)registration.
#include "MwApo.h"
#include <new>
#include <strsafe.h>

LONG g_objects = 0;
static LONG g_locks = 0;
static HMODULE g_module = nullptr;

class CMwFactory final : public IClassFactory
{
public:
    STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override
    {
        if (!ppv)
            return E_POINTER;
        if (riid == __uuidof(IUnknown) || riid == __uuidof(IClassFactory))
        {
            *ppv = static_cast<IClassFactory*>(this);
            return S_OK;
        }
        *ppv = nullptr;
        return E_NOINTERFACE;
    }
    STDMETHODIMP_(ULONG) AddRef() override { return 2; }
    STDMETHODIMP_(ULONG) Release() override { return 1; }

    STDMETHODIMP CreateInstance(IUnknown* outer, REFIID riid, void** ppv) override
    {
        if (!ppv)
            return E_POINTER;
        *ppv = nullptr;
        // COM aggregation rule: an aggregated object can only hand out its inner IUnknown.
        if (outer && riid != __uuidof(IUnknown))
            return CLASS_E_NOAGGREGATION;
        CMwApo* apo = new (std::nothrow) CMwApo(outer);
        if (!apo)
            return E_OUTOFMEMORY;
        HRESULT hr = apo->m_inner.QueryInterface(riid, ppv);
        apo->m_inner.Release(); // drop the construction reference
        return hr;
    }

    STDMETHODIMP LockServer(BOOL lock) override
    {
        if (lock)
            InterlockedIncrement(&g_locks);
        else
            InterlockedDecrement(&g_locks);
        return S_OK;
    }
};

static CMwFactory g_factory;

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID)
{
    if (reason == DLL_PROCESS_ATTACH)
    {
        g_module = instance;
        DisableThreadLibraryCalls(instance);
    }
    return TRUE;
}

STDAPI DllGetClassObject(REFCLSID rclsid, REFIID riid, LPVOID* ppv)
{
    if (rclsid != CLSID_MwApo)
        return CLASS_E_CLASSNOTAVAILABLE;
    return g_factory.QueryInterface(riid, ppv);
}

STDAPI DllCanUnloadNow()
{
    return (g_objects == 0 && g_locks == 0) ? S_OK : S_FALSE;
}

static HRESULT SetKey(HKEY root, const wchar_t* path, const wchar_t* name, const wchar_t* value)
{
    HKEY key;
    LSTATUS s = RegCreateKeyExW(root, path, 0, nullptr, 0, KEY_WRITE, nullptr, &key, nullptr);
    if (s != ERROR_SUCCESS)
        return HRESULT_FROM_WIN32(s);
    s = RegSetValueExW(key, name, 0, REG_SZ, reinterpret_cast<const BYTE*>(value),
                       static_cast<DWORD>((wcslen(value) + 1) * sizeof(wchar_t)));
    RegCloseKey(key);
    return HRESULT_FROM_WIN32(s);
}

// regsvr32 mw_apo.dll (elevated): COM class + audio engine APO registration.
// Attaching the APO to the QuadCast endpoint is done separately by mw-ctl / the service.
STDAPI DllRegisterServer()
{
    wchar_t path[MAX_PATH];
    if (!GetModuleFileNameW(g_module, path, MAX_PATH))
        return HRESULT_FROM_WIN32(GetLastError());

    const wchar_t* clsidKey = L"SOFTWARE\\Classes\\CLSID\\" MW_APO_CLSID_STR;
    const wchar_t* inprocKey = L"SOFTWARE\\Classes\\CLSID\\" MW_APO_CLSID_STR L"\\InprocServer32";
    HRESULT hr = SetKey(HKEY_LOCAL_MACHINE, clsidKey, nullptr, L"MicWright APO");
    if (SUCCEEDED(hr))
        hr = SetKey(HKEY_LOCAL_MACHINE, inprocKey, nullptr, path);
    if (SUCCEEDED(hr))
        hr = SetKey(HKEY_LOCAL_MACHINE, inprocKey, L"ThreadingModel", L"Both");
    if (SUCCEEDED(hr))
        hr = RegisterAPO(CMwApo::sRegProperties);
    return hr;
}

STDAPI DllUnregisterServer()
{
    UnregisterAPO(CLSID_MwApo);
    RegDeleteTreeW(HKEY_LOCAL_MACHINE, L"SOFTWARE\\Classes\\CLSID\\" MW_APO_CLSID_STR);
    return S_OK;
}
