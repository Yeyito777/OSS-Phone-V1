/*
 * moon-audio-volume: initialize audioserver stream volumes without AudioService.
 *
 * AudioPolicyManager starts with every stream's index range uninitialized
 * (min/max -1), so all playback is mixed at -inf dB until Java's AudioService
 * calls initStreamVolume() and setStreamVolumeIndex(). In Moon mode that
 * service never runs, and audioserver restarts lose its settings, so this
 * helper performs the same two calls through the device's own libaudioclient.
 *
 *   moon-audio-volume set STREAM MIN MAX DEVICE:INDEX [DEVICE:INDEX...]
 *   moon-audio-volume get STREAM DEVICE
 *
 * STREAM is an audio_stream_type_t (3 = music); DEVICE an audio_devices_t in
 * hex (0x2 = speaker, 0x40000000 = default).
 */
#include <cstdio>
#include <cstdlib>
#include <cstring>

/* Declarations matching AOSP's media/AudioSystem.h; the definitions are the
 * firmware's /system/lib64/libaudioclient.so, linked at runtime. */
typedef enum { AUDIO_STREAM_DEFAULT_ = -1 } audio_stream_type_t;
typedef enum { AUDIO_DEVICE_NONE_ = 0u } audio_devices_t;
namespace android {
typedef int32_t status_t;
class AudioSystem {
public:
    static status_t initStreamVolume(audio_stream_type_t stream, int indexMin, int indexMax);
    static status_t setStreamVolumeIndex(audio_stream_type_t stream, int index,
                                         audio_devices_t device);
    static status_t getStreamVolumeIndex(audio_stream_type_t stream, int *index,
                                         audio_devices_t device);
};
}  // namespace android

using android::AudioSystem;

static int usage() {
    fprintf(stderr, "usage: moon-audio-volume set STREAM MIN MAX DEVICE:INDEX...\n"
                    "       moon-audio-volume get STREAM DEVICE\n");
    return 2;
}

int main(int argc, char **argv) {
    if (argc == 4 && !strcmp(argv[1], "get")) {
        auto stream = (audio_stream_type_t)atoi(argv[2]);
        auto device = (audio_devices_t)strtoul(argv[3], nullptr, 0);
        int index = -1;
        android::status_t rc = AudioSystem::getStreamVolumeIndex(stream, &index, device);
        printf("%d\n", index);
        return rc == 0 ? 0 : 1;
    }
    if (argc < 6 || strcmp(argv[1], "set")) return usage();
    auto stream = (audio_stream_type_t)atoi(argv[2]);
    int min = atoi(argv[3]), max = atoi(argv[4]);
    android::status_t rc = AudioSystem::initStreamVolume(stream, min, max);
    if (rc != 0) {
        fprintf(stderr, "moon-audio-volume: initStreamVolume(%d) failed: %d\n", stream, rc);
        return 1;
    }
    for (int i = 5; i < argc; i++) {
        char *colon = strchr(argv[i], ':');
        if (!colon) return usage();
        auto device = (audio_devices_t)strtoul(argv[i], nullptr, 0);
        int index = atoi(colon + 1);
        rc = AudioSystem::setStreamVolumeIndex(stream, index, device);
        if (rc != 0) {
            fprintf(stderr, "moon-audio-volume: setStreamVolumeIndex(%d, %d, 0x%x) failed: %d\n",
                    stream, index, device, rc);
            return 1;
        }
    }
    return 0;
}
