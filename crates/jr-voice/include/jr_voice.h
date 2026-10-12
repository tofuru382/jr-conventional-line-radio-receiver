#ifndef JR_VOICE_H
#define JR_VOICE_H
#include <stdint.h>
#ifdef _WIN32
#define JR_VOICE_API __declspec(dllimport)
#define JR_VOICE_CALL __cdecl
#else
#define JR_VOICE_API __attribute__((visibility("default")))
#define JR_VOICE_CALL
#endif
#ifdef __cplusplus
extern "C" {
#endif
/* ABI 2. Seven uint32_t fields; size is 28 bytes. Radio acceptance and
 * corrected-bit statistics belong to the receiver frame decoder. */
typedef struct {
    uint32_t finished, failed, synthesized, output,
        last_decode_us, maximum_frame_us, maximum_flush_us;
} JrVoiceStatus;
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_abi_version(void);
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_status_size(void);
JR_VOICE_API const char *JR_VOICE_CALL jr_voice_version(void);
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_create(void **handle);
JR_VOICE_API void JR_VOICE_CALL jr_voice_destroy(void *handle);
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_reset(void *handle);
/* Serialized access; no retained caller buffers. Process 158 CELP bits packed
 * LSB-first into 20 bytes (top two bits must be zero). Indices strictly increase
 * across both process and advance. Drain read() after process/advance/flush.
 * Continuous input has seven frames (280 ms) of lookahead; the first output
 * arrives on the eighth accepted contiguous frame. Flush emits the tail.
 * Result: 0 success; negative means invalid call/payload (-1), stream state
 * (-2), undrained output (-3), or internal failure (-4). Internal failures
 * require reset; Rust panics do not cross this C ABI. */
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_process(void *handle, uint32_t index,
    const uint8_t *payload, int32_t length);
/* Advance queued boundary work for a rejected/nonvoice slot. No frame
 * parsing, channel correction, radio statistics or USB I/O occurs in Rust. */
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_advance(void *handle, uint32_t index);
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_flush(void *handle);
/* Result: 1 frame copied, 0 empty. Exactly 640 little-endian bytes:
 * 320 mono int16 samples at 8 kHz, retaining the original slot index. */
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_read(void *handle, uint32_t *index,
    uint8_t *pcm, int32_t length);
JR_VOICE_API int32_t JR_VOICE_CALL jr_voice_status(void *handle, JrVoiceStatus *status);
#ifdef __cplusplus
}
#endif
#endif
