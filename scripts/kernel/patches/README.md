# Patched vendor modules

`build-patched-module.sh PATCH MODULE_KO_PATH` rebuilds one of Samsung's
in-tree vendor modules with a patch. It uses the installed OSS kernel's build
tree (a throw-away copy), the same toolchain, paths and sandbox. It then
checks all 306 vendor modules with the patched one substituted
(`check-kmi.py`) and compares the module's code with Samsung's prebuilt,
function by function.

## mali-kbase-pid-namespace.patch (`mali_kbase.ko`, Mali r32p1)

**Problem.** Inside a PID namespace (stage 2: Android's init contained by
oss-init), every GPU context fails with `Failed to get pid pointer for
surfaceflinger/N` / `Common context initialization failed error = -3`,
and SurfaceFlinger finds no EGLConfig.

`kbase_context_common_init()` stores the caller's global TGID and resolves
it with `find_get_pid()`. That searches the caller's own namespace, where
the global number does not exist.

**Fix.** Two lines:

- `kbase_context_common_init`: `find_get_pid(kctx->tgid)` →
  `get_task_pid(current, PIDTYPE_TGID)`, the caller's own thread-group
  pid.
- `mali_oom_notifier_handler` (diagnostic print): `find_get_pid(kctx->pid)`
  → `get_pid(find_pid_ns(kctx->pid, &init_pid_ns))`. The stored number is
  global, so it is resolved in the initial namespace, not in the namespace
  of whichever task triggered the OOM.

Every other PID/TGID use in kbase was reviewed and needs no change. They
store or print the global number consistently: debugfs directory names
`<tgid>_<id>`, ktrace, timeline and `gpu_mem_total` tracepoints, the
per-process `kprcs` tree key, the dma-fence workqueue name and log lines.
Memory ownership checks use `kctx->process_mm` / `current->mm`, not PIDs.
Inside a container, tools reading the GPU memory tracepoints or the debugfs
names see global PIDs. That is cosmetic: it affects attribution only, not
function.

**Normal boot (no PID namespace): identical behaviour.**

- `find_get_pid(tgid)` is `find_pid_ns(tgid, task_active_pid_ns(current))`.
  In the initial namespace that returns the struct pid numbered `tgid`,
  which is the thread-group leader's pid: `task_tgid(current)`.
  `get_task_pid(current, PIDTYPE_TGID)` returns that same object, with the
  same extra reference that `put_pid()` drops afterwards.
- The OOM change equals the old lookup whenever the notifier runs in the
  initial namespace.
- No struct, export, ioctl or memory-layout change.

**Result** (`$KROOT/out/modules/mali-kbase-pid-namespace/`):

- `mali_kbase.ko` sha256
  `f36fd33955bca28479ed93e5c81613229b0ea2594e8df38a22eb74cbd4a614bc`
  (debug-stripped, as shipped).
- vermagic `5.10.223-android12-9-30597860-abA266MUBU1AYB8 SMP preempt
  mod_unload modversions aarch64`.
- `check-kmi.py` over all 306 vendor modules with this one substituted:
  0 problems.
- Against Samsung's prebuilt `mali_kbase.ko`:
  - `__versions` is identical for all 340 shared imports, including those
    from vendor modules. The only changes are −`find_get_pid` and
    +`get_task_pid`, `find_pid_ns`, `init_pid_ns`; all are exported by both
    the installed and the stock kernel with these CRCs.
  - The 136 exports are identical.
  - modinfo is identical except `srcversion`.
  - Of 2710 functions, exactly 2 differ in code: the two patched ones.

**Deployment.** For now only in the stage-2 recovery-slot image's own vendor
ramdisk; vendor_boot is untouched. Because the normal-boot behaviour is
identical, it could later replace the prebuilt in vendor_boot as well. That
needs a vendor_boot flash path and an attended test.
