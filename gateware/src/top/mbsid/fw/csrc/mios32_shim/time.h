/* time.h — freestanding shim for jsw_rand.c (mios32 random module).
 *
 * jsw_rand.c #includes <time.h> only for jsw_time_seed(), which seeds the
 * Mersenne Twister from time(0). The Lead engine path uses the deterministic
 * jsw_rand()/jsw_seed() API; on a freestanding target there is no wall clock, so
 * time() returns a constant (see cxx_runtime.c) -> a fixed, reproducible seed.
 * Reproducibility is desirable for M1's bit-exact oracle comparison.
 */
#ifndef MBSID_SHIM_TIME_H
#define MBSID_SHIM_TIME_H

#include <stddef.h>  /* size_t — the real <time.h> drags this in; jsw_rand.c needs it */

typedef long time_t;

time_t time(time_t *t);

#endif /* MBSID_SHIM_TIME_H */
