# padctl

CLI to control the fans and RGB lighting of a **Razer Laptop Cooling Pad**
(USB `1532:0f43`) on macOS, where Razer ships no support for it.

Talks to the pad's HID control interface (interface 0) via feature reports —
no drivers, no kexts, no root. Protocol replicated byte-for-byte from two
known-good implementations (see `reference/`): a Windows
[FanControl](https://github.com/Rem0o/FanControl.Releases) plugin for the fan
commands, and [openrazer](https://github.com/openrazer/openrazer)'s accessory
driver for the lighting commands.

## Build

```bash
cargo build --release   # -> target/release/padctl (self-contained)
```

## Usage

```bash
padctl list                        # show the pad's HID interfaces
padctl info                        # firmware version + serial

# Fans (500-3200 RPM, 50 RPM steps)
padctl fan set 1500
padctl fan set 60%
padctl fan get
padctl fan off

# Lighting (18-LED strip)
padctl rgb static ff6600
padctl rgb spectrum
padctl rgb wave --dir left
padctl rgb breath 0000ff           # 0 colors = random, 1 = single, 2 = dual
padctl rgb brightness 75           # no value: read current
padctl rgb off

# Automatic fan curve from CPU temperature (Ctrl-C to stop)
padctl curve                       # defaults: 55:800,65:1500,75:2200,85:3200
padctl curve --points "50:0,60:1200,75:2400,85:3200" --interval 5 --on-exit off
padctl curve --dry-run             # print decisions without touching the pad

# Protocol exploration: send a raw 90-byte packet (zero-padded)
padctl raw "00 1f 00 00 00 03 0f 84 01 00" --auto-crc --read
```

`-v` on any command dumps the raw packets sent/received.

The curve reads CPU die temperature from the SMC sensors (the same private
`IOHIDEventSystemClient` API used by macmon/stats — no root needed). If that
API is unavailable it falls back to coarse thermal-pressure estimates. A curve
point with RPM `0` means "fans off"; interpolated targets below 500 RPM also
turn the fans off.

## Protocol notes

Every command is a 91-byte HID feature report: report id `0x00` + a 90-byte
Razer packet:

| bytes  | meaning                                              |
|--------|------------------------------------------------------|
| 0      | status (send `0x00`; response `0x02`=OK, `0x01`=busy)|
| 1      | transaction id (`0x02` fan, `0x1f` rgb/info)         |
| 2–4    | remaining packets + protocol type (zero)             |
| 5      | data size                                            |
| 6, 7   | command class, command id                            |
| 8–87   | arguments                                            |
| 88     | crc = XOR of bytes 2..=87                            |
| 89     | `0x00`                                               |

Known commands (class/cmd, arguments from byte 8):

- Fan set: `0x0d/0x01`, args `01 05 <lo> <hi>` where the 16-bit value is RPM/50
- Fan off: `0x0d/0x10`, args `00 06`
- Fan read: plain GetFeature; RPM/50 at packet bytes 10–11
- RGB effects: `0x0f/0x02`, args `01 00 <effect> ...` (0=off, 1=static+RGB,
  2=breath, 3=spectrum, 4=wave+dir+0x28)
- Brightness: `0x0f/0x04` set / `0x0f/0x84` get, args `01 00 <0-255>`
- Firmware `0x00/0x81`, serial `0x00/0x82` (query then read)

Queries (`0x8x` commands) are sent, then the response is read back with
GetFeature after ~100 ms; busy responses are retried.

## Troubleshooting

- **Open fails with a permission error** — grant your terminal app *Input
  Monitoring* in System Settings → Privacy & Security (usually not needed:
  the control interface has no input reports).
- **Pad not found** — check `system_profiler SPUSBDataType | grep -A6 Cooling`.
- **Commands accepted but nothing happens** — quit Razer Synapse
  (`RazerAppEngine`) in case it is fighting over the device; unplug/replug
  the pad as a last resort.
- **No visible lighting change** — brightness may be persisted at 0:
  `padctl rgb brightness 100` first.
