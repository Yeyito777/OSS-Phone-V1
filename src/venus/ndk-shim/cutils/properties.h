/* NDK stand-in for AOSP cutils/properties.h: system properties through libc. */
#pragma once
#include <sys/system_properties.h>
#include <string.h>
#define PROPERTY_VALUE_MAX PROP_VALUE_MAX
#define PROPERTY_KEY_MAX PROP_NAME_MAX
static inline int property_get(const char *key, char *value, const char *default_value) {
    int n = __system_property_get(key, value);
    if (n > 0) return n;
    if (default_value) { strncpy(value, default_value, PROPERTY_VALUE_MAX - 1); value[PROPERTY_VALUE_MAX - 1] = 0; return (int)strlen(value); }
    value[0] = 0; return 0;
}
