// Loads mw_apo.dll the way audiodg does (DllGetClassObject → aggregated CreateInstance →
// Initialize → LockForProcess → APOProcess) without registering anything system-wide.
#include <windows.h>
#include <audioenginebaseapo.h>
#include <audiomediatype.h>
#include <mmreg.h>
#include <ks.h>
#include <ksmedia.h>
#include <math.h>
#include <stdio.h>
#include <vector>

// {5C3E1A57-0B2D-4C5A-9E0F-51C3A57E0001}
static const CLSID CLSID_MwApo = {0x5c3e1a57, 0x0b2d, 0x4c5a, {0x9e, 0x0f, 0x51, 0xc3, 0xa5, 0x7e, 0x00, 0x01}};

// Minimal outer object, to exercise the aggregation path like the audio engine does.
struct Outer : IUnknown
{
    LONG ref = 1;
    IUnknown* inner = nullptr;
    STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override
    {
        if (riid == __uuidof(IUnknown))
        {
            *ppv = this;
            AddRef();
            return S_OK;
        }
        return inner->QueryInterface(riid, ppv);
    }
    STDMETHODIMP_(ULONG) AddRef() override { return InterlockedIncrement(&ref); }
    STDMETHODIMP_(ULONG) Release() override { return InterlockedDecrement(&ref); }
};

#define CHECK(x)                                                                 \
    do                                                                           \
    {                                                                            \
        HRESULT _hr = (x);                                                       \
        if (FAILED(_hr))                                                         \
        {                                                                        \
            printf("FAIL %s: 0x%08lx\n", #x, static_cast<unsigned long>(_hr));   \
            return 1;                                                            \
        }                                                                        \
    } while (0)

int wmain(int argc, wchar_t** argv)
{
    const wchar_t* dll = argc > 1 ? argv[1] : L"mw_apo.dll";
    CHECK(CoInitializeEx(nullptr, COINIT_MULTITHREADED));
    HMODULE mod = LoadLibraryW(dll);
    if (!mod)
    {
        printf("FAIL LoadLibrary: %lu\n", GetLastError());
        return 1;
    }
    auto getClassObject = reinterpret_cast<HRESULT(STDAPICALLTYPE*)(REFCLSID, REFIID, void**)>(
        GetProcAddress(mod, "DllGetClassObject"));
    IClassFactory* factory = nullptr;
    CHECK(getClassObject(CLSID_MwApo, __uuidof(IClassFactory), reinterpret_cast<void**>(&factory)));

    // Aggregation: asking for anything but IUnknown must fail.
    Outer outer;
    void* bogus = nullptr;
    if (factory->CreateInstance(&outer, __uuidof(IAudioProcessingObject), &bogus) != CLASS_E_NOAGGREGATION)
    {
        printf("FAIL aggregation rule not enforced\n");
        return 1;
    }
    CHECK(factory->CreateInstance(&outer, __uuidof(IUnknown), reinterpret_cast<void**>(&outer.inner)));

    IAudioProcessingObject* apo = nullptr;
    IAudioProcessingObjectConfiguration* cfg = nullptr;
    IAudioProcessingObjectRT* rt = nullptr;
    IAudioSystemEffects2* fx = nullptr;
    CHECK(outer.QueryInterface(__uuidof(IAudioProcessingObject), reinterpret_cast<void**>(&apo)));
    CHECK(outer.QueryInterface(__uuidof(IAudioProcessingObjectConfiguration), reinterpret_cast<void**>(&cfg)));
    CHECK(outer.QueryInterface(__uuidof(IAudioProcessingObjectRT), reinterpret_cast<void**>(&rt)));
    CHECK(outer.QueryInterface(__uuidof(IAudioSystemEffects2), reinterpret_cast<void**>(&fx)));

    APOInitSystemEffects2 init{};
    init.APOInit.cbSize = sizeof(init);
    init.APOInit.clsid = CLSID_MwApo;
    CHECK(apo->Initialize(sizeof(init), reinterpret_cast<BYTE*>(&init)));

    UNCOMPRESSEDAUDIOFORMAT f{};
    f.guidFormatType = KSDATAFORMAT_SUBTYPE_IEEE_FLOAT;
    f.dwSamplesPerFrame = 2;
    f.dwBytesPerSampleContainer = 4;
    f.dwValidBitsPerSample = 32;
    f.fFramesPerSecond = 48000.0f;
    f.dwChannelMask = KSAUDIO_SPEAKER_STEREO;
    IAudioMediaType* type = nullptr;
    CHECK(CreateAudioMediaTypeFromUncompressedAudioFormat(&f, &type));
    IAudioMediaType* supported = nullptr;
    CHECK(apo->IsInputFormatSupported(type, type, &supported));
    if (supported)
        supported->Release();

    const UINT32 frames = 480;
    std::vector<float> inBuf(frames * 2), outBuf(frames * 2);
    APO_CONNECTION_DESCRIPTOR inDesc{APO_CONNECTION_BUFFER_TYPE_EXTERNAL, reinterpret_cast<UINT_PTR>(inBuf.data()), frames, type, APO_CONNECTION_DESCRIPTOR_SIGNATURE};
    APO_CONNECTION_DESCRIPTOR outDesc{APO_CONNECTION_BUFFER_TYPE_EXTERNAL, reinterpret_cast<UINT_PTR>(outBuf.data()), frames, type, APO_CONNECTION_DESCRIPTOR_SIGNATURE};
    APO_CONNECTION_DESCRIPTOR* ins[] = {&inDesc};
    APO_CONNECTION_DESCRIPTOR* outs[] = {&outDesc};
    CHECK(cfg->LockForProcess(1, ins, 1, outs));

    HNSTIME latency = 0;
    CHECK(apo->GetLatency(&latency));
    printf("latency: %.2f ms\n", latency / 10000.0);

    LPGUID effects = nullptr;
    UINT nEffects = 0;
    CHECK(fx->GetEffectsList(&effects, &nEffects, nullptr));
    CoTaskMemFree(effects);
    printf("effects advertised: %u\n", nEffects);

    APO_CONNECTION_PROPERTY inProp{reinterpret_cast<UINT_PTR>(inBuf.data()), frames, BUFFER_VALID, APO_CONNECTION_PROPERTY_SIGNATURE};
    APO_CONNECTION_PROPERTY outProp{reinterpret_cast<UINT_PTR>(outBuf.data()), 0, BUFFER_INVALID, APO_CONNECTION_PROPERTY_SIGNATURE};
    APO_CONNECTION_PROPERTY* inProps[] = {&inProp};
    APO_CONNECTION_PROPERTY* outProps[] = {&outProp};

    // 10 s of a quiet "voice" (harmonics + syllable envelope) over fan-like noise.
    unsigned seed = 1;
    double maxUs = 0, totalUs = 0, outPeak = 0, inPeak = 0;
    bool changed = false;
    LARGE_INTEGER freq, t0, t1;
    QueryPerformanceFrequency(&freq);
    const int blocks = 1000;
    for (int b = 0; b < blocks; ++b)
    {
        for (UINT32 i = 0; i < frames; ++i)
        {
            double t = (b * frames + i) / 48000.0;
            double env = 0.5 + 0.5 * sin(2 * 3.14159265 * 3 * t);
            double voice = 0;
            for (int k = 1; k < 10; ++k)
                voice += sin(2 * 3.14159265 * 140 * k * t) / k;
            seed = seed * 1664525u + 1013904223u;
            double noise = ((seed >> 8) / 16777216.0 * 2 - 1) * 0.003;
            float s = static_cast<float>(0.02 * env * voice + noise);
            inBuf[i * 2] = inBuf[i * 2 + 1] = s;
            inPeak = fmax(inPeak, fabs(s));
        }
        inProp.u32BufferFlags = (b % 97 == 5) ? BUFFER_SILENT : BUFFER_VALID;
        QueryPerformanceCounter(&t0);
        rt->APOProcess(1, inProps, 1, outProps);
        QueryPerformanceCounter(&t1);
        double us = (t1.QuadPart - t0.QuadPart) * 1e6 / freq.QuadPart;
        maxUs = fmax(maxUs, us);
        totalUs += us;
        if (outProp.u32BufferFlags != BUFFER_VALID || outProp.u32ValidFrameCount != frames)
        {
            printf("FAIL output flags/frames\n");
            return 1;
        }
        for (UINT32 i = 0; i < frames * 2; ++i)
        {
            if (!isfinite(outBuf[i]))
            {
                printf("FAIL non-finite output\n");
                return 1;
            }
            outPeak = fmax(outPeak, fabs(outBuf[i]));
            if (outBuf[i] != inBuf[i])
                changed = true;
        }
    }
    printf("in peak %.1f dBFS -> out peak %.1f dBFS\n", 20 * log10(inPeak), 20 * log10(outPeak + 1e-12));
    printf("process time: avg %.1f us, max %.1f us per 10 ms block (%.2f%% CPU)\n", totalUs / blocks, maxUs, totalUs / blocks / 100.0);
    if (!changed)
    {
        printf("FAIL output identical to input (DSP not running?)\n");
        return 1;
    }

    CHECK(cfg->UnlockForProcess());
    fx->Release();
    rt->Release();
    cfg->Release();
    apo->Release();
    type->Release();
    outer.inner->Release();
    factory->Release();
    auto canUnload = reinterpret_cast<HRESULT(STDAPICALLTYPE*)()>(GetProcAddress(mod, "DllCanUnloadNow"));
    printf("DllCanUnloadNow: %s\n", canUnload() == S_OK ? "yes" : "NO (leak)");
    printf("PASS\n");
    return 0;
}
