/*
 * moon-call-audio: voice-call audio routing without Android's AudioService.
 *
 * During a call the modem's voice path runs entirely in Samsung's audio HAL
 * (audio.primary.s5e8825 drives it through libaudio-ril / libsecril-client:
 * SetCallAudioPath, SetCallVolume, SetCallClockSync). What the framework
 * contributes is policy: the audio mode, the communication route and the
 * call volume. This helper makes the same calls through the firmware's
 * libaudioclient, resolved at run time, so it needs no platform libraries at
 * build time:
 *
 *   moon-call-audio mode normal|ringtone|in_call
 *   moon-call-audio speaker on|off
 *   moon-call-audio mute on|off
 *   moon-call-audio volume INDEX            (voice-call stream, 1-VOICE_MAX)
 *   moon-call-audio param KEY=VALUE[;...]   (primary HAL setParameters)
 *
 * moon-telephonyd runs it on every call state change. Root (uid 0) passes
 * audioserver's MODIFY_AUDIO_SETTINGS checks.
 */
#include <dlfcn.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

enum {
    AUDIO_MODE_NORMAL = 0,
    AUDIO_MODE_RINGTONE = 1,
    AUDIO_MODE_IN_CALL = 2,
    AUDIO_STREAM_VOICE_CALL = 0,
    FORCE_FOR_COMMUNICATION = 0,
    FORCE_NONE = 0,
    FORCE_SPEAKER = 1,
    DEVICE_OUT_EARPIECE = 0x1,
    DEVICE_OUT_SPEAKER = 0x2,
    DEVICE_OUT_DEFAULT = 0x40000000,
};
/* AudioService's STREAM_VOICE_CALL range on Samsung firmware is 1-5 by
 * default (ro.config.vc_call_vol_steps can change it); 4 is its default. */
#define VOICE_MIN 1
#define VOICE_MAX 5
#define VOICE_DEFAULT 4

typedef int32_t status_t;

static void *sym(void *lib, const char *name) {
    void *p = dlsym(lib, name);
    if (!p) fprintf(stderr, "moon-call-audio: missing %s\n", name);
    return p;
}

static int usage(void) {
    fprintf(stderr, "usage: moon-call-audio mode normal|ringtone|in_call\n"
                    "       moon-call-audio speaker on|off\n"
                    "       moon-call-audio mute on|off\n"
                    "       moon-call-audio volume INDEX\n"
                    "       moon-call-audio param KEY=VALUE\n");
    return 2;
}

static int on_off(const char *arg) {
    if (!strcmp(arg, "on")) return 1;
    if (!strcmp(arg, "off")) return 0;
    return -1;
}

static status_t set_voice_volume(void *ac, int index) {
    status_t (*init)(int, int, int) =
        (status_t (*)(int, int, int))sym(ac, "_ZN7android11AudioSystem16initStreamVolumeE19audio_stream_type_tii");
    status_t (*set)(int, int, unsigned) = (status_t (*)(int, int, unsigned))sym(
        ac, "_ZN7android11AudioSystem20setStreamVolumeIndexE19audio_stream_type_ti15audio_devices_t");
    if (!init || !set) return -1;
    if (index < VOICE_MIN) index = VOICE_MIN;
    if (index > VOICE_MAX) index = VOICE_MAX;
    status_t rc = init(AUDIO_STREAM_VOICE_CALL, VOICE_MIN, VOICE_MAX);
    if (rc == 0) rc = set(AUDIO_STREAM_VOICE_CALL, index, DEVICE_OUT_EARPIECE);
    if (rc == 0) rc = set(AUDIO_STREAM_VOICE_CALL, index, DEVICE_OUT_SPEAKER);
    if (rc == 0) rc = set(AUDIO_STREAM_VOICE_CALL, index, DEVICE_OUT_DEFAULT);
    return rc;
}

int main(int argc, char **argv) {
    if (argc != 3) return usage();
    void *ac = dlopen("libaudioclient.so", RTLD_NOW);
    if (!ac) {
        fprintf(stderr, "moon-call-audio: %s\n", dlerror());
        return 1;
    }
    const char *what = argv[1], *arg = argv[2];
    status_t rc;
    if (!strcmp(what, "mode")) {
        int mode;
        if (!strcmp(arg, "normal")) mode = AUDIO_MODE_NORMAL;
        else if (!strcmp(arg, "ringtone")) mode = AUDIO_MODE_RINGTONE;
        else if (!strcmp(arg, "in_call")) mode = AUDIO_MODE_IN_CALL;
        else return usage();
        status_t (*set_phone_state)(int, unsigned) =
            (status_t (*)(int, unsigned))sym(ac, "_ZN7android11AudioSystem13setPhoneStateE12audio_mode_tj");
        status_t (*set_force_use)(int, int) = (status_t (*)(int, int))sym(
            ac, "_ZN7android11AudioSystem11setForceUseE24audio_policy_force_use_t25audio_policy_forced_cfg_t");
        if (!set_phone_state || !set_force_use) return 1;
        if (mode == AUDIO_MODE_IN_CALL) set_voice_volume(ac, VOICE_DEFAULT);
        /* A call always starts on the earpiece. */
        if (mode != AUDIO_MODE_RINGTONE) set_force_use(FORCE_FOR_COMMUNICATION, FORCE_NONE);
        rc = set_phone_state(mode, 0);
    } else if (!strcmp(what, "speaker")) {
        int on = on_off(arg);
        if (on < 0) return usage();
        status_t (*set_force_use)(int, int) = (status_t (*)(int, int))sym(
            ac, "_ZN7android11AudioSystem11setForceUseE24audio_policy_force_use_t25audio_policy_forced_cfg_t");
        if (!set_force_use) return 1;
        rc = set_force_use(FORCE_FOR_COMMUNICATION, on ? FORCE_SPEAKER : FORCE_NONE);
    } else if (!strcmp(what, "mute")) {
        int on = on_off(arg);
        if (on < 0) return usage();
        status_t (*mute)(int) = (status_t (*)(int))sym(ac, "_ZN7android11AudioSystem14muteMicrophoneEb");
        if (!mute) return 1;
        rc = mute(on);
    } else if (!strcmp(what, "volume")) {
        rc = set_voice_volume(ac, atoi(arg));
    } else if (!strcmp(what, "param")) {
        /* android::String8 is one pointer; construct it in place. */
        void *utils = dlopen("libutils.so", RTLD_NOW);
        if (!utils) {
            fprintf(stderr, "moon-call-audio: %s\n", dlerror());
            return 1;
        }
        void (*string8)(void *, const char *) = (void (*)(void *, const char *))sym(utils, "_ZN7android7String8C1EPKc");
        void (*string8_free)(void *) = (void (*)(void *))sym(utils, "_ZN7android7String8D1Ev");
        status_t (*set_parameters)(int, const void *) =
            (status_t (*)(int, const void *))sym(ac, "_ZN7android11AudioSystem13setParametersEiRKNS_7String8E");
        if (!string8 || !string8_free || !set_parameters) return 1;
        void *storage[4] = {0};
        string8(storage, arg);
        rc = set_parameters(0, storage);
        string8_free(storage);
    } else {
        return usage();
    }
    if (rc != 0) {
        fprintf(stderr, "moon-call-audio: %s %s failed: %d\n", what, arg, rc);
        return 1;
    }
    return 0;
}
