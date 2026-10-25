#!/usr/bin/env python3
import hashlib
import json
import os
import pathlib
import secrets
import sys

if len(sys.argv) != 2:
    raise SystemExit("usage: generate-config.py OUTPUT")

pin = os.environ.get("A26_SHELL_PIN")
if pin is None:
    raise SystemExit("A26_SHELL_PIN is not present in the environment")
if not (pin.isascii() and pin.isdigit() and len(pin) == 6):
    raise SystemExit("A26_SHELL_PIN must contain exactly six ASCII digits")

salt = secrets.token_bytes(32)
digest = hashlib.sha256(salt + pin.encode("ascii")).hexdigest()
config = {
    "pin_salt_hex": salt.hex(),
    "pin_hash_hex": digest,
    "pin_length": 6,
    "start_locked": True,
    "initial_volume": 50,
    # Match the workstation's `xset r rate 200 45` configuration.
    "keyboard_repeat_delay_ms": 200,
    "keyboard_repeat_rate_hz": 45,
    "socket_path": "/run/a26-shell/control.sock",
}
# Optional clock settings: A26_SHELL_TIMEZONE is a POSIX TZ rule (for example
# EST5EDT,M3.2.0,M11.1.0) or a zone name when the rootfs has tzdata.
timezone = os.environ.get("A26_SHELL_TIMEZONE")
if timezone:
    config["timezone"] = timezone
if os.environ.get("A26_SHELL_CLOCK_24H") == "1":
    config["clock_24h"] = True

output = pathlib.Path(sys.argv[1])
output.parent.mkdir(parents=True, exist_ok=True)
output.write_text(json.dumps(config, indent=2) + "\n")
output.chmod(0o600)
