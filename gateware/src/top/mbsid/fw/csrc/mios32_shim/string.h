// Freestanding <string.h> shim. clang's freestanding mode ships only
// stddef/stdint/stdarg/... -- not string.h -- but the engine only uses
// memcpy/memset (and a couple of str* in SysEx). Declare the C-library
// signatures; the riscv32 firmware links these from compiler-rt / a tiny
// libc shim (or Rust-provided mem* intrinsics) at Task 1/4 link time.
#ifndef _MIOS32_SHIM_STRING_H
#define _MIOS32_SHIM_STRING_H

#include <stddef.h>

#ifdef __cplusplus
extern "C" {
#endif

void  *memcpy (void *dest, const void *src, size_t n);
void  *memset (void *s, int c, size_t n);
void  *memmove(void *dest, const void *src, size_t n);
int    memcmp (const void *a, const void *b, size_t n);
size_t strlen (const char *s);
char  *strcpy (char *dest, const char *src);
int    strcmp (const char *a, const char *b);
int    strncmp(const char *a, const char *b, size_t n);

#ifdef __cplusplus
}
#endif

#endif /* _MIOS32_SHIM_STRING_H */
