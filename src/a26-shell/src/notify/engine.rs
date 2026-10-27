//! moon-notifyd's state machine: commands, firing, ringing and the missed-event
//! rules of docs/notifications-protocol.md. Pure: time is passed in (Unix
//! seconds) and all I/O (sockets, sound, vibrator, files) is the daemon's.

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::model::*;
use super::schedule::{format_hm, next_fire};

pub const ALARM_RING_LIMIT: i64 = 10 * 60;
pub const TIMER_RING_LIMIT: i64 = 5 * 60;
/// A ringing notification (incoming call) rings at most this long.
pub const CALL_RING_LIMIT: i64 = 2 * 60;
/// The only app whose ringing notifications ring under do-not-disturb.
pub const PHONE_APP: &str = "phone";
/// Something that came due while the daemon was not running still rings if
/// it is less late than this; otherwise it becomes a missed notification.
pub const MISSED_GRACE: i64 = 15 * 60;
const CHIME_SPACING: i64 = 2;
pub const DAEMON_APP: &str = "moon-notifyd";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Alarm,
    Timer,
}

#[derive(Debug, Clone)]
pub struct Ring {
    pub kind: Kind,
    pub id: String,
    pub started_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// Play the notification chime once.
    Chime,
    /// Vibrate once for this many milliseconds.
    Buzz(u32),
    /// A tap haptic (revision 4): one short pulse, which the daemon drops
    /// if the vibrator is busy or one went out moments ago.
    Haptic(u32),
}

/// Haptic pulse lengths (ms). The A26's coin DC motor (2.8 V, fixed) needs
/// tens of milliseconds just to spin up and is faint: on the phone, 8-60 ms
/// taps went unfelt and even 1.5 s was "pretty faint", so these are long.
pub const HAPTIC_SELECTION_MS: u32 = 100;
pub const HAPTIC_LIGHT_MS: u32 = 120;
pub const HAPTIC_MEDIUM_MS: u32 = 150;
pub const HAPTIC_ERROR_MS: u32 = 300;

#[derive(Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
}

fn fail(code: &'static str, message: impl Into<String>) -> Failure {
    Failure {
        code,
        message: message.into(),
    }
}

type Reply = Result<Value, Failure>;

pub struct Engine {
    pub state: State,
    pub ringing: Vec<Ring>,
    /// Set whenever `state` changed and must be saved.
    pub dirty: bool,
    effects: Vec<Effect>,
    last_chime: i64,
    /// Ringing notifications (id, since): in memory only, so a call never
    /// rings again after a restart.
    call_rings: Vec<(String, i64)>,
}

impl Engine {
    /// Loads persisted state and applies §6: re-ring what was ringing, ring
    /// what came due less than MISSED_GRACE ago, report the rest as missed.
    pub fn start(mut state: State, now: i64) -> (Self, Vec<Value>) {
        state.version = 1;
        let mut engine = Self {
            state,
            ringing: Vec::new(),
            dirty: true,
            effects: Vec::new(),
            last_chime: i64::MIN / 2,
            call_rings: Vec::new(),
        };
        for n in &mut engine.state.notifications {
            n.ringing = false;
        }
        let mut events = Vec::new();
        let was_ringing: Vec<(Kind, String)> = engine
            .state
            .alarms
            .iter()
            .filter(|a| a.ringing)
            .map(|a| (Kind::Alarm, a.id.clone()))
            .chain(
                engine
                    .state
                    .timers
                    .iter()
                    .filter(|t| t.state == TimerState::Ringing)
                    .map(|t| (Kind::Timer, t.id.clone())),
            )
            .collect();
        for (kind, id) in was_ringing {
            engine.ringing.push(Ring {
                kind,
                id,
                started_at: now,
            });
        }
        // Due while we were away?
        let late_alarms: Vec<(String, i64)> = engine
            .state
            .alarms
            .iter()
            .filter(|a| !a.ringing)
            .filter_map(|a| {
                alarm_due(a)
                    .filter(|due| *due <= now)
                    .map(|due| (a.id.clone(), due))
            })
            .collect();
        for (id, due) in late_alarms {
            if now - due < MISSED_GRACE {
                events.extend(engine.ring_alarm(&id, now));
            } else {
                events.extend(engine.miss_alarm(&id, due, now));
            }
        }
        let late_timers: Vec<(String, i64)> = engine
            .state
            .timers
            .iter()
            .filter(|t| t.state == TimerState::Running)
            .filter_map(|t| {
                t.ends_at
                    .filter(|end| *end <= now)
                    .map(|end| (t.id.clone(), end))
            })
            .collect();
        for (id, end) in late_timers {
            if now - end < MISSED_GRACE {
                events.extend(engine.ring_timer(&id, now));
            } else {
                events.extend(engine.miss_timer(&id, now));
            }
        }
        engine.recompute_alarms(now);
        events.extend(engine.tick(now));
        (engine, events)
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    // ---- queries -------------------------------------------------------

    pub fn snapshot(&self, now: i64) -> Value {
        json!({
            "event": "snapshot",
            "notifications": self.notifications_newest_first(None),
            "alarms": self.alarms_sorted(),
            "timers": self.state.timers.iter().map(|t| timer_json(t, now)).collect::<Vec<_>>(),
            "ringing": self.ringing.iter().map(|r| json!({
                "kind": match r.kind { Kind::Alarm => "alarm", Kind::Timer => "timer" },
                "id": r.id,
            })).collect::<Vec<_>>(),
            "settings": self.state.settings,
            "now": now,
        })
    }

    /// The next time `tick` has work: a firing, a ring timeout, an expiry.
    pub fn next_due(&self) -> Option<i64> {
        let ring_ends = self.ringing.iter().map(|r| {
            r.started_at
                + match r.kind {
                    Kind::Alarm => ALARM_RING_LIMIT,
                    Kind::Timer => TIMER_RING_LIMIT,
                }
        });
        let expiries = self.state.notifications.iter().filter_map(|n| n.expires_at);
        let call_ends = self
            .call_rings
            .iter()
            .map(|(_, since)| since + CALL_RING_LIMIT);
        self.next_wake()
            .into_iter()
            .chain(ring_ends)
            .chain(expiries)
            .chain(call_ends)
            .min()
    }

    /// The earliest alarm, snooze or running timer: what deep suspend must
    /// wake the phone for (§7).
    pub fn next_wake(&self) -> Option<i64> {
        let alarms = self
            .state
            .alarms
            .iter()
            .filter(|a| !a.ringing)
            .filter_map(alarm_due);
        let timers = self
            .state
            .timers
            .iter()
            .filter(|t| t.state == TimerState::Running)
            .filter_map(|t| t.ends_at);
        alarms.chain(timers).min()
    }

    /// Sound of the oldest ringing item that has one; an incoming call
    /// takes precedence over alarms and timers.
    pub fn ring_sound(&self) -> Option<Sound> {
        if !self.call_rings.is_empty() {
            return Some(Sound::Ringtone);
        }
        self.ringing.iter().find_map(|ring| {
            let sound = match ring.kind {
                Kind::Alarm => self.alarm(&ring.id).map(|a| a.sound),
                Kind::Timer => self.timer(&ring.id).map(|t| t.sound),
            }?;
            (sound != Sound::None).then_some(sound)
        })
    }

    pub fn ring_vibrates(&self) -> bool {
        self.state.settings.vibrate
            && (!self.call_rings.is_empty()
                || self.ringing.iter().any(|ring| match ring.kind {
                    Kind::Alarm => self.alarm(&ring.id).is_some_and(|a| a.vibrate),
                    Kind::Timer => true,
                }))
    }

    // ---- time ----------------------------------------------------------

    /// Fires everything due at `now`; returns the events.
    pub fn tick(&mut self, now: i64) -> Vec<Value> {
        let mut events = Vec::new();
        let due_alarms: Vec<String> = self
            .state
            .alarms
            .iter()
            .filter(|a| !a.ringing && alarm_due(a).is_some_and(|due| due <= now))
            .map(|a| a.id.clone())
            .collect();
        for id in due_alarms {
            events.extend(self.ring_alarm(&id, now));
        }
        let due_timers: Vec<String> = self
            .state
            .timers
            .iter()
            .filter(|t| t.state == TimerState::Running && t.ends_at.is_some_and(|e| e <= now))
            .map(|t| t.id.clone())
            .collect();
        for id in due_timers {
            events.extend(self.ring_timer(&id, now));
        }
        let timed_out: Vec<Ring> = self
            .ringing
            .iter()
            .filter(|r| {
                now - r.started_at
                    >= match r.kind {
                        Kind::Alarm => ALARM_RING_LIMIT,
                        Kind::Timer => TIMER_RING_LIMIT,
                    }
            })
            .cloned()
            .collect();
        for ring in timed_out {
            match ring.kind {
                Kind::Alarm => {
                    events.extend(self.end_alarm(&ring.id, "timeout", now));
                    let fired_at = ring.started_at;
                    events.extend(self.miss_alarm_notice(&ring.id, fired_at, now));
                }
                Kind::Timer => {
                    events.extend(self.end_timer(&ring.id, "timeout", now));
                    events.extend(self.timer_done_notice(&ring.id, now));
                    self.delete_timer(&ring.id);
                    events.push(json!({"event": "timer_deleted", "id": ring.id}));
                }
            }
        }
        let rung_out: Vec<String> = self
            .call_rings
            .iter()
            .filter(|(_, since)| now - since >= CALL_RING_LIMIT)
            .map(|(id, _)| id.clone())
            .collect();
        for id in rung_out {
            events.extend(self.stop_call_ring(&id));
        }
        let expired: Vec<String> = self
            .state
            .notifications
            .iter()
            .filter(|n| n.expires_at.is_some_and(|e| e <= now))
            .map(|n| n.id.clone())
            .collect();
        for id in expired {
            events.extend(self.remove_notification(&id, "expired"));
        }
        events
    }

    /// The wall clock moved (NTP sync, manual set): reschedule alarms.
    pub fn clock_changed(&mut self, now: i64) -> Vec<Value> {
        self.recompute_alarms(now);
        let mut events: Vec<Value> = self
            .alarms_sorted()
            .into_iter()
            .map(|alarm| json!({"event": "alarm_changed", "alarm": alarm}))
            .collect();
        events.extend(self.tick(now));
        events
    }

    // ---- commands ------------------------------------------------------

    /// Handles one request (already parsed; `cmd` and `id` removed from
    /// `args`). `subscribe` is the daemon's; it never reaches here.
    pub fn handle(&mut self, cmd: &str, args: Map<String, Value>, now: i64) -> (Reply, Vec<Value>) {
        let mut events = Vec::new();
        let reply = self.dispatch(cmd, args, now, &mut events);
        (reply, events)
    }

    fn dispatch(
        &mut self,
        cmd: &str,
        args: Map<String, Value>,
        now: i64,
        ev: &mut Vec<Value>,
    ) -> Reply {
        match cmd {
            "ping" => {
                parse::<Empty>(args)?;
                Ok(json!({"version": 1, "revision": 5}))
            }
            "notify.post" => self.post(parse(args)?, now, ev),
            "haptic" => {
                let a: Haptic = parse(args)?;
                let ms = match a.style.as_str() {
                    "selection" => HAPTIC_SELECTION_MS,
                    "light" => HAPTIC_LIGHT_MS,
                    "medium" => HAPTIC_MEDIUM_MS,
                    "error" => HAPTIC_ERROR_MS,
                    _ => {
                        return Err(fail(
                            "invalid_argument",
                            "style is selection, light, medium or error",
                        ));
                    }
                };
                let ms = match a.ms {
                    None => ms,
                    Some(ms @ 5..=400) => ms,
                    Some(_) => return Err(fail("invalid_argument", "ms is 5..400")),
                };
                // Stateless: never while anything rings (whatever its
                // vibrate setting), and the daemon drops it if the
                // vibrator is busy. Not governed by `vibrate` (alerts) or
                // do-not-disturb: Moon's own `haptics` setting decides.
                if self.ringing.is_empty() && self.call_rings.is_empty() {
                    self.effects.push(Effect::Haptic(ms));
                }
                Ok(json!({}))
            }
            "notify.withdraw" => {
                let a: Withdraw = parse(args)?;
                let id = match (a.notification, a.app, a.tag) {
                    (Some(id), None, None) => id,
                    (None, Some(app), Some(tag)) => self
                        .state
                        .notifications
                        .iter()
                        .find(|n| n.app == app && n.tag.as_deref() == Some(tag.as_str()))
                        .map(|n| n.id.clone())
                        .ok_or_else(|| fail("not_found", format!("no notification {app}/{tag}")))?,
                    _ => {
                        return Err(fail(
                            "invalid_argument",
                            "give notification, or app and tag",
                        ));
                    }
                };
                ev.extend(self.remove_existing(&id, "withdrawn")?);
                Ok(json!({}))
            }
            "notify.list" => {
                let a: AppFilter = parse(args)?;
                Ok(json!({"notifications": self.notifications_newest_first(a.app.as_deref())}))
            }
            "notify.dismiss" => {
                let a: NotificationArg = parse(args)?;
                ev.extend(self.remove_existing(&a.notification, "dismissed")?);
                Ok(json!({}))
            }
            "notify.dismiss_all" => {
                let a: AppFilter = parse(args)?;
                let ids: Vec<String> = self
                    .state
                    .notifications
                    .iter()
                    .filter(|n| a.app.as_deref().is_none_or(|app| n.app == app))
                    .map(|n| n.id.clone())
                    .collect();
                for id in ids {
                    ev.extend(self.remove_notification(&id, "dismissed"));
                }
                Ok(json!({}))
            }
            "notify.activate" => {
                let a: NotificationArg = parse(args)?;
                let n = self.notification(&a.notification).cloned().ok_or_else(|| {
                    fail("not_found", format!("no notification {}", a.notification))
                })?;
                ev.extend(self.remove_notification(&a.notification, "activated"));
                ev.push(json!({
                    "event": "notification_activated",
                    "id": n.id, "app": n.app, "tag": n.tag, "open": n.open,
                }));
                Ok(json!({}))
            }
            "notify.mark_seen" => {
                let a: MarkSeen = parse(args)?;
                let all = a.all.unwrap_or(false);
                if all == a.ids.is_some() {
                    return Err(fail("invalid_argument", "give ids, or all: true"));
                }
                let wanted = a.ids.unwrap_or_default();
                let mut marked = Vec::new();
                for n in &mut self.state.notifications {
                    if !n.seen && (all || wanted.contains(&n.id)) {
                        n.seen = true;
                        marked.push(n.id.clone());
                    }
                }
                if !marked.is_empty() {
                    self.dirty = true;
                    ev.push(json!({"event": "notifications_seen", "ids": marked}));
                }
                Ok(json!({}))
            }
            "alarm.list" => {
                parse::<Empty>(args)?;
                Ok(json!({"alarms": self.alarms_sorted()}))
            }
            "alarm.create" => self.alarm_create(parse(args)?, now, ev),
            "alarm.update" => self.alarm_update(parse(args)?, now, ev),
            "alarm.delete" => {
                let id = parse::<AlarmArg>(args)?.alarm;
                self.alarm(&id)
                    .ok_or_else(|| fail("not_found", format!("no alarm {id}")))?;
                if self.is_ringing(Kind::Alarm, &id) {
                    ev.extend(self.end_alarm(&id, "stopped", now));
                }
                self.state.alarms.retain(|x| x.id != id);
                self.dirty = true;
                ev.push(json!({"event": "alarm_deleted", "id": id}));
                Ok(json!({}))
            }
            "alarm.stop" | "alarm.snooze" => {
                let id = parse::<AlarmArg>(args)?.alarm;
                if !self.is_ringing(Kind::Alarm, &id) {
                    return Err(fail("not_found", format!("alarm {} is not ringing", id)));
                }
                if cmd == "alarm.snooze" {
                    let minutes = self.alarm(&id).map_or(9, |x| x.snooze_minutes);
                    ev.extend(self.end_alarm(&id, "snoozed", now));
                    if let Some(alarm) = self.alarm_mut(&id) {
                        // Independent of `enabled`: a snoozed one-shot rings
                        // once more and stays disabled.
                        alarm.snoozed_until = Some(now + i64::from(minutes) * 60);
                    }
                } else {
                    ev.extend(self.end_alarm(&id, "stopped", now));
                }
                self.recompute_alarms(now);
                let alarm = self.alarm(&id).cloned();
                ev.push(json!({"event": "alarm_changed", "alarm": alarm}));
                Ok(json!(alarm))
            }
            "timer.list" => {
                parse::<Empty>(args)?;
                Ok(
                    json!({"timers": self.state.timers.iter().map(|t| timer_json(t, now)).collect::<Vec<_>>()}),
                )
            }
            "timer.create" => {
                let a: TimerCreate = parse(args)?;
                if !(1..=MAX_TIMER_SECONDS).contains(&a.duration_s) {
                    return Err(fail("invalid_argument", "duration_s must be 1..86400"));
                }
                if self.state.timers.len() >= MAX_TIMERS {
                    return Err(fail("limit_exceeded", "too many timers"));
                }
                let label = optional_label(a.label)?;
                let timer = Timer {
                    id: self.state.new_id('t'),
                    label,
                    duration_s: a.duration_s,
                    state: TimerState::Running,
                    ends_at: Some(now + a.duration_s),
                    remaining_s: a.duration_s,
                    sound: a.sound.unwrap_or(Sound::Chimes),
                };
                self.state.timers.push(timer.clone());
                self.dirty = true;
                let value = timer_json(&timer, now);
                ev.push(json!({"event": "timer_changed", "timer": value}));
                Ok(value)
            }
            "timer.pause" | "timer.resume" | "timer.restart" => {
                let id = parse::<TimerArg>(args)?.timer;
                let ringing = self.is_ringing(Kind::Timer, &id);
                let timer = self
                    .timer(&id)
                    .cloned()
                    .ok_or_else(|| fail("not_found", format!("no timer {}", id)))?;
                match (cmd, timer.state) {
                    ("timer.pause", TimerState::Running) => {
                        let left = timer
                            .ends_at
                            .map_or(timer.remaining_s, |e| (e - now).max(0));
                        let t = self.timer_mut(&id).unwrap();
                        t.state = TimerState::Paused;
                        t.ends_at = None;
                        t.remaining_s = left;
                    }
                    ("timer.resume", TimerState::Paused) => {
                        let t = self.timer_mut(&id).unwrap();
                        t.state = TimerState::Running;
                        t.ends_at = Some(now + t.remaining_s);
                    }
                    ("timer.restart", _) => {
                        if ringing {
                            ev.extend(self.end_timer(&id, "restarted", now));
                        }
                        let t = self.timer_mut(&id).unwrap();
                        t.state = TimerState::Running;
                        t.ends_at = Some(now + t.duration_s);
                        t.remaining_s = t.duration_s;
                    }
                    _ => {
                        return Err(fail(
                            "invalid_argument",
                            format!("timer {} cannot {cmd} now", id),
                        ));
                    }
                }
                self.dirty = true;
                let value = timer_json(self.timer(&id).unwrap(), now);
                ev.push(json!({"event": "timer_changed", "timer": value}));
                Ok(value)
            }
            "timer.cancel" | "timer.stop" => {
                let id = parse::<TimerArg>(args)?.timer;
                self.timer(&id)
                    .ok_or_else(|| fail("not_found", format!("no timer {}", id)))?;
                let ringing = self.is_ringing(Kind::Timer, &id);
                if cmd == "timer.stop" && !ringing {
                    return Err(fail("not_found", format!("timer {} is not ringing", id)));
                }
                if ringing {
                    ev.extend(self.end_timer(&id, "stopped", now));
                }
                self.delete_timer(&id);
                ev.push(json!({"event": "timer_deleted", "id": id}));
                Ok(json!({}))
            }
            "settings.get" => {
                parse::<Empty>(args)?;
                Ok(json!(self.state.settings))
            }
            "settings.set" => {
                let a: SettingsSet = parse(args)?;
                let s = &mut self.state.settings;
                if let Some(v) = a.alert_volume {
                    if v > 100 {
                        return Err(fail("invalid_argument", "alert_volume is 0..100"));
                    }
                    s.alert_volume = v;
                }
                if let Some(v) = a.vibrate {
                    s.vibrate = v;
                }
                if let Some(v) = a.notification_sounds {
                    s.notification_sounds = v;
                }
                if let Some(v) = a.do_not_disturb {
                    s.do_not_disturb = v;
                }
                self.dirty = true;
                ev.push(json!({"event": "settings_changed", "settings": self.state.settings}));
                Ok(json!(self.state.settings))
            }
            _ => Err(fail("unknown_command", format!("unknown command {cmd}"))),
        }
    }

    fn post(&mut self, a: Post, now: i64, ev: &mut Vec<Value>) -> Reply {
        if !valid_app(&a.app) {
            return Err(fail("invalid_argument", "app must be [a-z0-9-]{1,32}"));
        }
        if a.tag.as_deref().is_some_and(|t| !valid_tag(t)) {
            return Err(fail("invalid_argument", "tag must be [A-Za-z0-9._-]{1,64}"));
        }
        let title = clean_text(&a.title, 120)
            .ok_or_else(|| fail("invalid_argument", "title is 1..120 chars"))?;
        let body = match a.body {
            Some(b) if b.chars().count() > 1000 => {
                return Err(fail("invalid_argument", "body is at most 1000 chars"));
            }
            Some(b) => clean_text(&b, 1000),
            None => None,
        };
        if let Some(open) = &a.open
            && (!valid_app(&open.app) || open.target.as_deref().is_some_and(|t| t.len() > 256))
        {
            return Err(fail(
                "invalid_argument",
                "open.app must be an app id, open.target ≤ 256 bytes",
            ));
        }
        if a.expires_at.is_some_and(|e| e <= now) {
            return Err(fail("invalid_argument", "expires_at is in the past"));
        }
        let urgency = a.urgency.unwrap_or(Urgency::Normal);
        let existing = a.tag.as_ref().and_then(|tag| {
            self.state
                .notifications
                .iter()
                .position(|n| n.app == a.app && n.tag.as_ref() == Some(tag))
        });
        let replaced = existing.is_some();
        let id = match existing {
            Some(index) => self.state.notifications.remove(index).id,
            None => {
                self.make_room(&a.app, ev)?;
                self.state.new_id('n')
            }
        };
        self.call_rings.retain(|(ring, _)| *ring != id);
        let n = Notification {
            id: id.clone(),
            app: a.app,
            tag: a.tag,
            title,
            body,
            urgency,
            silent: a.silent.unwrap_or(false),
            posted_at: now,
            expires_at: a.expires_at,
            seen: false,
            open: a.open,
            ringing: a.ringing.unwrap_or(false),
        };
        ev.extend(self.add_notification(n, replaced, now));
        Ok(json!({"id": id, "replaced": replaced}))
    }

    /// Enforces the per-app and total limits by evicting the oldest low or
    /// normal notifications (§2).
    fn make_room(&mut self, app: &str, ev: &mut Vec<Value>) -> Result<(), Failure> {
        let per_app = self
            .state
            .notifications
            .iter()
            .filter(|n| n.app == app)
            .count();
        if per_app >= MAX_NOTIFICATIONS_PER_APP {
            let victim = self
                .state
                .notifications
                .iter()
                .find(|n| n.app == app && n.urgency != Urgency::High)
                .map(|n| n.id.clone())
                .ok_or_else(|| {
                    fail(
                        "limit_exceeded",
                        "too many high-urgency notifications from this app",
                    )
                })?;
            ev.extend(self.remove_notification(&victim, "evicted"));
        }
        if self.state.notifications.len() >= MAX_NOTIFICATIONS {
            let victim = self
                .state
                .notifications
                .iter()
                .find(|n| n.urgency != Urgency::High)
                .map(|n| n.id.clone())
                .ok_or_else(|| fail("limit_exceeded", "too many notifications"))?;
            ev.extend(self.remove_notification(&victim, "evicted"));
        }
        Ok(())
    }

    fn add_notification(&mut self, mut n: Notification, replaced: bool, now: i64) -> Vec<Value> {
        let settings = self.state.settings.clone();
        // Ringing (an incoming call) loops the ringtone instead of a chime.
        // Do-not-disturb silences it, except for the Phone app.
        if n.ringing && settings.do_not_disturb && n.app != PHONE_APP {
            n.ringing = false;
        }
        let audible =
            !n.ringing && n.urgency != Urgency::Low && !n.silent && !settings.do_not_disturb;
        if audible && now - self.last_chime >= CHIME_SPACING {
            self.last_chime = now;
            if settings.notification_sounds {
                self.effects.push(Effect::Chime);
            }
            if settings.vibrate {
                // At least 300 ms: shorter is hard to feel on the A26's motor.
                self.effects.push(Effect::Buzz(300));
            }
        }
        let wake = n.ringing || (n.urgency == Urgency::High && !settings.do_not_disturb);
        if n.ringing {
            self.call_rings.push((n.id.clone(), now));
        }
        let event = json!({"event": "notification_posted", "notification": n, "replaced": replaced, "wake": wake});
        self.state.notifications.push(n);
        self.dirty = true;
        vec![event]
    }

    fn remove_existing(&mut self, id: &str, reason: &str) -> Result<Vec<Value>, Failure> {
        if self.notification(id).is_none() {
            return Err(fail("not_found", format!("no notification {id}")));
        }
        Ok(self.remove_notification(id, reason))
    }

    /// The call rang for CALL_RING_LIMIT: stop ringing, keep the
    /// notification (re-posted in place with `ringing: false`).
    fn stop_call_ring(&mut self, id: &str) -> Vec<Value> {
        self.call_rings.retain(|(ring, _)| ring != id);
        let Some(n) = self.state.notifications.iter_mut().find(|n| n.id == id) else {
            return Vec::new();
        };
        n.ringing = false;
        self.dirty = true;
        vec![
            json!({"event": "notification_posted", "notification": n.clone(), "replaced": true, "wake": false}),
        ]
    }

    fn remove_notification(&mut self, id: &str, reason: &str) -> Vec<Value> {
        self.call_rings.retain(|(ring, _)| ring != id);
        let before = self.state.notifications.len();
        self.state.notifications.retain(|n| n.id != id);
        if self.state.notifications.len() == before {
            return Vec::new();
        }
        self.dirty = true;
        vec![json!({"event": "notification_removed", "id": id, "reason": reason})]
    }

    fn alarm_create(&mut self, a: AlarmFields, now: i64, ev: &mut Vec<Value>) -> Reply {
        if self.state.alarms.len() >= MAX_ALARMS {
            return Err(fail("limit_exceeded", "too many alarms"));
        }
        if a.alarm.is_some() {
            return Err(fail("invalid_argument", "alarm.create takes no alarm"));
        }
        let (Some(hour), Some(minute)) = (a.hour, a.minute) else {
            return Err(fail("invalid_argument", "hour and minute are required"));
        };
        let mut alarm = Alarm {
            id: self.state.new_id('a'),
            hour,
            minute,
            days: Vec::new(),
            label: String::new(),
            enabled: true,
            sound: Sound::Radar,
            vibrate: true,
            snooze_minutes: 9,
            next_fire_at: None,
            snoozed_until: None,
            ringing: false,
        };
        apply_alarm_fields(&mut alarm, a)?;
        self.state.alarms.push(alarm.clone());
        self.dirty = true;
        self.recompute_alarms(now);
        let alarm = self.alarm(&alarm.id).cloned();
        ev.push(json!({"event": "alarm_changed", "alarm": alarm}));
        Ok(json!(alarm))
    }

    fn alarm_update(&mut self, a: AlarmFields, now: i64, ev: &mut Vec<Value>) -> Reply {
        let id = a
            .alarm
            .clone()
            .ok_or_else(|| fail("invalid_argument", "alarm is required"))?;
        let mut alarm = self
            .alarm(&id)
            .cloned()
            .ok_or_else(|| fail("not_found", format!("no alarm {id}")))?;
        apply_alarm_fields(&mut alarm, a)?;
        alarm.snoozed_until = None;
        *self.alarm_mut(&id).unwrap() = alarm;
        self.dirty = true;
        self.recompute_alarms(now);
        let alarm = self.alarm(&id).cloned();
        ev.push(json!({"event": "alarm_changed", "alarm": alarm}));
        Ok(json!(alarm))
    }

    // ---- ringing -------------------------------------------------------

    fn ring_alarm(&mut self, id: &str, now: i64) -> Vec<Value> {
        let Some(alarm) = self.alarm_mut(id) else {
            return Vec::new();
        };
        alarm.ringing = true;
        alarm.snoozed_until = None;
        if alarm.days.is_empty() {
            alarm.enabled = false;
        }
        self.recompute_alarms(now);
        self.ringing.push(Ring {
            kind: Kind::Alarm,
            id: id.to_owned(),
            started_at: now,
        });
        self.dirty = true;
        vec![json!({"event": "alarm_ringing", "alarm": self.alarm(id)})]
    }

    fn end_alarm(&mut self, id: &str, reason: &str, now: i64) -> Vec<Value> {
        self.ringing
            .retain(|r| !(r.kind == Kind::Alarm && r.id == id));
        if let Some(alarm) = self.alarm_mut(id) {
            alarm.ringing = false;
        }
        self.recompute_alarms(now);
        self.dirty = true;
        vec![json!({"event": "alarm_ended", "id": id, "reason": reason})]
    }

    /// Came due long ago (daemon down): notify and move to the next time.
    fn miss_alarm(&mut self, id: &str, due: i64, now: i64) -> Vec<Value> {
        if let Some(alarm) = self.alarm_mut(id) {
            alarm.snoozed_until = None;
            if alarm.days.is_empty() {
                alarm.enabled = false;
            }
        }
        self.recompute_alarms(now);
        self.dirty = true;
        let mut events = self.miss_alarm_notice(id, due, now);
        events.push(json!({"event": "alarm_changed", "alarm": self.alarm(id)}));
        events
    }

    fn miss_alarm_notice(&mut self, id: &str, due: i64, now: i64) -> Vec<Value> {
        let Some(alarm) = self.alarm(id).cloned() else {
            return Vec::new();
        };
        let body = if alarm.label.is_empty() {
            format_hm(due)
        } else {
            format!("{} {}", format_hm(due), alarm.label)
        };
        self.daemon_notice(&format!("missed-{id}"), "Missed alarm", &body, now)
    }

    fn ring_timer(&mut self, id: &str, now: i64) -> Vec<Value> {
        let Some(timer) = self.timer_mut(id) else {
            return Vec::new();
        };
        timer.state = TimerState::Ringing;
        timer.ends_at = None;
        timer.remaining_s = 0;
        self.ringing.push(Ring {
            kind: Kind::Timer,
            id: id.to_owned(),
            started_at: now,
        });
        self.dirty = true;
        vec![json!({"event": "timer_ringing", "timer": timer_json(self.timer(id).unwrap(), now)})]
    }

    fn end_timer(&mut self, id: &str, reason: &str, _now: i64) -> Vec<Value> {
        self.ringing
            .retain(|r| !(r.kind == Kind::Timer && r.id == id));
        self.dirty = true;
        vec![json!({"event": "timer_ended", "id": id, "reason": reason})]
    }

    fn miss_timer(&mut self, id: &str, now: i64) -> Vec<Value> {
        let mut events = self.timer_done_notice(id, now);
        self.delete_timer(id);
        events.push(json!({"event": "timer_deleted", "id": id}));
        events
    }

    fn timer_done_notice(&mut self, id: &str, now: i64) -> Vec<Value> {
        let Some(timer) = self.timer(id).cloned() else {
            return Vec::new();
        };
        let body = if timer.label.is_empty() {
            "Timer done".to_owned()
        } else {
            timer.label
        };
        self.daemon_notice(&format!("timer-{id}"), "Timer done", &body, now)
    }

    fn daemon_notice(&mut self, tag: &str, title: &str, body: &str, now: i64) -> Vec<Value> {
        let existing = self
            .state
            .notifications
            .iter()
            .position(|n| n.app == DAEMON_APP && n.tag.as_deref() == Some(tag));
        let replaced = existing.is_some();
        let id = match existing {
            Some(index) => self.state.notifications.remove(index).id,
            None => self.state.new_id('n'),
        };
        let n = Notification {
            id,
            app: DAEMON_APP.to_owned(),
            tag: Some(tag.to_owned()),
            title: title.to_owned(),
            body: Some(body.to_owned()),
            urgency: Urgency::High,
            silent: true,
            posted_at: now,
            expires_at: None,
            seen: false,
            open: None,
            ringing: false,
        };
        self.add_notification(n, replaced, now)
    }

    // ---- helpers -------------------------------------------------------

    fn recompute_alarms(&mut self, now: i64) {
        for alarm in &mut self.state.alarms {
            let next = (alarm.enabled)
                .then(|| next_fire(alarm.hour, alarm.minute, &alarm.days, now))
                .flatten();
            if alarm.next_fire_at != next {
                alarm.next_fire_at = next;
                self.dirty = true;
            }
        }
    }

    fn is_ringing(&self, kind: Kind, id: &str) -> bool {
        self.ringing.iter().any(|r| r.kind == kind && r.id == id)
    }

    fn delete_timer(&mut self, id: &str) {
        self.state.timers.retain(|t| t.id != id);
        self.dirty = true;
    }

    fn notification(&self, id: &str) -> Option<&Notification> {
        self.state.notifications.iter().find(|n| n.id == id)
    }

    fn alarm(&self, id: &str) -> Option<&Alarm> {
        self.state.alarms.iter().find(|a| a.id == id)
    }

    fn alarm_mut(&mut self, id: &str) -> Option<&mut Alarm> {
        self.state.alarms.iter_mut().find(|a| a.id == id)
    }

    fn timer(&self, id: &str) -> Option<&Timer> {
        self.state.timers.iter().find(|t| t.id == id)
    }

    fn timer_mut(&mut self, id: &str) -> Option<&mut Timer> {
        self.state.timers.iter_mut().find(|t| t.id == id)
    }

    fn notifications_newest_first(&self, app: Option<&str>) -> Vec<Notification> {
        self.state
            .notifications
            .iter()
            .rev()
            .filter(|n| app.is_none_or(|a| n.app == a))
            .cloned()
            .collect()
    }

    fn alarms_sorted(&self) -> Vec<Alarm> {
        let mut alarms = self.state.alarms.clone();
        alarms.sort_by_key(|a| (a.next_fire_at.is_none(), a.next_fire_at, a.hour, a.minute));
        alarms
    }
}

/// When an alarm next needs to ring: its snooze, or its regular time if
/// enabled (`next_fire_at` is null when disabled).
fn alarm_due(alarm: &Alarm) -> Option<i64> {
    match (alarm.snoozed_until, alarm.next_fire_at) {
        (Some(s), Some(n)) => Some(s.min(n)),
        (s, n) => s.or(n),
    }
}

pub fn timer_json(timer: &Timer, now: i64) -> Value {
    let mut timer = timer.clone();
    if let (TimerState::Running, Some(end)) = (timer.state, timer.ends_at) {
        timer.remaining_s = (end - now).max(0);
    }
    json!(timer)
}

fn apply_alarm_fields(alarm: &mut Alarm, a: AlarmFields) -> Result<(), Failure> {
    if let Some(hour) = a.hour {
        if hour > 23 {
            return Err(fail("invalid_argument", "hour is 0..23"));
        }
        alarm.hour = hour;
    }
    if let Some(minute) = a.minute {
        if minute > 59 {
            return Err(fail("invalid_argument", "minute is 0..59"));
        }
        alarm.minute = minute;
    }
    if let Some(days) = a.days {
        alarm.days = clean_days(&days).ok_or_else(|| fail("invalid_argument", "days are 0..6"))?;
    }
    if a.label.is_some() {
        alarm.label = optional_label(a.label)?;
    }
    if let Some(enabled) = a.enabled {
        alarm.enabled = enabled;
    }
    if let Some(sound) = a.sound {
        alarm.sound = sound;
    }
    if let Some(vibrate) = a.vibrate {
        alarm.vibrate = vibrate;
    }
    if let Some(minutes) = a.snooze_minutes {
        if !(1..=30).contains(&minutes) {
            return Err(fail("invalid_argument", "snooze_minutes is 1..30"));
        }
        alarm.snooze_minutes = minutes;
    }
    Ok(())
}

fn optional_label(label: Option<String>) -> Result<String, Failure> {
    match label {
        None => Ok(String::new()),
        Some(text) if text.trim().is_empty() => Ok(String::new()),
        Some(text) => clean_text(&text, 40)
            .ok_or_else(|| fail("invalid_argument", "label is at most 40 chars")),
    }
}

fn parse<T: DeserializeOwned>(args: Map<String, Value>) -> Result<T, Failure> {
    serde_json::from_value(Value::Object(args)).map_err(|e| fail("invalid_argument", e.to_string()))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NotificationArg {
    notification: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlarmArg {
    alarm: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimerArg {
    timer: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppFilter {
    app: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Haptic {
    style: String,
    /// The pulse length in ms (5-400), overriding the style's default:
    /// Moon's tuned lengths (revision 5).
    ms: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Withdraw {
    notification: Option<String>,
    app: Option<String>,
    tag: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarkSeen {
    ids: Option<Vec<String>>,
    all: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Post {
    app: String,
    title: String,
    tag: Option<String>,
    body: Option<String>,
    urgency: Option<Urgency>,
    silent: Option<bool>,
    expires_at: Option<i64>,
    open: Option<Open>,
    ringing: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AlarmFields {
    alarm: Option<String>,
    hour: Option<u8>,
    minute: Option<u8>,
    days: Option<Vec<u8>>,
    label: Option<String>,
    enabled: Option<bool>,
    sound: Option<Sound>,
    vibrate: Option<bool>,
    snooze_minutes: Option<u8>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimerCreate {
    duration_s: i64,
    label: Option<String>,
    sound: Option<Sound>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsSet {
    alert_volume: Option<u8>,
    vibrate: Option<bool>,
    notification_sounds: Option<bool>,
    do_not_disturb: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::schedule::tests::with_tz;

    const T0: i64 = 1_793_000_000; // 2026-10-26, a Monday
    const TZ: &str = "UTC0";

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    fn run(engine: &mut Engine, cmd: &str, value: Value, now: i64) -> (Value, Vec<Value>) {
        let (reply, events) = engine.handle(cmd, args(value), now);
        (
            reply.unwrap_or_else(|f| panic!("{cmd}: {} {}", f.code, f.message)),
            events,
        )
    }

    fn error(engine: &mut Engine, cmd: &str, value: Value, now: i64) -> &'static str {
        engine.handle(cmd, args(value), now).0.unwrap_err().code
    }

    fn names(events: &[Value]) -> Vec<String> {
        events
            .iter()
            .map(|e| e["event"].as_str().unwrap().to_owned())
            .collect()
    }

    fn fresh() -> Engine {
        Engine::start(State::default(), T0).0
    }

    #[test]
    fn notifications_post_replace_withdraw_and_chime() {
        with_tz(TZ, || {
            let mut e = fresh();
            let (r, ev) = run(
                &mut e,
                "notify.post",
                json!({"app":"system","title":" Wi-Fi ","tag":"wifi"}),
                T0,
            );
            assert_eq!(r["replaced"], false);
            assert_eq!(names(&ev), ["notification_posted"]);
            assert_eq!(ev[0]["notification"]["title"], "Wi-Fi");
            assert_eq!(ev[0]["wake"], false);
            assert_eq!(e.take_effects(), [Effect::Chime, Effect::Buzz(300)]);
            let id = r["id"].as_str().unwrap().to_owned();
            // Same tag replaces in place, keeps the id; chime rate limited.
            let (r2, _) = run(
                &mut e,
                "notify.post",
                json!({"app":"system","title":"Wi-Fi off","tag":"wifi","urgency":"high"}),
                T0 + 1,
            );
            assert_eq!(r2["id"], id.as_str());
            assert_eq!(r2["replaced"], true);
            assert!(e.take_effects().is_empty());
            run(
                &mut e,
                "notify.post",
                json!({"app":"browser","title":"Download","urgency":"low"}),
                T0 + 5,
            );
            assert!(e.take_effects().is_empty(), "low is silent");
            let (list, _) = run(&mut e, "notify.list", json!({}), T0 + 5);
            assert_eq!(list["notifications"][0]["app"], "browser");
            assert_eq!(list["notifications"].as_array().unwrap().len(), 2);
            let (_, ev) = run(
                &mut e,
                "notify.withdraw",
                json!({"app":"system","tag":"wifi"}),
                T0 + 6,
            );
            assert_eq!(ev[0]["reason"], "withdrawn");
            assert_eq!(
                error(
                    &mut e,
                    "notify.withdraw",
                    json!({"notification": id}),
                    T0 + 6
                ),
                "not_found"
            );
            assert_eq!(
                error(
                    &mut e,
                    "notify.post",
                    json!({"app":"System","title":"x"}),
                    T0
                ),
                "invalid_argument"
            );
            assert_eq!(
                error(
                    &mut e,
                    "notify.post",
                    json!({"app":"system","title":"x","colour":1}),
                    T0
                ),
                "invalid_argument"
            );
            assert_eq!(error(&mut e, "nope", json!({}), T0), "unknown_command");
        });
    }

    #[test]
    fn do_not_disturb_activate_and_seen() {
        with_tz(TZ, || {
            let mut e = fresh();
            run(&mut e, "settings.set", json!({"do_not_disturb": true}), T0);
            let (r, ev) = run(
                &mut e,
                "notify.post",
                json!({"app":"system","title":"x","urgency":"high","open":{"app":"system","target":"wifi"}}),
                T0,
            );
            assert_eq!(ev[0]["wake"], false);
            assert!(e.take_effects().is_empty());
            let id = r["id"].as_str().unwrap().to_owned();
            let (_, ev) = run(&mut e, "notify.mark_seen", json!({"all": true}), T0);
            assert_eq!(ev[0]["ids"][0], id.as_str());
            let (_, ev) = run(&mut e, "notify.activate", json!({"notification": id}), T0);
            assert_eq!(
                names(&ev),
                ["notification_removed", "notification_activated"]
            );
            assert_eq!(ev[1]["open"]["target"], "wifi");
        });
    }

    #[test]
    fn ringing_call_loops_until_removed_and_beats_alarms() {
        with_tz(TZ, || {
            let mut e = fresh();
            let (r, ev) = run(
                &mut e,
                "notify.post",
                json!({"app":"phone","title":"Alice","tag":"call","urgency":"high","ringing":true}),
                T0,
            );
            assert_eq!(ev[0]["notification"]["ringing"], true);
            assert_eq!(ev[0]["wake"], true);
            assert!(e.take_effects().is_empty(), "ringtone, not a chime");
            assert_eq!(e.ring_sound(), Some(Sound::Ringtone));
            assert!(e.ring_vibrates());
            assert_eq!(e.next_due(), Some(T0 + CALL_RING_LIMIT));
            // Answered: the same tag re-posted without ringing stops it.
            run(
                &mut e,
                "notify.post",
                json!({"app":"phone","title":"Alice","tag":"call","body":"0:01"}),
                T0 + 5,
            );
            assert_eq!(e.ring_sound(), None);
            // A new call while an alarm rings: the ringtone wins.
            let (alarm, _) = run(&mut e, "alarm.create", json!({"hour":10,"minute":0}), T0);
            let due = alarm["next_fire_at"].as_i64().unwrap();
            e.tick(due);
            assert_eq!(e.ring_sound(), Some(Sound::Radar));
            let (call, _) = run(
                &mut e,
                "notify.post",
                json!({"app":"phone","title":"Bob","ringing":true}),
                due + 1,
            );
            assert_eq!(e.ring_sound(), Some(Sound::Ringtone));
            let id = call["id"].as_str().unwrap().to_owned();
            let snapshot = e.snapshot(due + 2);
            // Calls are not in the alarm/timer `ringing` list (older Moons
            // reject unknown kinds); the notification carries `ringing`.
            assert_eq!(snapshot["ringing"].as_array().unwrap().len(), 1);
            assert!(
                snapshot["notifications"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|n| n["ringing"] == true)
            );
            run(
                &mut e,
                "notify.dismiss",
                json!({"notification": id}),
                due + 3,
            );
            assert_eq!(e.ring_sound(), Some(Sound::Radar));
            let _ = r;
        });
    }

    #[test]
    fn ringing_stops_after_the_limit_and_respects_dnd() {
        with_tz(TZ, || {
            let mut e = fresh();
            run(
                &mut e,
                "notify.post",
                json!({"app":"phone","title":"Alice","ringing":true}),
                T0,
            );
            let ev = e.tick(T0 + CALL_RING_LIMIT);
            assert_eq!(ev[0]["notification"]["ringing"], false);
            assert_eq!(ev[0]["replaced"], true);
            assert_eq!(e.ring_sound(), None);
            assert_eq!(e.state.notifications.len(), 1, "the missed call stays");
            run(&mut e, "settings.set", json!({"do_not_disturb": true}), T0);
            let (_, ev) = run(
                &mut e,
                "notify.post",
                json!({"app":"messages","title":"x","ringing":true}),
                T0,
            );
            assert_eq!(ev[0]["notification"]["ringing"], false);
            assert_eq!(e.ring_sound(), None);
            let (_, ev) = run(
                &mut e,
                "notify.post",
                json!({"app":"phone","title":"y","ringing":true}),
                T0,
            );
            assert_eq!(ev[0]["notification"]["ringing"], true);
            assert_eq!(e.ring_sound(), Some(Sound::Ringtone));
            // A restart never rings an old call again.
            let (e2, _) = Engine::start(e.state.clone(), T0 + 10);
            assert_eq!(e2.ring_sound(), None);
            assert!(e2.state.notifications.iter().all(|n| !n.ringing));
        });
    }

    #[test]
    fn eviction_and_expiry() {
        with_tz(TZ, || {
            let mut e = fresh();
            for i in 0..MAX_NOTIFICATIONS_PER_APP {
                run(
                    &mut e,
                    "notify.post",
                    json!({"app":"browser","title": format!("n{i}")}),
                    T0,
                );
            }
            let (_, ev) = run(
                &mut e,
                "notify.post",
                json!({"app":"browser","title":"one more"}),
                T0,
            );
            assert_eq!(names(&ev), ["notification_removed", "notification_posted"]);
            assert_eq!(ev[0]["reason"], "evicted");
            run(
                &mut e,
                "notify.post",
                json!({"app":"system","title":"brief","expires_at": T0 + 30}),
                T0,
            );
            assert_eq!(e.next_due(), Some(T0 + 30));
            let ev = e.tick(T0 + 30);
            assert_eq!(ev[0]["reason"], "expired");
        });
    }

    #[test]
    fn alarm_rings_snoozes_and_stops() {
        with_tz(TZ, || {
            let mut e = fresh();
            // T0 is 2026-10-26 07:33:20 UTC; 10:00 today.
            let (alarm, _) = run(
                &mut e,
                "alarm.create",
                json!({"hour":10,"minute":0,"label":"Lecture"}),
                T0,
            );
            let due = alarm["next_fire_at"].as_i64().unwrap();
            assert_eq!(due % 86400, 10 * 3600);
            assert!(due > T0 && due - T0 < 3 * 3600);
            assert_eq!(e.next_wake(), Some(due));
            assert!(e.tick(due - 1).is_empty());
            let ev = e.tick(due);
            assert_eq!(names(&ev), ["alarm_ringing"]);
            let id = alarm["id"].as_str().unwrap().to_owned();
            assert_eq!(e.ring_sound(), Some(Sound::Radar));
            assert!(e.ring_vibrates());
            // One-shot: disabled once it rang, nothing further to wake for.
            assert_eq!(e.next_wake(), None);
            let (snoozed, ev) = run(&mut e, "alarm.snooze", json!({"alarm": id}), due + 20);
            assert_eq!(names(&ev), ["alarm_ended", "alarm_changed"]);
            assert_eq!(snoozed["snoozed_until"], due + 20 + 540);
            assert_eq!(snoozed["enabled"], false);
            assert_eq!(e.ring_sound(), None);
            assert_eq!(e.next_wake(), Some(due + 560));
            assert_eq!(names(&e.tick(due + 560)), ["alarm_ringing"]);
            let (stopped, _) = run(&mut e, "alarm.stop", json!({"alarm": id}), due + 600);
            assert_eq!(stopped["ringing"], false);
            assert_eq!(stopped["next_fire_at"], Value::Null);
            assert_eq!(e.next_wake(), None);
            assert_eq!(
                error(&mut e, "alarm.stop", json!({"alarm": id}), due + 601),
                "not_found"
            );
        });
    }

    #[test]
    fn repeating_alarm_times_out_into_a_missed_notice() {
        with_tz(TZ, || {
            let mut e = fresh();
            let (alarm, _) = run(
                &mut e,
                "alarm.create",
                json!({"hour":10,"minute":0,"days":[1,2,3,4,5]}),
                T0,
            );
            let due = alarm["next_fire_at"].as_i64().unwrap();
            e.tick(due);
            assert_eq!(e.next_due(), Some(due + ALARM_RING_LIMIT));
            let ev = e.tick(due + ALARM_RING_LIMIT);
            assert_eq!(names(&ev), ["alarm_ended", "notification_posted"]);
            assert_eq!(ev[0]["reason"], "timeout");
            assert_eq!(ev[1]["notification"]["title"], "Missed alarm");
            assert_eq!(ev[1]["notification"]["body"], "10:00");
            // Tuesday 10:00.
            assert_eq!(e.next_wake(), Some(due + 86400));
        });
    }

    #[test]
    fn haptics_are_stateless_pulses_that_never_cut_into_ringing() {
        with_tz(TZ, || {
            let mut e = fresh();
            let (r, _) = run(&mut e, "ping", json!({}), T0);
            assert_eq!(r["revision"], 5);
            e.take_effects();
            e.dirty = false;
            let (r, ev) = run(&mut e, "haptic", json!({"style": "light"}), T0);
            assert_eq!(r, json!({}));
            assert!(ev.is_empty());
            assert_eq!(e.take_effects(), [Effect::Haptic(HAPTIC_LIGHT_MS)]);
            assert!(!e.dirty);
            // Do-not-disturb and `vibrate` are for alerts, not haptics.
            run(
                &mut e,
                "settings.set",
                json!({"do_not_disturb": true, "vibrate": false}),
                T0,
            );
            e.take_effects();
            run(&mut e, "haptic", json!({"style": "error"}), T0);
            assert_eq!(e.take_effects(), [Effect::Haptic(HAPTIC_ERROR_MS)]);
            assert_eq!(
                error(&mut e, "haptic", json!({"style": "buzz"}), T0),
                "invalid_argument"
            );
            assert_eq!(
                error(&mut e, "haptic", json!({"style": "light", "ms": 500}), T0),
                "invalid_argument"
            );
            assert_eq!(
                error(&mut e, "haptic", json!({"style": "light", "extra": 1}), T0),
                "invalid_argument"
            );
            run(&mut e, "haptic", json!({"style": "light", "ms": 22}), T0);
            assert_eq!(e.take_effects(), [Effect::Haptic(22)]);
            // Nothing while a timer rings.
            run(&mut e, "timer.create", json!({"duration_s": 1}), T0);
            e.tick(T0 + 1);
            e.take_effects();
            run(&mut e, "haptic", json!({"style": "selection"}), T0 + 2);
            assert!(e.take_effects().is_empty());
        });
    }

    #[test]
    fn timer_pause_resume_ring_and_stop() {
        with_tz(TZ, || {
            let mut e = fresh();
            let (t, _) = run(
                &mut e,
                "timer.create",
                json!({"duration_s": 600, "label": "Pasta"}),
                T0,
            );
            let id = t["id"].as_str().unwrap().to_owned();
            assert_eq!(t["ends_at"], T0 + 600);
            let (t, _) = run(&mut e, "timer.pause", json!({"timer": id}), T0 + 100);
            assert_eq!(t["remaining_s"], 500);
            assert_eq!(t["state"], "paused");
            assert_eq!(e.next_wake(), None);
            let (t, _) = run(&mut e, "timer.resume", json!({"timer": id}), T0 + 1000);
            assert_eq!(t["ends_at"], T0 + 1500);
            let ev = e.tick(T0 + 1500);
            assert_eq!(names(&ev), ["timer_ringing"]);
            assert_eq!(e.ring_sound(), Some(Sound::Chimes));
            let (_, ev) = run(&mut e, "timer.stop", json!({"timer": id}), T0 + 1510);
            assert_eq!(names(&ev), ["timer_ended", "timer_deleted"]);
            assert!(e.state.timers.is_empty());
            assert_eq!(
                error(&mut e, "timer.create", json!({"duration_s": 0}), T0),
                "invalid_argument"
            );
        });
    }

    #[test]
    fn restart_rings_recent_and_reports_old_misses() {
        with_tz(TZ, || {
            let mut e = fresh();
            let (soon, _) = run(&mut e, "alarm.create", json!({"hour":10,"minute":0}), T0);
            let (daily, _) = run(
                &mut e,
                "alarm.create",
                json!({"hour":9,"minute":40,"days":[0,1,2,3,4,5,6]}),
                T0,
            );
            run(&mut e, "timer.create", json!({"duration_s": 60}), T0);
            let saved = e.state.clone();
            // Back 5 minutes after the 10:00 one-shot was due, and well over
            // 15 minutes after the 09:40 daily one and the timer.
            let due = soon["next_fire_at"].as_i64().unwrap();
            let now = due + 300;
            let (e2, ev) = Engine::start(saved, now);
            let kinds = names(&ev);
            assert!(kinds.contains(&"alarm_ringing".to_owned()), "{kinds:?}");
            assert!(
                ev.iter()
                    .any(|v| v["notification"]["title"] == "Missed alarm")
            );
            assert!(
                ev.iter()
                    .any(|v| v["notification"]["title"] == "Timer done")
            );
            assert_eq!(e2.ringing.len(), 1);
            let daily_now = e2
                .state
                .alarms
                .iter()
                .find(|a| a.id == daily["id"].as_str().unwrap())
                .unwrap();
            assert!(daily_now.next_fire_at.unwrap() > now);
            assert!(e2.state.timers.is_empty());
        });
    }

    #[test]
    fn alarm_validation() {
        with_tz(TZ, || {
            let mut e = fresh();
            assert_eq!(
                error(&mut e, "alarm.create", json!({"hour":24,"minute":0}), T0),
                "invalid_argument"
            );
            assert_eq!(
                error(
                    &mut e,
                    "alarm.create",
                    json!({"hour":1,"minute":0,"days":[7]}),
                    T0
                ),
                "invalid_argument"
            );
            assert_eq!(
                error(&mut e, "alarm.create", json!({"minute":0}), T0),
                "invalid_argument"
            );
            assert_eq!(
                error(
                    &mut e,
                    "alarm.create",
                    json!({"alarm":"a1","hour":1,"minute":0}),
                    T0
                ),
                "invalid_argument"
            );
            assert_eq!(
                error(&mut e, "alarm.update", json!({"alarm":"a404","hour":1}), T0),
                "not_found"
            );
            let (a, _) = run(
                &mut e,
                "alarm.create",
                json!({"hour":7,"minute":5,"days":[3,1,1],"sound":"none"}),
                T0,
            );
            assert_eq!(a["days"], json!([1, 3]));
            let (a, _) = run(
                &mut e,
                "alarm.update",
                json!({"alarm": a["id"], "enabled": false}),
                T0,
            );
            assert_eq!(a["next_fire_at"], Value::Null);
        });
    }
}
