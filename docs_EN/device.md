# Device module

Source: [`src/device.rs`](../src/device.rs) · Standalone binary:
[`src/bin/device.rs`](../src/bin/device.rs)

This module owns **RX-device state**: who generates the traffic the ESP32 is
capturing, and at what target rate. It is the only module that talks to the
device about its mode, and the only place in the crate that *writes* to it.

```text
GET  /espectre/v1/sensing ──parse_status──▶ DeviceStatus ─┬─ is_internal()
                                                          ├─ traffic_generator()
                                                          └─ csi_target_pps
PATCH /espectre/v1/sensing ◀── set_traffic_generator_mode ── (CLI only)
```

## Two modes, one meaning

| Device `traffic_generator_mode` | [`TrafficGenerator`] | Who sends the packets | Capture flag |
|---|---|---|---|
| `external` | `External` | This crate, as 4-byte UDP markers | `collect --pps N` |
| `ping`, `dns`, `dns_tcp`, `wifi_raw`, any future value | `Internal` | The device itself | `collect --internal` |

`is_internal()` treats **every** value other than `external` as internal,
including modes this crate does not know, because all of them mean "the device
sends the packets". A firmware that adds a new generator therefore keeps
working instead of being misread as external.

## Reading

### `read(endpoint) -> Option<DeviceStatus>`

One `GET /sensing`. `None` on any transport error, non-2xx status, or
unparseable body — callers treat `None` as **unknown**, not as a failure, so an
unreachable device only warns and a capture can still be produced and analysed
offline.

### `parse_status(body) -> Option<DeviceStatus>`

Split out of `read` so parsing is testable without a network. Unknown fields are
ignored (as the ESPectre API requires of clients); only `traffic_generator_mode`
and `csi_target_pps` are looked up. Returns `None` when the body is not a JSON
object.

### `DeviceStatus`

| Field | Type | Meaning |
|---|---|---|
| `traffic_generator_mode` | `Option<String>` | Mode reported by the firmware, verbatim |
| `csi_target_pps` | `Option<u32>` | Device target rate; `None` when absent, not an integer, or not positive |

`traffic_generator()` maps the raw string to [`TrafficGenerator`] and is `None`
when the firmware did not report a mode.

## Checking: `require_mode`

```rust
pub fn require_mode(
    status: Option<&DeviceStatus>,
    requested: TrafficGenerator,
) -> Result<(), DeviceError>
```

| Device status | Requested | Result | Why |
|---|---|---|---|
| unreadable (`None`) | any | `Ok(())` | Unknown is not wrong; a transient network error must not become fatal |
| no `traffic_generator_mode` field | any | `Ok(())` | Older firmware; nothing to contradict |
| `external` | `External` | `Ok(())` | Match |
| `ping` / `dns` / … | `Internal` | `Ok(())` | Match |
| `external` | `Internal` | `Err(Mismatch)` | The device would not generate the traffic being captured |
| `ping` / `dns` / … | `External` | `Err(Mismatch)` | A second, uncounted stimulus on top of the markers |

Callers must treat a mismatch as fatal and stop **before** opening the CSI
stream, so no capture file is created for a run that could not have recorded
anything. [`Mismatch::remedy`] prints the exact `device --mode …` command to
run, so the message and the fix cannot drift apart.

## Writing: `set_traffic_generator_mode`

```rust
pub async fn set_traffic_generator_mode(
    endpoint: &DeviceEndpoint,
    mode: &str,
) -> Result<DeviceStatus, DeviceError>
```

The **only** write in the crate; nothing in the capture path calls it. It:

1. rejects a mode that is not in [`TRAFFIC_GENERATOR_MODES`] *locally*, so a typo
   can never reach the device's NVS store;
2. sends `PATCH /sensing` with `{"traffic_generator_mode": mode}`;
3. re-reads the device instead of trusting the response — the PATCH reply is an
   acknowledgement, not proof. A failed re-read is `Unconfirmed` (outcome
   unknown), never "the write did not take effect".

## Errors

| Variant | Cause |
|---|---|
| `Http(reqwest::Error)` | A request failed or returned a non-success status |
| `Mismatch(Box<Mismatch>)` | The device's mode contradicts the requested one |
| `UnknownMode { mode }` | The requested mode is not one the device accepts |
| `Unconfirmed { mode }` | The write was accepted but the device could not be re-read |

Implements `Display` and `std::error::Error`; `?` works where
`DeviceError: Into<E>`.

## Standalone binary

```bash
# Read the mode and rate, change nothing
cargo run --bin device -- --ip 192.168.12.144 --show

# Switch the device to its own generator, then read it back
cargo run --bin device -- --ip 192.168.12.144 --mode ping

# Switch it back to external traffic (a desktop sends the markers)
cargo run --bin device -- --ip 192.168.12.144 --mode external

# Mode name only, for scripts
cargo run --bin device -- --show --quiet
```

Options: `--show`, `--mode MODE`, `--ip`, `--api-port`, `--origin`, `--quiet`,
`-h/--help`. Exactly one of `--show` / `--mode` is required.

Exit codes: `0` success, `2` bad arguments or unknown mode, `1` request failed,
or the write did not take effect.

`--show` prints four lines — device, traffic mode with a parenthetical saying
what it implies, target rate, and the **next step** (the `collect` command that
matches the current mode).

## Why the capture path never writes

The mode survives a reboot and changes what every measurement means, so it is an
operator decision, not a side effect of measuring. `collect`, `streaming` and the
default binary read `GET /sensing` and refuse a mismatched run; only
`cargo run --bin device -- --mode …` writes. That keeps a capture reproducible:
the same command against the same device mode produces the same kind of data,
and a wrong mode stops the run instead of silently recording the wrong stimulus.

## Requirements

- The ESP32 must be reachable at `host:api_port` and running ESPectre with a
  `/sensing` endpoint (API version that reports `traffic_generator_mode`).
- `Origin: https://test.espectre.dev` is sent with every request; override with
  `--origin` if the firmware validates a different one.
