#!/usr/bin/env python3
"""Off-device touch for Moon's evdev path (MOON_TOUCH=evdev with
MOON_TOUCH_REPLAY=FIFO:1079,2339): writes multi-touch protocol B events, as
the A26's sec_touchscreen reports them, into the FIFO at 120 Hz.

    touch-replay.py FIFO tap X Y
    touch-replay.py FIFO swipe X0 Y0 X1 Y1 [MS]
    touch-replay.py FIFO two X0 Y0 X1 Y1     two fingers down and up together
"""
import os
import struct
import sys
import time

EV_SYN, EV_KEY, EV_ABS = 0, 1, 3
SYN_REPORT, BTN_TOUCH = 0, 0x14A
ABS_MT_SLOT, ABS_MT_X, ABS_MT_Y, ABS_MT_ID = 0x2F, 0x35, 0x36, 0x39
FRAME = 1 / 120


def event(kind, code, value):
    now = time.time()
    return struct.pack("@qqHHi", int(now), int(now % 1 * 1e6), kind, code, value)


class Panel:
    def __init__(self, path):
        self.fd = os.open(path, os.O_WRONLY)
        self.next_id = int(time.time()) % 60000

    def report(self, *events):
        os.write(self.fd, b"".join(event(*e) for e in events) + event(EV_SYN, SYN_REPORT, 0))
        time.sleep(FRAME)

    def down(self, slot, x, y, first=True):
        self.next_id += 1
        evs = [(EV_ABS, ABS_MT_SLOT, slot), (EV_ABS, ABS_MT_ID, self.next_id),
               (EV_ABS, ABS_MT_X, x), (EV_ABS, ABS_MT_Y, y)]
        return evs + ([(EV_KEY, BTN_TOUCH, 1)] if first else [])

    @staticmethod
    def move(slot, x, y):
        return [(EV_ABS, ABS_MT_SLOT, slot), (EV_ABS, ABS_MT_X, x), (EV_ABS, ABS_MT_Y, y)]

    @staticmethod
    def up(slot, last=True):
        return [(EV_ABS, ABS_MT_SLOT, slot), (EV_ABS, ABS_MT_ID, -1)] + (
            [(EV_KEY, BTN_TOUCH, 0)] if last else [])


def main():
    panel = Panel(sys.argv[1])
    action, args = sys.argv[2], [int(a) for a in sys.argv[3:]]
    if action == "tap":
        x, y = args
        panel.report(*panel.down(0, x, y))
        panel.report(*panel.move(0, x, y))
        panel.report(*panel.up(0))
    elif action == "swipe":
        x0, y0, x1, y1 = args[:4]
        steps = max(int((args[4] if len(args) > 4 else 250) / 1000 / FRAME), 2)
        panel.report(*panel.down(0, x0, y0))
        for i in range(1, steps + 1):
            t = 1 - (1 - i / steps) ** 2  # ease out, like a finger slowing down
            panel.report(*panel.move(0, round(x0 + (x1 - x0) * t), round(y0 + (y1 - y0) * t)))
        panel.report(*panel.up(0))
    elif action == "two":
        x0, y0, x1, y1 = args
        panel.report(*panel.down(0, x0, y0), *panel.down(1, x1, y1, first=False))
        panel.report(*panel.up(0, last=False), *panel.up(1))
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main()
