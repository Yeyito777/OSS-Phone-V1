//! moon-notifyd: OSS-Phone-V1's notification, alarm and timer service.
//! See docs/notifications-protocol.md.

#[path = "../notify/mod.rs"]
mod notify;

fn main() {
    if std::env::args()
        .nth(1)
        .is_some_and(|a| a == "--help" || a == "-h")
    {
        println!("usage: moon-notifyd   (paths under $MOON_NOTIFY_ROOT if set)");
        return;
    }
    if let Err(error) = notify::daemon::run(notify::daemon::Paths::from_environment()) {
        eprintln!("moon-notifyd: {error}");
        std::process::exit(1);
    }
}
