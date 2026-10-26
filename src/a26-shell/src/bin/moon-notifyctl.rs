//! moon-notifyctl CMD [key=value ...]: one request to moon-notifyd, printing
//! the JSON result; `subscribe` prints events until interrupted.
//! Values that parse as JSON are sent as such, everything else as strings.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value};

fn socket() -> PathBuf {
    match std::env::var_os("MOON_NOTIFY_ROOT") {
        Some(root) => PathBuf::from(root).join("run/moon-notify/notify.sock"),
        None => PathBuf::from("/run/moon-notify/notify.sock"),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        eprintln!("usage: moon-notifyctl CMD [key=value ...]");
        return ExitCode::from(2);
    };
    let mut request = Map::new();
    request.insert("id".into(), Value::from(1));
    request.insert("cmd".into(), Value::from(cmd.clone()));
    for arg in args {
        let Some((key, value)) = arg.split_once('=') else {
            eprintln!("argument {arg:?} is not key=value");
            return ExitCode::from(2);
        };
        let value = serde_json::from_str(value).unwrap_or_else(|_| Value::from(value));
        request.insert(key.to_owned(), value);
    }
    match send(&cmd, Value::Object(request)) {
        Ok(ok) => {
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("moon-notifyctl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn send(cmd: &str, request: Value) -> std::io::Result<bool> {
    let mut stream = UnixStream::connect(socket())?;
    if cmd != "subscribe" {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    }
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let reader = BufReader::new(stream);
    let mut ok = false;
    for line in reader.lines() {
        let value: Value = serde_json::from_str(&line?)?;
        // Replies carry `ok`; events never do (some carry an object `id`).
        let is_reply = value.get("ok").is_some();
        if is_reply {
            ok = value["ok"] == Value::Bool(true);
            let shown = if ok {
                &value["result"]
            } else {
                &value["error"]
            };
            println!("{}", serde_json::to_string_pretty(shown)?);
            if cmd != "subscribe" || !ok {
                break;
            }
        } else {
            println!("{}", serde_json::to_string(&value)?);
            std::io::stdout().flush()?;
        }
    }
    Ok(ok)
}
