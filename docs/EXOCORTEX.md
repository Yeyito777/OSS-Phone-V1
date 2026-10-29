# Exocortex on the phone

The user's AI agent, [Exocortex](https://github.com/Yeyito777/Exocortex), runs
natively on the phone: its daemon runs as root in the Alpine chroot, a keeper
outside the chroot keeps it running in Moon and Android mode, and Moon's
Exocortex app (a26-system `--app exocortex`) is its client. Its conversations
can do anything on the phone, Android's side included.

```
 Android's root namespace (init, Magisk)          the chroot, /data/local/a26-linux
 ───────────────────────────────────────          ─────────────────────────────────
 service.d/95-exocortex.sh ── boot ──┐
 exocortex-keeper.sh keeper  ◄──────►│ watch each other
 exocortex-keeper.sh watchdog ◄──────┘
      │ unshare -m (a private mount namespace each)
      ├── exocortex-ns.sh daemon ─ chroot ─► exocortexd (/opt/exocortex), root
      │                                       ├─ Claude Code (Claude, /proxy whale)
      │                                       ├─ OpenAI (API key)
      │                                       └─ tools: chroot, android-sh, Moon's sockets
      └── exocortex-ns.sh notify ─ chroot ─► exocortex-notify ─► moon-notifyd
                                              Exocortex app ─► /opt/exocortex/config/runtime/exocortexd.sock
```

## Install and update

From the Mac, with the phone on adb:

```sh
scripts/exocortex/install.sh --openai-key      # first install: also logs in to OpenAI
scripts/exocortex/authorize-whale.sh           # once: the phone gets full access to whale
scripts/exocortex/install.sh                   # update (also step 7 of scripts/install-all.sh)
```

`install.sh` is idempotent. It installs the keeper (`/data/adb/exocortex`, the
Magisk hook `/data/adb/service.d/95-exocortex.sh`) and the chroot's
`exocortexd`, `exocortex-supervisor` and `android-sh` in `/usr/local/bin`,
then runs `scripts/device/exocortex/phone-setup.sh` inside the chroot:

| What | Where |
| --- | --- |
| Alpine packages (git, curl, openssh-client, ripgrep, libstdc++, …) | `apk add` |
| Bun for linux-aarch64-musl, pinned version and sha256 | `/opt/bun/bin/bun` |
| Claude Code, official installer (checks its manifest's sha256) | `/root/.local/bin/claude` |
| Exocortex, a git checkout of `main` (`--branch`) | `/opt/exocortex`, data in its `config/` |
| The phone's instructions (only if missing; else `system.md.oss-phone`) | `/opt/exocortex/config/system.md`, from `scripts/device/exocortex/system.md` |
| Claude through whale: `providers.anthropic.proxy.ssh = whale` | `/opt/exocortex/config/config.json` |
| The phone's own SSH key for whale, `Host whale`, whale's pinned host key | `/root/.ssh` |
| OSS-Phone-V1 and a26-system sources, for the agent | `/opt/src` |

then starts the keeper, or restarts the daemon when it runs. The phone can
also update itself: `git pull --ff-only && bun install` in `/opt/exocortex`,
then `exocortexd restart` (or the agent's own restart action).

## Credentials

- **OpenAI: an API key.** Exocortex's API-key login (`exocortexd login
  openai key`, requests to api.openai.com, the key's models only).
  `install.sh --openai-key` takes `OPENAI_API_KEY` from the Mac's `secrets`
  store and pipes it over adb into a root-only 0600 file, from which
  `exocortexd login openai key` reads it on stdin; the file is then deleted.
  The key lives only in Exocortex's credential store
  (`/opt/exocortex/config/secrets`, 0600). It never appears in argv, a Mac
  file or a log. Rotate by running `install.sh --openai-key` again.
- **Claude: whale's login, through /proxy.** Claude Code runs on the phone
  as the agent loop, not signed in; its requests go over `ssh whale
  exocortexd proxy` to whale's daemon, which adds its own Claude login
  (Exocortex `docs/provider-proxy.md`). The phone made its own key
  (`/root/.ssh/exocortex_whale_ed25519`, never copied off the phone).
  `authorize-whale.sh` adds it to whale's `authorized_keys` as a plain
  entry (`ssh-ed25519 … oss-phone-exocortex`), with no forced command or
  restrictions: the phone has the same access to whale as the user's Mac.
  The phone's daemon uses the ordinary /proxy bridge, so it also has
  `remote_bash` on whale, and the phone's agent can `ssh whale`. The script
  then checks from the phone (`check-whale-link.sh`) that the link answers
  with the full protocol, whale is logged in to Claude, and a command runs
  on whale; `authorize-whale.sh --check` repeats only the checks. To revoke,
  delete the line ending in `oss-phone-exocortex` from whale's
  `authorized_keys`. (Exocortex also has `exocortexd proxy --link-only` for a
  key that may only borrow the login; this phone deliberately does not use
  it.)

## Supervision

The daemon must outlive everything on the phone except a shutdown, in Moon
and Android mode. There is no systemd, and anything started from Moon's
session dies with the `moon-session` init service's cgroup.

- **Start.** Magisk's `service.d` runs `95-exocortex.sh` at every boot, in
  both modes (direct boot or not, Moon or the Android fallback). It starts
  `exocortex-keeper.sh boot`, which detaches the two loops (`nohup setsid`)
  and returns.
- **Keeper and watchdog.** Each moves itself to the root cgroup (nobody's
  init service), ignores SIGHUP, has `oom_score_adj` -1000, and restarts the
  other if it is gone: the watchdog checks every 5 s, the keeper every 2 s.
  A pid file plus the process's command line identify each role, so a reused
  PID never counts and a second copy exits.
- **The daemon.** The keeper starts it when it is not running, with backoff
  after a crash (2 s doubling to 60 s; reset after two minutes up) and at
  once after a requested restart. SIGTERM is the daemon's graceful shutdown:
  running turns settle and replay when it starts again. Killing the keeper
  does not stop the daemon; the next keeper adopts it by its pid file.
- **Its own mounts.** Each child runs in a private mount namespace
  (`exocortex-ns.sh`): the chroot's `/proc`, `/dev`, `/sys` and `/tmp` are its
  own, so Moon's chroot session ending (Moon or Xorg dying, Android taking
  over) cannot unmount them under the daemon. Moon's live sockets (`/run/a26-shell`,
  `/run/moon-notify`, `/run/a26-wifi`, the `/data/local/tmp` ones, X11) are
  `/run` symlinks into Android's namespace through `/proc/1/root`, so they
  follow Moon restarts; `/run/moon-session` is Moon's whole `/run`. DNS:
  the keeper keeps `/var/lib/exocortex-keeper/resolv.conf` current from
  Moon's DHCP-written one, else Android's `net.dns*`, else 1.1.1.1/8.8.8.8.
  The daemon's socket is on `/data`, the same file in every namespace.
- **Shutdown.** Moon's power menu runs `moon-power.sh`, which sets
  `moon.boot.mode=shutdown` (the keeper and watchdog stand down) and stops
  Exocortex gracefully alongside Moon's services, waiting for it (up to 25 s)
  before `sync` (docs/power.md). An Android shutdown sets `sys.powerctl`; the
  keeper then stops the daemon the same way and exits.
- **Control,** from the chroot (adb, or the agent itself): `exocortexd
  status|restart|stop|start` (through `exocortex-supervisor`, Exocortex's
  `EXOCORTEX_SUPERVISOR`; `stop` lasts until `start` or the next boot), and
  `exocortexd start` also starts a missing keeper. From Android's side:
  `sh /data/adb/exocortex/exocortex-keeper.sh status|start|shutdown`.
  `touch /data/adb/exocortex/disabled` stops everything and keeps it stopped.
- **Logs.** `/var/log/exocortex/` in the chroot (`keeper.log`, `daemon.out`,
  `notify.out`) and the daemon's own `/opt/exocortex/config/runtime/exocortex.log`.

| What dies | What happens |
| --- | --- |
| Moon, Xorg, the compositor, Moon's chroot session | nothing (own cgroup, session and mounts) |
| fallback to Android, `moon.boot.mode=android` | nothing; the daemon keeps running under Android |
| the daemon (crash, OOM, `kill -9`) | restarted by the keeper, with backoff |
| the keeper | restarted by the watchdog within 5 s; the daemon keeps running |
| the watchdog | restarted by the keeper within 2 s |
| both loops at once | the daemon keeps running; `exocortexd start` or the next boot brings the loops back |
| power menu, low-battery shutdown, Android shutdown | graceful stop, turns replay after boot |

### Proposed: an init service (needs the user's approval)

The gold standard is Android's init as the supervisor: init restarts a
service that exits and stops it in its shutdown sequence. That needs an rc in
the BOOT image's `overlay.d` (beside `moon-direct-boot.rc`), so it is not
installed; `scripts/device/exocortex/proposed/exocortex.rc` is the draft.
It adds an ordinary (not oneshot) service running the keeper loop in the
foreground (`exocortex-keeper.sh init`: no watchdog and no cgroup move,
since init is the supervisor and its service cgroup is the right one),
started at `post-fs-data` in every boot mode. On shutdown the keeper stops
the daemon gracefully as now and then idles (an exit would be restarted)
until init's own shutdown terminates the service. `95-exocortex.sh` does
nothing while `init.svc.exocortex` is running. Only overlay files change; the ramdisk
`/init` stays byte-identical (RESCUE.md's hard rule), and it would be flashed
with `flash-boot-inplace.sh` like the direct-boot image, attended. Until
then the keeper pair above does the same job from `/data`.

## The app and notifications

Moon's Exocortex app (`a26-system --app exocortex`, on the home screen) is a
full client of the daemon's socket: conversations and folders, history,
streaming replies with markdown, collapsed tool calls, a model picker, Stop.
`exocortex-notify`, kept by the same keeper, posts a moon-notifyd
notification (app `exocortex`, one per conversation) when a turn you were
waiting on finishes or a goal needs you, unless the app shows that
conversation; tapping it opens the app there. Details: a26-system
`docs/exocortex.md`.

## What the agent can do

Its tools run as root in the chroot, in the daemon's namespace.
- **Linux side:** everything in the chroot; `apk`.
- **Android side:** `android-sh` (`nsenter` into PID 1's mount namespace:
  Android's `/system/bin/sh`, getprop/setprop, `start`/`stop` of init
  services, logcat, `/data/adb/moon`).
- **Moon:** `a26-shellctl`, `moon-notifyctl`, `moon-telephonyctl`,
  `a26-systemctl`, `a26-shellshot`, `moon-power`, with `DISPLAY=:0`.
- **Instructions:** `/opt/exocortex/config/system.md` (from
  `scripts/device/exocortex/system.md`): the phone, its commands and logs,
  where to read (`/opt/src`), and the safety rules: no BOOT, RECOVERY,
  vendor_boot or boot-chain change without the user's explicit confirmation
  in the conversation, RESCUE.md, keep adb and Moon working, never kill its
  own keeper.

Claude Code refuses to bypass permissions as root unless it is told it is
in a deliberate sandbox, so the daemon runs with `IS_SANDBOX=1`: this phone
is the agent's own machine, root by design.

## Limits

- While any conversation is working, exocortex-notify holds Moon's
  `transfer` lease for the `exocortex` app (renewed every 60 s), so the phone
  does not deep-suspend mid-turn with the screen off; it releases it when
  nothing runs. In Android mode there is no lease: Android's own sleep
  applies.
- There is no compiler on the phone: Rust and NDK components are built on
  the user's machines (scripts here), the agent can install what they build.

## Verified on the A26 (2026-10-28, after v1.0)

- Install from scratch with `install.sh --openai-key` (1.5 min) and
  `authorize-whale.sh`: every whale check passed from the phone.
- Real turns: OpenAI (gpt-5.4-mini, API key) and Claude (Sonnet 5.5 through
  whale) ran Moon's `a26-shellctl state`, `android-sh -c getprop …` and read
  the battery, Alpine and Moon's sockets.
- Supervision: `kill -9` of the keeper (back within 5 s), the watchdog, the
  bridge and the daemon (each restarted). Moon killed: the supervisor's local
  recovery restarted Moon's whole chroot session while a 90 s tool call ran;
  it finished all its steps. Moon killed again: the supervisor gave the phone
  to Android (framework up, `moon.boot.mode=android`); the daemon, bridge and
  keeper kept running. `moon-power.sh reboot` from there and then Moon's own
  `system reboot` (the power menu's path): Exocortex stopped before `sync`
  (1.7 s and 3.3 s, rc 0), the keeper came back at boot from `service.d`, and
  the interrupted turns replayed and finished (one was a 40 s command mid-way
  through the second reboot).
- The app: list, chat, typing with Moon's keyboard, streaming markdown,
  Stop; while a turn ran Moon showed the `exocortex` transfer lease with deep
  suspend inhibited. Drag scrolls over a long chat while a reply streamed:
  1,011 frames, total (draw + present) mean 3.45 ms, p95 7.0 ms, 22 over
  8.33 ms (6 full frames, 16 partial). Notifications: a banner when a turn
  finished with the app in the background; tapping it opened the app at that
  conversation, and the notification went away.
