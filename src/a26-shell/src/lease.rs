use std::time::{Duration, Instant};

use serde::Serialize;

use crate::model::AppId;

const MEDIA_MAX: Duration = Duration::from_secs(30);
const TRANSFER_MAX: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
    Media,
    Transfer,
}

impl LeaseKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "media" => Some(Self::Media),
            "transfer" => Some(Self::Transfer),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Media => 0,
            Self::Transfer => 1,
        }
    }

    pub fn max_duration(self) -> Duration {
        match self {
            Self::Media => MEDIA_MAX,
            Self::Transfer => TRANSFER_MAX,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicLeaseState {
    pub kind: LeaseKind,
    pub remaining_ms: u64,
}

#[derive(Debug, Default)]
pub struct LeaseManager {
    deadlines: [[Option<Instant>; 2]; AppId::COUNT],
}

impl LeaseManager {
    pub fn acquire(
        &mut self,
        app: AppId,
        kind: LeaseKind,
        seconds: u64,
        now: Instant,
    ) -> Result<Duration, &'static str> {
        let duration = Duration::from_secs(seconds);
        if duration.is_zero() {
            return Err("lease duration must be positive");
        }
        if duration > kind.max_duration() {
            return Err("lease duration exceeds the policy limit");
        }
        self.deadlines[app.index()][kind.index()] = now.checked_add(duration);
        Ok(duration)
    }

    pub fn release(&mut self, app: AppId, kind: LeaseKind) {
        self.deadlines[app.index()][kind.index()] = None;
    }

    pub fn clear_app(&mut self, app: AppId) {
        self.deadlines[app.index()] = [None, None];
    }

    pub fn active(&mut self, app: AppId, now: Instant) -> bool {
        self.expire(now);
        self.deadlines[app.index()].iter().any(Option::is_some)
    }

    pub fn expire(&mut self, now: Instant) {
        for app in &mut self.deadlines {
            for deadline in app {
                if deadline.is_some_and(|deadline| deadline <= now) {
                    *deadline = None;
                }
            }
        }
    }

    pub fn public(&self, app: AppId, now: Instant) -> Vec<PublicLeaseState> {
        [LeaseKind::Media, LeaseKind::Transfer]
            .into_iter()
            .filter_map(|kind| {
                self.deadlines[app.index()][kind.index()]
                    .filter(|deadline| *deadline > now)
                    .map(|deadline| PublicLeaseState {
                        kind,
                        remaining_ms: deadline
                            .saturating_duration_since(now)
                            .as_millis()
                            .min(u64::MAX as u128) as u64,
                    })
            })
            .collect()
    }

    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.deadlines
            .iter()
            .flatten()
            .flatten()
            .copied()
            .filter(|deadline| *deadline > now)
            .min()
    }

    pub fn holds(&self, app: AppId, kind: LeaseKind, now: Instant) -> bool {
        self.deadlines[app.index()][kind.index()].is_some_and(|deadline| deadline > now)
    }

    pub fn any_active(&mut self, now: Instant) -> bool {
        self.expire(now);
        self.deadlines.iter().flatten().any(Option::is_some)
    }
}

/// Apps that keep the screen on while they are in front (`awake on`: a
/// recording, a video), each with the process that asked: it ends with
/// `awake off`, or when that process exits or restarts.
#[derive(Debug, Default)]
pub struct AwakeRequests([Option<u32>; AppId::COUNT]);

impl AwakeRequests {
    pub fn set(&mut self, app: AppId, pid: u32, on: bool) {
        self.0[app.index()] = on.then_some(pid);
    }

    /// The app asked, and `pid` is still the process that did.
    pub fn holds(&self, app: AppId, pid: Option<u32>) -> bool {
        self.0[app.index()].is_some_and(|asked| Some(asked) == pid)
    }

    /// For `state`: the running apps that asked.
    pub fn public(&self, pid: impl Fn(AppId) -> Option<u32>) -> Vec<&'static str> {
        AppId::ALL
            .into_iter()
            .filter(|app| self.holds(*app, pid(*app)))
            .map(AppId::cgroup_name)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leases_are_bounded_renewable_and_expire() {
        let start = Instant::now();
        let mut leases = LeaseManager::default();
        assert!(
            leases
                .acquire(AppId::Browser, LeaseKind::Media, 31, start)
                .is_err()
        );
        assert!(
            leases
                .acquire(AppId::Browser, LeaseKind::Transfer, 121, start)
                .is_err()
        );
        leases
            .acquire(AppId::Browser, LeaseKind::Media, 5, start)
            .unwrap();
        assert!(leases.active(AppId::Browser, start + Duration::from_secs(4)));
        leases
            .acquire(
                AppId::Browser,
                LeaseKind::Media,
                5,
                start + Duration::from_secs(4),
            )
            .unwrap();
        assert!(leases.active(AppId::Browser, start + Duration::from_secs(8)));
        assert!(!leases.active(AppId::Browser, start + Duration::from_secs(10)));
    }

    #[test]
    fn releasing_one_kind_preserves_the_other() {
        let start = Instant::now();
        let mut leases = LeaseManager::default();
        leases
            .acquire(AppId::System, LeaseKind::Media, 10, start)
            .unwrap();
        leases
            .acquire(AppId::System, LeaseKind::Transfer, 10, start)
            .unwrap();
        leases.release(AppId::System, LeaseKind::Media);
        let public = leases.public(AppId::System, start);
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].kind, LeaseKind::Transfer);
    }

    #[test]
    fn keeping_awake_lasts_as_long_as_the_process_that_asked() {
        let mut awake = AwakeRequests::default();
        awake.set(AppId::Exocortex, 41, true);
        assert!(awake.holds(AppId::Exocortex, Some(41)));
        assert!(!awake.holds(AppId::Exocortex, Some(42)), "restarted");
        assert!(!awake.holds(AppId::Exocortex, None), "exited");
        assert!(!awake.holds(AppId::System, Some(41)));
        assert_eq!(
            awake.public(|app| (app == AppId::Exocortex).then_some(41)),
            ["exocortex"]
        );
        assert!(awake.public(|_| None).is_empty());
        awake.set(AppId::Exocortex, 41, false);
        assert!(!awake.holds(AppId::Exocortex, Some(41)));
    }
}
