#!/usr/bin/env python3
"""A stand-in for moon-telephonyd (docs/telephony-protocol.md, revision 1.1) for Moon's off-device preview. No modem: calls and
messages are simulated, and `fake.*` commands script the other side.

  fake.incoming number=… [name=…]   a call rings (posts the ringing notification)
  fake.remote_hangup                 the other side hangs up (or stops ringing: missed)
  fake.sms peer=… text=…             a message arrives (posts its notification)
  fake.set signal=0-4 rat=… operator=… registration=… emergency_only=… sim=… airplane=… data=…
  (the A26 today: fake.set sim=absent registration=none emergency_only=true signal=null)
  fake.answer_after=SECONDS          outgoing calls are answered after this (default 3; 0 never)

usage: fake-telephonyd.py SOCKET [NOTIFY_SOCKET]
"""
import asyncio, json, re, sys, time, itertools

SOCKET = sys.argv[1] if len(sys.argv) > 1 else "/tmp/moon-ui/telephony.sock"
NOTIFY = sys.argv[2] if len(sys.argv) > 2 else "/tmp/moon-ui/notify.sock"
message_ids = itertools.count(100)
call_ids = itertools.count(1)
subscribers = set()
answer_after = 3.0


def now():
    return int(time.time())


state = {
    "available": True,
    "radio": "on",
    "airplane": False,
    "sim": {"slot": 1, "state": "ready", "pin_retries": None,
            "slots": [{"physical": 1, "logical": 1, "card": "present", "esim": False, "active": True}]},
    "network": {"registration": "home", "emergency_only": False, "data_registration": "home", "operator": "Freedom",
                "numeric": "30249", "rat": "lte", "ims_registered": False,
                "cell": {"mcc": "302", "mnc": "490", "area": 12345, "id": 67890}},
    "signal": {"level": 3, "dbm": -97},
    "data": {"enabled": True, "state": "connected", "apn": "internet.freedommobile.ca",
             "interface": "rmnet0", "addresses": ["10.12.34.56/30"], "dns": [], "error": None},
    "calls": [],
    "sms": {"unread": 0},
}
audio = {"speaker": False, "mute": False}
messages = []   # oldest first
history = []    # newest first


def seed():
    t = now()
    convo = [
        ("+16475551234", "in", "Are you still coming tonight?", 5400),
        ("+16475551234", "out", "Yes! Leaving in 10", 5300),
        ("+16475551234", "in", "Perfect, see you soon", 5200),
        ("+14165550199", "out", "Can you send me the notes from today?", 90000),
        ("+14165550199", "in", "Sure, give me a sec", 89000),
        ("+14165550199", "in", "Done, check your email 📚", 88000),
        ("32665", "in", "Freedom: Your bill of $40.00 is ready. Reply HELP for help.", 260000),
    ]
    for peer, direction, text, ago in convo:
        messages.append({"id": f"m{next(message_ids)}", "peer": peer, "direction": direction,
                         "text": text, "time": t - ago,
                         "status": "received" if direction == "in" else "delivered",
                         "read": True, "error": None})
    calls = [
        ("+16475551234", "incoming", 3600, 184, False, False),
        ("+14165550199", "outgoing", 20000, 62, False, False),
        ("+18005550100", "incoming", 70000, 0, True, False),
        ("+16475551234", "outgoing", 150000, 0, False, False),
        ("+14165550199", "incoming", 400000, 1240, False, False),
    ]
    for number, direction, ago, duration, missed, rejected in calls:
        started = t - ago
        history.append({"id": f"c{next(call_ids)}", "number": number, "name": None,
                        "direction": direction, "started_at": started,
                        "connected_at": started + 5 if duration else None,
                        "ended_at": started + 5 + duration, "duration_s": duration,
                        "missed": missed, "rejected": rejected})


class Err(Exception):
    def __init__(self, code, message):
        self.code, self.message = code, message


def emit(event):
    line = (json.dumps(event) + "\n").encode()
    for writer in list(subscribers):
        try:
            writer.write(line)
        except Exception:
            subscribers.discard(writer)


def unread():
    state["sms"]["unread"] = sum(1 for m in messages if m["direction"] == "in" and not m["read"])


def state_changed():
    unread()
    emit({"event": "state_changed", "state": state})


def calls_changed():
    emit({"event": "calls_changed", "calls": state["calls"]})


async def notify(cmd, **args):
    try:
        reader, writer = await asyncio.open_unix_connection(NOTIFY)
        writer.write((json.dumps({"id": 1, "cmd": cmd, **args}) + "\n").encode())
        await writer.drain()
        await asyncio.wait_for(reader.readline(), 1)
        writer.close()
    except Exception as error:
        print("notify failed:", cmd, error, flush=True)


def tag(prefix, peer):
    """A notification tag for a peer: moon-notifyd allows [A-Za-z0-9._-]."""
    return (prefix + re.sub(r"[^A-Za-z0-9._-]", "_", peer))[:64]


def spawn(coro):
    asyncio.get_running_loop().create_task(coro)


def find_call(predicate):
    return next((c for c in state["calls"] if predicate(c)), None)


def end_call(call, missed=False, rejected=False):
    state["calls"].remove(call)
    ended = now()
    connected = call.get("connected_at")
    duration = ended - connected if connected else 0
    history.insert(0, {"id": call["id"], "number": call["number"], "name": call["name"],
                       "direction": call["direction"], "started_at": call["started_at"],
                       "connected_at": connected, "ended_at": ended, "duration_s": duration,
                       "missed": missed, "rejected": rejected})
    del history[500:]
    done = dict(call, state="ended")
    emit({"event": "call_ended", "call": done, "duration_s": duration, "missed": missed})
    emit({"event": "call_history_changed", "ids": [call["id"]]})
    calls_changed()
    if call["direction"] == "incoming":
        spawn(notify("notify.withdraw", app="phone", tag=f"call-{call['id']}"))
        if missed:
            peer = call["number"] or "Unknown"
            spawn(notify("notify.post", app="phone", tag=tag("missed-", peer), title="Missed call",
                         body=peer, urgency="normal",
                         open={"app": "phone", "target": f"call:{peer}"}))
    if not state["calls"]:
        audio.update(speaker=False, mute=False)


async def progress_outgoing(call_id):
    await asyncio.sleep(1.0)
    call = find_call(lambda c: c["id"] == call_id)
    if not call or call["state"] != "dialing":
        return
    call["state"] = "alerting"
    calls_changed()
    if answer_after <= 0:
        return
    await asyncio.sleep(answer_after)
    call = find_call(lambda c: c["id"] == call_id)
    if not call or call["state"] != "alerting":
        return
    call["state"] = "active"
    call["connected_at"] = now()
    calls_changed()


async def ring_timeout(call_id):
    await asyncio.sleep(120)
    call = find_call(lambda c: c["id"] == call_id and c["state"] in ("incoming", "waiting"))
    if call:
        end_call(call, missed=True)


async def progress_sms(message, fail):
    await asyncio.sleep(1.0)
    message["status"] = "failed" if fail else "sent"
    if fail:
        message["error"] = "radio_error: generic_failure"
    emit({"event": "sms_updated", "message": message})
    if fail:
        return
    await asyncio.sleep(1.5)
    message["status"] = "delivered"
    emit({"event": "sms_updated", "message": message})


def check(req, allowed):
    extra = set(req) - set(allowed) - {"id", "cmd"}
    if extra:
        raise Err("invalid_argument", f"unknown fields {sorted(extra)}")


def thread_list():
    threads = {}
    for m in messages:
        t = threads.setdefault(m["peer"], {"peer": m["peer"], "last": m, "count": 0, "unread": 0})
        t["count"] += 1
        if m["time"] >= t["last"]["time"]:
            t["last"] = m
        if m["direction"] == "in" and not m["read"]:
            t["unread"] += 1
    return sorted(threads.values(), key=lambda t: t["last"]["time"], reverse=True)


def selected(req):
    if "peer" in req:
        return [m for m in messages if m["peer"] == req["peer"]]
    if "ids" in req:
        return [m for m in messages if m["id"] in req["ids"]]
    raise Err("invalid_argument", "peer or ids")


def handle(req, writer):
    global answer_after
    cmd = req.get("cmd")
    if cmd == "ping":
        return {"version": 1}
    if cmd == "subscribe":
        subscribers.add(writer)
        unread()
        spawn(send_later(writer, {"event": "snapshot", "state": state, "now": now()}))
        return {}
    if cmd == "status":
        unread()
        return state
    if cmd == "sms.threads":
        return {"threads": thread_list()}
    if cmd == "sms.list":
        check(req, ["peer", "before", "limit"])
        # Newest first; messages are stored oldest first (ties keep that order).
        items = [m for m in reversed(messages) if m["peer"] == req["peer"]]
        items.sort(key=lambda m: m["time"], reverse=True)
        if req.get("before"):
            index = next((i for i, m in enumerate(items) if m["id"] == req["before"]), None)
            if index is None:
                raise Err("not_found", "before")
            items = items[index + 1:]
        return {"messages": items[:int(req.get("limit", 50))]}
    if cmd == "sms.send":
        check(req, ["to", "text"])
        if not req["to"] or not req["text"]:
            raise Err("invalid_argument", "to and text")
        message = {"id": f"m{next(message_ids)}", "peer": req["to"], "direction": "out",
                   "text": req["text"][:1600], "time": now(), "status": "sending",
                   "read": True, "error": None}
        messages.append(message)
        emit({"event": "sms_updated", "message": message})
        spawn(progress_sms(message, req["to"].endswith("666")))
        return {"message": message}
    if cmd == "sms.mark_read":
        changed = [m for m in selected(req) if not m["read"]]
        for m in changed:
            m["read"] = True
        if changed:
            emit({"event": "sms_read", "ids": [m["id"] for m in changed]})
            for peer in {m["peer"] for m in changed}:
                spawn(notify("notify.withdraw", app="messages", tag=tag("sms-", peer)))
            state_changed()
        return {"count": len(changed)}
    if cmd == "sms.delete":
        gone = selected(req)
        for m in gone:
            messages.remove(m)
        if gone:
            emit({"event": "sms_deleted", "ids": [m["id"] for m in gone]})
            state_changed()
        return {"count": len(gone)}
    if cmd == "call.dial":
        check(req, ["number"])
        if state["sim"]["state"] != "ready":
            raise Err("unavailable", "no SIM ready")
        if find_call(lambda c: c["state"] in ("dialing", "alerting", "active")):
            raise Err("busy", "a call is in progress")
        call = {"id": f"c{next(call_ids)}", "number": req["number"], "name": None,
                "state": "dialing", "direction": "outgoing", "multiparty": False,
                "started_at": now(), "connected_at": None}
        state["calls"].append(call)
        calls_changed()
        spawn(progress_outgoing(call["id"]))
        return {}
    if cmd == "call.answer":
        call = find_call(lambda c: c["state"] in ("incoming", "waiting"))
        if not call:
            raise Err("invalid_state", "no incoming call")
        for other in state["calls"]:
            if other["state"] == "active":
                other["state"] = "held"
        call["state"] = "active"
        call["connected_at"] = now()
        calls_changed()
        spawn(notify("notify.withdraw", app="phone", tag=f"call-{call['id']}"))
        return {}
    if cmd == "call.reject":
        call = find_call(lambda c: c["state"] in ("incoming", "waiting"))
        if not call:
            raise Err("invalid_state", "no incoming call")
        end_call(call, rejected=True)
        return {}
    if cmd == "call.hangup":
        check(req, ["call"])
        if "call" in req:
            call = find_call(lambda c: c["id"] == req["call"])
        else:
            call = (find_call(lambda c: c["state"] in ("active", "dialing", "alerting"))
                    or find_call(lambda c: c["state"] in ("incoming", "waiting"))
                    or find_call(lambda c: c["state"] == "held"))
        if not call:
            raise Err("invalid_state", "no call")
        end_call(call, rejected=call["state"] in ("incoming", "waiting"))
        held = find_call(lambda c: c["state"] == "held")
        if held and not find_call(lambda c: c["state"] == "active"):
            held["state"] = "active"
            calls_changed()
        return {}
    if cmd == "call.swap":
        for call in state["calls"]:
            if call["state"] == "active":
                call["state"] = "held"
            elif call["state"] == "held":
                call["state"] = "active"
        calls_changed()
        return {}
    if cmd == "call.dtmf":
        check(req, ["digits"])
        print("dtmf", req["digits"], flush=True)
        return {}
    if cmd == "call.audio":
        check(req, ["speaker", "mute"])
        for key in ("speaker", "mute"):
            if key in req:
                audio[key] = bool(req[key])
        return dict(audio)
    if cmd == "call.history":
        check(req, ["before", "limit"])
        items = history
        if req.get("before"):
            index = next((i for i, c in enumerate(items) if c["id"] == req["before"]), None)
            if index is None:
                raise Err("not_found", "before")
            items = items[index + 1:]
        return {"calls": items[:int(req.get("limit", 50))]}
    if cmd == "call.history_delete":
        check(req, ["ids", "all"])
        gone = [c for c in history if req.get("all") or c["id"] in req.get("ids", [])]
        for c in gone:
            history.remove(c)
        if gone:
            emit({"event": "call_history_changed", "ids": [c["id"] for c in gone]})
        return {"count": len(gone)}
    if cmd == "radio.set":
        state["airplane"] = bool(req["airplane"])
        state["radio"] = "off" if state["airplane"] else "on"
        state_changed()
        return {}
    if cmd in ("data.get", "data.set"):
        if "enabled" in req:
            state["data"]["enabled"] = bool(req["enabled"])
            state["data"]["state"] = "connected" if req["enabled"] else "off"
            state_changed()
        return {"data": state["data"], "apn": {"apn": state["data"]["apn"], "protocol": "IPV4V6",
                                                "user": "", "auth": 0}}
    if cmd == "fake.incoming":
        number = req.get("number") or None
        waiting = bool(find_call(lambda c: c["state"] in ("active", "held")))
        call = {"id": f"c{next(call_ids)}", "number": number, "name": req.get("name"),
                "state": "waiting" if waiting else "incoming", "direction": "incoming",
                "multiparty": False, "started_at": now(), "connected_at": None}
        state["calls"].append(call)
        emit({"event": "call_incoming", "call": call})
        calls_changed()
        spawn(notify("notify.post", app="phone", tag=f"call-{call['id']}", title="Incoming call",
                     body=number or "Unknown", urgency="high", ringing=True,
                     open={"app": "phone", "target": f"call:{number or ''}"}))
        spawn(ring_timeout(call["id"]))
        return {"call": call}
    if cmd == "fake.remote_hangup":
        call = (find_call(lambda c: c["state"] in ("incoming", "waiting"))
                or find_call(lambda c: c["state"] in ("active", "alerting", "dialing", "held")))
        if not call:
            raise Err("invalid_state", "no call")
        end_call(call, missed=call["state"] in ("incoming", "waiting"))
        return {}
    if cmd == "fake.sms":
        message = {"id": f"m{next(message_ids)}", "peer": req["peer"], "direction": "in",
                   "text": req["text"], "time": now(), "status": "received", "read": False,
                   "error": None}
        messages.append(message)
        emit({"event": "sms_received", "message": message})
        state_changed()
        spawn(notify("notify.post", app="messages", tag=tag("sms-", req["peer"]), title=req["peer"],
                     body=req["text"], urgency="normal",
                     open={"app": "messages", "target": f"thread:{req['peer']}"}))
        return {"message": message}
    if cmd == "fake.set":
        if "signal" in req:
            level = req["signal"]
            state["signal"] = None if level is None else {"level": int(level), "dbm": -120 + 12 * int(level)}
        for key in ("rat", "operator", "registration", "emergency_only"):
            if key in req:
                state["network"][key] = req[key]
        if "sim" in req:
            state["sim"]["state"] = req["sim"]
            state["sim"]["slots"][0]["card"] = "absent" if req["sim"] == "absent" else "present"
        if "airplane" in req:
            state["airplane"] = bool(req["airplane"])
            state["radio"] = "off" if state["airplane"] else "on"
        if "data" in req:
            state["data"]["state"] = req["data"]
        if "available" in req:
            state["available"] = bool(req["available"])
        state_changed()
        return state
    if cmd == "fake.answer_after":
        answer_after = float(req["seconds"])
        return {}
    raise Err("unknown_command", str(cmd))


async def send_later(writer, event):
    await asyncio.sleep(0)
    writer.write((json.dumps(event) + "\n").encode())


async def client(reader, writer):
    try:
        while line := await reader.readline():
            try:
                req = json.loads(line)
                if req.get("cmd") not in ("status", "ping"):
                    print("cmd", json.dumps(req), flush=True)
                result = handle(req, writer)
                reply = {"id": req.get("id"), "ok": True, "result": result}
            except Err as error:
                reply = {"id": req.get("id"), "ok": False,
                         "error": {"code": error.code, "message": error.message}}
            except Exception as error:
                reply = {"id": None, "ok": False,
                         "error": {"code": "invalid_request", "message": repr(error)}}
            writer.write((json.dumps(reply) + "\n").encode())
            await writer.drain()
    except Exception:
        pass
    finally:
        subscribers.discard(writer)


async def main():
    import os
    try:
        os.unlink(SOCKET)
    except FileNotFoundError:
        pass
    seed()
    server = await asyncio.start_unix_server(client, SOCKET)
    print("fake-telephonyd on", SOCKET, flush=True)
    async with server:
        await server.serve_forever()


asyncio.run(main())
