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
 *     <t_ms> ch    <chn>         set current MIDI channel (sticky, default 0)
 *     <t_ms> at    <val>         channel aftertouch on current channel
 *     <t_ms> syx   <hexbytes>    feed literal SysEx bytes (e.g. syx f0...f7)
 *     <t_ms> syxpc <row>         encode sid_bank_preset_0[row] as an MBSID
 *                                RAM Write dump (type 0x08, bank 0) and feed
 *                                it byte-wise through the SysEx receiver
 *     <t_ms> kn    <knob 0..7> <val8 0..255>
 *     <t_ms> pr    <par> <val16 0..65535>
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
    enum Kind { PATCH, PC, ON, OFF, CC, BEND, AT, CH, SYX, SYXPC, KN, PR, END } kind;
    int a;   // patch row / note / cc num / bend value
    int b;   // velocity / cc value
    std::vector<uint8_t> bytes;  // SYX only: literal SysEx bytes
};

static inline std::vector<SeqEvent> seq_parse(const char *path) {
    std::vector<SeqEvent> evts;
    FILE *f = fopen(path, "r");
    if (!f) { fprintf(stderr, "seq_parse: cannot open %s\n", path); exit(2); }
    // Wide enough for a full 1036-byte SysEx dump hex-encoded inline via a
    // 'syx' line (2072 hex chars + "<t_ms> syx " prefix + margin).
    char line[2560];
    int lineno = 0;
    while (fgets(line, sizeof(line), f)) {
        ++lineno;
        // strip comment
        char *hash = strchr(line, '#');
        if (hash) *hash = '\0';
        // 'syx' carries an arbitrary-length hex string: parse it specially.
        // NB: match the event keyword as its own token first — sscanf's
        // literal "syx" in a format string matches only those 3 characters,
        // so "%d syx %s" would also (mis)match a line starting "0 syxpc ..."
        // (no word-boundary check), silently feeding "pc" to the hex parser.
        {
            int t = 0, off = 0;
            char kw[32];
            char hex[2200];
            if (sscanf(line, "%d %31s%n", &t, kw, &off) == 2 && !strcmp(kw, "syx")) {
                if (sscanf(line + off, "%2199s", hex) != 1) { fprintf(stderr, "seq_parse: %s:%d missing syx hex\n", path, lineno); exit(2); }
                SeqEvent e; e.t_ms = t; e.kind = SeqEvent::SYX; e.a = e.b = 0;
                size_t n = strlen(hex);
                if (n % 2) { fprintf(stderr, "seq_parse: %s:%d odd hex length\n", path, lineno); exit(2); }
                for (size_t i = 0; i < n; i += 2) {
                    unsigned v;
                    if (sscanf(hex + i, "%2x", &v) != 1) { fprintf(stderr, "seq_parse: %s:%d bad hex\n", path, lineno); exit(2); }
                    e.bytes.push_back((uint8_t)v);
                }
                evts.push_back(e);
                continue;
            }
        }
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
        else if (!strcmp(ev, "at"))    e.kind = SeqEvent::AT;
        else if (!strcmp(ev, "ch"))    e.kind = SeqEvent::CH;
        else if (!strcmp(ev, "syxpc")) e.kind = SeqEvent::SYXPC;
        else if (!strcmp(ev, "kn"))    e.kind = SeqEvent::KN;
        else if (!strcmp(ev, "pr"))    e.kind = SeqEvent::PR;
        else if (!strcmp(ev, "end"))   e.kind = SeqEvent::END;
        else { fprintf(stderr, "seq_parse: %s:%d unknown event '%s'\n", path, lineno, ev); exit(2); }
        evts.push_back(e);
    }
    fclose(f);
    return evts;
}

/* Encode a 512-byte patch as an MBSID Patch Write dump (1036 bytes):
 * F0 00 00 7E 4B 00 | 02 | type | bank | patch | 1024 nibbles lo-first |
 * (-sum)&0x7F | F7.  Mirrors MbSidSysEx::cmdPatchWrite's expectations. */
static inline void seq_encode_patch_dump(const unsigned char *patch512,
                                         uint8_t type, uint8_t bank,
                                         uint8_t pnum, unsigned char out[1036]) {
    static const unsigned char hdr[6] = {0xF0, 0x00, 0x00, 0x7E, 0x4B, 0x00};
    int k = 0;
    for (int i = 0; i < 6; ++i) out[k++] = hdr[i];
    out[k++] = 0x02;
    out[k++] = type; out[k++] = bank; out[k++] = pnum;
    unsigned sum = 0;
    for (int i = 0; i < 512; ++i) {
        unsigned char lo = patch512[i] & 0x0F, hi = (patch512[i] >> 4) & 0x0F;
        out[k++] = lo; out[k++] = hi; sum += lo + hi;
    }
    out[k++] = (unsigned char)((-(int)sum) & 0x7F);
    out[k++] = 0xF7;
}

/* Backend concept (duck-typed):
 *   void          init();
 *   int           load_patch(int row);   // 0 = ok
 *   int           program_change(int patch); // bankLoad(0,0,patch); 0 = ok
 *   void          note_on(int chn, int note, int vel);
 *   void          note_off(int chn, int note);
 *   void          cc(int chn, int num, int val);
 *   void          bend(int chn, int val14);
 *   void          aftertouch(int chn, int val);
 *   void          sysex_byte(uint8_t b);       // feed one literal SysEx byte
 *   void          sysex_patch_dump(int row);   // encode+feed a RAM Write dump
 *   void          knob(int k, int v);          // set knob k (0..7) to v (0..255)
 *   void          par(int par, int val16);     // set parameter par to val16
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
    int cur_chn = 0;

    size_t ei = 0;
    for (int t = 0; t <= last_t; ++t) {
        bool stop = false;
        // apply all events scheduled at this ms (in file order)
        for (; ei < evts.size() && evts[ei].t_ms == t; ++ei) {
            const SeqEvent &e = evts[ei];
            switch (e.kind) {
            case SeqEvent::PATCH: be.load_patch(e.a);              break;
            case SeqEvent::PC:    be.program_change(e.a);          break;
            case SeqEvent::CH:    cur_chn = e.a;                   break;
            case SeqEvent::ON:    be.note_on(cur_chn, e.a, e.b);   break;
            case SeqEvent::OFF:   be.note_off(cur_chn, e.a);       break;
            case SeqEvent::CC:    be.cc(cur_chn, e.a, e.b);        break;
            case SeqEvent::BEND:  be.bend(cur_chn, e.a);           break;
            case SeqEvent::AT:    be.aftertouch(cur_chn, e.a);     break;
            case SeqEvent::SYX:
                for (size_t bi = 0; bi < e.bytes.size(); ++bi)
                    be.sysex_byte(e.bytes[bi]);
                break;
            case SeqEvent::SYXPC:
                be.sysex_patch_dump(e.a);
                break;
            case SeqEvent::KN:    be.knob(e.a, e.b);             break;
            case SeqEvent::PR:    be.par(e.a, e.b);              break;
            case SeqEvent::END:   stop = true;                     break;
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
