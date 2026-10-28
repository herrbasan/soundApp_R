#ifndef FFMPEG_DECODER_H
#define FFMPEG_DECODER_H

extern "C" {
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libswresample/swresample.h>
#include <libavfilter/avfilter.h>
#include <libavfilter/buffersrc.h>
#include <libavfilter/buffersink.h>
#include <libavutil/opt.h>
}

#include <string>
#include <vector>
#include <functional>

/**
 * FFmpegDecoder - High-performance audio decoder using FFmpeg libraries
 * 
 * Features:
 * - Decodes any audio format FFmpeg supports
 * - Instant seeking via av_seek_frame()
 * - Streams samples on-demand for real-time playback
 * - Output: float32 stereo at 44.1kHz (via libswresample)
 */
class FFmpegDecoder {
private:
    AVFormatContext* formatCtx;
    AVCodecContext* codecCtx;
    SwrContext* swrCtx;
    AVPacket* packet;
    AVFrame* frame;
    int audioStreamIndex;
    
    // Filter graph for time/pitch manipulation
    AVFilterGraph* filterGraph;
    AVFilterContext* bufferSrcCtx;
    AVFilterContext* bufferSinkCtx;
    AVFrame* filteredFrame;
    bool filtersEnabled;
    double pitchShift;     // In semitones (-12 to +12)
    double timeStretch;    // Playback rate (0.25 to 4.0)
    int64_t filterPts;      // PTS counter for filter input (in samples)
    
    // Decoded sample buffer (interleaved float32 stereo)
    float* sampleBuffer;
    int sampleBufferSize;     // Total capacity in samples
    int samplesInBuffer;      // Current number of samples
    int bufferReadPos;        // Read position in samples

    // Decoder/resampler drain state
    bool eofSignaled;
    bool decoderDrained;
    bool resamplerDrained;

    // Metadata helpers
    static std::string getTag(AVDictionary* dict, const char* key);
    static int parseTrackNumber(const std::string& str, int* total);
    bool initFilters();
    void closeFilters();
    
    // Output format (per-instance sample rate, fixed stereo)
    static const int DEFAULT_OUTPUT_SAMPLE_RATE = 44100;
    static const int OUTPUT_CHANNELS = 2;

    int outputSampleRate;
    int threadCount;
    
    bool initResampler();
    int decodeNextFrame();
    void flushBuffers();
    
public:
    FFmpegDecoder();
    ~FFmpegDecoder();
    
    // Lifecycle
    bool open(const char* filePath, int outSampleRate = DEFAULT_OUTPUT_SAMPLE_RATE, int threads = 0);
    void close();
    
    // Playback
    bool seek(double seconds);
    int read(float* outBuffer, int numSamples);
    
    // Time/Pitch manipulation
    bool setPitchShift(double semitones);
    bool setTimeStretch(double rate);
    double getPitchShift() const { return pitchShift; }
    double getTimeStretch() const { return timeStretch; }
    
    // Metadata
    double getDuration() const;
    int getSampleRate() const { return outputSampleRate; }
    int getChannels() const { return OUTPUT_CHANNELS; }
    int64_t getTotalSamples() const;

    struct AudioMetadata {
        std::string title;
        std::string artist;
        std::string album;
        std::string albumArtist;
        std::string genre;
        std::string date;
        std::string comment;
        int trackNumber = 0;
        int trackTotal = 0;
        int discNumber = 0;
        int discTotal = 0;

        std::string codec;
        std::string codecLongName;
        std::string format;
        std::string formatLongName;
        double duration = 0.0;
        int bitrate = 0;
        int sampleRate = 0;
        int channels = 0;
        int bitsPerSample = 0;

        std::vector<uint8_t> coverArt;
        std::string coverArtMimeType;
    };

    AudioMetadata getMetadata() const;
    static AudioMetadata getFileMetadata(const char* filePath);
    
    struct WaveformData {
        std::vector<float> peaksL;
        std::vector<float> peaksR;
        int points;
    };

    WaveformData getWaveform(int numPoints);
    
    // Streaming waveform with progress callback
    typedef std::function<bool(const WaveformData&, float)> ProgressCallback;
    WaveformData getWaveformStreaming(int numPoints, int64_t chunkSizeBytes, ProgressCallback callback);
    
    // Status
    bool isOpen() const { return formatCtx != nullptr; }
    bool hasError() const;
};

#endif // FFMPEG_DECODER_H
