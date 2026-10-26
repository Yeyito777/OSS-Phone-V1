//! moon-telephonyctl CMD [key=value ...]: one request to moon-telephonyd,
//! printing the JSON result; `subscribe` prints events until interrupted.
//! Values that parse as JSON are sent as such, everything else as strings.
//! `-` as a value reads it from one line of stdin (e.g. pin=- for a SIM PIN,
//! so it never appears in argv).
//!
//! `moon-telephonyctl lpac ARGS...` runs lpac (the open-source eSIM LPA) with
//! its stdio APDU driver and carries its APDUs to the card through
//! moon-telephonyd's logical-channel commands (root only).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::process::{Command, Stdio};
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
    if cmd == "lpac" {
        return lpac(args.collect());
    }
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

/// A request/response connection for the lpac bridge.
struct Session {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
}

impl Session {
    fn open() -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket())?;
        stream.set_read_timeout(Some(Duration::from_secs(20)))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream,
            reader,
            next_id: 1,
        })
    }

    fn call(&mut self, cmd: &str, args: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let mut request = match args {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        request.insert("id".into(), Value::from(id));
        request.insert("cmd".into(), Value::from(cmd));
        let mut line = serde_json::to_vec(&Value::Object(request)).map_err(|e| e.to_string())?;
        line.push(b'\n');
        self.stream.write_all(&line).map_err(|e| e.to_string())?;
        loop {
            let mut reply = String::new();
            if self
                .reader
                .read_line(&mut reply)
                .map_err(|e| e.to_string())?
                == 0
            {
                return Err("moon-telephonyd closed the connection".into());
            }
            let value: Value = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
            if value.get("id") != Some(&Value::from(id)) {
                continue;
            }
            return if value["ok"] == Value::Bool(true) {
                Ok(value["result"].clone())
            } else {
                Err(value["error"]["message"]
                    .as_str()
                    .unwrap_or("error")
                    .to_owned())
            };
        }
    }
}

fn lpac(args: Vec<String>) -> ExitCode {
    let mut session = match Session::open() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("moon-telephonyctl: {e}");
            return ExitCode::FAILURE;
        }
    };
    let program = std::env::var("LPAC").unwrap_or_else(|_| "lpac".into());
    let mut child = match Command::new(&program)
        .args(&args)
        .env("LPAC_APDU", "stdio")
        .env(
            "LPAC_HTTP",
            std::env::var("LPAC_HTTP").unwrap_or_else(|_| "curl".into()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("moon-telephonyctl: cannot run {program}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut to_lpac = child.stdin.take().unwrap();
    let from_lpac = BufReader::new(child.stdout.take().unwrap());
    let mut channel: Option<i64> = None;
    for line in from_lpac.lines() {
        let Ok(line) = line else { break };
        let parsed: Option<Value> = serde_json::from_str(&line).ok();
        let Some(payload) = parsed
            .as_ref()
            .filter(|v| v["type"] == "apdu")
            .map(|v| &v["payload"])
        else {
            println!("{line}");
            continue;
        };
        let param = payload["param"].as_str().unwrap_or("");
        let reply = match payload["func"].as_str().unwrap_or("") {
            "connect" => serde_json::json!({"ecode": 0}),
            "disconnect" => {
                if let Some(c) = channel.take() {
                    let _ = session.call("sim.channel_close", serde_json::json!({"channel": c}));
                }
                serde_json::json!({"ecode": 0})
            }
            "logic_channel_open" => {
                match session.call("sim.channel_open", serde_json::json!({"aid": param})) {
                    Ok(r) => {
                        let c = r["channel"].as_i64().unwrap_or(-1);
                        channel = Some(c);
                        serde_json::json!({"ecode": c})
                    }
                    Err(e) => {
                        eprintln!("moon-telephonyctl: open channel: {e}");
                        serde_json::json!({"ecode": -1})
                    }
                }
            }
            "logic_channel_close" => {
                let c = i64::from_str_radix(param, 16).unwrap_or(-1);
                let _ = session.call("sim.channel_close", serde_json::json!({"channel": c}));
                if channel == Some(c) {
                    channel = None;
                }
                serde_json::json!({"ecode": 0})
            }
            "transmit" => match channel {
                Some(c) => match session
                    .call("sim.apdu", serde_json::json!({"channel": c, "apdu": param}))
                {
                    Ok(r) => serde_json::json!({"ecode": 0, "data": r["response"]}),
                    Err(e) => {
                        eprintln!("moon-telephonyctl: APDU: {e}");
                        serde_json::json!({"ecode": -1})
                    }
                },
                None => serde_json::json!({"ecode": -1}),
            },
            _ => serde_json::json!({"ecode": -1}),
        };
        let mut out =
            serde_json::to_vec(&serde_json::json!({"type": "apdu", "payload": reply})).unwrap();
        out.push(b'\n');
        if to_lpac
            .write_all(&out)
            .and_then(|_| to_lpac.flush())
            .is_err()
        {
            break;
        }
    }
    if let Some(c) = channel {
        let _ = session.call("sim.channel_close", serde_json::json!({"channel": c}));
    }
    match child.wait() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        _ => ExitCode::FAILURE,
    }
}
