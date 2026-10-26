use std::env;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::freezer::{FreezerGroup, FreezerState};
use crate::lease::{LeaseKind, LeaseManager, PublicLeaseState};
use crate::model::{AppId, AppLifecycle, PublicAppState};

const DEFAULT_SYSTEM_APP: &str = "/opt/a26-system/bin/a26-system";
const DEFAULT_BROWSER_APP: &str = "/opt/vimbrowser-a26/bin/vimbrowser-a26";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowVisibility {
    Show(u32),
    Hide(u32),
}

#[derive(Debug, Default)]
pub struct RegistryUpdate {
    pub visibility: Vec<WindowVisibility>,
    pub freeze_after_hide: Vec<AppId>,
    pub resumed: Option<AppId>,
    pub active_process_exited: Option<AppId>,
    pub active_process_failed: Option<AppId>,
}

#[derive(Debug, Clone, Copy)]
struct ManagedWindow {
    id: u32,
    visible: bool,
}

#[derive(Debug)]
struct Application {
    id: AppId,
    executable: PathBuf,
    /// Phone and Messages are the System app's binary with `--app NAME`.
    args: Vec<String>,
    child: Option<Child>,
    pidfd: Option<OwnedFd>,
    lifecycle: AppLifecycle,
    windows: Vec<ManagedWindow>,
    freezer: FreezerGroup,
    last_used: Instant,
}

impl Application {
    fn new(id: AppId, executable: PathBuf) -> Self {
        let args = match id {
            AppId::Phone | AppId::Messages => vec!["--app".into(), id.cgroup_name().into()],
            AppId::System | AppId::Browser => Vec::new(),
        };
        Self {
            id,
            executable,
            args,
            child: None,
            pidfd: None,
            lifecycle: AppLifecycle::Stopped,
            windows: Vec::new(),
            freezer: FreezerGroup::prepare(id),
            last_used: Instant::now(),
        }
    }

    fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    fn public(&self, leases: Vec<PublicLeaseState>, now: Instant) -> PublicAppState {
        PublicAppState {
            app: self.id,
            lifecycle: self.lifecycle,
            pid: self.pid(),
            windows: self.windows.iter().map(|window| window.id).collect(),
            freezer_cgroup: self.freezer.public_path(),
            freezer_state: self.freezer.state(),
            leases,
            last_used_ms_ago: now
                .saturating_duration_since(self.last_used)
                .as_millis()
                .min(u64::MAX as u128) as u64,
        }
    }

    fn stop(&mut self) {
        if let Err(error) = self.freezer.thaw() {
            eprintln!(
                "cannot thaw {} while stopping: {error}",
                self.id.display_name()
            );
        }
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
        if let Err(error) = self.freezer.terminate_all() {
            eprintln!(
                "cannot terminate complete {} cgroup: {error}",
                self.id.display_name()
            );
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
        self.pidfd = None;
        self.lifecycle = AppLifecycle::Stopped;
        self.windows.clear();
        self.freezer.note_stopped();
    }
}

pub struct AppRegistry {
    applications: [Application; AppId::COUNT],
    active: Option<AppId>,
    leases: LeaseManager,
}

impl AppRegistry {
    pub fn from_environment() -> Self {
        let system = env::var_os("A26_SYSTEM_APP")
            .map(PathBuf::from)
            .unwrap_or_else(|| DEFAULT_SYSTEM_APP.into());
        let browser = env::var_os("A26_BROWSER_APP")
            .map(PathBuf::from)
            .unwrap_or_else(|| DEFAULT_BROWSER_APP.into());
        Self::new(system, browser)
    }

    fn new(system: PathBuf, browser: PathBuf) -> Self {
        Self {
            applications: [
                Application::new(AppId::System, system.clone()),
                Application::new(AppId::Browser, browser),
                Application::new(AppId::Phone, system.clone()),
                Application::new(AppId::Messages, system),
            ],
            active: None,
            leases: LeaseManager::default(),
        }
    }

    pub fn active(&self) -> Option<AppId> {
        self.active
    }

    pub fn public(&self) -> Vec<PublicAppState> {
        let now = Instant::now();
        self.applications
            .iter()
            .map(|application| application.public(self.leases.public(application.id, now), now))
            .collect()
    }

    pub fn active_windows(&self) -> Vec<u32> {
        self.active
            .map(|id| {
                self.app(id)
                    .windows
                    .iter()
                    .map(|window| window.id)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn primary_active_window(&self) -> Option<u32> {
        self.active
            .and_then(|id| self.app(id).windows.first().map(|window| window.id))
    }

    pub fn owner_of(&self, window: u32) -> Option<AppId> {
        self.applications.iter().find_map(|application| {
            application
                .windows
                .iter()
                .any(|managed| managed.id == window)
                .then_some(application.id)
        })
    }

    pub fn is_active_window(&self, window: u32) -> bool {
        self.active
            .is_some_and(|active| self.owner_of(window) == Some(active))
    }

    pub fn register_window(&mut self, owner: AppId, window: u32) -> WindowRegistration {
        let should_show = self.active == Some(owner);
        let application = self.app_mut(owner);
        if let Some(existing) = application
            .windows
            .iter_mut()
            .find(|managed| managed.id == window)
        {
            existing.visible = should_show;
        } else {
            application.windows.push(ManagedWindow {
                id: window,
                visible: should_show,
            });
        }
        if should_show && application.lifecycle == AppLifecycle::Launching {
            application.lifecycle = AppLifecycle::Foreground;
        }
        WindowRegistration {
            owner,
            should_show,
            primary: application.windows.first().map(|managed| managed.id) == Some(window),
        }
    }

    pub fn note_mapped(&mut self, window: u32) -> bool {
        let Some(owner) = self.owner_of(window) else {
            return true;
        };
        let should_show = self.active == Some(owner);
        if let Some(managed) = self
            .app_mut(owner)
            .windows
            .iter_mut()
            .find(|managed| managed.id == window)
        {
            managed.visible = should_show;
        }
        should_show
    }

    pub fn note_unmapped(&mut self, window: u32) {
        let Some(owner) = self.owner_of(window) else {
            return;
        };
        let application = self.app_mut(owner);
        let intentionally_hidden = application
            .windows
            .iter()
            .find(|managed| managed.id == window)
            .is_some_and(|managed| !managed.visible);
        if !intentionally_hidden {
            application.windows.retain(|managed| managed.id != window);
        }
    }

    pub fn remove_window(&mut self, window: u32) {
        for application in &mut self.applications {
            application.windows.retain(|managed| managed.id != window);
        }
    }

    pub fn reconcile(&mut self, desired: Option<AppId>) -> RegistryUpdate {
        let mut update = RegistryUpdate::default();
        let now = Instant::now();
        self.leases.expire(now);
        for application in &mut self.applications {
            let result = application.child.as_mut().map(Child::try_wait);
            match result {
                Some(Ok(Some(status))) => {
                    eprintln!("{} exited with {status}", application.id.display_name());
                    application.child = None;
                    application.pidfd = None;
                    application.lifecycle = AppLifecycle::Stopped;
                    application.windows.clear();
                    application.freezer.note_stopped();
                    self.leases.clear_app(application.id);
                    if desired == Some(application.id) {
                        update.active_process_exited = Some(application.id);
                    }
                }
                Some(Err(error)) => {
                    eprintln!("cannot inspect {}: {error}", application.id.display_name());
                    application.stop();
                    if desired == Some(application.id) {
                        update.active_process_failed = Some(application.id);
                    }
                }
                Some(Ok(None)) | None => {}
            }
        }

        if update.active_process_exited.is_some() || update.active_process_failed.is_some() {
            self.active = None;
            return update;
        }

        // An app with a lease remains thawed while backgrounded. As soon as its
        // final bounded lease expires, the ordinary reconciliation pass puts
        // it back into the freezer even if another app is foreground.
        for id in AppId::ALL {
            let needs_freeze = {
                let application = self.app(id);
                desired != Some(id)
                    && application.lifecycle == AppLifecycle::Background
                    && matches!(
                        application.freezer.state(),
                        FreezerState::Thawed | FreezerState::Unknown
                    )
            };
            if needs_freeze && !self.leases.active(id, now) {
                update.freeze_after_hide.push(id);
            }
        }

        if self.active != desired {
            if let Some(previous) = self.active {
                let leased = self.leases.active(previous, now);
                let application = self.app_mut(previous);
                if application.lifecycle != AppLifecycle::Stopped {
                    application.lifecycle = AppLifecycle::Background;
                    for window in &mut application.windows {
                        if window.visible {
                            window.visible = false;
                            update.visibility.push(WindowVisibility::Hide(window.id));
                        }
                    }
                    if !leased {
                        update.freeze_after_hide.push(previous);
                    }
                }
            }
            self.active = desired;
        }

        let Some(active) = desired else {
            return update;
        };
        let application = self.app_mut(active);
        match application.lifecycle {
            AppLifecycle::Background if application.windows.is_empty() => {
                // The process survived, but it no longer owns a reusable
                // top-level window. Treat its next MapRequest as a warm launch
                // rather than claiming that an invisible app was resumed.
                application.lifecycle = AppLifecycle::Launching;
            }
            AppLifecycle::Background => {
                if let Err(error) = application.freezer.thaw() {
                    eprintln!("cannot resume {}: {error}", application.id.display_name());
                    application.stop();
                    update.active_process_failed = Some(active);
                    self.active = None;
                    return update;
                }
                application.lifecycle = AppLifecycle::Foreground;
                application.last_used = now;
                for window in &mut application.windows {
                    if !window.visible {
                        window.visible = true;
                        update.visibility.push(WindowVisibility::Show(window.id));
                    }
                }
                update.resumed = Some(active);
            }
            AppLifecycle::Stopped => {
                let mut command = Command::new(&application.executable);
                command
                    .args(&application.args)
                    .env(
                        "DISPLAY",
                        env::var("DISPLAY").unwrap_or_else(|_| ":0".into()),
                    )
                    .stdin(Stdio::null());
                if let Err(error) = application.freezer.configure_spawn(&mut command) {
                    eprintln!(
                        "cannot isolate {} before launch: {error}",
                        application.id.display_name()
                    );
                    update.active_process_failed = Some(active);
                    self.active = None;
                    return update;
                }
                match command.spawn() {
                    Ok(process) => {
                        eprintln!(
                            "started {} pid={}",
                            application.id.display_name(),
                            process.id()
                        );
                        application.child = Some(process);
                        application.pidfd = match open_pidfd(application.pid().unwrap_or_default())
                        {
                            Ok(fd) => Some(fd),
                            Err(error) => {
                                eprintln!(
                                    "{} pidfd unavailable; using bounded status polling: {error}",
                                    application.id.display_name()
                                );
                                None
                            }
                        };
                        application.freezer.note_spawned();
                        application.lifecycle = AppLifecycle::Launching;
                        application.last_used = now;
                    }
                    Err(error) => {
                        eprintln!("cannot start {}: {error}", application.executable.display());
                        update.active_process_failed = Some(active);
                        self.active = None;
                    }
                }
            }
            AppLifecycle::Launching | AppLifecycle::Foreground => {}
        }
        update
    }

    pub fn shutdown(&mut self) {
        for application in &mut self.applications {
            application.stop();
        }
        self.active = None;
        self.leases = LeaseManager::default();
    }

    pub fn freeze_background(&mut self, id: AppId) -> std::io::Result<()> {
        let application = self.app_mut(id);
        if application.lifecycle == AppLifecycle::Background {
            application.freezer.freeze()?;
        }
        Ok(())
    }

    pub fn acquire_lease(
        &mut self,
        app: AppId,
        kind: LeaseKind,
        seconds: u64,
        now: Instant,
    ) -> Result<Duration, &'static str> {
        if self.app(app).lifecycle == AppLifecycle::Stopped {
            return Err("cannot lease a stopped app");
        }
        let duration = self.leases.acquire(app, kind, seconds, now)?;
        if self.app(app).lifecycle == AppLifecycle::Background
            && self.app(app).freezer.thaw().is_err()
        {
            self.leases.release(app, kind);
            return Err("cannot thaw leased app");
        }
        Ok(duration)
    }

    pub fn release_lease(
        &mut self,
        app: AppId,
        kind: LeaseKind,
        now: Instant,
    ) -> Result<(), &'static str> {
        self.leases.release(app, kind);
        if self.app(app).lifecycle == AppLifecycle::Background && !self.leases.active(app, now) {
            self.app(app)
                .freezer
                .freeze()
                .map_err(|_| "cannot freeze released app")?;
        }
        Ok(())
    }

    pub fn renew_browser_media_activity(&mut self, now: Instant) {
        if self.app(AppId::Browser).lifecycle != AppLifecycle::Stopped {
            // The AudioTrack bridge emits no page data, only a recent marker
            // while Vimbrowser's PCM contains audible samples. Renewal remains
            // subject to the same five-second bounded media lease as IPC.
            let _ = self.acquire_lease(AppId::Browser, LeaseKind::Media, 5, now);
        }
    }

    pub fn browser_running(&self) -> bool {
        self.app(AppId::Browser).lifecycle != AppLifecycle::Stopped
    }

    pub fn poll_fds(&self) -> Vec<RawFd> {
        self.applications
            .iter()
            .filter_map(|application| application.pidfd.as_ref().map(AsRawFd::as_raw_fd))
            .collect()
    }

    pub fn needs_process_poll(&self) -> bool {
        self.applications
            .iter()
            .any(|application| application.child.is_some() && application.pidfd.is_none())
    }

    pub fn next_lease_deadline(&self, now: Instant) -> Option<Instant> {
        self.leases.next_deadline(now)
    }

    /// The app is playing media (its media lease is held).
    pub fn media_active(&self, app: AppId, now: Instant) -> bool {
        self.leases.holds(app, LeaseKind::Media, now)
    }

    pub fn has_active_leases(&mut self, now: Instant) -> bool {
        self.leases.any_active(now)
    }

    /// Apps with a live process, least recently used first (the order of
    /// the app switcher's cards).
    pub fn running_by_recency(&self) -> Vec<AppId> {
        let mut running: Vec<(AppId, Instant)> = AppId::ALL
            .into_iter()
            .filter(|id| self.app(*id).lifecycle != AppLifecycle::Stopped)
            .map(|id| (id, self.app(id).last_used))
            .collect();
        running.sort_by_key(|(_, last_used)| *last_used);
        running.into_iter().map(|(id, _)| id).collect()
    }

    /// Closes a background app from the app switcher: its whole freezer
    /// cgroup (every Chromium helper included) is killed, like an eviction.
    pub fn terminate(&mut self, id: AppId) -> bool {
        if self.active == Some(id) || self.app(id).lifecycle == AppLifecycle::Stopped {
            return false;
        }
        self.leases.clear_app(id);
        self.app_mut(id).stop();
        true
    }

    pub fn evict_lru_background(&mut self, now: Instant) -> Option<AppId> {
        self.leases.expire(now);
        let mut candidate: Option<(AppId, Instant)> = None;
        for id in AppId::ALL {
            let background = self.app(id).lifecycle == AppLifecycle::Background;
            let leased = self.leases.active(id, now);
            let last_used = self.app(id).last_used;
            if background && !leased && candidate.is_none_or(|(_, oldest)| last_used < oldest) {
                candidate = Some((id, last_used));
            }
        }
        let candidate = candidate?.0;
        self.leases.clear_app(candidate);
        self.app_mut(candidate).stop();
        Some(candidate)
    }

    fn app(&self, id: AppId) -> &Application {
        &self.applications[id.index()]
    }

    fn app_mut(&mut self, id: AppId) -> &mut Application {
        &mut self.applications[id.index()]
    }
}

fn open_pidfd(pid: u32) -> io::Result<OwnedFd> {
    // SAFETY: pidfd_open has no pointer arguments. A successful returned fd is
    // uniquely owned here and immediately wrapped in OwnedFd.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0_u32) } as i32;
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh successful pidfd_open result.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[derive(Debug, Clone, Copy)]
pub struct WindowRegistration {
    pub owner: AppId,
    pub should_show: bool,
    pub primary: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AppRegistry {
        AppRegistry::new("/bin/true".into(), "/bin/true".into())
    }

    #[test]
    fn registry_keeps_windows_separate_across_lifecycle_transitions() {
        let mut registry = registry();
        registry.active = Some(AppId::System);
        registry.app_mut(AppId::System).lifecycle = AppLifecycle::Foreground;
        registry.app_mut(AppId::System).child =
            Some(Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap());
        registry.register_window(AppId::System, 11);

        let update = registry.reconcile(None);
        assert_eq!(update.visibility, vec![WindowVisibility::Hide(11)]);
        assert_eq!(update.freeze_after_hide, vec![AppId::System]);
        assert_eq!(
            registry.app(AppId::System).lifecycle,
            AppLifecycle::Background
        );
        assert_eq!(registry.owner_of(11), Some(AppId::System));

        let update = registry.reconcile(Some(AppId::System));
        assert_eq!(update.resumed, Some(AppId::System));
        assert_eq!(update.visibility, vec![WindowVisibility::Show(11)]);
        assert_eq!(registry.primary_active_window(), Some(11));
        registry.shutdown();
    }

    #[test]
    fn intentional_background_unmap_does_not_forget_window() {
        let mut registry = registry();
        registry.active = Some(AppId::Browser);
        registry.app_mut(AppId::Browser).lifecycle = AppLifecycle::Foreground;
        registry.register_window(AppId::Browser, 22);
        registry.reconcile(None);
        registry.note_unmapped(22);
        assert_eq!(registry.owner_of(22), Some(AppId::Browser));
        registry.remove_window(22);
        assert_eq!(registry.owner_of(22), None);
    }

    #[test]
    fn windowless_background_process_returns_to_launching() {
        let mut registry = registry();
        registry.app_mut(AppId::System).lifecycle = AppLifecycle::Background;
        let update = registry.reconcile(Some(AppId::System));
        assert_eq!(update.resumed, None);
        assert_eq!(
            registry.app(AppId::System).lifecycle,
            AppLifecycle::Launching
        );
    }

    #[test]
    fn audio_activity_renews_only_a_running_browser_media_lease() {
        let mut registry = registry();
        let now = Instant::now();
        registry.renew_browser_media_activity(now);
        assert!(registry.leases.public(AppId::Browser, now).is_empty());

        registry.app_mut(AppId::Browser).lifecycle = AppLifecycle::Foreground;
        registry.app_mut(AppId::Browser).child =
            Some(Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap());
        registry.renew_browser_media_activity(now);
        let public = registry.leases.public(AppId::Browser, now);
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].kind, LeaseKind::Media);
        registry.shutdown();
    }

    #[test]
    fn memory_pressure_evicts_oldest_unleased_background_app() {
        let mut registry = registry();
        let now = Instant::now();
        for id in [AppId::System, AppId::Browser] {
            registry.app_mut(id).lifecycle = AppLifecycle::Background;
            registry.app_mut(id).child =
                Some(Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap());
        }
        registry.app_mut(AppId::System).last_used = now - Duration::from_secs(20);
        registry.app_mut(AppId::Browser).last_used = now - Duration::from_secs(10);
        registry
            .acquire_lease(AppId::System, LeaseKind::Transfer, 10, now)
            .unwrap();

        assert_eq!(registry.evict_lru_background(now), Some(AppId::Browser));
        assert_eq!(
            registry.app(AppId::Browser).lifecycle,
            AppLifecycle::Stopped
        );
        assert_eq!(
            registry.app(AppId::System).lifecycle,
            AppLifecycle::Background
        );
        registry
            .release_lease(AppId::System, LeaseKind::Transfer, now)
            .unwrap();
        assert_eq!(registry.evict_lru_background(now), Some(AppId::System));
    }

    #[test]
    fn switcher_close_kills_only_background_apps_and_orders_by_recency() {
        let mut registry = registry();
        let now = Instant::now();
        for id in [AppId::System, AppId::Browser] {
            registry.app_mut(id).lifecycle = AppLifecycle::Background;
            registry.app_mut(id).child =
                Some(Command::new("sh").args(["-c", "sleep 30"]).spawn().unwrap());
        }
        registry.app_mut(AppId::System).last_used = now - Duration::from_secs(5);
        registry.app_mut(AppId::Browser).last_used = now - Duration::from_secs(50);
        assert_eq!(
            registry.running_by_recency(),
            vec![AppId::Browser, AppId::System]
        );
        registry.active = Some(AppId::System);
        assert!(!registry.terminate(AppId::System));
        assert!(registry.terminate(AppId::Browser));
        assert_eq!(
            registry.app(AppId::Browser).lifecycle,
            AppLifecycle::Stopped
        );
        assert!(registry.app(AppId::Browser).child.is_none());
        assert!(!registry.terminate(AppId::Browser));
        assert_eq!(registry.running_by_recency(), vec![AppId::System]);
        registry.active = None;
        registry.app_mut(AppId::System).stop();
    }
}
