/*
 * moon-audiod: a native AAudio sink for Moon's Linux applications.
 *
 * Drop-in replacement for the Java AudioTrack bridge (moon.audio.Bridge) that
 * needs no Android Java framework: AAudio talks to audioserver's AudioFlinger
 * directly. Two framework duties are covered elsewhere: moon-permissiond
 * answers AudioFlinger's package lookup, and audio-policy.cpp sets the music
 * stream volume that AudioService would. Run it as root: audioserver exempts
 * service UIDs from AppOps, which also lives in system_server. Applications
 * write 48 kHz stereo signed-16-bit PCM to a FIFO; the volume file holds 0-100
 * (a linear gain, as AudioTrack.setStereoVolume was). MOON_AUDIO_STREAM_INDEX
 * (0-15, default 5) is the music stream's speaker index, the loudness ceiling.
 *
 * An optional ALERT_FIFO carries moon-notifyd's alarm, timer and
 * notification sounds (same format, already at their final level). They play
 * as a second, ALARM-usage stream that AudioFlinger mixes with the media one,
 * so an alarm is heard over the browser and is not capped by the media index.
 *
 *   moon-audiod PCM_FIFO VOLUME_FILE PID_FILE MEDIA_HEARTBEAT [ALERT_FIFO]
 *   moon-audiod tone [MILLISECONDS] [VOLUME_PERCENT]
 */
#include <aaudio/AAudio.h>
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <pthread.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

#define RATE 48000
#define CHANNELS 2
#define FRAME_BYTES 4
#define CHUNK_FRAMES 960
#define HEARTBEAT_NS 500000000LL
#define WRITE_TIMEOUT_NS 2000000000LL

int moon_set_music_index(int index);
int moon_set_alarm_full(void);

static volatile sig_atomic_t stopping;
static int stream_index = 5;
static const char *pid_path;

static void on_signal(int sig) {
    (void)sig;
    stopping = 1;
}

static int64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000000000LL + ts.tv_nsec;
}

static int read_volume(const char *path, int fallback) {
    char text[16];
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return fallback;
    ssize_t count = read(fd, text, sizeof(text) - 1);
    close(fd);
    if (count <= 0) return fallback;
    text[count] = 0;
    char *end;
    long value = strtol(text, &end, 10);
    if (end == text) return fallback;
    if (value < 0) value = 0;
    if (value > 100) value = 100;
    return (int)value;
}

static void set_stream_index(void) {
    if (moon_set_music_index(stream_index) < 0)
        fprintf(stderr, "moon-audiod: setting music stream index %d failed\n", stream_index);
}

static void apply_volume(int16_t *samples, int count, int volume) {
    if (volume >= 100) return;
    int32_t gain = volume * 65536 / 100;
    for (int i = 0; i < count; i++) samples[i] = (int16_t)((samples[i] * gain) >> 16);
}

static AAudioStream *open_stream_for(int alert) {
    AAudioStreamBuilder *builder;
    AAudioStream *stream = NULL;
    if (AAudio_createStreamBuilder(&builder) != AAUDIO_OK) return NULL;
    AAudioStreamBuilder_setDirection(builder, AAUDIO_DIRECTION_OUTPUT);
    AAudioStreamBuilder_setSharingMode(builder, AAUDIO_SHARING_MODE_SHARED);
    AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_NONE);
    AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
    AAudioStreamBuilder_setSampleRate(builder, RATE);
    AAudioStreamBuilder_setChannelCount(builder, CHANNELS);
    AAudioStreamBuilder_setUsage(builder, alert ? AAUDIO_USAGE_ALARM : AAUDIO_USAGE_MEDIA);
    AAudioStreamBuilder_setContentType(builder,
                                       alert ? AAUDIO_CONTENT_TYPE_SONIFICATION : AAUDIO_CONTENT_TYPE_MUSIC);
    AAudioStreamBuilder_setBufferCapacityInFrames(builder, RATE / 5);
    /* A restarted audioserver has forgotten the stream volume. */
    if (alert) {
        if (moon_set_alarm_full() < 0) fprintf(stderr, "moon-audiod: setting alarm stream volume failed\n");
    } else {
        set_stream_index();
    }
    aaudio_result_t rc = AAudioStreamBuilder_openStream(builder, &stream);
    AAudioStreamBuilder_delete(builder);
    if (rc != AAUDIO_OK) {
        fprintf(stderr, "moon-audiod: openStream failed: %s\n", AAudio_convertResultToText(rc));
        return NULL;
    }
    return stream;
}

static AAudioStream *open_stream(void) { return open_stream_for(0); }

static int write_all(AAudioStream *stream, int16_t *samples, int frames) {
    int offset = 0;
    while (offset < frames && !stopping) {
        aaudio_result_t wrote = AAudioStream_write(stream, samples + offset * CHANNELS,
                                                   frames - offset, WRITE_TIMEOUT_NS);
        if (wrote < 0) {
            fprintf(stderr, "moon-audiod: write failed: %s\n", AAudio_convertResultToText(wrote));
            return -1;
        }
        offset += wrote;
    }
    return 0;
}

static int tone(int milliseconds, int volume) {
    AAudioStream *stream = open_stream();
    if (!stream) return 1;
    fprintf(stderr, "moon-audiod: tone stream rate=%d channels=%d device=%d sharing=%d\n",
            AAudioStream_getSampleRate(stream), AAudioStream_getChannelCount(stream),
            AAudioStream_getDeviceId(stream), AAudioStream_getSharingMode(stream));
    if (AAudioStream_requestStart(stream) != AAUDIO_OK) return 1;
    int16_t chunk[CHUNK_FRAMES * CHANNELS];
    int total = RATE * milliseconds / 1000;
    int rc = 0;
    for (int frame = 0; frame < total && rc == 0; frame += CHUNK_FRAMES) {
        for (int i = 0; i < CHUNK_FRAMES; i++) {
            double t = (double)(frame + i) / RATE;
            /* 10 ms fade in/out so the speaker does not click. */
            double env = fmin(1.0, fmin((frame + i) / 480.0, (total - frame - i) / 480.0));
            if (env < 0) env = 0;
            int16_t s = (int16_t)(sin(2 * M_PI * 660.0 * t) * 8000.0 * env);
            chunk[2 * i] = chunk[2 * i + 1] = s;
        }
        apply_volume(chunk, CHUNK_FRAMES * CHANNELS, volume);
        rc = write_all(stream, chunk, CHUNK_FRAMES);
    }
    usleep(300000);
    AAudioStream_requestStop(stream);
    fprintf(stderr, "moon-audiod: tone done frames_written=%lld xruns=%d\n",
            (long long)AAudioStream_getFramesWritten(stream), AAudioStream_getXRunCount(stream));
    AAudioStream_close(stream);
    return rc ? 1 : 0;
}

static int write_pid(const char *path) {
    char tmp[512];
    snprintf(tmp, sizeof(tmp), "%s.new", path);
    FILE *f = fopen(tmp, "w");
    if (!f) return -1;
    fprintf(f, "%d\n", getpid());
    fflush(f);
    fsync(fileno(f));
    fclose(f);
    return rename(tmp, path);
}

static void remove_own_pid(void) {
    char text[32];
    int fd = open(pid_path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return;
    ssize_t count = read(fd, text, sizeof(text) - 1);
    close(fd);
    if (count <= 0) return;
    text[count] = 0;
    if (atoi(text) == getpid()) unlink(pid_path);
}

static int audible(const int16_t *samples, int count) {
    for (int i = 0; i < count; i++)
        if (samples[i] > 16 || samples[i] < -16) return 1;
    return 0;
}

static void mark_media_active(const char *path) {
    int fd = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0660);
    if (fd < 0) return;
    (void)!write(fd, "active\n", 7);
    close(fd);
}

static int bridge(const char *pcm, const char *volume_path, const char *heartbeat) {
    int volume = read_volume(volume_path, 50);
    AAudioStream *stream = open_stream();
    if (!stream) return 1;
    if (write_pid(pid_path) != 0) {
        fprintf(stderr, "moon-audiod: cannot write pid file: %s\n", strerror(errno));
        return 1;
    }
    unlink(heartbeat);
    fprintf(stderr, "moon audio bridge ready rate=48000 channels=2 format=s16le backend=aaudio\n");

    union {
        uint8_t bytes[CHUNK_FRAMES * FRAME_BYTES + FRAME_BYTES];
        int16_t samples[(CHUNK_FRAMES * FRAME_BYTES + FRAME_BYTES) / 2];
    } buf;
    int rc = 0;
    while (!stopping && rc == 0) {
        int fd = open(pcm, O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            if (errno == EINTR) continue;
            fprintf(stderr, "moon-audiod: open %s: %s\n", pcm, strerror(errno));
            rc = 1;
            break;
        }
        int carried = 0, playing = 0;
        int64_t last_heartbeat = 0;
        for (;;) {
            ssize_t count = read(fd, buf.bytes + carried, sizeof(buf.bytes) - carried);
            if (count < 0 && errno == EINTR) {
                if (stopping) break;
                continue;
            }
            if (count <= 0) break;
            count += carried;
            int aligned = (int)count - (int)(count % FRAME_BYTES);
            carried = (int)count - aligned;
            volume = read_volume(volume_path, volume);
            if (!playing) {
                aaudio_result_t started = AAudioStream_requestStart(stream);
                if (started != AAUDIO_OK) {
                    fprintf(stderr, "moon-audiod: start failed: %s\n",
                            AAudio_convertResultToText(started));
                    rc = 1;
                    break;
                }
                playing = 1;
            }
            int samples = aligned / 2;
            int64_t now = now_ns();
            if (audible(buf.samples, samples) && now - last_heartbeat >= HEARTBEAT_NS) {
                mark_media_active(heartbeat);
                last_heartbeat = now;
            }
            apply_volume(buf.samples, samples, volume);
            if (write_all(stream, buf.samples, aligned / FRAME_BYTES) != 0) {
                /* audioserver restarted or the route died: reopen once. */
                AAudioStream_close(stream);
                stream = open_stream();
                if (!stream) { rc = 1; break; }
                playing = 0;
                continue;
            }
            if (carried > 0) memmove(buf.bytes, buf.bytes + aligned, carried);
        }
        close(fd);
        if (playing && stream) {
            AAudioStream_requestPause(stream);
            AAudioStream_requestFlush(stream);
        }
        unlink(heartbeat);
    }
    if (stream) {
        AAudioStream_requestStop(stream);
        AAudioStream_close(stream);
    }
    remove_own_pid();
    unlink(heartbeat);
    return rc;
}

/* Plays ALERT_FIFO until moon-audiod stops. A writer's close ends one sound;
 * the stream is paused and flushed in between, and reopened if audioserver
 * restarted. Unlike the media path there is no volume file and no heartbeat. */
static void *alerts(void *arg) {
    const char *path = arg;
    AAudioStream *stream = NULL;
    union {
        uint8_t bytes[CHUNK_FRAMES * FRAME_BYTES + FRAME_BYTES];
        int16_t samples[(CHUNK_FRAMES * FRAME_BYTES + FRAME_BYTES) / 2];
    } buf;
    while (!stopping) {
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0) {
            if (errno == EINTR) continue;
            fprintf(stderr, "moon-audiod: open %s: %s\n", path, strerror(errno));
            sleep(2);
            continue;
        }
        int carried = 0, playing = 0;
        for (;;) {
            ssize_t count = read(fd, buf.bytes + carried, sizeof(buf.bytes) - carried);
            if (count < 0 && errno == EINTR) {
                if (stopping) break;
                continue;
            }
            if (count <= 0) break;
            count += carried;
            int aligned = (int)count - (int)(count % FRAME_BYTES);
            carried = (int)count - aligned;
            if (!stream && !(stream = open_stream_for(1))) break;
            if (!playing) {
                if (AAudioStream_requestStart(stream) != AAUDIO_OK) {
                    AAudioStream_close(stream);
                    stream = NULL;
                    break;
                }
                playing = 1;
            }
            if (write_all(stream, buf.samples, aligned / FRAME_BYTES) != 0) {
                AAudioStream_close(stream);
                stream = NULL;
                playing = 0;
                continue;
            }
            if (carried > 0) memmove(buf.bytes, buf.bytes + aligned, carried);
        }
        close(fd);
        if (playing && stream) {
            /* Let the tail play out, then release the device. */
            usleep(250000);
            AAudioStream_requestPause(stream);
            AAudioStream_requestFlush(stream);
        }
    }
    if (stream) AAudioStream_close(stream);
    return NULL;
}

int main(int argc, char **argv) {
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_signal;
    sigaction(SIGTERM, &sa, NULL);
    sigaction(SIGINT, &sa, NULL);
    signal(SIGPIPE, SIG_IGN);

    const char *index_env = getenv("MOON_AUDIO_STREAM_INDEX");
    if (index_env && *index_env) stream_index = atoi(index_env);

    if (argc >= 2 && strcmp(argv[1], "tone") == 0) {
        int ms = argc >= 3 ? atoi(argv[2]) : 400;
        int vol = argc >= 4 ? atoi(argv[3]) : 30;
        if (ms <= 0 || ms > 5000) ms = 400;
        if (vol < 0 || vol > 100) vol = 30;
        return tone(ms, vol);
    }
    if (argc != 5 && argc != 6) {
        fprintf(stderr, "usage: moon-audiod PCM_FIFO VOLUME_FILE PID_FILE MEDIA_HEARTBEAT [ALERT_FIFO]\n"
                        "       moon-audiod tone [MILLISECONDS] [VOLUME_PERCENT]\n");
        return 2;
    }
    pid_path = argv[3];
    if (argc == 6) {
        pthread_t thread;
        if (pthread_create(&thread, NULL, alerts, argv[5]) != 0)
            fprintf(stderr, "moon-audiod: alert thread failed; alerts disabled\n");
        else
            pthread_detach(thread);
    }
    return bridge(argv[1], argv[2], argv[4]);
}
