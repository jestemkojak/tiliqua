// Freestanding mios32 facade for the MBSID "Lead" engine on Tiliqua (VexiiRiscv,
// riscv32im, no real mios32 / no FreeRTOS / no STM32 HAL).
//
// This is the umbrella header the vendored engine includes as <mios32.h>.
// It provides ONLY what the Lead subset actually references:
//   - the s8..u32 integer typedefs (ex mios32_datatypes.h)
//   - the MIDI package/port/event/chn types (ex mios32_midi.h) used by the
//     engine's MIDI handling and SysEx code
//   - inline no-op stubs for the handful of MIOS32_* symbols the subset calls
//
// Stub policy (Task 0): IRQ/LED/STOPWATCH/TIMER/callback-registration are
// safe no-ops on a single-hart core whose audio tick already runs inside a
// critical section. Symbols that return DATA consumed by the engine are NOT
// blindly stubbed — see notes per symbol below.
#ifndef _MIOS32_H
#define _MIOS32_H

#include <stddef.h>   // freestanding: NULL, size_t (engine uses NULL via <mios32.h>)
#include <stdint.h>   // int32_t/uint32_t: width-exact on BOTH host (LP64) and target (ilp32)

#ifdef __cplusplus
extern "C" {
#endif

/////////////////////////////////////////////////////////////////////////////
// Integer types (verbatim layout from mios32/include/mios32/mios32_datatypes.h)
/////////////////////////////////////////////////////////////////////////////
#if !defined(__STM32F10x_H) && !defined(__STM32F4xx_H)

// NOTE: 32-bit types are int32_t/uint32_t, NOT (signed/unsigned) long. On the
// ilp32 target `long` is 32-bit, but on the LP64 host oracle it is 64-bit — so
// `long` would let the host miss 32-bit-overflow math the target performs. Using
// the fixed-width stdint types makes the host oracle width-identical to target.
typedef int32_t      s32;
typedef signed short s16;
typedef signed char  s8;

typedef int32_t      const sc32;
typedef signed short const sc16;
typedef signed char  const sc8;

typedef volatile int32_t      vs32;
typedef volatile signed short vs16;
typedef volatile signed char  vs8;

typedef uint32_t       u32;
typedef unsigned short u16;
typedef unsigned char  u8;

typedef uint32_t       const uc32;
typedef unsigned short const uc16;
typedef unsigned char  const uc8;

typedef volatile uint32_t       vu32;
typedef volatile unsigned short vu16;
typedef volatile unsigned char  vu8;

#define U8_MAX     ((u8)255)
#define S8_MAX     ((s8)127)
#define S8_MIN     ((s8)-128)
#define U16_MAX    ((u16)65535u)
#define S16_MAX    ((s16)32767)
#define S16_MIN    ((s16)-32768)
#define U32_MAX    ((u32)4294967295uL)
#define S32_MAX    ((s32)2147483647)
#define S32_MIN    ((s32)-2147483648)

#endif

/////////////////////////////////////////////////////////////////////////////
// MIDI types (verbatim layout from mios32/include/mios32/mios32_midi.h)
/////////////////////////////////////////////////////////////////////////////
typedef enum {
  DEFAULT    = 0x00,
  MIDI_DEBUG = 0x01,

  USB0 = 0x10, USB1 = 0x11, USB2 = 0x12, USB3 = 0x13,
  USB4 = 0x14, USB5 = 0x15, USB6 = 0x16, USB7 = 0x17,

  UART0 = 0x20, UART1 = 0x21, UART2 = 0x22, UART3 = 0x23,

  IIC0 = 0x30, IIC1 = 0x31, IIC2 = 0x32, IIC3 = 0x33,
  IIC4 = 0x34, IIC5 = 0x35, IIC6 = 0x36, IIC7 = 0x37,

  OSC0 = 0x40, OSC1 = 0x41, OSC2 = 0x42, OSC3 = 0x43,
  OSC4 = 0x44, OSC5 = 0x45, OSC6 = 0x46, OSC7 = 0x47,

  SPIM0 = 0x50, SPIM1 = 0x51, SPIM2 = 0x52, SPIM3 = 0x53,
  SPIM4 = 0x54, SPIM5 = 0x55, SPIM6 = 0x56, SPIM7 = 0x57
} mios32_midi_port_t;

typedef enum {
  NoteOff       = 0x8,
  NoteOn        = 0x9,
  PolyPressure  = 0xa,
  CC            = 0xb,
  ProgramChange = 0xc,
  Aftertouch    = 0xd,
  PitchBend     = 0xe
} mios32_midi_event_t;

typedef enum {
  Chn1,  Chn2,  Chn3,  Chn4,  Chn5,  Chn6,  Chn7,  Chn8,
  Chn9,  Chn10, Chn11, Chn12, Chn13, Chn14, Chn15, Chn16
} mios32_midi_chn_t;

typedef union {
  struct {
    u32 ALL;
  };
  struct {
    u8 cin_cable;
    u8 evnt0;
    u8 evnt1;
    u8 evnt2;
  };
  struct {
    u8 type:4;
    u8 cable:4;
    u8 chn:4;   // mios32_midi_chn_t
    u8 event:4; // mios32_midi_event_t
    u8 value1;
    u8 value2;
  };
  struct {
    u8 cin:4;
    u8 dummy1_cable:4;
    u8 dummy1_chn:4;
    u8 dummy1_event:4;
    u8 note:8;
    u8 velocity:8;
  };
  struct {
    u8 dummy2_cin:4;
    u8 dummy2_cable:4;
    u8 dummy2_chn:4;
    u8 dummy2_event:4;
    u8 cc_number:8;
    u8 value:8;
  };
  struct {
    u8 dummy3_cin:4;
    u8 dummy3_cable:4;
    u8 dummy3_chn:4;
    u8 dummy3_event:4;
    u8 program_change:8;
    u8 dummy3:8;
  };
} mios32_midi_package_t;

/////////////////////////////////////////////////////////////////////////////
// MIOS32_* stubs referenced by the Lead subset
/////////////////////////////////////////////////////////////////////////////

// Single-hart core; the engine audio tick already runs in a critical section,
// so disabling/enabling IRQs around short engine critical regions is a no-op.
static inline s32 MIOS32_IRQ_Disable(void) { return 0; }
static inline s32 MIOS32_IRQ_Enable(void)  { return 0; }

// SysEx TX. On Tiliqua MBSID does not emit SysEx replies (no patch dumps in
// the Lead-only port); swallow and report success. If/when SysEx replies are
// wanted, this routes to the firmware MIDI TX path instead.
static inline s32 MIOS32_MIDI_SendSysEx(mios32_midi_port_t port, u8 *stream, u32 count) {
  (void)port; (void)stream; (void)count; return 0;
}

// DEBUG_MSG: in real mios32 this is MIOS32_MIDI_SendDebugMessage (printf-style
// debug console over MIDI). Log-only -> no-op here. Variadic macro that
// discards its args (kept side-effect-free; never consumed by the engine).
#define DEBUG_MSG(...) do { } while (0)

// notestack.c calls MIOS32_MIDI_SendDebugMessage() directly (printf-style console
// dump in NOTESTACK_SendDebugMessage). Debug-only, never on the engine hot path:
// a per-TU no-op inline (variadic, args discarded) keeps it link-symbol-free.
static inline s32 MIOS32_MIDI_SendDebugMessage(const char *format, ...) {
  (void)format; return 0;
}

// sprintf: used only by MbSidEnvironment to format human-readable patch names
// (display strings). NOT on the Lead register-write path — M1 has no UI that
// shows patch names, so --gc-sections drops the only caller and the firmware
// links fine WITHOUT any (s)printf implementation. Declared here so the engine
// TUs compile; if a future UI calls it, provide a real formatter at link time.
// snprintf declared too for safer future use.
int sprintf (char *str, const char *fmt, ...);
int snprintf(char *str, size_t size, const char *fmt, ...);

// SysEx device id used to match incoming patch-dump headers. Fixed to 0
// (matches the engine default). NOTE: returns data the SysEx parser compares
// against — value is a deliberate choice, not arbitrary. Change here to set
// the device id the firmware should answer to.
static inline s32 MIOS32_MIDI_DeviceIDGet(void) { return 0; }

/////////////////////////////////////////////////////////////////////////////
// Link-time stubs: Task semaphore functions (provided by firmware)
/////////////////////////////////////////////////////////////////////////////
// All are extern void, safe to implement as no-ops (single-hart, no real RTOS).
// The engine calls these to guard MIDI IN/OUT and SD card accesses, but on
// a single-hart core with audio already in a critical section, these are
// effectively disabled. Firmware must provide all six at link time:
extern void TASKS_MIDIOUTSemaphoreTake(void);
extern void TASKS_MIDIOUTSemaphoreGive(void);
extern void TASKS_MIDIINSemaphoreTake(void);
extern void TASKS_MIDIINSemaphoreGive(void);
extern void TASKS_SDCardSemaphoreTake(void);
extern void TASKS_SDCardSemaphoreGive(void);
extern void TASKS_LCDTake(void);
extern void TASKS_LCDGive(void);

#ifdef __cplusplus
}
#endif

#endif /* _MIOS32_H */
