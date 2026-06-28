/* cxx_runtime.c — minimal freestanding C++/RTOS runtime stubs the MBSID engine
 * pulls in when linked freestanding (Task 4).
 *
 * Each stub here exists ONLY to satisfy the linker. None should be reachable on
 * the Lead register-write path at runtime; operator new/delete in particular must
 * never be called (the engine is heap-free — a call indicates a porting bug, so
 * the stubs trap by spinning). If --gc-sections already drops a symbol, defining
 * it here is harmless.
 *
 * NOT provided here (let Rust's compiler_builtins supply them):
 *   memcpy/memset/memmove/memcmp  — provided by compiler_builtins (mem feature).
 */

#include <stddef.h>
#include <stdarg.h>

/* sprintf — minimal implementation for bankPatchNameGet error strings.
 * Supports only the formats actually used: %c (char) and %[0N]d (int).
 * Only called on invalid-bank/invalid-patch error paths, never on the
 * hot Lead register-write path. */
int sprintf(char *buf, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    char *p = buf;
    for (; *fmt; fmt++) {
        if (*fmt != '%') { *p++ = *fmt; continue; }
        fmt++;
        char pad = ' '; int width = 0;
        if (*fmt == '0') { pad = '0'; fmt++; }
        while (*fmt >= '0' && *fmt <= '9') { width = width * 10 + (*fmt++ - '0'); }
        if (*fmt == 'c') {
            *p++ = (char)va_arg(ap, int);
        } else if (*fmt == 'd') {
            int v = va_arg(ap, int);
            char tmp[12]; int n = 0;
            if (v < 0) { *p++ = '-'; v = -v; }
            do { tmp[n++] = (char)('0' + (v % 10)); v /= 10; } while (v);
            while (n < width) { tmp[n++] = pad; }
            while (n > 0) { *p++ = tmp[--n]; }
        } else if (*fmt == '%') {
            *p++ = '%';
        }
    }
    *p = 0;
    va_end(ap);
    return (int)(p - buf);
}

/* time() — no wall clock on the freestanding target; jsw_rand's jsw_time_seed()
 * is the only caller, so a constant yields a fixed, reproducible RNG seed. */
long time(long *t) { if (t) *t = 0; return 0; }

/* RTOS task semaphores — single-hart, no preemption: genuine no-ops. */
void TASKS_MIDIOUTSemaphoreTake(void) {}
void TASKS_MIDIOUTSemaphoreGive(void) {}
void TASKS_MIDIINSemaphoreTake(void)  {}
void TASKS_MIDIINSemaphoreGive(void)  {}
void TASKS_SDCardSemaphoreTake(void)  {}
void TASKS_SDCardSemaphoreGive(void)  {}
void TASKS_LCDTake(void)              {}
void TASKS_LCDGive(void)              {}

/* (MIOS32_MIDI_DeviceIDGet / MIOS32_MIDI_SendDebugMessage are provided as
 * static-inline no-ops by the mios32_shim facade — not duplicated here.) */

/* C++ ABI runtime. __cxa_pure_virtual / heap operators must never run on the
 * Lead path; spin if they ever do (a finding, not a fallback). */
static void cxx_trap(void) { for (;;) {} }

void __cxa_pure_virtual(void) { cxx_trap(); }
int  __cxa_atexit(void (*f)(void *), void *a, void *d) { (void)f; (void)a; (void)d; return 0; }
void *__dso_handle = 0;

/* -fno-use-cxa-atexit routes static-object destructors to plain atexit(). The
 * firmware never returns from main(), so registered destructors never run:
 * record nothing, report success. */
int atexit(void (*f)(void)) { (void)f; return 0; }

/* operator new(size_t) / operator new[](size_t) — itanium-mangled. */
void *_Znwj(size_t n) { (void)n; cxx_trap(); return 0; }
void *_Znaj(size_t n) { (void)n; cxx_trap(); return 0; }
/* operator delete(void*) / delete[](void*) / sized variants. */
void _ZdlPv(void *p)         { (void)p; }
void _ZdaPv(void *p)         { (void)p; }
void _ZdlPvj(void *p, size_t n) { (void)p; (void)n; }
void _ZdaPvj(void *p, size_t n) { (void)p; (void)n; }
