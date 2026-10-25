/*
 * Music stream volume through the firmware's own libaudioclient.
 *
 * AudioPolicyManager starts with every stream's index range uninitialized
 * (min/max -1), so playback is mixed at -inf dB until Java's AudioService
 * calls initStreamVolume() and setStreamVolumeIndex(). Moon mode has no
 * AudioService, and an audioserver restart forgets both calls, so moon-audiod
 * repeats them whenever it opens a stream. The stream index stays fixed (it
 * sets the loudness ceiling); Moon's 0-100 volume is a software gain on top,
 * exactly as the Java bridge's AudioTrack.setStereoVolume() was.
 */
#include <cstdint>

/* Declarations matching AOSP's media/AudioSystem.h; the definitions live in
 * /system/lib64/libaudioclient.so. */
typedef enum { AUDIO_STREAM_DEFAULT_ = -1 } audio_stream_type_t;
typedef enum { AUDIO_DEVICE_NONE_ = 0u } audio_devices_t;
namespace android {
typedef int32_t status_t;
class AudioSystem {
public:
    static status_t initStreamVolume(audio_stream_type_t stream, int indexMin, int indexMax);
    static status_t setStreamVolumeIndex(audio_stream_type_t stream, int index,
                                         audio_devices_t device);
};
}  // namespace android

static const audio_stream_type_t STREAM_MUSIC = (audio_stream_type_t)3;
static const audio_devices_t DEVICE_OUT_SPEAKER = (audio_devices_t)0x2;
static const audio_devices_t DEVICE_OUT_DEFAULT = (audio_devices_t)0x40000000u;
/* Samsung's STREAM_MUSIC range on this firmware (AudioService uses 0-15). */
static const int MUSIC_INDEX_MAX = 15;

extern "C" int moon_set_music_index(int index) {
    using android::AudioSystem;
    if (index < 0) index = 0;
    if (index > MUSIC_INDEX_MAX) index = MUSIC_INDEX_MAX;
    if (AudioSystem::initStreamVolume(STREAM_MUSIC, 0, MUSIC_INDEX_MAX) != 0) return -1;
    if (AudioSystem::setStreamVolumeIndex(STREAM_MUSIC, index, DEVICE_OUT_SPEAKER) != 0) return -1;
    if (AudioSystem::setStreamVolumeIndex(STREAM_MUSIC, index, DEVICE_OUT_DEFAULT) != 0) return -1;
    return index;
}
