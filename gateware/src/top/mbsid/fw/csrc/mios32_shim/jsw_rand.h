// Facade for the mios32 random module (declarations verbatim from
// mios32/modules/random/jsw_rand.h). REAL module: jsw_rand() returns 32-bit
// PRNG values the engine (MbSidRandomGen) consumes -- NOT a no-op stub.
// The implementation (jsw_rand.c, Mersenne Twister, public domain) must be
// compiled & linked in Task 1/4.
#ifndef JSW_RAND_H
#define JSW_RAND_H

#ifdef __cplusplus
extern "C" {
#endif

extern void          jsw_seed ( unsigned long s );
extern unsigned long jsw_rand ( void );
extern unsigned      jsw_time_seed ( void );

#ifdef __cplusplus
}
#endif

#endif /* JSW_RAND_H */
