/* Hand-written build configuration for desk-echo-guard (no autotools).
 * Matches the SpeexDSP build in the pyaec / aec-rs wheel, which is the
 * fixed-point build (no float instructions in its mdf.c) with KISS FFT. */
#ifndef SPEEXDSP_EG_CONFIG_H
#define SPEEXDSP_EG_CONFIG_H

#define FIXED_POINT
#define USE_KISS_FFT
#define EXPORT

#if defined(_MSC_VER) && !defined(__cplusplus)
#  define inline __inline
#endif

#endif
