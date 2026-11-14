# Module documentation

The Rust crate is split into pipeline modules plus the shared record schema.
Each module has its own file in [`src/`](../src) and a standalone binary for
command-line use.

| Module | Source | Purpose | Standalone command | Docs |
|---|---|---|---|---|
| Collection | `src/collection.rs` | Stream raw CSI from ESPectre into a `.bin` | `cargo run --bin collect -- --help` | [collection.md](collection.md) |
| Decoder | `src/decoder.rs` | Parse and validate V8 CSI frames | `cargo run --bin decode -- capture_v8.bin` | [decoder.md](decoder.md) |
| Saving | `src/saving.rs` | Export timestamp/amplitude/phase to `.npz` | `cargo run --bin save -- capture_v8.bin` | [saving.md](saving.md) |
| Visualization | `src/visualization.rs` | Render 2D CSI amplitude heatmap dashboard | `cargo run --bin visualize -- capture_v8.bin` | [visualization.md](visualization.md) |
| Streaming | `src/streaming.rs` | Collect, decode live, and serve a 30 s sliding heatmap | `cargo run --bin streaming -- --help` | [streaming.md](streaming.md) |
| Schema | `src/csi_header.rs` | 64-byte V8 record header fields and constants | — | [decoder.md](decoder.md) |

## Typical flow

```bash
# 1. Record raw frames (default main does the same with no arguments)
cargo run --bin collect -- --ip 10.178.22.9 --output capture_v8.bin

# 2. Inspect the stream (optional)
cargo run --bin decode -- capture_v8.bin --quiet

# 3. Export numeric arrays for analysis
cargo run --bin save -- capture_v8.bin

# 4. Visualize the 2D amplitude heatmap
cargo run --bin visualize -- capture_v8.bin --open

# 5. Or collect and visualize live at the same time
cargo run --bin streaming -- --ip 10.178.22.9 --output capture_v8.bin --pps 100 --port 8080
```

The default binary (`cargo run` / `cargo run --bin rt-wireless-sensing`) calls
the collection module with `CollectionConfig::default()`.

## Library use

Add the crate's modules to another Rust program:

```rust
use rt_wireless_sensing::{
    collection::{CollectionConfig, collect},
    decoder::FrameStreamDecoder,
    saving::save_capture,
    streaming::{StreamingConfig, run},
    visualization::VisualizationData,
};
```

All public items are documented with `cargo doc --open`.
