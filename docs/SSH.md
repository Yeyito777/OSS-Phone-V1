# SSH into the phone

```sh
ssh oss-phone          # from the Mac or from whale, on any network
ssh oss-phone-lan      # from the Mac, straight to the phone's Wi-Fi address
scp file oss-phone:/root/
```

Each lands in the chroot as root with the full environment: bash, `PATH`
with `/usr/local/bin` (`android-sh`, `exocortexd`, Moon's tools under
`/opt`), Bun and Claude Code, `DISPLAY=:0`. `android-sh` reaches Android's
side as usual.

## How it works

```
 Mac ──ssh── whale:22 ──(ProxyJump)── whale 127.0.0.1:2290 ══ reverse tunnel ══╗
 whale ─────────────────────────────── whale 127.0.0.1:2290 ════════════════════╣
 Mac ──────────────── phone Wi-Fi address:2222 (oss-phone-lan) ─────────────────╣
                                                                     phone sshd :2222
```

- **The server** is Alpine's OpenSSH (`openssh-server`) in the chroot, run
  by the Exocortex keeper as `/usr/sbin/sshd -D -e -f
  /etc/ssh/oss-phone-sshd.conf` (`exocortex-ns.sh sshd`). Like the daemon
  it runs from the root cgroup in a mount namespace of its own (its own
  `/proc`, `/dev`, `/tmp`, and a devpts instance of its own for session
  ptys), so Moon, Xorg, a chroot-session teardown and the fallback to
  Android leave it running, and the keeper restarts it with backoff
  (docs/EXOCORTEX.md, "Supervision"). Port 2222 on every address, root by
  key only (no passwords). Host keys are made once (`ssh-keygen -A`) in
  `/etc/ssh` on /data, so the phone keeps its identity across updates.
  Its log is `/var/log/exocortex/sshd.out`.
- **The tunnel** (`/usr/local/bin/oss-phone-tunnel`, `exocortex-ns.sh
  tunnel`, kept by the keeper too) is `ssh -R
  127.0.0.1:2290:127.0.0.1:2222 whale` with keepalives
  (`ServerAliveInterval 15`) and `ExitOnForwardFailure`, using the phone's
  own key for whale. A phone that vanishes (power loss, a network change)
  would leave its old session holding the port on whale for hours, so the
  session's command on whale is a watchdog fed by a heartbeat line from the
  phone every 5 s: 60 s without one and it ends its own session. A new tunnel
  also ends the previous one's session first (`~/.cache/oss-phone-tunnel.ppid`
  on whale). Its settings are `/etc/oss-phone/tunnel.conf` (`TUNNEL_HOST`,
  `TUNNEL_PORT`, `SSHD_PORT`; created once by the installer). Its log is
  `/var/log/exocortex/tunnel.out`.
- **The clients.** `Host oss-phone` on the Mac goes through whale
  (`ProxyJump whale` to `127.0.0.1:2290`); on whale it is
  `127.0.0.1:2290` directly. `Host oss-phone-lan` on the Mac is the phone's
  current Wi-Fi address, port 2222 (UofT's Wi-Fi lets them reach each
  other; on a network that isolates clients only `oss-phone` works). All
  three use `HostKeyAlias oss-phone`, pinned in each `known_hosts`.
- **Over USB**, with no network at all (Android mode without Wi-Fi, say):
  `adb forward tcp:22222 tcp:2222`, then `ssh -p 22222 -o HostKeyAlias=oss-phone root@127.0.0.1`.

## Install

```sh
scripts/exocortex/install.sh       # the server, its host keys, the tunnel (also install-all.sh step 7)
scripts/exocortex/ssh-access.sh    # keys and client configs; then checks
```

`ssh-access.sh` adds this Mac's key (the one `~/.ssh/config` uses for
whale) and whale's user keys (`~/.ssh/id_*.pub`) to the phone's
`/root/.ssh/authorized_keys`, writes the `oss-phone` blocks into the Mac's
and whale's `~/.ssh/config` (each backed up first as
`config.bak-oss-phone-<time>`; the block sits between `# >>> oss-phone` and
`# <<< oss-phone` and is replaced on a rerun, so rerun it when the phone's
Wi-Fi address changes), pins the phone's host key, then checks `ssh
oss-phone` from both machines, `oss-phone-lan` and `scp`. `--check` runs only
the checks.

## Verified (2026-10-28)

- `ssh oss-phone` from the Mac and from whale, `ssh oss-phone-lan` from the
  Mac, `scp` both ways: `root`, `aarch64`, `android-sh -c "getprop
  ro.product.model"` → `SM-A266M`; an interactive session gets bash, the full
  `PATH` and a pty from its own devpts.
- Moon killed: the supervisor rebuilt Moon's session; `ssh oss-phone`
  worked throughout but for one attempt while the tunnel reconnected.
- whale cut off for 80 s (iptables on the phone): whale's end of the old
  tunnel ended itself after under 60 s and freed the port; the tunnel was
  back 12 s after the link returned.
- Reboot through `moon-power.sh`: `ssh oss-phone` answered 39 s after the
  kernel started.
- Over USB with `adb forward`.
