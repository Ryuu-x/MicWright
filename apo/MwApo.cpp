#include <initguid.h>
#include "MwApo.h"
#include <string.h>

extern LONG g_objects;

const CRegAPOProperties<1> CMwApo::sRegProperties(
    CLSID_MwApo,
    L"MicWright",
    L"MicWright (MIT)",
    1,                       // major
    0,                       // minor
    __uuidof(IAudioProcessingObject),
    static_cast<APO_FLAG>(APO_FLAG_DEFAULT),
    1, 1, 1, 1,              // min/max input/output connections
    0xffffffff);             // max instances

CMwApo::CMwApo(IUnknown* outer)
    : CBaseAudioProcessingObject(sRegProperties)
{
    m_inner.self = this;
    m_outer = outer ? outer : static_cast<IUnknown*>(&m_inner);
    InterlockedIncrement(&g_objects);
}

CMwApo::~CMwApo()
{
    if (m_dsp)
    {
        mw_destroy(m_dsp);
        m_dsp = nullptr;
    }
    InterlockedDecrement(&g_objects);
}

STDMETHODIMP CMwApo::Inner::QueryInterface(REFIID riid, void** ppv)
{
    if (!ppv)
        return E_POINTER;
    *ppv = nullptr;
    if (riid == __uuidof(IUnknown))
        *ppv = static_cast<IUnknown*>(this);
    else if (riid == __uuidof(IAudioProcessingObject))
        *ppv = static_cast<IAudioProcessingObject*>(self);
    else if (riid == __uuidof(IAudioProcessingObjectRT))
        *ppv = static_cast<IAudioProcessingObjectRT*>(self);
    else if (riid == __uuidof(IAudioProcessingObjectConfiguration))
        *ppv = static_cast<IAudioProcessingObjectConfiguration*>(self);
    else if (riid == __uuidof(IAudioSystemEffects) || riid == __uuidof(IAudioSystemEffects2))
        *ppv = static_cast<IAudioSystemEffects2*>(self);
    else
        return E_NOINTERFACE;
    static_cast<IUnknown*>(*ppv)->AddRef();
    return S_OK;
}

STDMETHODIMP_(ULONG) CMwApo::Inner::AddRef()
{
    return InterlockedIncrement(&self->m_ref);
}

STDMETHODIMP_(ULONG) CMwApo::Inner::Release()
{
    LONG r = InterlockedDecrement(&self->m_ref);
    if (r == 0)
        delete self;
    return r;
}

STDMETHODIMP CMwApo::Initialize(UINT32 cbDataSize, BYTE* pbyData)
{
    // APOInitSystemEffects/2/3 carry endpoint info we don't need; accept any of them.
    if (cbDataSize > 0 && !pbyData)
        return E_POINTER;
    if (cbDataSize > 0 && cbDataSize < sizeof(APOInitBaseStruct))
        return E_INVALIDARG;
    m_bIsInitialized = true;
    return S_OK;
}

STDMETHODIMP CMwApo::GetLatency(HNSTIME* pTime)
{
    if (!pTime)
        return E_POINTER;
    UINT32 frames = m_dsp ? mw_latency_frames(m_dsp) : 0;
    // HNSTIME is in 100 ns units.
    *pTime = m_rate > 0 ? static_cast<HNSTIME>(frames * 10000000.0 / m_rate) : 0;
    return S_OK;
}

STDMETHODIMP CMwApo::Reset()
{
    if (m_dsp)
        mw_reset(m_dsp);
    return S_OK;
}

STDMETHODIMP CMwApo::LockForProcess(UINT32 u32NumInputConnections, APO_CONNECTION_DESCRIPTOR** ppInputConnections,
                                     UINT32 u32NumOutputConnections, APO_CONNECTION_DESCRIPTOR** ppOutputConnections)
{
    HRESULT hr = CBaseAudioProcessingObject::LockForProcess(u32NumInputConnections, ppInputConnections,
                                                            u32NumOutputConnections, ppOutputConnections);
    if (FAILED(hr))
        return hr;
    UNCOMPRESSEDAUDIOFORMAT fmt{};
    hr = ppInputConnections[0]->pFormat->GetUncompressedAudioFormat(&fmt);
    if (FAILED(hr))
        return hr;
    m_channels = fmt.dwSamplesPerFrame;
    m_rate = fmt.fFramesPerSecond;
    if (m_dsp)
        mw_destroy(m_dsp);
    // Allocation happens here, never on the real-time thread. NULL means pass-through.
    // Non-float formats are rejected by the base class, so the buffers are always FLOAT32.
    m_dsp = mw_create(static_cast<UINT32>(m_rate), m_channels);
    return S_OK;
}

STDMETHODIMP CMwApo::UnlockForProcess()
{
    if (m_dsp)
    {
        mw_destroy(m_dsp);
        m_dsp = nullptr;
    }
    return CBaseAudioProcessingObject::UnlockForProcess();
}

STDMETHODIMP_(void) CMwApo::APOProcess(UINT32 u32NumInputConnections, APO_CONNECTION_PROPERTY** ppInputConnections,
                                        UINT32 u32NumOutputConnections, APO_CONNECTION_PROPERTY** ppOutputConnections)
{
    if (u32NumInputConnections < 1 || u32NumOutputConnections < 1)
        return;
    APO_CONNECTION_PROPERTY* in = ppInputConnections[0];
    APO_CONNECTION_PROPERTY* out = ppOutputConnections[0];
    const UINT32 frames = in->u32ValidFrameCount;
    float* pin = reinterpret_cast<float*>(in->pBuffer);
    float* pout = reinterpret_cast<float*>(out->pBuffer);
    const size_t bytes = static_cast<size_t>(frames) * m_channels * sizeof(float);

    if (in->u32BufferFlags == BUFFER_INVALID)
    {
        out->u32ValidFrameCount = frames;
        out->u32BufferFlags = BUFFER_INVALID;
        return;
    }

    if (in->u32BufferFlags == BUFFER_SILENT)
    {
        // Silent buffers may contain garbage; feed real zeros so the chain's state decays.
        memset(pout, 0, bytes);
        pin = pout;
    }

    if (!m_dsp || mw_process(m_dsp, pin, pout, frames) != 0)
    {
        if (pin != pout)
            memcpy(pout, pin, bytes);
    }

    out->u32ValidFrameCount = frames;
    out->u32BufferFlags = BUFFER_VALID;
}

STDMETHODIMP CMwApo::GetEffectsList(LPGUID* ppEffectsIds, UINT* pcEffects, HANDLE Event)
{
    UNREFERENCED_PARAMETER(Event);
    if (!ppEffectsIds || !pcEffects)
        return E_POINTER;
    // Advertise what the chain does so communication apps can avoid double-processing.
    static const GUID effects[] = {
        AUDIO_EFFECT_TYPE_NOISE_SUPPRESSION,
        AUDIO_EFFECT_TYPE_AUTOMATIC_GAIN_CONTROL,
    };
    const UINT n = ARRAYSIZE(effects);
    *ppEffectsIds = static_cast<LPGUID>(CoTaskMemAlloc(sizeof(GUID) * n));
    if (!*ppEffectsIds)
    {
        *pcEffects = 0;
        return E_OUTOFMEMORY;
    }
    memcpy(*ppEffectsIds, effects, sizeof(GUID) * n);
    *pcEffects = n;
    return S_OK;
}
