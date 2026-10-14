// C bridge over the Electron SoundApp's native FFmpeg decoder.
//
// We pull `decoder.cpp` in directly (rather than a header + separate TU) so the
// whole FFmpegDecoder implementation — including its rubberband/atempo filter
// graph for pitch & tempo — is compiled into one object under our `/I` include
// paths. The `extern "C"` surface below is a flat, POD-only ABI that Rust can
// bind with `#[repr(C)]`; no C++ types cross the boundary.
//
// Phase 2.3 adds formant control to the rubberband stage. The vendored
// `decoder.cpp` builds its rubberband args with a fixed `"tempo=%f:pitch=%f"`
// format and exposes no formant setter, so we interpose `snprintf` for the
// duration of that one include: our shim forwards to `vsnprintf`, and — only
// for the rubberband argument string — appends `:formant=preserved` or
// `:formant=shifted`. `decoder.cpp` itself is never edited; the hook lives
// entirely in this translation unit.

#include <cstdio>
#include <cstring>
#include <cstdarg>

// Pre-include the decoder's own header (which pulls in the FFmpeg headers and,
// transitively, <stdio.h>) *before* the macro below is defined, so no system
// header ever declares `snprintf` under the shim's name.
#include "D:/Work/_GIT/SoundApp/libs/ffmpeg-napi-interface/src/decoder.h"

static bool g_formant_preserve = false;

static int sa_snprintf(char* buf, size_t n, const char* fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int r = vsnprintf(buf, n, fmt, ap);
    va_end(ap);
    if (fmt && std::strstr(fmt, "tempo=%f:pitch=%f") && n > 0) {
        size_t cur = std::strlen(buf);
        if (cur + 1 < n) {
            std::strncat(buf,
                         g_formant_preserve ? ":formant=preserved" : ":formant=shifted",
                         n - cur - 1);
        }
    }
    return r;
}

#define snprintf sa_snprintf
#include "D:/Work/_GIT/SoundApp/libs/ffmpeg-napi-interface/src/decoder.cpp"
#undef snprintf

extern "C" {

struct SaAudioMeta {
    char title[128];
    char artist[128];
    char album[128];
    char genre[64];
    char codec[64];
    char format[64];
    double duration;
    int bitrate;
    int sample_rate;
    int channels;
    int bits_per_sample;
    int has_cover_art;
    int cover_art_len;
};

static void sa_copy_str(char* dst, size_t cap, const std::string& src) {
    if (cap == 0) return;
    strncpy(dst, src.c_str(), cap - 1);
    dst[cap - 1] = '\0';
}

void* sa_ffmpeg_open(const char* path_utf8, int out_sample_rate) {
    if (!path_utf8) return nullptr;
    FFmpegDecoder* dec = new FFmpegDecoder();
    if (!dec->open(path_utf8, out_sample_rate, 0)) {
        delete dec;
        return nullptr;
    }
    return static_cast<void*>(dec);
}

void sa_ffmpeg_close(void* handle) {
    if (!handle) return;
    delete static_cast<FFmpegDecoder*>(handle);
}

int sa_ffmpeg_read(void* handle, float* out_buf, int num_samples) {
    if (!handle || !out_buf || num_samples <= 0) return 0;
    return static_cast<FFmpegDecoder*>(handle)->read(out_buf, num_samples);
}

int sa_ffmpeg_seek(void* handle, double seconds) {
    if (!handle) return 0;
    return static_cast<FFmpegDecoder*>(handle)->seek(seconds) ? 1 : 0;
}

double sa_ffmpeg_duration(void* handle) {
    if (!handle) return 0.0;
    return static_cast<FFmpegDecoder*>(handle)->getDuration();
}

int sa_ffmpeg_set_pitch_semitones(void* handle, double semitones) {
    if (!handle) return 0;
    return static_cast<FFmpegDecoder*>(handle)->setPitchShift(semitones) ? 1 : 0;
}

int sa_ffmpeg_set_tempo(void* handle, double tempo) {
    if (!handle) return 0;
    return static_cast<FFmpegDecoder*>(handle)->setTimeStretch(tempo) ? 1 : 0;
}

// Select whether the rubberband stage preserves (1) or shifts (0) the formant
// spectrum. The flag is applied to the graph if/when one is built; rebuilding
// the current graph lets an already-active instance pick it up immediately.
int sa_ffmpeg_set_formant(void* handle, int preserve) {
    bool want = preserve != 0;
    if (want == g_formant_preserve) return 1;
    g_formant_preserve = want;
    if (!handle) return 1;
    FFmpegDecoder* dec = static_cast<FFmpegDecoder*>(handle);
    // Reinitialise the filter graph (no-op when no rubberband stage is active).
    dec->setPitchShift(dec->getPitchShift());
    return 1;
}

int sa_ffmpeg_probe_metadata(const char* path_utf8, SaAudioMeta* out_meta) {
    if (!path_utf8 || !out_meta) return 0;
    memset(out_meta, 0, sizeof(SaAudioMeta));

    FFmpegDecoder::AudioMetadata m = FFmpegDecoder::getFileMetadata(path_utf8);

    sa_copy_str(out_meta->title, sizeof(out_meta->title), m.title);
    sa_copy_str(out_meta->artist, sizeof(out_meta->artist), m.artist);
    sa_copy_str(out_meta->album, sizeof(out_meta->album), m.album);
    sa_copy_str(out_meta->genre, sizeof(out_meta->genre), m.genre);
    sa_copy_str(out_meta->codec, sizeof(out_meta->codec), m.codec);
    sa_copy_str(out_meta->format, sizeof(out_meta->format), m.format);

    out_meta->duration = m.duration;
    out_meta->bitrate = m.bitrate;
    out_meta->sample_rate = m.sampleRate;
    out_meta->channels = m.channels;
    out_meta->bits_per_sample = m.bitsPerSample;
    out_meta->has_cover_art = m.coverArt.empty() ? 0 : 1;
    out_meta->cover_art_len = static_cast<int>(m.coverArt.size());

    return (m.sampleRate > 0 || m.duration > 0.0) ? 1 : 0;
}

}  // extern "C"
