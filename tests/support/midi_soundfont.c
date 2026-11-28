#include "qemu/osdep.h"
#include "db2010-midi-stream.h"
#include <assert.h>

static void render(DB2010MidiStream *stream, unsigned channel, unsigned program,
                   unsigned note, bool soundfont)
{
    DB2010AudioSink sink = {0};
    uint16_t events[] = {
        0xb0 | channel, 0, 122, 0, 0,
        0xc0 | channel, program, 0, 0, 0,
        0x90 | channel, note, 110, 0, 0,
        0x80 | channel, note, 0, 960, 0,
        0, 0, 0, 0, 0,
    };
    db2010_midi_stream_discard_soundfont(stream);
    db2010_midi_stream_init(stream);
    assert(!db2010_midi_stream_append(stream, events, sizeof(events) / sizeof(*events)));
    db2010_midi_stream_run(stream, 0);
    assert(db2010_midi_stream_uses_soundfont(stream) == soundfont);
    assert(!db2010_midi_stream_advance(stream, &sink, 10000000));
    db2010_midi_stream_pause(stream, &sink);
    assert(!db2010_midi_stream_advance(stream, &sink, 510000000));
    assert(stream->frames == 480);
    db2010_midi_stream_run(stream, 510000000);
    assert(db2010_midi_stream_uses_soundfont(stream) == soundfont);
    assert(!db2010_midi_stream_advance(stream, &sink, 610000000));
    assert(stream->completed && stream->frames == 4032 && stream->events == 4);
    assert(stream->credit == sizeof(events) / sizeof(*events));
    assert(stream->nonzero_samples > 0);
}

int main(int argc, char **argv)
{
    DB2010MidiStream stream = {0};
    bool soundfont = argc == 2;
    render(&stream, 0, 0, 60, soundfont);
    uint64_t piano = stream.pcm_hash;
    render(&stream, 0, 40, 60, soundfont);
    assert((stream.pcm_hash != piano) == soundfont);
    render(&stream, 9, 0, 60, soundfont);
    assert((stream.pcm_hash != piano) == soundfont);
    printf("%016" PRIx64 "\n", piano);
    db2010_midi_stream_discard_soundfont(&stream);
    return 0;
}
