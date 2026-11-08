// MicWright capture SFX APO.
//
// A thin COM shell around the Rust chain (mw_dsp.h). It supports COM aggregation,
// which the audio engine requires, and never does anything but pass-through or
// call mw_process() on the real-time thread.
#pragma once

#include <windows.h>
#include <audioenginebaseapo.h>
#include <baseaudioprocessingobject.h>
#include "mw_dsp.h"

// {5C3E1A57-0B2D-4C5A-9E0F-51C3A57E0001}
DEFINE_GUID(CLSID_MwApo, 0x5c3e1a57, 0x0b2d, 0x4c5a, 0x9e, 0x0f, 0x51, 0xc3, 0xa5, 0x7e, 0x00, 0x01);
#define MW_APO_CLSID_STR L"{5C3E1A57-0B2D-4C5A-9E0F-51C3A57E0001}"

class CMwApo final : public CBaseAudioProcessingObject, public IAudioSystemEffects2
{
public:
    explicit CMwApo(IUnknown* outer);
    ~CMwApo() override;

    static const CRegAPOProperties<1> sRegProperties;

    // Non-delegating IUnknown (the identity of the inner object when aggregated).
    struct Inner : IUnknown
    {
        CMwApo* self;
        STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override;
        STDMETHODIMP_(ULONG) AddRef() override;
        STDMETHODIMP_(ULONG) Release() override;
    } m_inner;

    // Delegating IUnknown (all other interfaces).
    STDMETHODIMP QueryInterface(REFIID riid, void** ppv) override { return m_outer->QueryInterface(riid, ppv); }
    STDMETHODIMP_(ULONG) AddRef() override { return m_outer->AddRef(); }
    STDMETHODIMP_(ULONG) Release() override { return m_outer->Release(); }

    // IAudioProcessingObject
    STDMETHODIMP Initialize(UINT32 cbDataSize, BYTE* pbyData) override;
    STDMETHODIMP GetLatency(HNSTIME* pTime) override;
    STDMETHODIMP Reset() override;

    // IAudioProcessingObjectConfiguration
    STDMETHODIMP LockForProcess(UINT32 u32NumInputConnections, APO_CONNECTION_DESCRIPTOR** ppInputConnections,
                                UINT32 u32NumOutputConnections, APO_CONNECTION_DESCRIPTOR** ppOutputConnections) override;
    STDMETHODIMP UnlockForProcess() override;

    // IAudioProcessingObjectRT
    STDMETHODIMP_(void) APOProcess(UINT32 u32NumInputConnections, APO_CONNECTION_PROPERTY** ppInputConnections,
                                   UINT32 u32NumOutputConnections, APO_CONNECTION_PROPERTY** ppOutputConnections) override;

    // IAudioSystemEffects2
    STDMETHODIMP GetEffectsList(LPGUID* ppEffectsIds, UINT* pcEffects, HANDLE Event) override;

private:
    LONG m_ref = 1;
    IUnknown* m_outer;
    MwHandle* m_dsp = nullptr;
    UINT32 m_channels = 0;
    FLOAT32 m_rate = 0;
};
