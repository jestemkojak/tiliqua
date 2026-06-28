/* seq.h — shared sequence parser + 1 kHz driver loop for the MBSID host oracle.
 *
 * Both oracle.cpp (engine reference) and shim_driver.cpp (mbsid_* ABI) include
 * this so they parse the SAME file and run the SAME control loop — the only
 * difference is the Backend they hand to run_sequence().
 *
 * Sequence file format (one event per line, '#' comments and blanks ignored):
 *     <t_ms> patch <row>        select sid_bank_preset_0[row] via sysexSetPatch
 *     <t_ms> pc    <row>        select sid_bank_preset_0[row] via bankLoad (Program Change path)
 *     <t_ms> on    <note> <vel>
 *     <t_ms> off   <note>
 *     <t_ms> cc    <num> <val>
 *     <t_ms> bend  <val14>      0..16383, 8192 = centre
 *     <t_ms> end                stop the loop after this ms-tick
 *
 * Trace format emitted: "<t_ms> <L|R> <reg> <hexval>\n" for every changed
 * register 0..31 after each tick (L block then R block).
 */
#ifndef HOST_ORACLE_SEQ_H
#define HOST_ORACLE_SEQ_H

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <cstdint>
#include <vector>
#include <string>

struct SeqEvent {
    int t_ms;
    enum Kind { PATCH, PC, ON, OFF, CC, BEND, END } kind;
    int a;   // patch row / note / cc num / bend value
    int b;   // velocity / cc value
};

static inline std::vector<SeqEvent> seq_parse(const char *path) {
    std::vector<SeqEvent> evts;
    FILE *f = fopen(path, "r");
    if (!f) { fprintf(stderr, "seq_parse: cannot open %s\n", path); exit(2); }
    char line[256];
    int lineno = 0;
    while (fgets(line, sizeof(line), f)) {
        ++lineno;
        // strip comment
        char *hash = strchr(line, '#');
        if (hash) *hash = '\0';
        char ev[32];
        int t = 0, a = 0, b = 0;
        int n = sscanf(line, "%d %31s %d %d", &t, ev, &a, &b);
        if (n < 2) continue;  // blank / comment-only
        SeqEvent e; e.t_ms = t; e.a = a; e.b = b;
        if      (!strcmp(ev, "patch")) e.kind = SeqEvent::PATCH;
        else if (!strcmp(ev, "pc"))    e.kind = SeqEvent::PC;
        else if (!strcmp(ev, "on"))    e.kind = SeqEvent::ON;
        else if (!strcmp(ev, "off"))   e.kind = SeqEvent::OFF;
        else if (!strcmp(ev, "cc"))    e.kind = SeqEvent::CC;
        else if (!strcmp(ev, "bend"))  e.kind = SeqEvent::BEND;
        else if (!strcmp(ev, "end"))   e.kind = SeqEvent::END;
        else { fprintf(stderr, "seq_parse: %s:%d unknown event '%s'\n", path, lineno, ev); exit(2); }
        evts.push_back(e);
    }
    fclose(f);
    return evts;
}

/* Backend concept (duck-typed):
 *   void          init();
 *   int           load_patch(int row);   // 0 = ok
 *   int           program_change(int patch); // bankLoad(0,0,patch); 0 = ok
 *   void          note_on(int note, int vel);
 *   void          note_off(int note);
 *   void          cc(int num, int val);
 *   void          bend(int val14);
 *   int           tick();                 // advance one ms
 *   const uint8_t *regs();                // 32-byte L image
 *   const uint8_t *regs_r();              // 32-byte R image
 */
template <class Backend>
static inline void run_sequence(const char *path, Backend &be, FILE *out) {
    std::vector<SeqEvent> evts = seq_parse(path);
    int last_t = 0;
    bool have_end = false;
    for (size_t i = 0; i < evts.size(); ++i) {
        if (evts[i].t_ms > last_t) last_t = evts[i].t_ms;
        if (evts[i].kind == SeqEvent::END) have_end = true;
    }
    (void)have_end;

    be.init();
    unsigned char shadow_l[32];
    unsigned char shadow_r[32];
    memset(shadow_l, 0, sizeof(shadow_l));
    memset(shadow_r, 0, sizeof(shadow_r));

    size_t ei = 0;
    for (int t = 0; t <= last_t; ++t) {
        bool stop = false;
        // apply all events scheduled at this ms (in file order)
        for (; ei < evts.size() && evts[ei].t_ms == t; ++ei) {
            const SeqEvent &e = evts[ei];
            switch (e.kind) {
            case SeqEvent::PATCH: be.load_patch(e.a);        break;
            case SeqEvent::PC:    be.program_change(e.a);    break;
            case SeqEvent::ON:    be.note_on(e.a, e.b);      break;
            case SeqEvent::OFF:   be.note_off(e.a);          break;
            case SeqEvent::CC:    be.cc(e.a, e.b);           break;
            case SeqEvent::BEND:  be.bend(e.a);              break;
            case SeqEvent::END:   stop = true;               break;
            }
        }
        // one engine tick per ms (1 kHz control rate)
        be.tick();
        const uint8_t *l = be.regs();
        const uint8_t *r = be.regs_r();
        for (int reg = 0; reg < 32; ++reg) {
            if (l[reg] != shadow_l[reg]) {
                shadow_l[reg] = l[reg];
                fprintf(out, "%d L %d %02x\n", t, reg, l[reg]);
            }
        }
        for (int reg = 0; reg < 32; ++reg) {
            if (r[reg] != shadow_r[reg]) {
                shadow_r[reg] = r[reg];
                fprintf(out, "%d R %d %02x\n", t, reg, r[reg]);
            }
        }
        if (stop) break;
    }
}

#endif /* HOST_ORACLE_SEQ_H */
