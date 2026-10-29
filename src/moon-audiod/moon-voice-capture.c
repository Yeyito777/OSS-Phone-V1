/*
 * moon-voice-capture: records the microphone through AAudio, for Moon's
 * hold-the-power-key voice input (docs/voice.md).
 *
 * Writes 16 kHz mono signed-16-bit little-endian PCM to stdout until SIGTERM
 * or SIGINT (then it stops cleanly and exits 0), stdout closes, or
 * MAX_SECONDS (default 120) pass. Like moon-audiod it needs no framework:
 * run as root, audioserver lets service UIDs record, and moon-permissiond
 * answers AudioFlinger's lookups. The input preset is VOICE_RECOGNITION
 * (the main microphone, no call processing). One line on stderr says when
 * the stream runs.
 *
 *   moon-voice-capture [MAX_SECONDS] > voice.pcm
 */
#include <aaudio/AAudio.h>
#include <errno.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define RATE 16000
#define CHUNK_FRAMES 320
#define READ_TIMEOUT_NS 200000000LL

static volatile sig_atomic_t stopping;

static void on_signal(int sig) {
    (void)sig;
    stopping = 1;
}

static int write_all(const void *data, size_t size) {
    const char *bytes = data;
    while (size > 0) {
        ssize_t wrote = write(STDOUT_FILENO, bytes, size);
        if (wrote < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        bytes += wrote;
        size -= (size_t)wrote;
    }
    return 0;
}

int main(int argc, char **argv) {
    long max_seconds = argc > 1 ? strtol(argv[1], NULL, 10) : 120;
    if (max_seconds <= 0) max_seconds = 120;

    struct sigaction action;
    memset(&action, 0, sizeof(action));
    action.sa_handler = on_signal;
    sigaction(SIGTERM, &action, NULL);
    sigaction(SIGINT, &action, NULL);
    signal(SIGPIPE, SIG_IGN);

    AAudioStreamBuilder *builder;
    AAudioStream *stream = NULL;
    if (AAudio_createStreamBuilder(&builder) != AAUDIO_OK) return 1;
    AAudioStreamBuilder_setDirection(builder, AAUDIO_DIRECTION_INPUT);
    AAudioStreamBuilder_setSharingMode(builder, AAUDIO_SHARING_MODE_SHARED);
    AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_NONE);
    AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setSampleRate(builder, RATE);
    AAudioStreamBuilder_setChannelCount(builder, 1);
    AAudioStreamBuilder_setInputPreset(builder, AAUDIO_INPUT_PRESET_VOICE_RECOGNITION);
    aaudio_result_t rc = AAudioStreamBuilder_openStream(builder, &stream);
    AAudioStreamBuilder_delete(builder);
    if (rc != AAUDIO_OK) {
        fprintf(stderr, "moon-voice-capture: openStream failed: %s\n", AAudio_convertResultToText(rc));
        return 1;
    }
    rc = AAudioStream_requestStart(stream);
    if (rc != AAUDIO_OK) {
        fprintf(stderr, "moon-voice-capture: start failed: %s\n", AAudio_convertResultToText(rc));
        AAudioStream_close(stream);
        return 1;
    }
    fprintf(stderr, "moon-voice-capture: recording rate=%d channels=%d device=%d\n",
            AAudioStream_getSampleRate(stream), AAudioStream_getChannelCount(stream),
            AAudioStream_getDeviceId(stream));

    int16_t chunk[CHUNK_FRAMES];
    int64_t frames = 0;
    int status = 0;
    while (!stopping && frames < (int64_t)max_seconds * RATE) {
        aaudio_result_t read = AAudioStream_read(stream, chunk, CHUNK_FRAMES, READ_TIMEOUT_NS);
        if (read < 0) {
            fprintf(stderr, "moon-voice-capture: read failed: %s\n", AAudio_convertResultToText(read));
            status = 1;
            break;
        }
        if (read > 0 && write_all(chunk, (size_t)read * sizeof(int16_t)) < 0) break;
        frames += read;
    }
    AAudioStream_requestStop(stream);
    AAudioStream_close(stream);
    fprintf(stderr, "moon-voice-capture: %lld ms\n", (long long)(frames * 1000 / RATE));
    return status;
}
