//! `open browser URL`: the Browser (vimbrowser) opens a link in a new tab,
//! and is started first when it is not running.
//!
//! Moon brings the Browser forward as for any `open` target (a tapped
//! notification, `a26-shellctl open browser URL`, after unlocking when
//! locked); here the link is handed to vimbrowser over its own IPC socket,
//! one command per connection: `open-focus-tab URL` (builds before it have
//! only `open-tab`, which there opens in front too). A thread waits for the
//! socket while a cold start brings CEF up, so the event loop never blocks.
//! Links are never logged.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

const DEFAULT_SOCKET: &str = "/opt/vimbrowser-a26/rootfs/var/lib/vimbrowser/profile/ipc.sock";
/// How long a cold start may take to answer (CEF on SwiftShader).
const START_WAIT: Duration = Duration::from_secs(45);
const RETRY: Duration = Duration::from_millis(250);
const REPLY_WAIT: Duration = Duration::from_secs(10);
/// Longer links are refused (a page's own links are far shorter).
const MAX_URL: usize = 8192;

/// An `http` or `https` link with a host, no spaces or control characters.
pub fn valid_url(url: &str) -> bool {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| {
            // Schemes are case-insensitive.
            let (scheme, rest) = url.split_once("://")?;
            ["http", "https"]
                .iter()
                .any(|known| scheme.eq_ignore_ascii_case(known))
                .then_some(rest)
        });
    let Some(rest) = rest else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= MAX_URL
        && !host.is_empty()
        && !host.starts_with(['@', ':'])
        && url
            .chars()
            .all(|c| c.is_ascii_graphic() || (!c.is_ascii() && !c.is_control()))
}

/// The thread that hands links to the Browser, in order.
#[derive(Default)]
pub struct BrowserLinks {
    sender: Option<Sender<String>>,
}

impl BrowserLinks {
    /// Opens `url` in the Browser once its socket answers (Moon has just
    /// started it or brought it forward).
    pub fn open(&mut self, url: String) {
        if let Some(sender) = &self.sender
            && sender.send(url.clone()).is_ok()
        {
            return;
        }
        let (sender, receiver) = mpsc::channel::<String>();
        let socket = std::env::var_os("A26_BROWSER_IPC")
            .map(PathBuf::from)
            .unwrap_or_else(|| DEFAULT_SOCKET.into());
        let spawned = std::thread::Builder::new()
            .name("browser-links".into())
            .spawn(move || {
                for url in receiver {
                    match deliver(&socket, &url) {
                        Ok(()) => eprintln!("browser: opened a link"),
                        Err(error) => eprintln!("browser: link not opened: {error}"),
                    }
                }
            });
        if let Err(error) = spawned {
            eprintln!("browser: cannot start the link thread: {error}");
            return;
        }
        let _ = sender.send(url);
        self.sender = Some(sender);
    }
}

fn deliver(socket: &std::path::Path, url: &str) -> Result<(), String> {
    let reply = command(socket, &format!("open-focus-tab {url}"))?;
    let reply = if reply.starts_with("ERR unknown command") {
        command(socket, &format!("open-tab {url}"))?
    } else {
        reply
    };
    match reply.strip_prefix("ERR ") {
        Some(error) => Err(error.trim().to_owned()),
        None => Ok(()),
    }
}

/// One vimbrowser IPC command: connects (waiting out a starting Browser),
/// sends the line, returns the reply's first line.
fn command(socket: &std::path::Path, line: &str) -> Result<String, String> {
    let deadline = Instant::now() + START_WAIT;
    let mut stream = loop {
        match UnixStream::connect(socket) {
            Ok(stream) => break stream,
            Err(error) if Instant::now() >= deadline => {
                return Err(format!("the Browser did not answer: {error}"));
            }
            Err(_) => std::thread::sleep(RETRY),
        }
    };
    let io = |error: std::io::Error| error.to_string();
    stream.set_read_timeout(Some(REPLY_WAIT)).map_err(io)?;
    stream.set_write_timeout(Some(REPLY_WAIT)).map_err(io)?;
    stream
        .write_all(format!("{line}\n").as_bytes())
        .map_err(io)?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(io)?;
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_links_open() {
        for good in [
            "https://example.com",
            "http://example.com/a?b=c#d",
            "HTTPS://Example.com/path",
            "https://user@example.com:8443/x",
            "https://例え.jp/パス",
        ] {
            assert!(valid_url(good), "{good}");
        }
        for bad in [
            "",
            "example.com",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ftp://example.com",
            "https://",
            "https:///path",
            "https://@evil",
            "https://exa mple.com",
            "https://example.com/\u{7}",
            "https://example.com/\nopen-tab x",
        ] {
            assert!(!valid_url(bad), "{bad:?}");
        }
        assert!(!valid_url(&format!(
            "https://example.com/{}",
            "a".repeat(MAX_URL)
        )));
    }

    #[test]
    fn links_go_to_the_browser_socket_with_a_fallback_for_older_builds() {
        let dir = std::env::temp_dir().join(format!("moon-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("ipc.sock");
        // An older Browser: open-focus-tab is unknown, open-tab works.
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let server = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(&mut stream).read_line(&mut line).unwrap();
                let reply = if line.starts_with("open-focus-tab ") {
                    "ERR unknown command\n"
                } else {
                    "{\"tabs\":[]}\n"
                };
                stream.write_all(reply.as_bytes()).unwrap();
                seen.push(line);
            }
            seen
        });
        deliver(&socket, "https://example.com/a").unwrap();
        assert_eq!(
            server.join().unwrap(),
            [
                "open-focus-tab https://example.com/a\n",
                "open-tab https://example.com/a\n"
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
