// C ABI of crates/mw-dsp-ffi (Rust staticlib). Keep in sync with crates/mw-dsp-ffi/src/lib.rs.
#pragma once
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct MwHandle MwHandle;

// Returns NULL on failure; the caller must then pass audio through.
MwHandle* mw_create(uint32_t sample_rate, uint32_t channels);
uint32_t mw_latency_frames(const MwHandle* h);
// Interleaved float. Returns 0 if `output` holds processed audio, non-zero if the
// caller must copy input to output itself. `input` and `output` may alias.
int32_t mw_process(MwHandle* h, const float* input, float* output, uint32_t frames);
void mw_reset(MwHandle* h);
void mw_destroy(MwHandle* h);

#ifdef __cplusplus
}
#endif
