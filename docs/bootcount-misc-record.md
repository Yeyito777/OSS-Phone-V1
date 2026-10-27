# Boot-attempt counter in the misc partition (record format v1)

Store for oss-init's boot-attempt counter (`notes/stage2/PROMOTION-PLAN.md`
§2, on `stage2-native-init`). It replaces the planned `oss-bootcount.ko`: no
PMU register could be proven unused by the bootloader or firmware, so the
counter lives on flash and needs no kernel module.

## Where

- **Partition:** `misc` = `/dev/block/sda21`, 1 048 576 bytes (live phone,
  2026-10-27).
- **Do not hard-code major:minor.** It reads 259:5 today, but 259 is the
  extended block major and its minor numbers are assigned at probe time.
  oss-init finds the partition instead:
  1. Mount sysfs.
  2. Scan `/sys/class/block/*/uevent` for `PARTNAME=misc`.
  3. Read that entry's `dev` (`MAJ:MIN`), `mknod` a private block node, and
     check its size is exactly 1 048 576 bytes before any write.
- **Two slots:** slot A = byte offset `0x40000` (4 KiB block 64), slot B =
  `0x41000` (block 65). Each is one full 4096-byte block.
- **Live map (read-only `dd`, 2026-10-27):** only blocks 8 (`0x8000`, the
  AOSP system area) and 127 (`0x7f000`, Samsung signer metadata) contain
  non-zero bytes. `0x3f000`–`0x41fff` is all zero, and the BCB (block 0) is
  currently zero too.

## Record (little-endian, at the start of a slot; the rest of the block is zero)

| Offset | Size | Field | Value |
| --- | --- | --- | --- |
| 0x00 | 8 | magic | ASCII `OSSBOOTC` |
| 0x08 | 2 | version | 1 |
| 0x0a | 2 | size | 32 (bytes covered by the CRC, including this header) |
| 0x0c | 4 | seq | +1 on every write (wraps, compared with serial-number arithmetic) |
| 0x10 | 4 | count | consecutive boot attempts without a confirmed-healthy session |
| 0x14 | 4 | max | the threshold N oss-init used (3); informational |
| 0x18 | 4 | flags | 0 (reserved) |
| 0x1c | 4 | crc32 | IEEE CRC-32 (zlib `crc32()`, init 0) over bytes 0x00–0x1b |

## Read

- **Valid slot:** magic, version 1, size 32 and crc32 all match.
- **Current record:**
  - If both slots are valid, the one with the newer seq
    (`(int32_t)(seqA - seqB) > 0` ⇒ A).
  - If one is valid, that one.
  - If none is valid, count = 0, seq = 0.
- **Interlock:** if a slot is neither valid nor all zero, something else is
  using that block. Do not write anything, log it, and behave as if there
  were no store (contain as usual, as when the module was absent).

## Write (bump at oss-init's first action, clear from the healthy-session hook)

1. Read the current record (above).
2. Build the new record:
   - **bump:** `count = min(count + 1, 0xffff)`.
   - **clear:** `count = 0`, written only if the current count is not
     already 0 (idempotent, no flash wear).
   - `seq = seq + 1`, `max = 3`, `flags = 0`, then compute the CRC.
3. Write a full 4096-byte block (record followed by zeros) to the slot
   **not** holding the current record (both invalid ⇒ slot A). Use
   `pwrite` on a node opened `O_RDWR | O_DSYNC`, then `fsync`. A torn or
   failed write leaves the other slot intact, so the worst case is losing
   one increment.
4. Read the block back and compare it. On mismatch, log and treat as no
   store.
5. **Decision:** if the new count is `>= 3` after a bump, skip containment
   and exec stock init (PROMOTION-PLAN §2).

Nothing outside `0x40000`–`0x41fff` is ever written.

## Semantics and caveats

- **Persists across warm reset, panic and cold power-off.** A phone the user
  power-cycles during a boot loop still escalates to the stock-init skip
  after 3 attempts. That fails safe.
- **Clear side:** `moon-boot-confirm.sh` needs a writer that produces this
  format (CRC included). Have oss-init provide it as a mode, e.g.
  `oss-init --bootcount clear` (or `show`), so there is one implementation.
  The old sysfs/`insmod clear=1` path goes away. Its reload fallback could
  not have cleared anything: `insmod` of a loaded module fails with
  `EEXIST`, and `rmmod` only unloads it.
- **Flash-tool interaction:** writing the whole-partition image
  `misc-bcb-clear.img` (1 MiB, zero at these offsets) erases the counter,
  which simply resets it. A future misc restore should write only block 0 if
  the counter must survive.
- **Wear:** at most two 4 KiB writes per boot (bump, clear), on a UFS
  partition with wear levelling. Negligible.

## Implementation (oss-init v7, `scripts/native-init/bootcount-misc.h`)

Implemented as specified, with two deliberate refinements:

1. **When the bump happens.** The misc partition does not exist at oss-init's
   first action: UFS is a vendor module (`ufs-exynos-core.ko`, 29 symbol
   dependencies plus regulator/PHY providers) that the container's stock first
   stage loads; misc appears at ~1.9 s and first stage switches root at ~4.7 s
   (v5 on-device log). So oss-init starts the container as usual and a helper,
   in its own private mount namespace, waits for `PARTNAME=misc`, **freezes the
   container** (pidfd `SIGSTOP`), bumps, and reports; oss-init then thaws the
   container or kills it and takes the skip path. The freeze lands during
   module loading, before any dm device or mount, i.e. the same state as an
   early container failure. A container that is already live is never killed
   for the count.
2. **Our own damaged slot is not "foreign".** Literally, "neither valid nor all
   zero ⇒ someone else uses it" would turn one torn write of ours into a
   permanently disabled store. A slot that starts with `OSSBOOTC`, has only
   zeros after byte 32, but fails the version/size/CRC check is classified
   DAMAGED: treated as no record, and overwritable. Anything else non-zero is
   FOREIGN (nothing written).

Skip target: magiskinit as `/init` (today's proven boot, as oss-pid1 does), the
stock init via the held fd only if that exec fails. A promoted build
(`-DOSS_PROMOTED=1`) also skips when there is no trustworthy count (absent,
damaged, foreign, I/O or verify error, wrong size, ambiguous); misc never
appearing within 15 s contains as usual in both modes. Single writer:
`oss-init --bootcount show|clear|init|bump`.
