#include "decoder.h"
#include <cstring>
#include <algorithm>
#include <cstdlib>
#include <cmath>

FFmpegDecoder::FFmpegDecoder() 
    : formatCtx(nullptr)
    , codecCtx(nullptr)
    , swrCtx(nullptr)
    , packet(nullptr)
    , frame(nullptr)
    , audioStreamIndex(-1)
    , filterGraph(nullptr)
    , bufferSrcCtx(nullptr)
    , bufferSinkCtx(nullptr)
    , filteredFrame(nullptr)
    , filtersEnabled(false)
    , pitchShift(0.0)
    , timeStretch(1.0)
    , filterPts(0)
    , sampleBuffer(nullptr)
    , sampleBufferSize(0)
    , samplesInBuffer(0)
    , bufferReadPos(0)
    , eofSignaled(false)
    , decoderDrained(false)
    , resamplerDrained(false)
    , outputSampleRate(DEFAULT_OUTPUT_SAMPLE_RATE)
    , threadCount(0)
{
    packet = av_packet_alloc();
    frame = av_frame_alloc();
    filteredFrame = av_frame_alloc();
}

FFmpegDecoder::~FFmpegDecoder() {
    close();
    if (packet) av_packet_free(&packet);
    if (frame) av_frame_free(&frame);
    if (filteredFrame) av_frame_free(&filteredFrame);
}

bool FFmpegDecoder::open(const char* filePath, int outSampleRate, int threads) {
    outputSampleRate = (outSampleRate > 0) ? outSampleRate : DEFAULT_OUTPUT_SAMPLE_RATE;
    threadCount = threads;

    // Open input file
    if (avformat_open_input(&formatCtx, filePath, nullptr, nullptr) < 0) {
        return false;
    }
    
    // Retrieve stream information
    if (avformat_find_stream_info(formatCtx, nullptr) < 0) {
        avformat_close_input(&formatCtx);
        return false;
    }
    
    // Find audio stream
    audioStreamIndex = -1;
    for (unsigned int i = 0; i < formatCtx->nb_streams; i++) {
        if (formatCtx->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO) {
            audioStreamIndex = i;
            break;
        }
    }
    
    if (audioStreamIndex == -1) {
        avformat_close_input(&formatCtx);
        return false;
    }
    
    // Get codec parameters
    AVCodecParameters* codecParams = formatCtx->streams[audioStreamIndex]->codecpar;
    
    // Find decoder
    const AVCodec* codec = avcodec_find_decoder(codecParams->codec_id);
    if (!codec) {
        avformat_close_input(&formatCtx);
        return false;
    }
    
    // Allocate codec context
    codecCtx = avcodec_alloc_context3(codec);
    if (!codecCtx) {
        avformat_close_input(&formatCtx);
        return false;
    }
    
    // Copy codec parameters to context
    if (avcodec_parameters_to_context(codecCtx, codecParams) < 0) {
        avcodec_free_context(&codecCtx);
        avformat_close_input(&formatCtx);
        return false;
    }
    
    // Configure threading (0 = auto-detect, >0 = specific thread count)
    if (threadCount > 0) {
        codecCtx->thread_count = threadCount;
    }
    codecCtx->thread_type = FF_THREAD_FRAME | FF_THREAD_SLICE;
    
    // Open codec
    if (avcodec_open2(codecCtx, codec, nullptr) < 0) {
        avcodec_free_context(&codecCtx);
        avformat_close_input(&formatCtx);
        return false;
    }
    
    // Initialize resampler
    if (!initResampler()) {
        close();
        return false;
    }
    
    // Allocate sample buffer (1 second of audio)
    sampleBufferSize = outputSampleRate * OUTPUT_CHANNELS;
    sampleBuffer = new float[sampleBufferSize];

    eofSignaled = false;
    decoderDrained = false;
    resamplerDrained = false;
    
    return true;
}

bool FFmpegDecoder::initResampler() {
    // Determine input channel layout (FFmpeg 7.0+ uses ch_layout instead of channel_layout)
    AVChannelLayout in_ch_layout;
    if (codecCtx->ch_layout.nb_channels > 0) {
        in_ch_layout = codecCtx->ch_layout;
    } else {
        // Fallback to stereo if not specified
        av_channel_layout_default(&in_ch_layout, 2);
    }
    
    // Create output channel layout (stereo)
    AVChannelLayout out_ch_layout = AV_CHANNEL_LAYOUT_STEREO;
    
    // Create resampler context (FFmpeg 7.0+ API)
    int ret = swr_alloc_set_opts2(
        &swrCtx,
        &out_ch_layout,                // Output channel layout
        AV_SAMPLE_FMT_FLT,             // Output sample format (float32)
        outputSampleRate,              // Output sample rate
        &in_ch_layout,                 // Input channel layout
        codecCtx->sample_fmt,          // Input sample format
        codecCtx->sample_rate,         // Input sample rate
        0, nullptr
    );
    
    if (ret < 0 || !swrCtx) {
        return false;
    }
    
    // Initialize resampler
    if (swr_init(swrCtx) < 0) {
        swr_free(&swrCtx);
        return false;
    }
    
    return true;
}

void FFmpegDecoder::close() {
    delete[] sampleBuffer;
    sampleBuffer = nullptr;
    
    closeFilters();
    
    if (swrCtx) {
        swr_free(&swrCtx);
        swrCtx = nullptr;
    }
    
    if (codecCtx) {
        avcodec_free_context(&codecCtx);
        codecCtx = nullptr;
    }
    
    if (formatCtx) {
        avformat_close_input(&formatCtx);
        formatCtx = nullptr;
    }
    
    audioStreamIndex = -1;
    samplesInBuffer = 0;
    bufferReadPos = 0;

    eofSignaled = false;
    decoderDrained = false;
    resamplerDrained = false;
}

bool FFmpegDecoder::seek(double seconds) {
    if (!formatCtx) return false;
    
    // Convert seconds to stream time base
    AVStream* stream = formatCtx->streams[audioStreamIndex];
    int64_t timestamp = static_cast<int64_t>(seconds * AV_TIME_BASE);
    
    // Seek to timestamp
    if (av_seek_frame(formatCtx, audioStreamIndex, 
                     av_rescale_q(timestamp, AV_TIME_BASE_Q, stream->time_base),
                     AVSEEK_FLAG_BACKWARD) < 0) {
        return false;
    }
    
    // Flush codec buffers
    avcodec_flush_buffers(codecCtx);

    // Reset resampler state (important for gapless looping / consistent output after seek)
    if (swrCtx) {
        swr_close(swrCtx);
        if (swr_init(swrCtx) < 0) {
            // If re-init fails, keep going; caller will see missing audio rather than crash
        }
    }
    
    // Clear sample buffer
    samplesInBuffer = 0;
    bufferReadPos = 0;

    // Reset filter PTS
    filterPts = 0;

    // Reset EOF/drain state
    eofSignaled = false;
    decoderDrained = false;
    resamplerDrained = false;
    
    return true;
}

int FFmpegDecoder::decodeNextFrame() {
    while (true) {
        // If filters are enabled, try to get filtered frames first
        if (filtersEnabled && bufferSinkCtx) {
            int ret = av_buffersink_get_frame(bufferSinkCtx, filteredFrame);
            if (ret >= 0) {
                uint8_t* output_buffer = reinterpret_cast<uint8_t*>(sampleBuffer);
                int out_samples = swr_convert(
                    swrCtx,
                    &output_buffer,
                    sampleBufferSize / OUTPUT_CHANNELS,
                    const_cast<const uint8_t**>(filteredFrame->data),
                    filteredFrame->nb_samples
                );
                av_frame_unref(filteredFrame);
                
                if (out_samples < 0) return -1;
                
                samplesInBuffer = out_samples * OUTPUT_CHANNELS;
                bufferReadPos = 0;
                return samplesInBuffer;
            } else if (ret != AVERROR(EAGAIN) && ret != AVERROR_EOF) {
                return -1;
            }
        }
        
        // Get decoded frame from codec
        int ret = avcodec_receive_frame(codecCtx, frame);
        if (ret == 0) {
            // If filters enabled, push frame to filter graph
            if (filtersEnabled && bufferSrcCtx && bufferSinkCtx) {
                frame->pts = filterPts;
                filterPts += frame->nb_samples;
                ret = av_buffersrc_add_frame_flags(bufferSrcCtx, frame, AV_BUFFERSRC_FLAG_KEEP_REF);
                av_frame_unref(frame);
                
                if (ret < 0) return -1;
                
                // Loop back to try getting filtered output
                continue;
            }
            
            // No filters - resample directly
            uint8_t* output_buffer = reinterpret_cast<uint8_t*>(sampleBuffer);
            int out_samples = swr_convert(
                swrCtx,
                &output_buffer,
                sampleBufferSize / OUTPUT_CHANNELS,
                const_cast<const uint8_t**>(frame->data),
                frame->nb_samples
            );
            av_frame_unref(frame);
            
            if (out_samples < 0) return -1;

            samplesInBuffer = out_samples * OUTPUT_CHANNELS;
            bufferReadPos = 0;
            return samplesInBuffer;
        }

        if (ret == AVERROR_EOF) {
            decoderDrained = true;
        } else if (ret != AVERROR(EAGAIN)) {
            return -1;
        }

        // 2) If we hit decoder EOF, try draining the resampler (it can hold delayed samples)
        if (decoderDrained && !resamplerDrained) {
            uint8_t* output_buffer = reinterpret_cast<uint8_t*>(sampleBuffer);
            int out_samples = swr_convert(
                swrCtx,
                &output_buffer,
                sampleBufferSize / OUTPUT_CHANNELS,
                nullptr,
                0
            );
            if (out_samples < 0) return -1;
            if (out_samples > 0) {
                samplesInBuffer = out_samples * OUTPUT_CHANNELS;
                bufferReadPos = 0;
                return samplesInBuffer;
            }
            resamplerDrained = true;
            return 0;
        }

        // 3) Need more input packets (or need to flush the decoder at EOF)
        if (!eofSignaled) {
            ret = av_read_frame(formatCtx, packet);
            if (ret < 0) {
                if (ret == AVERROR_EOF) {
                    // Signal EOF to decoder to flush internal buffers
                    eofSignaled = true;
                    avcodec_send_packet(codecCtx, nullptr);
                    continue;
                }
                return -1;
            }

            if (packet->stream_index != audioStreamIndex) {
                av_packet_unref(packet);
                continue;
            }

            ret = avcodec_send_packet(codecCtx, packet);
            av_packet_unref(packet);
            if (ret < 0) return -1;
            continue;
        }

        // 4) EOF already signaled and no frame available: if not drained, loop will handle; otherwise done.
        if (!decoderDrained) {
            // Keep trying to receive until AVERROR_EOF is observed
            continue;
        }

        return 0;
    }
}

int FFmpegDecoder::read(float* outBuffer, int numSamples) {
    if (!formatCtx || !outBuffer) return 0;
    
    int totalRead = 0;
    
    while (totalRead < numSamples) {
        // If buffer is empty, decode next frame
        if (bufferReadPos >= samplesInBuffer) {
            int decoded = decodeNextFrame();
            if (decoded <= 0) {
                break; // End of file or error
            }
        }
        
        // Copy from internal buffer
        int available = samplesInBuffer - bufferReadPos;
        int toCopy = std::min(available, numSamples - totalRead);
        
        memcpy(outBuffer + totalRead, sampleBuffer + bufferReadPos, toCopy * sizeof(float));
        
        bufferReadPos += toCopy;
        totalRead += toCopy;
    }
    
    return totalRead;
}

double FFmpegDecoder::getDuration() const {
    if (!formatCtx) return 0.0;
    
    if (formatCtx->duration != AV_NOPTS_VALUE) {
        return static_cast<double>(formatCtx->duration) / AV_TIME_BASE;
    }
    
    AVStream* stream = formatCtx->streams[audioStreamIndex];
    if (stream->duration != AV_NOPTS_VALUE) {
        return static_cast<double>(stream->duration) * av_q2d(stream->time_base);
    }
    
    return 0.0;
}

int64_t FFmpegDecoder::getTotalSamples() const {
    return static_cast<int64_t>(getDuration() * outputSampleRate);
}

bool FFmpegDecoder::hasError() const {
    return false; // TODO: Implement error tracking
}

std::string FFmpegDecoder::getTag(AVDictionary* dict, const char* key) {
    if (!dict) return "";
    AVDictionaryEntry* entry = av_dict_get(dict, key, nullptr, 0);
    return entry ? entry->value : "";
}

int FFmpegDecoder::parseTrackNumber(const std::string& str, int* total) {
    if (str.empty()) {
        if (total) *total = 0;
        return 0;
    }

    size_t slash = str.find('/');
    if (slash != std::string::npos) {
        if (total) *total = std::atoi(str.c_str() + slash + 1);
        return std::atoi(str.substr(0, slash).c_str());
    }

    if (total) *total = 0;
    return std::atoi(str.c_str());
}

FFmpegDecoder::AudioMetadata FFmpegDecoder::getMetadata() const {
    AudioMetadata meta;
    if (!formatCtx || audioStreamIndex < 0) return meta;

    AVDictionary* dict = formatCtx->metadata;
    AVStream* audioStream = formatCtx->streams[audioStreamIndex];
    AVCodecParameters* codecParams = audioStream->codecpar;

    // Tags
    meta.title = getTag(dict, "title");
    meta.artist = getTag(dict, "artist");
    meta.album = getTag(dict, "album");
    meta.albumArtist = getTag(dict, "album_artist");
    if (meta.albumArtist.empty()) meta.albumArtist = getTag(dict, "ALBUMARTIST");
    meta.genre = getTag(dict, "genre");
    meta.date = getTag(dict, "date");
    if (meta.date.empty()) meta.date = getTag(dict, "year");
    meta.comment = getTag(dict, "comment");

    std::string track = getTag(dict, "track");
    meta.trackNumber = parseTrackNumber(track, &meta.trackTotal);

    std::string disc = getTag(dict, "disc");
    meta.discNumber = parseTrackNumber(disc, &meta.discTotal);

    // Format info
    const AVCodec* codec = avcodec_find_decoder(codecParams->codec_id);
    if (codec) {
        meta.codec = codec->name;
        meta.codecLongName = codec->long_name ? codec->long_name : "";
    }

    meta.format = formatCtx->iformat ? formatCtx->iformat->name : "";
    meta.formatLongName = (formatCtx->iformat && formatCtx->iformat->long_name) ? formatCtx->iformat->long_name : "";
    meta.duration = getDuration();
    meta.bitrate = formatCtx->bit_rate > 0 ? static_cast<int>(formatCtx->bit_rate) : codecParams->bit_rate;
    meta.sampleRate = codecParams->sample_rate;
    meta.channels = codecParams->ch_layout.nb_channels;
    meta.bitsPerSample = codecParams->bits_per_raw_sample > 0 ?
                         codecParams->bits_per_raw_sample :
                         codecParams->bits_per_coded_sample;

    // Cover art
    for (unsigned int i = 0; i < formatCtx->nb_streams; i++) {
        AVStream* stream = formatCtx->streams[i];
        if (stream->disposition & AV_DISPOSITION_ATTACHED_PIC) {
            AVPacket& pkt = stream->attached_pic;
            meta.coverArt.assign(pkt.data, pkt.data + pkt.size);

            if (stream->codecpar->codec_id == AV_CODEC_ID_MJPEG ||
                stream->codecpar->codec_id == AV_CODEC_ID_JPEG2000) {
                meta.coverArtMimeType = "image/jpeg";
            } else if (stream->codecpar->codec_id == AV_CODEC_ID_PNG) {
                meta.coverArtMimeType = "image/png";
            } else if (stream->codecpar->codec_id == AV_CODEC_ID_BMP) {
                meta.coverArtMimeType = "image/bmp";
            } else {
                meta.coverArtMimeType = "image/jpeg";
            }
            break;
        }
    }

    return meta;
}

FFmpegDecoder::AudioMetadata FFmpegDecoder::getFileMetadata(const char* filePath) {
    AudioMetadata meta;
    AVFormatContext* fmtCtx = nullptr;

    if (avformat_open_input(&fmtCtx, filePath, nullptr, nullptr) < 0) {
        return meta;
    }

    if (avformat_find_stream_info(fmtCtx, nullptr) < 0) {
        avformat_close_input(&fmtCtx);
        return meta;
    }

    int audioIdx = -1;
    for (unsigned int i = 0; i < fmtCtx->nb_streams; i++) {
        if (fmtCtx->streams[i]->codecpar->codec_type == AVMEDIA_TYPE_AUDIO) {
            audioIdx = static_cast<int>(i);
            break;
        }
    }

    if (audioIdx == -1) {
        avformat_close_input(&fmtCtx);
        return meta;
    }

    AVDictionary* dict = fmtCtx->metadata;
    AVStream* audioStream = fmtCtx->streams[audioIdx];
    AVCodecParameters* codecParams = audioStream->codecpar;

    meta.title = getTag(dict, "title");
    meta.artist = getTag(dict, "artist");
    meta.album = getTag(dict, "album");
    meta.albumArtist = getTag(dict, "album_artist");
    if (meta.albumArtist.empty()) meta.albumArtist = getTag(dict, "ALBUMARTIST");
    meta.genre = getTag(dict, "genre");
    meta.date = getTag(dict, "date");
    if (meta.date.empty()) meta.date = getTag(dict, "year");
    meta.comment = getTag(dict, "comment");

    std::string track = getTag(dict, "track");
    meta.trackNumber = parseTrackNumber(track, &meta.trackTotal);

    std::string disc = getTag(dict, "disc");
    meta.discNumber = parseTrackNumber(disc, &meta.discTotal);

    const AVCodec* codec = avcodec_find_decoder(codecParams->codec_id);
    if (codec) {
        meta.codec = codec->name;
        meta.codecLongName = codec->long_name ? codec->long_name : "";
    }

    meta.format = fmtCtx->iformat ? fmtCtx->iformat->name : "";
    meta.formatLongName = (fmtCtx->iformat && fmtCtx->iformat->long_name) ? fmtCtx->iformat->long_name : "";

    if (fmtCtx->duration != AV_NOPTS_VALUE) {
        meta.duration = static_cast<double>(fmtCtx->duration) / AV_TIME_BASE;
    } else if (audioStream->duration != AV_NOPTS_VALUE) {
        meta.duration = static_cast<double>(audioStream->duration) * av_q2d(audioStream->time_base);
    }

    meta.bitrate = fmtCtx->bit_rate > 0 ? static_cast<int>(fmtCtx->bit_rate) : codecParams->bit_rate;
    meta.sampleRate = codecParams->sample_rate;
    meta.channels = codecParams->ch_layout.nb_channels;
    meta.bitsPerSample = codecParams->bits_per_raw_sample > 0 ?
                         codecParams->bits_per_raw_sample :
                         codecParams->bits_per_coded_sample;

    for (unsigned int i = 0; i < fmtCtx->nb_streams; i++) {
        AVStream* stream = fmtCtx->streams[i];
        if (stream->disposition & AV_DISPOSITION_ATTACHED_PIC) {
            AVPacket& pkt = stream->attached_pic;
            meta.coverArt.assign(pkt.data, pkt.data + pkt.size);

            if (stream->codecpar->codec_id == AV_CODEC_ID_MJPEG ||
                stream->codecpar->codec_id == AV_CODEC_ID_JPEG2000) {
                meta.coverArtMimeType = "image/jpeg";
            } else if (stream->codecpar->codec_id == AV_CODEC_ID_PNG) {
                meta.coverArtMimeType = "image/png";
            } else if (stream->codecpar->codec_id == AV_CODEC_ID_BMP) {
                meta.coverArtMimeType = "image/bmp";
            } else {
                meta.coverArtMimeType = "image/jpeg";
            }
            break;
        }
    }

    avformat_close_input(&fmtCtx);
    return meta;
}

bool FFmpegDecoder::initFilters() {
    closeFilters();
    
    if (!codecCtx) return false;
    
    filterGraph = avfilter_graph_alloc();
    if (!filterGraph) return false;
    
    char args[512];
    snprintf(args, sizeof(args),
        "time_base=%d/%d:sample_rate=%d:sample_fmt=%s:channel_layout=stereo",
        1, codecCtx->sample_rate,
        codecCtx->sample_rate,
        av_get_sample_fmt_name(codecCtx->sample_fmt));
    
    const AVFilter* bufferSrc = avfilter_get_by_name("abuffer");
    int ret = avfilter_graph_create_filter(&bufferSrcCtx, bufferSrc, "in",
                                           args, nullptr, filterGraph);
    if (ret < 0) {
        closeFilters();
        return false;
    }
    
    const AVFilter* bufferSink = avfilter_get_by_name("abuffersink");
    ret = avfilter_graph_create_filter(&bufferSinkCtx, bufferSink, "out",
                                       nullptr, nullptr, filterGraph);
    if (ret < 0) {
        closeFilters();
        return false;
    }
    
    AVFilterContext* lastFilter = bufferSrcCtx;
    AVFilterContext* currentFilter = nullptr;
    
    if (timeStretch != 1.0 || pitchShift != 0.0) {
        const AVFilter* rubberband = avfilter_get_by_name("rubberband");
        if (rubberband) {
            double pitchRatio = pow(2.0, pitchShift / 12.0);
            char rubberbandArgs[256];
            snprintf(rubberbandArgs, sizeof(rubberbandArgs), 
                "tempo=%f:pitch=%f", timeStretch, pitchRatio);
            
            ret = avfilter_graph_create_filter(&currentFilter, rubberband, "rubberband",
                                              rubberbandArgs, nullptr, filterGraph);
            if (ret >= 0) {
                ret = avfilter_link(lastFilter, 0, currentFilter, 0);
                if (ret >= 0) {
                    lastFilter = currentFilter;
                }
            }
        }
        
        if (ret < 0) {
            double atempoRate = timeStretch;
            if (atempoRate < 0.5) atempoRate = 0.5;
            if (atempoRate > 2.0) atempoRate = 2.0;
            
            const AVFilter* atempo = avfilter_get_by_name("atempo");
            if (atempo) {
                char atempoArgs[64];
                snprintf(atempoArgs, sizeof(atempoArgs), "tempo=%f", atempoRate);
                
                ret = avfilter_graph_create_filter(&currentFilter, atempo, "atempo",
                                                  atempoArgs, nullptr, filterGraph);
                if (ret >= 0) {
                    ret = avfilter_link(lastFilter, 0, currentFilter, 0);
                    if (ret >= 0) {
                        lastFilter = currentFilter;
                    }
                }
            }
        }
    }
    
    ret = avfilter_link(lastFilter, 0, bufferSinkCtx, 0);
    if (ret < 0) {
        closeFilters();
        return false;
    }
    
    ret = avfilter_graph_config(filterGraph, nullptr);
    if (ret < 0) {
        closeFilters();
        return false;
    }
    
    filtersEnabled = true;
    return true;
}

void FFmpegDecoder::closeFilters() {
    // Flush filter graph before closing to clear internal buffers (rubberband, etc.)
    if (bufferSrcCtx && bufferSinkCtx && filteredFrame) {
        // Send NULL frame to signal EOF and drain filter
        av_buffersrc_add_frame_flags(bufferSrcCtx, nullptr, 0);
        // Pull and discard all remaining filtered frames using a temporary frame
        AVFrame* drainFrame = av_frame_alloc();
        while (av_buffersink_get_frame(bufferSinkCtx, drainFrame) >= 0) {
            av_frame_unref(drainFrame);
        }
        av_frame_free(&drainFrame);
    }
    
    if (filteredFrame) {
        av_frame_unref(filteredFrame);
    }
    if (filterGraph) {
        avfilter_graph_free(&filterGraph);
        filterGraph = nullptr;
    }
    bufferSrcCtx = nullptr;
    bufferSinkCtx = nullptr;
    filtersEnabled = false;
}

bool FFmpegDecoder::setPitchShift(double semitones) {
    if (semitones < -12.0) semitones = -12.0;
    if (semitones > 12.0) semitones = 12.0;
    
    pitchShift = semitones;
    
    // Clear buffers to apply change immediately
    samplesInBuffer = 0;
    bufferReadPos = 0;
    filterPts = 0;
    
    if (codecCtx && (pitchShift != 0.0 || timeStretch != 1.0)) {
        // Flush codec and resampler
        avcodec_flush_buffers(codecCtx);
        if (frame) av_frame_unref(frame);
        if (filteredFrame) av_frame_unref(filteredFrame);
        if (swrCtx) {
            swr_close(swrCtx);
            swr_init(swrCtx);
        }
        return initFilters();
    }
    
    closeFilters();
    return true;
}

bool FFmpegDecoder::setTimeStretch(double rate) {
    if (rate < 0.25) rate = 0.25;
    if (rate > 4.0) rate = 4.0;
    
    timeStretch = rate;
    
    // Clear buffers to apply change immediately
    samplesInBuffer = 0;
    bufferReadPos = 0;
    filterPts = 0;
    
    if (codecCtx && (pitchShift != 0.0 || timeStretch != 1.0)) {
        // Flush codec and resampler
        avcodec_flush_buffers(codecCtx);
        if (frame) av_frame_unref(frame);
        if (filteredFrame) av_frame_unref(filteredFrame);
        if (swrCtx) {
            swr_close(swrCtx);
            swr_init(swrCtx);
        }
        return initFilters();
    }
    
    closeFilters();
    return true;
}

FFmpegDecoder::WaveformData FFmpegDecoder::getWaveform(int numPoints) {
    WaveformData data;
    data.points = numPoints;
    data.peaksL.resize(numPoints, 0.0f);
    data.peaksR.resize(numPoints, 0.0f);

    if (!formatCtx || audioStreamIndex < 0) return data;

    // Save current position (in seconds)
    double originalPos = 0.0;
    AVStream* stream = formatCtx->streams[audioStreamIndex];
    if (formatCtx->pb) {
        // More reliable to track time than avio_tell for compressed streams
        originalPos = 0.0; // We'll just seek back to 0 or leave it to the caller to seek back
    }
    
    // Seek to beginning for analysis
    seek(0);

    const int64_t totalFrames = getTotalSamples();
    const int64_t framesPerPoint = std::max((int64_t)1, totalFrames / numPoints);
    
    int currentPoint = 0;
    int64_t pointFramesAccumulated = 0;
    float maxL = 0.0f;
    float maxR = 0.0f;

    const int readBatchSize = 4096;
    float batchBuffer[readBatchSize * 2]; // Stereo

    while (currentPoint < numPoints) {
        int readSamples = this->read(batchBuffer, readBatchSize * 2);
        if (readSamples <= 0) break;

        for (int i = 0; i < readSamples; i += 2) {
            float l = std::abs(batchBuffer[i]);
            float r = std::abs(batchBuffer[i+1]);
            
            if (l > maxL) maxL = l;
            if (r > maxR) maxR = r;
            
            pointFramesAccumulated++;
            
            if (pointFramesAccumulated >= framesPerPoint) {
                if (currentPoint < numPoints) {
                    data.peaksL[currentPoint] = maxL;
                    data.peaksR[currentPoint] = maxR;
                    currentPoint++;
                }
                maxL = 0.0f;
                maxR = 0.0f;
                pointFramesAccumulated = 0;
                
                if (currentPoint >= numPoints) break;
            }
        }
    }

    // Handle last point if file ended early or rounding
    if (currentPoint < numPoints && pointFramesAccumulated > 0) {
        data.peaksL[currentPoint] = maxL;
        data.peaksR[currentPoint] = maxR;
    }

    // Note: We don't seek back here because getWaveform is usually 
    // destructive to the playback state anyway. The caller should seek if needed.

    return data;
}

FFmpegDecoder::WaveformData FFmpegDecoder::getWaveformStreaming(int numPoints, int64_t chunkSizeBytes, ProgressCallback callback) {
    WaveformData data;
    data.points = numPoints;
    data.peaksL.resize(numPoints, 0.0f);
    data.peaksR.resize(numPoints, 0.0f);

    if (!formatCtx || audioStreamIndex < 0) return data;

    seek(0);

    const int64_t totalFrames = getTotalSamples();
    const int64_t framesPerPoint = std::max((int64_t)1, totalFrames / numPoints);
    
    int currentPoint = 0;
    int64_t pointFramesAccumulated = 0;
    int64_t bytesProcessed = 0;
    int64_t nextCallbackAt = chunkSizeBytes;
    float maxL = 0.0f;
    float maxR = 0.0f;

    const int readBatchSize = 4096;
    float batchBuffer[readBatchSize * 2];
    const int bytesPerSample = sizeof(float) * 2; // Stereo float32

    while (currentPoint < numPoints) {
        int readSamples = this->read(batchBuffer, readBatchSize * 2);
        if (readSamples <= 0) break;

        bytesProcessed += readSamples * sizeof(float);

        for (int i = 0; i < readSamples; i += 2) {
            float l = std::abs(batchBuffer[i]);
            float r = std::abs(batchBuffer[i+1]);
            
            if (l > maxL) maxL = l;
            if (r > maxR) maxR = r;
            
            pointFramesAccumulated++;
            
            if (pointFramesAccumulated >= framesPerPoint) {
                if (currentPoint < numPoints) {
                    data.peaksL[currentPoint] = maxL;
                    data.peaksR[currentPoint] = maxR;
                    currentPoint++;
                }
                maxL = 0.0f;
                maxR = 0.0f;
                pointFramesAccumulated = 0;
                
                if (currentPoint >= numPoints) break;
            }
        }

        // Call callback every chunk
        if (callback && bytesProcessed >= nextCallbackAt) {
            float progress = (float)currentPoint / (float)numPoints;
            bool shouldContinue = callback(data, progress);
            if (!shouldContinue) {
                // Abort requested
                return data;
            }
            nextCallbackAt += chunkSizeBytes;
        }
    }

    // Handle last point
    if (currentPoint < numPoints && pointFramesAccumulated > 0) {
        data.peaksL[currentPoint] = maxL;
        data.peaksR[currentPoint] = maxR;
    }

    // Final callback
    if (callback) {
        callback(data, 1.0f);
    }

    return data;
}
