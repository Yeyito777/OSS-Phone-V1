/*
 * moon-permissiond: a minimal native "permission" service for Moon mode.
 *
 * Android's native daemons reach system_server's IPermissionController
 * ("permission") for package and permission lookups. Without the Java
 * framework the lookup blocks: libbinder's PermissionController waits up to
 * 10 s for the service, and AudioFlinger::createTrack does that lookup on
 * every new playback track, so audioserver's 5 s TimeCheck aborts it. This
 * daemon answers those calls the way Android does for its own service UIDs,
 * so audioserver can create tracks with no framework.
 *
 * It also stands in for AudioService ("audio", android.media.IAudioService),
 * whose native IAudioManager calls are playback-activity reports. Without it,
 * AudioFlinger's mixer looks the service up on every mix cycle of a track whose
 * mute state it could not report, logging "getOrCreateAudioManager(): binding
 * to audio service failed" every ~20 ms. The stand-in accepts the one-way
 * reports and hands out player/recorder ids for the two synchronous calls.
 *
 * Both services must stop before the framework starts, so system_server
 * registers the real ones.
 *
 *   moon-permissiond [PID_FILE]
 */
#include <android/binder_ibinder.h>
#include <android/binder_parcel.h>
#include <android/binder_status.h>
#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define AID_SYSTEM 1000
#define AID_APP_START 10000
#define MODE_ALLOWED 0
#define MODE_IGNORED 1

enum {
    CHECK_PERMISSION = FIRST_CALL_TRANSACTION,
    NOTE_OP,
    GET_PACKAGES_FOR_UID,
    IS_RUNTIME_PERMISSION,
    GET_PACKAGE_UID,
};

static bool string_allocator(void *data, int32_t length, char **buffer) {
    char **out = data;
    if (length < 0) {
        *out = NULL;
        *buffer = NULL;
        return true;
    }
    *out = calloc(1, (size_t)length + 1);
    *buffer = *out;
    return *out != NULL;
}

static char *read_string(const AParcel *in) {
    char *value = NULL;
    if (AParcel_readString(in, &value, string_allocator) != STATUS_OK) {
        free(value);
        return NULL;
    }
    return value;
}

/* Android grants its own service UIDs (system, root, native daemons); in Moon
 * mode no app UIDs run, and any that did would be refused. */
static bool trusted_uid(int32_t uid) {
    return uid >= 0 && (uid % 100000) < AID_APP_START;
}

static binder_status_t on_transact(AIBinder *binder, transaction_code_t code,
                                   const AParcel *in, AParcel *out) {
    (void)binder;
    int32_t uid = -1, pid = -1, flags = 0;
    char *text = NULL, *package = NULL;
    binder_status_t rc = STATUS_OK;
    AStatus *ok = AStatus_newOk();

    switch (code) {
    case CHECK_PERMISSION:
        text = read_string(in);
        AParcel_readInt32(in, &pid);
        AParcel_readInt32(in, &uid);
        rc = AParcel_writeStatusHeader(out, ok);
        if (rc == STATUS_OK) rc = AParcel_writeInt32(out, trusted_uid(uid));
        break;
    case NOTE_OP:
        text = read_string(in);
        AParcel_readInt32(in, &uid);
        package = read_string(in);
        rc = AParcel_writeStatusHeader(out, ok);
        if (rc == STATUS_OK)
            rc = AParcel_writeInt32(out, trusted_uid(uid) ? MODE_ALLOWED : MODE_IGNORED);
        break;
    case GET_PACKAGES_FOR_UID:
        AParcel_readInt32(in, &uid);
        rc = AParcel_writeStatusHeader(out, ok);
        if (rc != STATUS_OK) break;
        if (uid == AID_SYSTEM) {
            rc = AParcel_writeInt32(out, 1);
            if (rc == STATUS_OK) rc = AParcel_writeString(out, "android", 7);
        } else {
            rc = AParcel_writeInt32(out, -1); /* null String[]: no package */
        }
        break;
    case IS_RUNTIME_PERMISSION:
        text = read_string(in);
        rc = AParcel_writeStatusHeader(out, ok);
        if (rc == STATUS_OK) rc = AParcel_writeInt32(out, 0);
        break;
    case GET_PACKAGE_UID:
        package = read_string(in);
        AParcel_readInt32(in, &flags);
        rc = AParcel_writeStatusHeader(out, ok);
        if (rc == STATUS_OK)
            rc = AParcel_writeInt32(out, package && !strcmp(package, "android") ? AID_SYSTEM : -1);
        break;
    default:
        rc = STATUS_UNKNOWN_TRANSACTION;
    }
    free(text);
    free(package);
    AStatus_delete(ok);
    return rc;
}

/* frameworks/native include/audiomanager/IAudioManager.h */
enum {
    TRACK_PLAYER = FIRST_CALL_TRANSACTION,
    PLAYER_ATTRIBUTES,
    PLAYER_EVENT,
    RELEASE_PLAYER,
    TRACK_RECORDER,
    RECORDER_EVENT,
    RELEASE_RECORDER,
    PLAYER_SESSION_ID,
    PORT_EVENT,
};

static binder_status_t on_transact_audio(AIBinder *binder, transaction_code_t code,
                                         const AParcel *in, AParcel *out) {
    (void)binder;
    (void)in;
    static int32_t next_id = 1;
    switch (code) {
    case TRACK_PLAYER:
    case TRACK_RECORDER: {
        AStatus *ok = AStatus_newOk();
        binder_status_t rc = AParcel_writeStatusHeader(out, ok);
        AStatus_delete(ok);
        if (rc == STATUS_OK) rc = AParcel_writeInt32(out, __atomic_fetch_add(&next_id, 1, __ATOMIC_RELAXED));
        return rc;
    }
    case PLAYER_ATTRIBUTES:
    case PLAYER_EVENT:
    case RELEASE_PLAYER:
    case RECORDER_EVENT:
    case RELEASE_RECORDER:
    case PLAYER_SESSION_ID:
    case PORT_EVENT:
        /* One-way activity reports: accepted, nothing to track. */
        return STATUS_OK;
    default:
        return STATUS_UNKNOWN_TRANSACTION;
    }
}

static void *on_create(void *args) { return args; }
static void on_destroy(void *data) { (void)data; }

int main(int argc, char **argv) {
    void *ndk = dlopen("libbinder_ndk.so", RTLD_NOW);
    if (!ndk) {
        fprintf(stderr, "moon-permissiond: %s\n", dlerror());
        return 1;
    }
    /* LL-NDK entry points, absent from the public NDK stubs. */
    binder_exception_t (*add_service)(AIBinder *, const char *) =
        (binder_exception_t (*)(AIBinder *, const char *))dlsym(ndk, "AServiceManager_addService");
    bool (*set_max_threads)(uint32_t) =
        (bool (*)(uint32_t))dlsym(ndk, "ABinderProcess_setThreadPoolMaxThreadCount");
    void (*start_pool)(void) = (void (*)(void))dlsym(ndk, "ABinderProcess_startThreadPool");
    void (*join_pool)(void) = (void (*)(void))dlsym(ndk, "ABinderProcess_joinThreadPool");
    if (!add_service || !set_max_threads || !start_pool || !join_pool) {
        fprintf(stderr, "moon-permissiond: libbinder_ndk lacks service-manager entry points\n");
        return 1;
    }

    AIBinder_Class *klass = AIBinder_Class_define("android.os.IPermissionController",
                                                  on_create, on_destroy, on_transact);
    AIBinder *service = AIBinder_new(klass, NULL);
    set_max_threads(2);
    binder_exception_t ex = add_service(service, "permission");
    if (ex != EX_NONE) {
        fprintf(stderr, "moon-permissiond: addService(permission) failed: %d\n", ex);
        return 1;
    }
    /* Best effort: audio still works without it, only noisier. */
    AIBinder_Class *audio_class = AIBinder_Class_define("android.media.IAudioService",
                                                        on_create, on_destroy, on_transact_audio);
    AIBinder *audio = AIBinder_new(audio_class, NULL);
    ex = add_service(audio, "audio");
    if (ex != EX_NONE) fprintf(stderr, "moon-permissiond: addService(audio) failed: %d\n", ex);
    if (argc >= 2) {
        FILE *f = fopen(argv[1], "w");
        if (f) {
            fprintf(f, "%d\n", getpid());
            fclose(f);
        }
    }
    fprintf(stderr, "moon-permissiond: serving permission for service UIDs\n");
    start_pool();
    join_pool();
    return 0;
}
