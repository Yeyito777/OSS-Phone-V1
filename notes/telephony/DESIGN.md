# Telephony without the Android framework (design and findings)

Device: SM-A266M (Exynos 1280, Shannon modem), firmware A266MUBU1AYB8,
Android 15 vendor stack, Moon direct-boot mode (zygote and system_server
never start). Goal: SIM status, signal, SMS, voice calls with audio and mobile
data from the Linux side. Protocol: `docs/telephony-protocol.md`.

## 1. What runs in Moon mode (read-only survey, 2026-10-26)

* **Modem boot is independent of the framework.** `cbd` (CP boot daemon,
  `SS310 modem`, SHMEM link) loads the `radio` partition into the CP and
  reaches `Complete normal bootup`; cpif exposes `umts_ipc0`/`umts_boot0`.
  `moon-mode-stop.list` keeps the whole cellular family (cpboot-daemon,
  ril-daemon, multiclient-daemon, imsd, cass, the RIL rc's BCS/DIAG/DR/SMD
  daemons).
* **rild is a HIDL radio HAL, version 1.6.** One `rild` process
  (`libsec-ril.so`, RIL version 15, two SIM slots, `dsds`) serves
  `android.hardware.radio@1.0…1.6::IRadio/slot1` and `/slot2`,
  `android.hardware.radio.config@1.0/1.1/1.3::IRadioConfig/default` and
  `@1.0-1.2::ISap`, plus Samsung's `vendor.samsung.hardware.radio@2.0-2.2::ISehRadio`,
  `radio.bridge@2.0::ISehBridge` (used by multiclientd) and
  `radio.channel@2.0::ISehChannel/imsd`. No AIDL `android.hardware.radio.*`
  service exists on this firmware.
* **Nobody is rild's client.** Every indication logs
  `radioService[0]->mRadioIndication == NULL`. Samsung's RIL powers the
  radio on by itself (`ril.radiostate=10`, RIL_RADIO_STATE_ON) and keeps
  reporting network state and physical channel configs on slot 0, so the
  modem camps without the framework.
* **hwbinder is usable from the chroot.** Moon's processes run as
  `u:r:magisk:s0` (allowed to find and call any hwservice) and the chroot
  has `/dev/hwbinder` (binderfs bind). A musl process therefore can be a HIDL
  client with no Android libraries, as libgbinder/ofono-binder-plugin are on
  Sailfish OS, Droidian and Ubuntu Touch.

## 1a. On the phone (2026-10-26, no SIM inserted)

* `moon-telephonyd selftest` (IBase only, like lshal): hwservicemanager
  `get()` returned rild's IRadio for both slots; `interfaceChain` =
  `@1.6 … @1.0::IRadio, IBase`, `getDebugInfo` = rild's pid. The transport
  worked first time.
* `probe` (query-only client): `setResponseFunctions` accepted on both slots
  and on IRadioConfig; rild sent `rilConnected` and `radioStateChanged(ON)`
  at once. Answers: card **absent** on both slots; slot 1 voice
  registration **REG_DENIED_EM** (emergency calls only) on **UMTS**, cell
  302/610 (Bell) LAC 10500; data not registered; IMS not registered; no
  calls or data calls; baseband `A266MUBU1AYB8`. `getSignalStrength` fails
  with INVALID_RESPONSE without a SIM, and Samsung's IRadioConfig answers
  `getSimSlotsStatus` with REQUEST_NOT_SUPPORTED.
* The daemon itself (run by hand under a scratch root, data disabled) shows
  the same as protocol state (`sim.state` `absent`, `network.registration`
  `denied`, `emergency_only` true, `rat` `umts`) and refuses SMS/PIN
  commands before they reach the modem.
* rild handles a client going away: `RadioImplDeathRecipient::serviceDied`
  and `RadioConfigImplDeathRecipient::serviceDied` in its log when our
  process exits, and a new client registers cleanly.
* **No eSIM.** `iccOpenLogicalChannel` to the ISD-R
  (`A0000005591010FFFFFFFF8900000100`) returns SIM_ABSENT on both slots, the
  firmware declares no `android.hardware.telephony.euicc` feature (only
  Samsung's generic EsimClient app exists), and `ril.simslottype1/2=0`,
  `ril.removableEsim1/2=0`: the SM-A266M (GTO) is dual physical nano-SIM. A
  Freedom line needs a physical SIM in this phone.

## 2. Architecture

```
 Moon (status bar, Phone, Messages)          moon-notifyd
          │ JSON lines                          ▲ notify.post
          ▼ /run/moon-telephony/telephony.sock  │
 ┌──────────────────────── moon-telephonyd (static musl, Android root ns) ─┐
 │ hwbinder client ── IRadio@1.0/slot1,slot2 ──► rild (libsec-ril) ─► CP   │
 │ SMS codec + store (/var/lib/moon-telephony)                             │
 │ exec moon-call-audio (bionic) ─► libaudioclient ─► audioserver ─► HAL   │
 │ exec mobile-data hook ─► ip addr/route/rule on rmnetN                   │
 └─────────────────────────────────────────────────────────────────────────┘
```

* **`moon-telephonyd`** is one Rust process (`src/moon-telephonyd`), a
  single thread blocking in poll(2) on the binder fd, its clients and
  moon-notifyd. It runs in Android's root namespace like moon-audiod, so it
  can exec bionic helpers; its socket is bind-mounted into the chroot.
* **Its own HIDL transport** (`binder.rs`, ~600 lines): the binder kernel
  UAPI and libhwbinder's parcel format (scatter-gather buffer objects for
  `hidl_string`/`hidl_vec`, parent fixups), hwservicemanager `get()`, local
  callback objects answering IBase (`interfaceChain`, `ping`,
  `getDebugInfo`…), death notifications. libgbinder was the reference; a
  pure-Rust implementation keeps the binary static with no glib.
* **Radio client** (`radio.rs`): registers `IRadioResponse`/
  `IRadioIndication` **@1.0** objects for both slots, so rild answers with the
  @1.0 methods and structs (it falls back to them when the client's
  interface chain has no newer version). Requests are one-way with serials;
  `responseAcknowledgement` is sent for `*_ACK_EXP` responses/indications
  (rild holds a wakelock until then).
* **SMS** (`sms.rs`): GSM 7-bit (with the extension table) and UCS-2
  SMS-SUBMIT with concatenation (8-bit reference, surrogate pairs never
  split), SMS-DELIVER and SMS-STATUS-REPORT decoding (alphanumeric senders,
  timestamps). Incoming messages are acknowledged
  (`acknowledgeLastIncomingGsmSms`) only after they are saved; if saving fails
  the ack says "memory capacity exceeded" so the network retries.
* **Calls** follow `getCurrentCalls` after every `callStateChanged`; the
  daemon gives each call a stable id, detects incoming/missed/ended calls and
  posts notifications.
* **Call audio**: Samsung's real audio HAL is the 32-bit
  `audio.primary.s5e8825.so`, which links `libaudio-ril.so`/
  `libsecril-client.so` and drives the CP voice path itself
  (`SetCallAudioPath`, `SetCallVolume`, `SetCallClockSync`,
  `SetRealCallStatus`), keyed on the audio mode. What the framework adds is
  policy, which `moon-call-audio` reproduces through the firmware's
  libaudioclient (resolved with dlopen, so no platform libraries at build
  time): `AudioSystem::setPhoneState(IN_CALL)` (which also calls
  `setMode`), `setForceUse(FOR_COMMUNICATION, SPEAKER|NONE)`,
  `initStreamVolume`/`setStreamVolumeIndex(VOICE_CALL)`,
  `muteMicrophone`, and a `param` passthrough for Samsung's call keys
  (`g_call_state`, `g_call_sim_slot`, `l_call_rat_type`, …) should the HAL
  need them. moon-permissiond already answers audioserver's permission
  checks; root passes MODIFY_AUDIO_SETTINGS.
* **Mobile data**: `setupDataCall` (@1.0, Freedom's
  `internet.freedommobile.ca`, IPV4V6), then the `mobile-data` hook
  configures rmnetN with the Wi-Fi backend's scheme (table 1000+ifindex,
  policy rule 10560 after Wi-Fi's 10550, so Wi-Fi wins when up) and writes the
  chroot's resolv.conf when Wi-Fi is down.

## 2a. eSIM (for eUICC hardware)

Android's eSIM stack is EuiccService (the LPA, here Samsung's EsimClient)
talking SGP.22 ES10 APDUs to the eUICC's ISD-R through the radio HAL's
logical channels (`iccOpenLogicalChannel`/`iccTransmitApduLogicalChannel`).
Nothing in that path needs the framework: **lpac** (open-source SGP.22 LPA,
Alpine package `lpac`) does the same with its HTTP driver for the SM-DP+.
lpac's own `gbinder_hidl` APDU driver would call `setResponseFunctions` and
steal rild's callbacks from moon-telephonyd, so the daemon exposes root-only
`sim.channel_open/apdu/channel_close` and `moon-telephonyctl lpac ARGS…`
bridges lpac's `stdio` APDU driver to them (`lpac chip info`,
`lpac profile download -a 'LPA:1$…'`, `lpac profile enable ICCID`,
`lpac notification process`). Not testable on this phone (no eUICC); on
eUICC hardware the open question is whether the modem allows ISD-R access
from a non-framework client.

## 3. Prior art

* **ofono + ofono-binder-plugin + libgbinder-radio** (mer-hybris; Sailfish
  OS, Droidian, Ubuntu Touch on Halium): exactly this path, a native
  IRadio@1.x client over hwbinder from glibc userspace. Its ofono needs
  Sailfish's fork (slots API) and glib/D-Bus; we only take its protocol
  knowledge.
* **postmarketOS on old Samsung Shannon/XMM phones** used
  libsamsung-ipc/Replicant's samsung-ril, speaking Samsung's IPC to the modem
  directly. On this generation rild owns `umts_ipc0`, the IPC protocol is
  undocumented, and replacing rild would lose the vendor's carrier
  configuration; keeping rild and its HAL is far less work and risk.
* **Prior art in this project**: moon-audiod/moon-permissiond showed that
  the framework's duties toward native services can be replaced by small
  native stand-ins; telephony needs none so far (rild never calls the
  framework).

## 4. The VoLTE problem

Freedom Mobile is shutting 3G down region by region (all regions by
2027-03-01; Ottawa/Quebec, Manitoba and Peterborough already). Without 3G a
phone needs VoLTE for calls **and SMS**; LTE without IMS keeps data only. On
this Samsung the IMS client is the framework app `imsservice` (SIP signalling
in Java) with the vendor `imsd` for media and channels
(`ISehChannel/imsd`). In Moon mode only CS voice (3G or CSFB from LTE) and SMS
over the CS domain/SGs can work. Native VoLTE means writing an IMS UA: IMS
PDN and P-CSCF discovery through setupDataCall, IMS-AKA through
`requestIsimAuthentication`/`requestIccSimAuthentication`, IPsec SAs (xfrm),
SIP REGISTER/INVITE, and the voice media path through Samsung's imsd/CP. That
is a research project of its own; see §6.

## 5. Plan and status

| Stage | What | Status |
| --- | --- | --- |
| 0 | Read-only survey, design, protocol | done |
| 1 | hwbinder transport, `selftest` (IBase only), `probe` (queries) | **verified on the phone** |
| 2 | SIM, network, signal state in Moon's status bar | daemon verified without a SIM (absent, emergency-only, RAT, cell); signal/registration with a SIM pending; UI by the UI agent (branch `telephony-ui`) |
| 3 | SMS receive/send, threads, notifications | built, awaiting phone + user OK for a real SMS |
| 4 | Voice calls, call audio (earpiece, speaker, mute) | built, awaiting phone + user help |
| 5 | Mobile data | built, awaiting phone + OK |
| 6 | Boot integration: start after Moon is ready, stop before the framework | hooks in moon-post-ready.sh, a26-enter-chroot.sh, a26-android-graphics-restore.sh and `install-telephony.sh`; not yet installed |
| — | eSIM | no eUICC on this phone; lpac bridge built for hardware that has one |
| 7 | VoLTE/IMS | research |

## 6. Open questions

* Whether Samsung's rild needs a client's initial requests before it reports
  everything (e.g. `reportStkServiceIsRunning`, `setIndicationFilter`,
  ISehRadio calls the Samsung framework makes).
* Whether the audio HAL needs Samsung's `g_call_state` parameters in
  addition to the IN_CALL mode to open the CP voice path.
* Whether Freedom's network delivers SMS over SGs to a CS-capable LTE device
  without IMS.
