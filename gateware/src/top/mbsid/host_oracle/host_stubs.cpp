/* host_stubs.cpp — host-only link stubs for the MBSID oracle build.
 *
 * The vendored engine's tasks.h (under MIOS32_FAMILY_EMULATION) routes its
 * mutex macros to these extern "C" semaphore helpers.  On a single-threaded
 * host they are side-effect-free no-ops.  Defined HERE, never in the vendored
 * tree.  (notestack.c / jsw_rand.c are linked REAL, not stubbed.)
 */
extern "C" {
    // MIOS32 IRQ guards used by modules/sid/sid.c (the SID register update is
    // wrapped in a short critical section). Single-threaded host -> no-ops.
    void MIOS32_IRQ_Disable(void) {}
    void MIOS32_IRQ_Enable(void)  {}

    void TASKS_SDCardSemaphoreTake(void) {}
    void TASKS_SDCardSemaphoreGive(void) {}
    void TASKS_MIDIINSemaphoreTake(void)  {}
    void TASKS_MIDIINSemaphoreGive(void)  {}
    void TASKS_MIDIOUTSemaphoreTake(void) {}
    void TASKS_MIDIOUTSemaphoreGive(void) {}
    void TASKS_LCDSemaphoreTake(void) {}
    void TASKS_LCDSemaphoreGive(void) {}
    // Names declared by the riscv shim's mios32.h (harmless on host):
    void TASKS_LCDTake(void) {}
    void TASKS_LCDGive(void) {}
    // Debug-console sink: notestack.c calls this printf-style logger directly
    // (the engine never consumes its output). No-op on host.
    int MIOS32_MIDI_SendDebugMessage(const char *fmt, ...) { (void)fmt; return 0; }
}
