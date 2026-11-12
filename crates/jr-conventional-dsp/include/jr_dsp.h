#ifndef JR_CONVENTIONAL_DSP_H
#define JR_CONVENTIONAL_DSP_H
#include <stdint.h>

#if defined(_WIN32)
#define JR_DSP_CALL __cdecl
#else
#define JR_DSP_CALL
#endif
#ifdef __cplusplus
extern "C" {
#endif

typedef struct { float real, imaginary; } JrComplex32;
/* carrier_error is the smoothed sequence-channel residual magnitude in AGC
 * units, not radians or BER. carrier_locked is a confidence heuristic only.
 * frequency_offset_hz is the carrier-loop estimate; adaptive channel phase
 * may absorb part of the residual offset in a time-varying channel.
 */
typedef struct {
    int64_t symbols, locked_symbols;
    double carrier_error, frequency_offset_hz;
    int32_t carrier_locked, reserved;
} JrDspSnapshot;

/* ABI 2. Snapshot size is 40 bytes; IQ size is 8 bytes on supported targets. */
int32_t JR_DSP_CALL jr_dsp_abi_version(void);
int32_t JR_DSP_CALL jr_dsp_snapshot_size(void);
/* Profiles: 0 recovered JR / standard phase labels; 1 conjugated diagnostic. */
int32_t JR_DSP_CALL jr_dsp_create(double rate, double offset, int32_t profile, void **handle);
void JR_DSP_CALL jr_dsp_destroy(void *handle);
/* Serialized access, disjoint valid buffers, no retained caller pointers.
 * Nonempty input needs capacity >= 2 * input_len + 4; zero lengths allow null.
 * One dibit (0..3) per output byte. No output is dropped on insufficient space.
 * Counts and carrier status describe the end of the complete native block.
 * Errors: -1 rate, -2 offset, -3 profile, -4 buffer, -5 handle,
 *         -6 nonfinite IQ, -7 internal failure (retire the handle).
 */
int32_t JR_DSP_CALL jr_dsp_process(void *handle, const JrComplex32 *input, int32_t input_len,
    uint8_t *output, int32_t capacity, int32_t *written, JrDspSnapshot *snapshot);

/* Passive diagnostic read. Capacity 0..1024; latest differential symbols,
 * oldest first, after receive filtering, timing and carrier recovery, and
 * ISI/complex-gain removal using the current best sequence survivor's channel.
 * Unsliced observations, not hard traceback decisions; the known pi/4 increment
 * is restored for the diagonal references. Normalized by the window RMS
 * amplitude. EVM is a percentage
 * of that amplitude relative to the nearest ideal diagonal corner. No points
 * and zero EVM means no measurable symbol power, not perfect reception.
 * All points including outliers contribute; no clipping or noise calibration.
 * Uses the same serialized access and buffer/error rules as process.
 */
int32_t JR_DSP_CALL jr_dsp_read_signal(void *handle, JrComplex32 *output,
    int32_t capacity, int32_t *written, double *evm);

#ifdef __cplusplus
}
#endif
#endif
