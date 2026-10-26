//! moon-telephonyctl CMD [key=value ...]: one request to moon-telephonyd,
//! printing the JSON result; `subscribe` prints events until interrupted.
//! Values that parse as JSON are sent as such, everything else as strings.
//! `-` as a value reads it from one line of stdin (e.g. pin=- for a SIM PIN,
//! so it never appears in argv).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value};

fn socket() -> PathBuf {
    if let Some(path) = std::env::var_os("MOON_TELEPHONY_SOCKET") {
        return path.into();
    }
    match std::env::var_os("MOON_TELEPHONY_ROOT") {
        Some(root) => PathBuf::from(root).join("data/local/tmp/moon-telephony/telephony.sock"),
        // Inside the chroot the socket is bind-mounted at /run/moon-telephony.
        None if std::path::Path::new("/run/moon-telephony/telephony.sock").exists() => {
            "/run/moon-telephony/telephony.sock".into()
        }
        None => "/data/local/tmp/moon-telephony/telephony.sock".into(),
    }
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(cmd) = args.next() else {
        eprintln!("usage: moon-telephonyctl CMD [key=value ...]");
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
        let value = if value == "-" {
            let mut line = String::new();
            if std::io::stdin().read_line(&mut line).is_err() {
                return ExitCode::from(2);
            }
            Value::from(line.trim_end_matches(['\r', '\n']))
        } else {
            serde_json::from_str(value).unwrap_or_else(|_| Value::from(value))
        };
        request.insert(key.to_owned(), value);
    }
    match send(&cmd, Value::Object(request)) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("moon-telephonyctl: {error}");
            ExitCode::FAILURE
        }
    }
}

fn send(cmd: &str, request: Value) -> std::io::Result<bool> {
    let mut stream = UnixStream::connect(socket())?;
    if cmd != "subscribe" {
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    }
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    stream.write_all(&line)?;
    let reader = BufReader::new(stream);
    let mut ok = false;
    for line in reader.lines() {
        let value: Value = serde_json::from_str(&line?)?;
        // Replies carry `ok`; events never do.
        if value.get("ok").is_some() {
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
