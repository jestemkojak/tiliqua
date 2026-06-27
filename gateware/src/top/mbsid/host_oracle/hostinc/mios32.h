/* hostinc/mios32.h — host-build redirect to the real shim umbrella header.
 *
 * The full mios32_shim dir cannot be on the host include path because its
 * freestanding <string.h> shadows host libc's (cstring needs strchr/strstr/...
 * which the freestanding subset omits).  This dir exposes ONLY mios32.h, so
 * the engine still gets the exact same umbrella header while <string.h>,
 * <stdint.h> etc. resolve to the host system headers.
 */
#include "../../fw/csrc/mios32_shim/mios32.h"
