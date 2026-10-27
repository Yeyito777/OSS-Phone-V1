/* moon-gpu-vkprobe: what the Mali's Vulkan driver offers, for remoting it to
 * the browser's glibc root (notes/gpu/BROWSER.md): API and driver versions,
 * memory types, and the extensions Venus (virglrenderer's Vulkan server)
 * and dmabuf sharing need. Read-only; touches nothing but the GPU driver.
 *
 *   moon-gpu-vkprobe [-a]     (-a: list every extension) */
#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *wanted[] = {
    /* Venus (vkr) on the host side */
    "VK_KHR_external_memory_fd", "VK_EXT_external_memory_dma_buf",
    "VK_EXT_image_drm_format_modifier", "VK_EXT_queue_family_foreign",
    "VK_KHR_external_semaphore_fd", "VK_KHR_external_fence_fd",
    "VK_EXT_external_memory_host", "VK_KHR_timeline_semaphore",
    "VK_KHR_driver_properties", "VK_KHR_maintenance4",
    /* Android sharing */
    "VK_ANDROID_external_memory_android_hardware_buffer",
    "VK_ANDROID_native_buffer",
};

static int has(const VkExtensionProperties *e, uint32_t n, const char *name) {
    for (uint32_t i = 0; i < n; i++)
        if (!strcmp(e[i].extensionName, name))
            return 1;
    return 0;
}

int main(int argc, char **argv) {
    int all = argc > 1 && !strcmp(argv[1], "-a");
    void *lib = dlopen("libvulkan.so", RTLD_NOW);
    if (!lib) {
        printf("no libvulkan.so: %s\n", dlerror());
        return 1;
    }
    PFN_vkGetInstanceProcAddr gipa = (PFN_vkGetInstanceProcAddr)dlsym(lib, "vkGetInstanceProcAddr");
    PFN_vkEnumerateInstanceVersion env =
        (PFN_vkEnumerateInstanceVersion)gipa(NULL, "vkEnumerateInstanceVersion");
    uint32_t iv = VK_API_VERSION_1_0;
    if (env)
        env(&iv);
    printf("loader: Vulkan %u.%u.%u\n", VK_VERSION_MAJOR(iv), VK_VERSION_MINOR(iv),
           VK_VERSION_PATCH(iv));
    PFN_vkCreateInstance ci = (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance");
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .apiVersion = iv};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app};
    VkInstance inst;
    VkResult r = ci(&ici, NULL, &inst);
    if (r != VK_SUCCESS) {
        printf("vkCreateInstance: %d\n", r);
        return 1;
    }
#define I(name) PFN_##name name = (PFN_##name)gipa(inst, #name)
    I(vkEnumeratePhysicalDevices);
    I(vkGetPhysicalDeviceProperties);
    I(vkEnumerateDeviceExtensionProperties);
    I(vkGetPhysicalDeviceMemoryProperties);
    I(vkGetPhysicalDeviceQueueFamilyProperties);
    uint32_t n = 0;
    vkEnumeratePhysicalDevices(inst, &n, NULL);
    VkPhysicalDevice pd[4];
    if (n > 4)
        n = 4;
    vkEnumeratePhysicalDevices(inst, &n, pd);
    for (uint32_t d = 0; d < n; d++) {
        VkPhysicalDeviceProperties p;
        vkGetPhysicalDeviceProperties(pd[d], &p);
        printf("device %u: %s, Vulkan %u.%u.%u, driver 0x%x, vendor 0x%x\n", d, p.deviceName,
               VK_VERSION_MAJOR(p.apiVersion), VK_VERSION_MINOR(p.apiVersion),
               VK_VERSION_PATCH(p.apiVersion), p.driverVersion, p.vendorID);
        uint32_t ne = 0;
        vkEnumerateDeviceExtensionProperties(pd[d], NULL, &ne, NULL);
        VkExtensionProperties *e = calloc(ne, sizeof *e);
        vkEnumerateDeviceExtensionProperties(pd[d], NULL, &ne, e);
        printf("  %u device extensions\n", ne);
        for (size_t i = 0; i < sizeof wanted / sizeof *wanted; i++)
            printf("  %-52s %s\n", wanted[i], has(e, ne, wanted[i]) ? "yes" : "no");
        if (all)
            for (uint32_t i = 0; i < ne; i++)
                printf("    %s %u\n", e[i].extensionName, e[i].specVersion);
        free(e);
        VkPhysicalDeviceMemoryProperties m;
        vkGetPhysicalDeviceMemoryProperties(pd[d], &m);
        for (uint32_t i = 0; i < m.memoryTypeCount; i++)
            printf("  memory type %u: heap %u flags 0x%x\n", i, m.memoryTypes[i].heapIndex,
                   m.memoryTypes[i].propertyFlags);
        uint32_t nq = 0;
        vkGetPhysicalDeviceQueueFamilyProperties(pd[d], &nq, NULL);
        printf("  %u queue families\n", nq);
    }
    return 0;
}
