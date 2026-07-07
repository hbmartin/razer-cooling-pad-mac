# padctl

CLI to control the fans and RGB lighting of a **Razer Laptop Cooling Pad**
(USB `1532:0f43`) on macOS, where Razer ships no support for it.

Talks to the pad's HID control interface (interface 0) via feature reports —
no drivers, no kexts, no root. Protocol replicated byte-for-byte from two
known-good implementations: a Windows
[FanControl](https://github.com/Rem0o/FanControl.Releases) plugin for the fan
commands, and [openrazer](https://github.com/openrazer/openrazer)'s accessory
driver (`driver/razerchromacommon.c`) for the lighting commands.

## Install

Homebrew (tap):

```bash
brew tap hbmartin/tap
brew install padctl
```

crates.io:

```bash
cargo install padctl
```

Prebuilt binary — tagged releases attach a universal (arm64 + x86_64) macOS
binary. It is ad-hoc signed, so if you download it **via a browser**
Gatekeeper will quarantine it; clear the flag once:

```bash
tar xzf padctl-*-macos-universal.tar.gz
xattr -d com.apple.quarantine padctl   # only needed for browser downloads
```

(`curl`/`brew`/`cargo` installs don't set the quarantine attribute.)

From source:

```bash
cargo build --release   # -> target/release/padctl (self-contained)
```

Shell completions and a man page are built in:

```bash
padctl completions zsh > ~/.zfunc/_padctl     # also bash/fish/elvish/powershell
padctl manpage > /usr/local/share/man/man1/padctl.1
```

## Usage

```bash
padctl list                        # show the pad's HID interfaces (+ serials)
padctl info                        # firmware version + serial
padctl status                      # fan, brightness, firmware, serial, CPU temp

# Fans (500-3200 RPM, 50 RPM steps)
padctl fan set 1500
padctl fan set 60%
padctl fan set off                 # also: 0 or 0%
padctl fan get
padctl fan off

# Lighting (18-LED strip)
padctl rgb static ff6600
padctl rgb spectrum
padctl rgb wave --dir left --speed 40
padctl rgb breath 0000ff           # 0 colors = random, 1 = single, 2 = dual
padctl rgb brightness 75           # no value: read current
padctl rgb off

# Per-LED custom frames (experimental on this device, see below)
padctl rgb custom ff0000 00ff00 0000ff   # 1-18 colors, stretched to fit
padctl rgb gradient 0000ff ff0000        # linear gradient across the strip
padctl rgb thermal                       # live CPU-temp meter, green→red

# Temperature
padctl temp                        # the reading the fan curve would use
padctl sensors                     # every sensor padctl can see

# Machine-readable output for scripting (also: info, temp, sensors, fan get)
padctl status --json | jq .cpu_temp_celsius

# Automatic fan curve from CPU temperature (Ctrl-C to stop)
padctl curve                       # defaults: 55:800,65:1500,75:2200,85:3200
padctl curve --points "50:0,60:1200,75:2400,85:3200" --interval 5 --on-exit off
padctl curve --smooth 15 --down-delay 30
padctl curve --dry-run             # print decisions without touching the pad

# Live dashboard for the curve, with in-place tuning (same flags as curve)
padctl watch
padctl watch --dry-run             # observe/tune without touching the pad

# Protocol exploration: send a raw 90-byte packet (zero-padded)
padctl raw "00 1f 00 00 00 03 0f 84 01 00" --auto-crc --read
```

Global flags on every command:

- `-v` enables debug logging: raw packets sent/received, every curve poll.
- `--verify` reads back the device status after each command and fails
  loudly if the device rejected it (best effort).
- `--serial <S>` / `--path <P>` select a specific pad when more than one is
  plugged in (values shown by `padctl list`).

## The fan curve

`padctl curve` reads CPU die temperature from the SMC sensors (the same
private `IOHIDEventSystemClient` API used by macmon/stats — no root needed).
If that API is unavailable it falls back to coarse thermal-pressure
estimates. A curve point with RPM `0` means "fans off"; interpolated targets
below 500 RPM also turn the fans off.

The loop is built to run unattended:

- **Smoothing** — readings go through an exponential moving average
  (`--smooth`, default 15 s time constant; 0 disables) so brief load spikes
  don't rev the fans.
- **Asymmetric ramping** — spin-up is immediate, spin-down waits until the
  lower target persists (`--down-delay`, default 30 s; 0 disables), on top
  of a 100 RPM hysteresis band. No more oscillating fan noise.
- **Reconnect** — if the pad is unplugged, replugged, or the machine sleeps,
  the curve keeps running and re-attaches automatically.
- **Signals** — SIGINT/SIGTERM/SIGHUP all trigger the `--on-exit` behavior
  (`off` by default), so `launchctl bootout` and plain `kill` are safe.
- **Quiet logs** — every line is timestamped, but only *decision changes*
  are logged (a new speed sent, or the start of a pending spin-down), plus a
  status heartbeat every 10 minutes so the log shows the loop is alive. Run
  with `-v` to see every poll. At the default 5 s poll interval this keeps
  an unattended log file to a few lines per hour instead of thousands.

## Tune it live (`padctl watch`)

`padctl watch` runs the exact same control loop as `padctl curve` (same
flags, same config file, same smoothing/governor logic) but renders it as a
live terminal dashboard instead of log lines: raw vs smoothed temperature,
target vs actual RPM (read back from the pad), the curve shape with the
current operating point, and a scrolling panel of recent decisions.

Tuning happens in place — no restart cycles:

| Key | Action |
| --- | --- |
| `S` / `s` | smoothing time constant +1 s / −1 s (0 disables) |
| `D` / `d` | spin-down delay +5 s / −5 s (0 disables) |
| `Tab` / `Shift-Tab` | select a curve point |
| `↑` / `↓` | selected point RPM ±50 (below 500 snaps to off) |
| `←` / `→` | selected point temperature ±1 °C |
| `w` | write the current tuning to `~/.config/padctl/config.toml` |
| `r` | reset tuning to the startup values |
| `Space` | pause/resume the control loop |
| `q` / `Esc` / `Ctrl-C` | quit (honors `--on-exit`, default `off`) |

Changes take effect on the next control tick (forced immediately on every
keypress). `w` updates only the `[curve]` tuning keys and keeps the rest of
the config file — comments, `[lighting]` — intact; if you quit with unsaved
changes, the equivalent `--points/--smooth/--down-delay` flags are printed
so nothing is lost. With `--dry-run` the dashboard never touches the pad,
so it's safe to run alongside the launchd service while experimenting
(without `--dry-run`, stop the service first — two writers would fight
over the fan). `-v` records every poll in the decisions panel, not just
decision changes.

## Run it at login (launchd service)

```bash
padctl config init                 # write ~/.config/padctl/config.toml
padctl service install             # LaunchAgent: runs `padctl curve` at login
padctl service status
tail -f ~/Library/Logs/padctl.log
padctl service uninstall
```

The service runs plain `padctl curve`, which reads its settings from
`~/.config/padctl/config.toml` — edit the config (see `padctl config show`)
instead of baking flags into the plist. CLI flags override the config for
interactive runs. Install from a stable binary location (e.g.
`/usr/local/bin/padctl`), not a build tree.

## Lighting at login

The config file's `[lighting]` section describes the lighting you want, and
the fan curve applies it on startup and on every reconnect — so the launchd
service restores your preferred look at login and after the pad is
unplugged/replugged (the pad forgets custom frames when it loses power):

```toml
[lighting]
effect = "static"        # off | static | spectrum | wave | breath | gradient | custom
colors = ["ff6600"]      # static: 1, breath: 0-2, gradient: 2, custom: 1-18
brightness = 80          # 0-100, applied before the effect
#wave_dir = "right"      # wave only
#wave_speed = 40         # wave only
#driver_mode = false     # gradient/custom only (experimental effects)
```

Apply it on demand without touching the fans:

```bash
padctl rgb apply
```

## Custom frames / thermal lighting (experimental)

`rgb custom`, `rgb gradient`, and `rgb thermal` drive the strip per-LED via
extended-matrix custom frames. The packet layout follows openrazer's closest
relatives of this pad (Laptop Stand Chroma, Base Station V2 Chroma —
transaction id `0x1F`, dynamic packet length), but the cooling pad itself has
no upstream custom-frame implementation to copy, so treat these as
experimental. If the lighting doesn't change, retry with `--driver-mode`,
which switches the device to driver mode first (normal mode is restored on
exit).

`rgb thermal` polls the CPU temperature and renders it live: `--style meter`
(default) lights 0-18 LEDs green→red like a level meter, `--style solid`
shifts the whole strip's color.

## Linux

macOS is the primary target, but the crate builds, lints, and tests on Linux
(hidraw backend, temperatures from `/sys/class/thermal`), which is also what
CI uses for most checks. Building needs `libudev-dev` and `pkg-config`.
Talking to the pad on Linux requires a udev rule granting access to the
hidraw node, and `padctl service` is macOS-only.

## Development

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

CI (GitHub Actions):

- **Lint** and **Linux build/test** run on GitHub-hosted `ubuntu-latest`.
- **macOS build/test/clippy** — the parts Linux can't check (IOKit
  temperature source, launchd service, mac hidapi backend) — run on a
  **self-hosted macOS runner**. Register one under *Settings → Actions →
  Runners* with the default `self-hosted` + `macOS` labels; the jobs are
  guarded so fork PRs never reach it.
- **Release**: pushing a `v*` tag builds a universal binary on the mac
  runner and attaches it to a GitHub release. Two follow-up jobs then run on
  GitHub-hosted runners, each skipping itself cleanly if its secret is not
  configured: publishing the crate to crates.io (`CARGO_REGISTRY_TOKEN`) and
  pushing an updated formula to `hbmartin/homebrew-tap`
  (`HOMEBREW_TAP_TOKEN`, a PAT with push access to the tap).
- **Dependabot** files weekly PRs for Cargo dependencies and pinned
  workflow actions.

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
  2=breath, 3=spectrum, 4=wave+dir+speed)
- Custom frame store: `0x0f/0x03`, args `00 00 <row> <start> <stop> <rgb...>`,
  data size = 3×LEDs + 5; apply with effect `0x08` (args `00 00 08`, ds `0x0c`)
- Brightness: `0x0f/0x04` set / `0x0f/0x84` get, args `01 00 <0-255>`
- Device mode: `0x00/0x04`, args `<mode> 00` (0=normal, 3=driver)
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
  the pad as a last resort. `--verify` makes rejects visible.
- **No visible lighting change** — brightness may be persisted at 0:
  `padctl rgb brightness 100` first.
- **Custom frames show nothing** — try `--driver-mode` (see above).

## License

[MIT](LICENSE)
