/* Compare portable checkpoint history with an independently retained upstream
 * decoder, frame by frame. SPDX-License-Identifier: GPL-2.0-or-later */
#include "db2010-amr-nb.h"
#include "sp_dec.h"
#include "amrdecode.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int main(int argc, char **argv)
{
    void *reference = NULL;
    DB2010AmrNb history = {0};
    unsigned frames = 0;
    if (GSMInitDecode(&reference, (Word8 *)"reference")) { return 1; }
    for (;;) {
        uint8_t frame[32];
        int c = getchar();
        if (c == EOF) { break; }
        frame[0] = c;
        unsigned bytes = db2010_amr_nb_frame_bytes(frame[0]);
        if (!bytes || fread(frame + 1, 1, bytes - 1, stdin) != bytes - 1) {
            fprintf(stderr, "invalid-frame\n"); return 2;
        }
        int16_t actual[160], expected[160];
        if (!db2010_amr_nb_decode(&history, frame, bytes, actual) ||
            !db2010_amr_nb_state_valid(&history)) {
            fprintf(stderr, "invalid-history at %u\n", frames); return 2;
        }
        unsigned type = frame[0] & 4 ? (frame[0] >> 3) & 15 : 15;
        if (AMRDecode(reference, (enum Frame_Type_3GPP)type, frame + 1, expected, MIME_IETF) < 0 ||
            memcmp(actual, expected, sizeof(actual))) {
            fprintf(stderr, "upstream-mismatch at %u\n", frames); return 3;
        }
        if (argc == 2 && !strcmp(argv[1], "tamper")) {
            DB2010AmrNb damaged = history;
            damaged.tags[0]++;
            if (db2010_amr_nb_state_valid(&damaged)) { return 4; }
            DB2010AmrNb before = damaged;
            if (db2010_amr_nb_decode(&damaged, frame, bytes, expected) ||
                memcmp(&before, &damaged, sizeof(before))) { return 4; }
        }
        for (unsigned i = 0; i < 160; i++) {
            putchar((uint16_t)actual[i] & 255); putchar((uint16_t)actual[i] >> 8);
        }
        frames++;
    }
    GSMDecodeFrameExit(&reference);
    fprintf(stderr, "frames=%u\n", frames);
    return 0;
}
