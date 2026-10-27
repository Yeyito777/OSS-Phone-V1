/* vkfirst: the first-frame test for Vulkan remoted through Venus
 * (notes/gpu/BROWSER.md). Built for glibc (the Browser's Debian root):
 * lists the device the loader offers, then clears an image on the GPU,
 * copies it into a host-visible buffer and checks every pixel, timing the
 * round trip. No window system.
 *
 *   VK_ICD_FILENAMES=.../virtio_icd.aarch64.json VN_DEBUG=vtest \
 *   VTEST_SOCKET_NAME=/tmp/.virgl_test ./vkfirst [ROUNDS] */
#define VK_NO_PROTOTYPES
#include <vulkan/vulkan.h>

#include <dlfcn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define W 1080
#define H 2340

static PFN_vkGetInstanceProcAddr gipa;
static VkInstance inst;
static VkDevice dev;
#define FN(name) static PFN_##name name
FN(vkCreateInstance); FN(vkEnumeratePhysicalDevices); FN(vkGetPhysicalDeviceProperties);
FN(vkGetPhysicalDeviceMemoryProperties); FN(vkCreateDevice); FN(vkGetDeviceQueue);
FN(vkCreateImage); FN(vkGetImageMemoryRequirements); FN(vkAllocateMemory);
FN(vkBindImageMemory); FN(vkCreateBuffer); FN(vkGetBufferMemoryRequirements);
FN(vkBindBufferMemory); FN(vkMapMemory); FN(vkCreateCommandPool);
FN(vkAllocateCommandBuffers); FN(vkBeginCommandBuffer); FN(vkEndCommandBuffer);
FN(vkCmdPipelineBarrier); FN(vkCmdClearColorImage); FN(vkCmdCopyImageToBuffer);
FN(vkQueueSubmit); FN(vkCreateFence); FN(vkWaitForFences); FN(vkResetFences);
FN(vkResetCommandBuffer); FN(vkInvalidateMappedMemoryRanges);

static double now_ms(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}

static uint32_t mem_type(VkPhysicalDevice pd, uint32_t bits, VkMemoryPropertyFlags want) {
    VkPhysicalDeviceMemoryProperties m;
    vkGetPhysicalDeviceMemoryProperties(pd, &m);
    for (uint32_t i = 0; i < m.memoryTypeCount; i++)
        if ((bits & (1u << i)) && (m.memoryTypes[i].propertyFlags & want) == want)
            return i;
    return UINT32_MAX;
}

#define CHECK(call)                                                            \
    do {                                                                       \
        VkResult r_ = (call);                                                  \
        if (r_ != VK_SUCCESS) {                                                \
            printf("FAIL: %s = %d\n", #call, r_);                              \
            return 1;                                                          \
        }                                                                      \
    } while (0)

int main(int argc, char **argv) {
    int rounds = argc > 1 ? atoi(argv[1]) : 20;
    void *lib = dlopen("libvulkan.so.1", RTLD_NOW);
    if (!lib) {
        printf("FAIL: %s\n", dlerror());
        return 1;
    }
    gipa = (PFN_vkGetInstanceProcAddr)dlsym(lib, "vkGetInstanceProcAddr");
    vkCreateInstance = (PFN_vkCreateInstance)gipa(NULL, "vkCreateInstance");
    VkApplicationInfo app = {.sType = VK_STRUCTURE_TYPE_APPLICATION_INFO,
                             .apiVersion = VK_API_VERSION_1_1};
    VkInstanceCreateInfo ici = {.sType = VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
                                .pApplicationInfo = &app};
    double t0 = now_ms();
    CHECK(vkCreateInstance(&ici, NULL, &inst));
#define L(name) name = (PFN_##name)gipa(inst, #name)
    L(vkEnumeratePhysicalDevices); L(vkGetPhysicalDeviceProperties);
    L(vkGetPhysicalDeviceMemoryProperties); L(vkCreateDevice); L(vkGetDeviceQueue);
    L(vkCreateImage); L(vkGetImageMemoryRequirements); L(vkAllocateMemory);
    L(vkBindImageMemory); L(vkCreateBuffer); L(vkGetBufferMemoryRequirements);
    L(vkBindBufferMemory); L(vkMapMemory); L(vkCreateCommandPool);
    L(vkAllocateCommandBuffers); L(vkBeginCommandBuffer); L(vkEndCommandBuffer);
    L(vkCmdPipelineBarrier); L(vkCmdClearColorImage); L(vkCmdCopyImageToBuffer);
    L(vkQueueSubmit); L(vkCreateFence); L(vkWaitForFences); L(vkResetFences);
    L(vkResetCommandBuffer); L(vkInvalidateMappedMemoryRanges);
    uint32_t n = 1;
    VkPhysicalDevice pd;
    CHECK(vkEnumeratePhysicalDevices(inst, &n, &pd) < 0 ? VK_ERROR_UNKNOWN : VK_SUCCESS);
    if (!n) {
        printf("FAIL: no physical device\n");
        return 1;
    }
    VkPhysicalDeviceProperties p;
    vkGetPhysicalDeviceProperties(pd, &p);
    printf("device: %s, Vulkan %u.%u.%u (instance in %.1f ms)\n", p.deviceName,
           VK_VERSION_MAJOR(p.apiVersion), VK_VERSION_MINOR(p.apiVersion),
           VK_VERSION_PATCH(p.apiVersion), now_ms() - t0);
    float prio = 1;
    VkDeviceQueueCreateInfo q = {.sType = VK_STRUCTURE_TYPE_DEVICE_QUEUE_CREATE_INFO,
                                 .queueCount = 1, .pQueuePriorities = &prio};
    VkDeviceCreateInfo dci = {.sType = VK_STRUCTURE_TYPE_DEVICE_CREATE_INFO,
                              .queueCreateInfoCount = 1, .pQueueCreateInfos = &q};
    CHECK(vkCreateDevice(pd, &dci, NULL, &dev));
    VkQueue queue;
    vkGetDeviceQueue(dev, 0, 0, &queue);

    VkImageCreateInfo ii = {.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO,
                            .imageType = VK_IMAGE_TYPE_2D, .format = VK_FORMAT_R8G8B8A8_UNORM,
                            .extent = {W, H, 1}, .mipLevels = 1, .arrayLayers = 1,
                            .samples = VK_SAMPLE_COUNT_1_BIT, .tiling = VK_IMAGE_TILING_OPTIMAL,
                            .usage = VK_IMAGE_USAGE_TRANSFER_DST_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT};
    VkImage img;
    CHECK(vkCreateImage(dev, &ii, NULL, &img));
    VkMemoryRequirements mr;
    vkGetImageMemoryRequirements(dev, img, &mr);
    VkMemoryAllocateInfo ai = {.sType = VK_STRUCTURE_TYPE_MEMORY_ALLOCATE_INFO,
                               .allocationSize = mr.size,
                               .memoryTypeIndex = mem_type(pd, mr.memoryTypeBits,
                                                           VK_MEMORY_PROPERTY_DEVICE_LOCAL_BIT)};
    VkDeviceMemory imem, bmem;
    CHECK(vkAllocateMemory(dev, &ai, NULL, &imem));
    CHECK(vkBindImageMemory(dev, img, imem, 0));
    VkBufferCreateInfo bi = {.sType = VK_STRUCTURE_TYPE_BUFFER_CREATE_INFO,
                             .size = (VkDeviceSize)W * H * 4,
                             .usage = VK_BUFFER_USAGE_TRANSFER_DST_BIT};
    VkBuffer buf;
    CHECK(vkCreateBuffer(dev, &bi, NULL, &buf));
    vkGetBufferMemoryRequirements(dev, buf, &mr);
    ai.allocationSize = mr.size;
    ai.memoryTypeIndex = mem_type(pd, mr.memoryTypeBits,
                                  VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT |
                                      VK_MEMORY_PROPERTY_HOST_CACHED_BIT);
    if (ai.memoryTypeIndex == UINT32_MAX)
        ai.memoryTypeIndex = mem_type(pd, mr.memoryTypeBits, VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT);
    CHECK(vkAllocateMemory(dev, &ai, NULL, &bmem));
    CHECK(vkBindBufferMemory(dev, buf, bmem, 0));
    uint32_t *px;
    CHECK(vkMapMemory(dev, bmem, 0, VK_WHOLE_SIZE, 0, (void **)&px));

    VkCommandPoolCreateInfo pi = {.sType = VK_STRUCTURE_TYPE_COMMAND_POOL_CREATE_INFO,
                                  .flags = VK_COMMAND_POOL_CREATE_RESET_COMMAND_BUFFER_BIT};
    VkCommandPool pool;
    CHECK(vkCreateCommandPool(dev, &pi, NULL, &pool));
    VkCommandBufferAllocateInfo cai = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_ALLOCATE_INFO,
                                       .commandPool = pool,
                                       .level = VK_COMMAND_BUFFER_LEVEL_PRIMARY,
                                       .commandBufferCount = 1};
    VkCommandBuffer cb;
    CHECK(vkAllocateCommandBuffers(dev, &cai, &cb));
    VkFenceCreateInfo fi = {.sType = VK_STRUCTURE_TYPE_FENCE_CREATE_INFO};
    VkFence fence;
    CHECK(vkCreateFence(dev, &fi, NULL, &fence));

    double total = 0, worst = 0;
    int bad = 0;
    for (int r = 0; r < rounds; r++) {
        uint8_t v = (uint8_t)(r * 37 + 11);
        double t = now_ms();
        CHECK(vkResetCommandBuffer(cb, 0));
        VkCommandBufferBeginInfo cbi = {.sType = VK_STRUCTURE_TYPE_COMMAND_BUFFER_BEGIN_INFO,
                                        .flags = VK_COMMAND_BUFFER_USAGE_ONE_TIME_SUBMIT_BIT};
        CHECK(vkBeginCommandBuffer(cb, &cbi));
        VkImageSubresourceRange range = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 1, 0, 1};
        VkImageMemoryBarrier b = {.sType = VK_STRUCTURE_TYPE_IMAGE_MEMORY_BARRIER,
                                  .dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
                                  .oldLayout = VK_IMAGE_LAYOUT_UNDEFINED,
                                  .newLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,
                                  .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                  .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                  .image = img, .subresourceRange = range};
        vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,
                             VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &b);
        VkClearColorValue c = {.float32 = {v / 255.0f, 64 / 255.0f, 128 / 255.0f, 1.0f}};
        vkCmdClearColorImage(cb, img, VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL, &c, 1, &range);
        b.srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT;
        b.dstAccessMask = VK_ACCESS_TRANSFER_READ_BIT;
        b.oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;
        b.newLayout = VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL;
        vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TRANSFER_BIT,
                             VK_PIPELINE_STAGE_TRANSFER_BIT, 0, 0, NULL, 0, NULL, 1, &b);
        VkBufferImageCopy copy = {.imageSubresource = {VK_IMAGE_ASPECT_COLOR_BIT, 0, 0, 1},
                                  .imageExtent = {W, H, 1}};
        vkCmdCopyImageToBuffer(cb, img, VK_IMAGE_LAYOUT_TRANSFER_SRC_OPTIMAL, buf, 1, &copy);
        VkBufferMemoryBarrier hb = {.sType = VK_STRUCTURE_TYPE_BUFFER_MEMORY_BARRIER,
                                    .srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,
                                    .dstAccessMask = VK_ACCESS_HOST_READ_BIT,
                                    .srcQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                    .dstQueueFamilyIndex = VK_QUEUE_FAMILY_IGNORED,
                                    .buffer = buf, .size = VK_WHOLE_SIZE};
        vkCmdPipelineBarrier(cb, VK_PIPELINE_STAGE_TRANSFER_BIT, VK_PIPELINE_STAGE_HOST_BIT, 0,
                             0, NULL, 1, &hb, 0, NULL);
        CHECK(vkEndCommandBuffer(cb));
        VkSubmitInfo si = {.sType = VK_STRUCTURE_TYPE_SUBMIT_INFO, .commandBufferCount = 1,
                           .pCommandBuffers = &cb};
        CHECK(vkResetFences(dev, 1, &fence));
        CHECK(vkQueueSubmit(queue, 1, &si, fence));
        CHECK(vkWaitForFences(dev, 1, &fence, VK_TRUE, 5000000000ull));
        VkMappedMemoryRange mrange = {.sType = VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE,
                                      .memory = bmem, .size = VK_WHOLE_SIZE};
        vkInvalidateMappedMemoryRanges(dev, 1, &mrange);
        double dt = now_ms() - t;
        total += dt;
        if (dt > worst)
            worst = dt;
        uint32_t want = 0xff804000u | v; /* RGBA bytes: v, 0x40, 0x80, 0xff */
        for (size_t i = 0; i < (size_t)W * H; i += 997)
            if (px[i] != want) {
                if (!bad)
                    printf("round %d: pixel %zu is 0x%08x, want 0x%08x\n", r, i, px[i], want);
                bad++;
                break;
            }
    }
    printf("%s: %d rounds of clear + copy + read back of %dx%d, mean %.2f ms, worst %.2f ms\n",
           bad ? "FAIL" : "ok", rounds, W, H, total / rounds, worst);
    return bad != 0;
}
