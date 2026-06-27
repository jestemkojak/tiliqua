// Facade for the mios32 notestack module (declarations verbatim from
// mios32/modules/notestack/notestack.h). This is a REAL module the engine
// consumes structurally (notestack_t fields are read/written; NOTESTACK_*
// functions return note data the arp/voice-queue use) -- it is NOT a no-op
// stub. The implementation (notestack.c) must be compiled & linked in Task 1/4.
#ifndef _NOTESTACK_H
#define _NOTESTACK_H

#include <mios32.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
  NOTESTACK_MODE_PUSH_TOP = 0,
  NOTESTACK_MODE_PUSH_BOTTOM,
  NOTESTACK_MODE_PUSH_TOP_HOLD,
  NOTESTACK_MODE_PUSH_BOTTOM_HOLD,
  NOTESTACK_MODE_SORT,
  NOTESTACK_MODE_SORT_HOLD
} notestack_mode_t;

typedef union {
  u16 ALL;
  struct {
    u8 note:7;
    u8 depressed:1;
    u8 tag;
  };
} notestack_item_t;

typedef struct {
  notestack_mode_t mode;
  u8               size;
  u8               len;
  notestack_item_t *note_items;
} notestack_t;

extern s32 NOTESTACK_Init(notestack_t *n, notestack_mode_t mode, notestack_item_t *note_items, u8 size);
extern s32 NOTESTACK_Push(notestack_t *n, u8 new_note, u8 tag);
extern s32 NOTESTACK_Pop(notestack_t *n, u8 old_note);
extern s32 NOTESTACK_CountActiveNotes(notestack_t *n);
extern s32 NOTESTACK_RemoveNonActiveNotes(notestack_t *n);
extern s32 NOTESTACK_Clear(notestack_t *n);
extern s32 NOTESTACK_SendDebugMessage(notestack_t *n);

#ifdef __cplusplus
}
#endif

#endif /* _NOTESTACK_H */
