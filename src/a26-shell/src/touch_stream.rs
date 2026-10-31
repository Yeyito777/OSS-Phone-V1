//! Touches handed to an app that reads them itself (`touch stream` on the
//! control socket). The Browser's engine wants every finger with its own
//! id, to scroll with momentum, pinch and long-press, where XTEST can only
//! give it taps. Moon still decides which touches are the app's: one on the
//! keyboard, a Moon overlay or the bottom-edge gesture zone stays Moon's.
//!
//! The subscriber's connection gets one reply line, as any command does, and
//! then one line per contact change, in screen pixels, with the time Moon
//! got it (CLOCK_MONOTONIC milliseconds, so the app's fling velocity does not
//! depend on when it gets to read the line):
//!
//! ```text
//! begin ID X Y MS
//! move ID X Y MS
//! end ID X Y MS
//! cancel ID
//! ```
//!
//! `cancel` replaces `end` when the touch stopped being the app's (Moon
//! recovered a lost release, or another app came to the front). Writes never
//! block Moon: a subscriber that cannot take a line is dropped, and its app
//! gets XTEST taps again.

use std::io::Write;
use std::os::unix::net::UnixStream;

use crate::model::AppId;

/// CLOCK_MONOTONIC in milliseconds: the same clock in every process (and
/// chroot) on the phone.
fn now_ms() -> u64 {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec through a valid pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time) };
    time.tv_sec as u64 * 1000 + time.tv_nsec as u64 / 1_000_000
}

#[derive(Debug, Clone, Copy)]
struct Contact {
    id: u32,
    app: AppId,
    x: i16,
    y: i16,
}

#[derive(Default)]
pub struct TouchStream {
    subscribers: Vec<(AppId, UnixStream)>,
    contacts: Vec<Contact>,
}

impl TouchStream {
    /// `touch stream`: the app's touches go to this connection from now on
    /// (a newer subscription of the same app replaces the older one).
    pub fn subscribe(&mut self, app: AppId, mut stream: UnixStream) {
        let reply = serde_json::json!({
            "ok": true,
            "result": { "touch_stream": app.cgroup_name() },
        });
        let Ok(mut line) = serde_json::to_vec(&reply) else {
            return;
        };
        line.push(b'\n');
        if stream.set_nonblocking(true).is_err() || stream.write_all(&line).is_err() {
            return;
        }
        self.drop_app(app);
        self.subscribers.push((app, stream));
    }

    pub fn wants(&self, app: AppId) -> bool {
        self.subscribers
            .iter()
            .any(|(subscriber, _)| *subscriber == app)
    }

    /// The app that has the fingers down now, if any.
    pub fn active_app(&self) -> Option<AppId> {
        self.contacts.first().map(|contact| contact.app)
    }

    pub fn owns(&self, id: u32) -> bool {
        self.contacts.iter().any(|contact| contact.id == id)
    }

    /// A finger came down on the app; false if it could not be told (the
    /// touch is then Moon's as usual).
    pub fn begin(&mut self, app: AppId, id: u32, x: i16, y: i16) -> bool {
        if self.owns(id) || !self.send(app, &format!("begin {id} {x} {y} {}\n", now_ms())) {
            return false;
        }
        self.contacts.push(Contact { id, app, x, y });
        true
    }

    /// A finger moved. An update may carry one axis only; the other keeps
    /// its last value.
    pub fn update(&mut self, id: u32, x: Option<i16>, y: Option<i16>) {
        let Some(contact) = self.contacts.iter_mut().find(|contact| contact.id == id) else {
            return;
        };
        let (old_x, old_y) = (contact.x, contact.y);
        contact.x = x.unwrap_or(contact.x);
        contact.y = y.unwrap_or(contact.y);
        if (contact.x, contact.y) == (old_x, old_y) {
            return;
        }
        let Contact { app, x, y, .. } = *contact;
        if !self.send(app, &format!("move {id} {x} {y} {}\n", now_ms())) {
            self.forget_app(app);
        }
    }

    /// A finger lifted. While another app is in front by now, the app is
    /// told the touch was cancelled instead.
    pub fn end(&mut self, id: u32, in_front: Option<AppId>) {
        let Some(position) = self.contacts.iter().position(|contact| contact.id == id) else {
            return;
        };
        let Contact { app, x, y, .. } = self.contacts.remove(position);
        let line = if in_front == Some(app) {
            format!("end {id} {x} {y} {}\n", now_ms())
        } else {
            format!("cancel {id}\n")
        };
        if !self.send(app, &line) {
            self.forget_app(app);
        }
    }

    /// Every contact stops being the app's.
    pub fn cancel_all(&mut self) {
        for Contact { id, app, .. } in std::mem::take(&mut self.contacts) {
            self.send(app, &format!("cancel {id}\n"));
        }
    }

    fn send(&mut self, app: AppId, line: &str) -> bool {
        let Some(position) = self
            .subscribers
            .iter()
            .position(|(subscriber, _)| *subscriber == app)
        else {
            return false;
        };
        if self.subscribers[position]
            .1
            .write_all(line.as_bytes())
            .is_ok()
        {
            return true;
        }
        // Gone, or not reading: its touches are Moon's again.
        eprintln!("touch stream: {} stopped reading", app.cgroup_name());
        self.subscribers.remove(position);
        false
    }

    fn drop_app(&mut self, app: AppId) {
        self.subscribers
            .retain(|(subscriber, _)| *subscriber != app);
        self.forget_app(app);
    }

    fn forget_app(&mut self, app: AppId) {
        self.contacts.retain(|contact| contact.app != app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};

    fn subscribed(app: AppId) -> (TouchStream, BufReader<UnixStream>) {
        let (ours, theirs) = UnixStream::pair().unwrap();
        let mut stream = TouchStream::default();
        stream.subscribe(app, ours);
        let mut reader = BufReader::new(theirs);
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        assert!(reply.contains("\"ok\":true"), "{reply}");
        (stream, reader)
    }

    /// A line without its time, which is checked to be a plausible one.
    fn line(reader: &mut BufReader<UnixStream>) -> String {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let mut words: Vec<&str> = line.split_whitespace().collect();
        if words[0] != "cancel" {
            let ms: u64 = words.pop().unwrap().parse().unwrap();
            assert!(ms.abs_diff(now_ms()) < 5_000, "{line}");
        }
        words.join(" ")
    }

    #[test]
    fn contacts_reach_the_subscriber_with_ids() {
        let (mut stream, mut reader) = subscribed(AppId::Browser);
        assert!(stream.wants(AppId::Browser));
        assert!(!stream.wants(AppId::System));
        assert!(stream.begin(AppId::Browser, 4, 500, 900));
        assert!(stream.begin(AppId::Browser, 5, 700, 900));
        stream.update(4, None, Some(880));
        stream.update(4, None, Some(880));
        stream.update(5, Some(720), None);
        stream.end(4, Some(AppId::Browser));
        stream.end(5, Some(AppId::Phone));
        assert_eq!(line(&mut reader), "begin 4 500 900");
        assert_eq!(line(&mut reader), "begin 5 700 900");
        assert_eq!(line(&mut reader), "move 4 500 880");
        assert_eq!(line(&mut reader), "move 5 720 900");
        assert_eq!(line(&mut reader), "end 4 500 880");
        assert_eq!(line(&mut reader), "cancel 5");
        assert!(stream.active_app().is_none());
    }

    #[test]
    fn a_gone_subscriber_gives_its_touches_back() {
        let (mut stream, reader) = subscribed(AppId::Browser);
        drop(reader);
        assert!(!stream.begin(AppId::Browser, 1, 10, 10));
        assert!(!stream.wants(AppId::Browser));
        assert!(!stream.owns(1));
    }

    #[test]
    fn cancel_all_ends_every_contact() {
        let (mut stream, mut reader) = subscribed(AppId::Browser);
        assert!(stream.begin(AppId::Browser, 2, 1, 2));
        stream.cancel_all();
        assert!(!stream.owns(2));
        assert_eq!(line(&mut reader), "begin 2 1 2");
        assert_eq!(line(&mut reader), "cancel 2");
    }
}
