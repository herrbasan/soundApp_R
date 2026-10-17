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

// swscale.h ships without an `extern "C"` guard, unlike the av* headers pulled in
// by decoder.h — without this wrapper the linker sees C++-mangled sws_* symbols.
extern "C" {
#include <libswscale/swscale.h>
}

extern "C" {

struct SaAudioMeta {
    char title[128];
    char artist[128];
    char album[128];
    char genre[64];
    char codec[64];
    char format[64];
    char codec_long_name[128];
    char format_long_name[128];
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
    if (!dec->open(path_utf8, out_sample_rate, 1)) {
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

// Decode an already-extracted cover-art payload (JPEG/PNG/BMP bytes from the
// attached picture stream) and scale it to `target_side` x `target_side` RGBA.
// No file I/O: the bytes come from the metadata probe's single format open.
static int sa_decode_cover_rgba(const std::vector<uint8_t>& bytes,
                                const std::string& mime,
                                uint8_t* out_rgba,
                                int target_side) {
    if (bytes.empty() || !out_rgba || target_side <= 0) return 0;

    AVCodecID id = AV_CODEC_ID_MJPEG;
    if (bytes.size() >= 8 &&
        bytes[0] == 0x89 && bytes[1] == 'P' && bytes[2] == 'N' && bytes[3] == 'G') {
        id = AV_CODEC_ID_PNG;
    } else if (bytes.size() >= 2 && bytes[0] == 'B' && bytes[1] == 'M') {
        id = AV_CODEC_ID_BMP;
    } else if (bytes.size() >= 12 &&
               bytes[0] == 'R' && bytes[1] == 'I' && bytes[2] == 'F' && bytes[3] == 'F' &&
               bytes[8] == 'W' && bytes[9] == 'E' && bytes[10] == 'B' && bytes[11] == 'P') {
        id = AV_CODEC_ID_WEBP;
    } else if (mime == "image/png") {
        id = AV_CODEC_ID_PNG;
    } else if (mime == "image/bmp") {
        id = AV_CODEC_ID_BMP;
    }

    const AVCodec* dec = avcodec_find_decoder(id);
    if (!dec) return 0;
    AVCodecContext* ctx = avcodec_alloc_context3(dec);
    if (!ctx) return 0;
    // Single-threaded cover decode: avoids 24-thread DLL_THREAD_ATTACH loader-lock
    // contention with the UI thread's OpenGL context / swap_buffers.
    ctx->thread_count = 1;

    int ok = 0;
    if (avcodec_open2(ctx, dec, nullptr) >= 0) {
        AVPacket* pkt = av_packet_alloc();
        AVFrame* frame = av_frame_alloc();
        if (pkt && frame && av_new_packet(pkt, static_cast<int>(bytes.size())) >= 0) {
            memcpy(pkt->data, bytes.data(), bytes.size());
            if (avcodec_send_packet(ctx, pkt) >= 0 &&
                avcodec_receive_frame(ctx, frame) >= 0 &&
                frame->width > 0 && frame->height > 0) {
                SwsContext* sws = sws_getContext(
                    frame->width, frame->height,
                    static_cast<AVPixelFormat>(frame->format),
                    target_side, target_side, AV_PIX_FMT_RGBA,
                    SWS_BILINEAR, nullptr, nullptr, nullptr);
                if (sws) {
                    uint8_t* dst_data[4] = {out_rgba, nullptr, nullptr, nullptr};
                    int dst_linesize[4] = {target_side * 4, 0, 0, 0};
                    sws_scale(sws, frame->data, frame->linesize, 0, frame->height,
                              dst_data, dst_linesize);
                    sws_freeContext(sws);
                    ok = 1;
                }
            }
        }
        av_packet_free(&pkt);
        av_frame_free(&frame);
    }
    avcodec_free_context(&ctx);
    return ok;
}

// Probe metadata AND decode the embedded cover art from ONE format open.
// `getFileMetadata` returns the attached picture bytes alongside the tags, so
// the cover never costs a second `avformat_open_input` over the network.
// Returns 1 when usable metadata was found.
int sa_ffmpeg_probe_metadata(const char* path_utf8, SaAudioMeta* out_meta,
                             uint8_t* out_cover_rgba, int cover_side) {
    if (!path_utf8 || !out_meta) return 0;
    memset(out_meta, 0, sizeof(SaAudioMeta));

    FFmpegDecoder::AudioMetadata m = FFmpegDecoder::getFileMetadata(path_utf8);

    sa_copy_str(out_meta->title, sizeof(out_meta->title), m.title);
    sa_copy_str(out_meta->artist, sizeof(out_meta->artist), m.artist);
    sa_copy_str(out_meta->album, sizeof(out_meta->album), m.album);
    sa_copy_str(out_meta->genre, sizeof(out_meta->genre), m.genre);
    sa_copy_str(out_meta->codec, sizeof(out_meta->codec), m.codec);
    sa_copy_str(out_meta->format, sizeof(out_meta->format), m.format);
    sa_copy_str(out_meta->codec_long_name, sizeof(out_meta->codec_long_name), m.codecLongName);
    sa_copy_str(out_meta->format_long_name, sizeof(out_meta->format_long_name), m.formatLongName);

    out_meta->duration = m.duration;
    out_meta->bitrate = m.bitrate;
    out_meta->sample_rate = m.sampleRate;
    out_meta->channels = m.channels;
    out_meta->bits_per_sample = m.bitsPerSample;
    out_meta->has_cover_art = m.coverArt.empty() ? 0 : 1;
    out_meta->cover_art_len = static_cast<int>(m.coverArt.size());

    if (out_cover_rgba && cover_side > 0) {
        sa_decode_cover_rgba(m.coverArt, m.coverArtMimeType, out_cover_rgba, cover_side);
    }

    return (m.sampleRate > 0 || m.duration > 0.0) ? 1 : 0;
}

// Extract metadata + cover art directly from an already-open FFmpegDecoder handle
// (zero extra avformat_open_input / avformat_find_stream_info over SMB).
int sa_ffmpeg_read_handle_metadata(void* handle, SaAudioMeta* out_meta,
                                   uint8_t* out_cover_rgba, int cover_side) {
    if (!handle || !out_meta) return 0;
    memset(out_meta, 0, sizeof(SaAudioMeta));

    FFmpegDecoder::AudioMetadata m = static_cast<FFmpegDecoder*>(handle)->getMetadata();

    sa_copy_str(out_meta->title, sizeof(out_meta->title), m.title);
    sa_copy_str(out_meta->artist, sizeof(out_meta->artist), m.artist);
    sa_copy_str(out_meta->album, sizeof(out_meta->album), m.album);
    sa_copy_str(out_meta->genre, sizeof(out_meta->genre), m.genre);
    sa_copy_str(out_meta->codec, sizeof(out_meta->codec), m.codec);
    sa_copy_str(out_meta->format, sizeof(out_meta->format), m.format);
    sa_copy_str(out_meta->codec_long_name, sizeof(out_meta->codec_long_name), m.codecLongName);
    sa_copy_str(out_meta->format_long_name, sizeof(out_meta->format_long_name), m.formatLongName);

    out_meta->duration = m.duration;
    out_meta->bitrate = m.bitrate;
    out_meta->sample_rate = m.sampleRate;
    out_meta->channels = m.channels;
    out_meta->bits_per_sample = m.bitsPerSample;
    out_meta->has_cover_art = m.coverArt.empty() ? 0 : 1;
    out_meta->cover_art_len = static_cast<int>(m.coverArt.size());

    if (out_cover_rgba && cover_side > 0 && !m.coverArt.empty()) {
        sa_decode_cover_rgba(m.coverArt, m.coverArtMimeType, out_cover_rgba, cover_side);
    }

    return (m.sampleRate > 0 || m.duration > 0.0) ? 1 : 0;
}

}  // extern "C"
