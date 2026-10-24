// C ABI shim over Rubber Band Library v4.0.0 (RubberBandStretcher, R3/finer
// engine) for the soundapp stretcher thread. The full library is vendored in
// this directory (see COPYING — GPL-2.0-or-later / commercial dual licence);
// this shim is the only file we author.
//
// The stretcher runs stereo, so the ABI is fixed to two channels: callers
// pass de-interleaved L/R pointers and frame counts. Options are assembled by
// the Rust side from the public RubberBandStretcher::Options bit values and
// passed through untouched, so the engine/mode choice lives in one place.

#include "rubberband/RubberBandStretcher.h"

#include <cstring>

using RubberBand::RubberBandStretcher;

extern "C" {

struct RbHandle {
    RubberBandStretcher *st;
};

RbHandle* rb_create(unsigned sample_rate, unsigned channels, unsigned options) {
    RubberBandStretcher::Options opts = static_cast<RubberBandStretcher::Options>(options);
    RubberBandStretcher* st = new RubberBandStretcher(sample_rate, channels, opts);
    RbHandle* h = new RbHandle{st};
    return h;
}

void rb_destroy(RbHandle* h) {
    if (!h) return;
    delete h->st;
    delete h;
}

void rb_reset(RbHandle* h) {
    h->st->reset();
}

void rb_set_time_ratio(RbHandle* h, double ratio) {
    h->st->setTimeRatio(ratio);
}

void rb_set_pitch_scale(RbHandle* h, double scale) {
    h->st->setPitchScale(scale);
}

void rb_set_formant_preserved(RbHandle* h, int preserved) {
    using O = RubberBandStretcher::Options;
    h->st->setFormantOption(preserved ? RubberBandStretcher::OptionFormantPreserved
                                      : static_cast<O>(RubberBandStretcher::OptionFormantShifted));
}

size_t rb_get_preferred_start_pad(RbHandle* h) {
    return h->st->getPreferredStartPad();
}

size_t rb_get_start_delay(RbHandle* h) {
    return h->st->getStartDelay();
}

int rb_available(RbHandle* h) {
    return h->st->available();
}

// Feed `frames` de-interleaved stereo frames. `in_l`/`in_r` must be non-null
// unless `frames` is 0 (the final-flush call passes null, 0, 1).
void rb_process(RbHandle* h, const float* in_l, const float* in_r,
                size_t frames, int final_) {
    const float* in[2] = { in_l, in_r };
    h->st->process(in, frames, final_ != 0);
}

// Retrieve up to `frames` de-interleaved stereo frames; returns frames got.
size_t rb_retrieve(RbHandle* h, float* out_l, float* out_r, size_t frames) {
    float* out[2] = { out_l, out_r };
    return h->st->retrieve(out, frames);
}

}
