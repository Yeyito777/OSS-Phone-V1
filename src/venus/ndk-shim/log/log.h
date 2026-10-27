/* NDK stand-in for AOSP liblog headers (virglrenderer on Android, built with the NDK). */
#pragma once
#include <android/log.h>
#define LOG_PRI(prio, tag, ...) __android_log_print(prio, tag, __VA_ARGS__)
