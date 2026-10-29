# Plan: Decode ESPectre V8 CSI Capture in Rust

## 1. Objective

Implement a reliable Rust decoder for `capture_v8.bin`, a raw binary
capture saved from the ESPectre CSI HTTP endpoint. The decoder should:

1.  Parse the ESPectre HTTP frame envelope.
2.  Extract and validate each V8 CSI record.
3.  Decode the CSI metadata and raw I/Q payload.
4.  Report malformed or truncated data without silently desynchronizing.
5.  Produce a transport-neutral representation suitable for later DSP
    and AI inference.

This plan covers decoding only. DSP feature extraction, localization,
and AI inference are out of scope.

## 2. Evidence for the V8 frame layout

### 2.1 ESPectre repository definitions

The following definitions were found in ESPectre's
`tools/lib/csi_io.py`:

-   `CSI_HEADER_FORMAT = "<HBBBBIHHQQIQBbbQIIBBB"`
-   `CSI_HEADER_STRUCT = struct.Struct(CSI_HEADER_FORMAT)`
-   `RAW_CSI_RECORD_MAGIC = 0x4353`
-   `RAW_CSI_RECORD_VERSION = 8`
-   `RAW_CSI_HTTP_FRAME_STRUCT = struct.Struct("<IBBH16sQHHQQQ")`
-   `RAW_CSI_RESPONSE_MAGIC = 0x52505345`
-   `RAW_CSI_PROTOCOL_VERSION = 1`

Repository references:

-   [`tools/lib/csi_io.py`](https://github.com/francescopace/espectre/blob/main/tools/lib/csi_io.py)
-   [ESPectre API
    documentation](https://github.com/francescopace/espectre/blob/main/docs/API.md)
-   [ESPectre CSI
    documentation](https://github.com/francescopace/espectre/blob/main/docs/CSI.md)

The `<` prefix in each Python `struct` format specifies little-endian
byte order with standard sizes and no native alignment padding.

### 2.2 CSI record header

`CSI_HEADER_FORMAT` describes a 64-byte CSI record header. Its fields,
in order, are:

  Field                     Type implied by Python format      Size
  ----------------------- ------------------------------- ---------
  `magic`                           unsigned 16-bit (`H`)   2 bytes
  `version`                          unsigned 8-bit (`B`)    1 byte
  `header_len`                       unsigned 8-bit (`B`)    1 byte
  `chip_code`                        unsigned 8-bit (`B`)    1 byte
  `flags`                            unsigned 8-bit (`B`)    1 byte
  `seq_num`                         unsigned 32-bit (`I`)   4 bytes
  `num_sc`                          unsigned 16-bit (`H`)   2 bytes
  `csi_len_bytes`                   unsigned 16-bit (`H`)   2 bytes
  `device_id`                       unsigned 64-bit (`Q`)   8 bytes
  `device_ticks_us`                 unsigned 64-bit (`Q`)   8 bytes
  `wifi_rx_ts_us`                   unsigned 32-bit (`I`)   4 bytes
  `wifi_rx_start_ts_ns`             unsigned 64-bit (`Q`)   8 bytes
  `channel`                          unsigned 8-bit (`B`)    1 byte
  `rssi_dbm`                           signed 8-bit (`b`)    1 byte
  `noise_floor_dbm`                    signed 8-bit (`b`)    1 byte
  `transport_counter`               unsigned 64-bit (`Q`)   8 bytes
  `fresh_counter`                   unsigned 32-bit (`I`)   4 bytes
  `request_counter`                 unsigned 32-bit (`I`)   4 bytes
  `phy_mode_code`                    unsigned 8-bit (`B`)    1 byte
  `ltf_type_code`                    unsigned 8-bit (`B`)    1 byte
  `channel_width_code`               unsigned 8-bit (`B`)    1 byte

The total is 64 bytes. The CSI payload follows this header and has
`csi_len_bytes` bytes. The Python parser validates that
`csi_len_bytes == num_sc * 2`.

### 2.3 HTTP frame envelope

`RAW_CSI_HTTP_FRAME_STRUCT = struct.Struct("<IBBH16sQHHQQQ")` describes
a separate 60-byte prefix around a CSI record. It must be parsed before
the CSI record itself.

Do not assume that this HTTP frame prefix and the 64-byte CSI header are
interchangeable. The HTTP frame contains transport/session/framing
metadata; the CSI header contains CSI-record metadata.

Before implementing the Rust parser, inspect the field unpacking and
`_consume_raw_frames()` / `DirectRawCSIReceiver` logic in
`tools/lib/csi_io.py` to confirm the exact meaning of every HTTP frame
field, particularly the record-length field and any flags/counters. Use
the repository's implementation as the authority rather than inferring
field meanings from their sizes alone.

## 3. Evidence from `capture_v8.bin`

The first 140 bytes were inspected with:

``` bash
xxd -g 1 -l 140 capture_v8.bin
```

The relevant bytes were:

``` text
00000000: 45 53 50 52 01 08 3c 00 d9 5c 15 bb da e4 09 e6
...
00000030: 00 00 00 00 00 00 00 00 00 00 00 00 53 43 08 40
00000040: 05 0a 01 00 00 00 40 00 80 00 ...
...
00000070: 00 01 00 00 00 01 00 00 00 03 03 01 00 00 00 00
00000080: 00 00 00 00 05 ec 06 eb 06 eb 06 ea
```

These bytes support the nested layout:

-   Offsets `0..4`: `45 53 50 52`, which is `0x52505345` when
    interpreted as a little-endian `u32`. This matches
    `RAW_CSI_RESPONSE_MAGIC`.
-   Offset `4`: `01`, matching `RAW_CSI_PROTOCOL_VERSION`.
-   Offset `6..8`: `3c 00`, which is decimal `60`, consistent with the
    HTTP frame prefix size.
-   Offset `60..62`: `53 43`, which is `0x4353` as a little-endian
    `u16`. This matches `RAW_CSI_RECORD_MAGIC`.
-   Offset `62`: `08`, matching CSI record version 8.
-   Offset `63`: `40`, which is decimal `64`, matching the CSI header
    size.
-   Offset `124` (`0x7C`): the bytes `05 ec 06 eb 06 eb 06 ea` begin the
    CSI payload, consistent with a 60-byte HTTP prefix followed by a
    64-byte CSI header.

This is strong evidence that the capture starts with a 60-byte HTTP
frame prefix followed by a V8 CSI record. It validates the first frame's
layout, but the decoder must still validate every frame throughout the
entire file.

## 4. Implementation plan

### Step 1 --- Confirm the repository's framing contract

-   Read the exact unpacking of `RAW_CSI_HTTP_FRAME_STRUCT` and the
    corresponding `_consume_raw_frames()` / `DirectRawCSIReceiver` code.
-   Record each frame field's meaning, offset, type, and validation
    rule.
-   Confirm whether the frame length field counts the CSI record bytes,
    the entire frame, or another defined portion.
-   Identify the session-ID handling, sequence-number checks, and any
    frame flags.
-   Do not begin parsing `CsiHeader` at file offset zero; the first 60
    bytes belong to the HTTP frame prefix.

**Deliverable:** a documented HTTP-frame field map backed by repository
code.

### Step 2 --- Define Rust data structures

Keep the transport envelope separate from the CSI record:

-   `HttpFrameHeader`: fields from `RAW_CSI_HTTP_FRAME_STRUCT`.
-   `CsiHeader`: the 21 fields from `CSI_HEADER_STRUCT`.
-   `CsiPacket`: decoded metadata plus `iq_raw: Vec<i8>` and optional
    derived arrays later.

Use fixed-width integer types (`u8`, `u16`, `u32`, `u64`, `i8`) that
match the Python format. Do not read the file directly into a Rust
struct by memory casting: Rust struct layout and padding are not a safe
substitute for explicit little-endian parsing.

**Deliverable:** Rust types that mirror the documented schema.

### Step 3 --- Implement little-endian parsing helpers

-   Read each integer using explicit little-endian conversion.
-   Check that the buffer contains enough bytes before reading a field
    or header.
-   Parse the 60-byte HTTP frame header.
-   Parse the 64-byte CSI header at the correct offset inside the frame.
-   Keep parsing and validation separate from file I/O.

**Deliverable:** functions that parse one HTTP frame header and one CSI
header from a byte slice.

### Step 4 --- Parse one complete frame

For each candidate frame:

1.  Check the HTTP response magic and protocol version.
2.  Validate the HTTP frame header length.
3.  Read and validate the frame's record-length field according to the
    repository contract.
4.  Ensure the buffer contains the entire declared frame/record.
5.  Check the CSI record magic, version, and `header_len`.
6.  Validate `csi_len_bytes == num_sc * 2`.
7.  Ensure the CSI payload fits entirely inside the declared record.
8.  Extract the raw CSI bytes without changing their order.
9.  Apply validity flags to optional timestamps and metadata in the same
    way as the Python reference parser.

Reject or report invalid records with useful offset/error information.
Do not silently skip bytes unless a deliberate resynchronization
strategy is implemented and tested.

**Deliverable:** a parser that returns one complete decoded record and
the number of bytes consumed.

### Step 5 --- Parse the entire file incrementally

-   Read `capture_v8.bin` as binary bytes.
-   Maintain a cursor or buffer offset.
-   Parse successive frames using the length fields; do not assume
    TCP/HTTP stream chunks align with frame boundaries.
-   Support multiple frames in one read and a frame split across reads.
-   At end-of-file, report any incomplete trailing frame instead of
    treating it as a valid record.
-   Track frame count, valid record count, invalid records, sequence
    gaps, and bytes consumed.

**Deliverable:** a file-level decoder that processes the complete
capture.

### Step 6 --- Decode I/Q representation

Follow the reference implementation in
`CSIPacket._ensure_derived_arrays()`:

-   Preserve the raw payload as signed 8-bit values.
-   The repository's current convention uses even-indexed values as Q
    and odd-indexed values as I.
-   Construct complex CSI as `I + jQ`.
-   Derive amplitude with `abs(complex)` and phase with `angle(complex)`
    only when required.

Verify this convention against the same repository code and a known
sample before relying on derived features. Do not reorder or normalize
the raw bytes during initial parsing.

**Deliverable:** raw I/Q plus optional complex, amplitude, and phase
representations.

### Step 7 --- Validate against the Python reference

-   Use the ESPectre Python parser as the reference implementation.
-   Decode the same capture with Python and Rust.
-   Compare record count, sequence numbers, header fields, payload
    lengths, raw I/Q arrays, and selected amplitude/phase values.
-   Verify the first record's offsets against the hex dump.
-   Test a truncated file, a corrupted magic, an unsupported version, an
    invalid payload length, and a file with multiple records.
-   Check for sequence gaps and consistency between declared lengths and
    actual file size.

**Deliverable:** tests showing that Rust and Python decode the same
records consistently.

### Step 8 --- Produce a useful decoded output

For the first iteration, output a concise summary per record or a
JSON/CSV metadata report containing:

-   record index and sequence number
-   device ID and timestamps, where valid
-   channel, RSSI, noise floor, and CSI freshness
-   number of subcarriers and payload length
-   validation status

Keep the full I/Q arrays in a binary or structured numeric output if
needed; avoid forcing large arrays into a CSV metadata table.

**Deliverable:** a repeatable decoder command and a validation report.

## 5. Acceptance criteria

The decoder is ready for downstream DSP work when:

-   Every frame is parsed using the repository-defined HTTP framing
    contract.
-   Every CSI record passes magic, version, header-size, and
    payload-length checks.
-   The parser handles multiple records and detects incomplete/corrupted
    data.
-   Rust output matches the Python reference for record counts,
    metadata, and raw I/Q payload.
-   The decoder reports sequence gaps and malformed records instead of
    silently hiding them.
-   Raw I/Q data is preserved exactly; derived complex/amplitude/phase
    values are computed only after the raw decode is validated.

## 6. Out of scope for this task

-   CSI denoising, filtering, Doppler/STFT, or other DSP.
-   Human activity recognition, gesture recognition, respiration
    sensing, or localization.
-   Model loading or AI inference.
-   Changing the ESPectre firmware or capture protocol.

These should be considered only after the binary decoder has been
validated.