# Technical Design Document

## Overview

This design integrates `ironrdp-egfx` 0.3 into RustConn's embedded RDP client to enable the GFX pipeline with H.264/AVC decoding. The architecture leverages `ironrdp-egfx`'s internal H.264 decoding — our code implements the `GraphicsPipelineHandler` callback trait and composites decoded RGBA pixels into the existing `DecodedImage` framebuffer.

### Key Architectural Insight

`ironrdp-egfx`'s `GraphicsPipelineClient` is a `DvcProcessor` that:
- Manages surfaces internally (create/delete/map)
- Decodes H.264 internally via an injected `H264Decoder` trait object
- Delivers **already-decoded RGBA pixel data** via `GraphicsPipelineHandler::on_bitmap_updated`
- Handles ZGFX decompression, frame acknowledgments, and capability negotiation

Our implementation is therefore a **thin handler + registration layer**, not a full codec pipeline.

## Architecture

### Component Diagram

```
┌─────────────────────────────────────────────────────────────┐
│ rustconn-core/src/rdp_client/                               │
│                                                             │
│  connection.rs                                              │
│  ┌─────────────────────────────────────────────────┐        │
│  │ DrdynvcClient::new()                            │        │
│  │   .with_dynamic_channel(DisplayControlClient)   │        │
│  │   .with_dynamic_channel(EchoClient)             │        │
│  │   .with_dynamic_channel(GraphicsPipelineClient) │ ← NEW  │
│  └─────────────────────────────────────────────────┘        │
│                                                             │
│  gfx_handler.rs  ← NEW MODULE                              │
│  ┌─────────────────────────────────────────────────┐        │
│  │ RustConnGfxHandler: GraphicsPipelineHandler     │        │
│  │  - surface_mappings: HashMap<u16, (u32, u32)>   │        │
│  │  - pending_updates: Vec<GfxFrameUpdate>         │        │
│  │  - event_tx: Sender<RdpClientEvent>             │        │
│  │  - framebuffer: shared ref to DecodedImage      │        │
│  │  - consecutive_empty: u32                       │        │
│  └─────────────────────────────────────────────────┘        │
│                                                             │
│  session.rs (existing — minor additions)                    │
│  ┌─────────────────────────────────────────────────┐        │
│  │ After ActiveStage::process():                   │        │
│  │   drain handler's pending_updates → FrameUpdate │        │
│  └─────────────────────────────────────────────────┘        │
│                                                             │
│  graphics.rs (existing — update is_supported/auto_select)   │
│  ┌─────────────────────────────────────────────────┐        │
│  │ is_supported() → true when gfx-h264 feature on  │        │
│  │ auto_select() → prefer GFX modes when available  │        │
│  └─────────────────────────────────────────────────┘        │
└─────────────────────────────────────────────────────────────┘
```

### Data Flow

```
Server → RDP FastPath/SlowPath → ActiveStage::process()
  → DVC routing → GraphicsPipelineClient::process()
    → H.264 decode (internal, OpenH264 via dlopen)
    → RustConnGfxHandler::on_bitmap_updated(BitmapUpdate)
      → copy RGBA into framebuffer at surface offset
      → accumulate dirty region
    → RustConnGfxHandler::on_frame_complete(frame_id)
      → flush dirty region → pending_updates queue
  ← ActiveStageOutput::ResponseFrame (frame ack)
← Session loop: drain pending_updates → RdpClientEvent::FrameUpdate
```

## Components

### 1. New Module: `gfx_handler.rs`

Location: `rustconn-core/src/rdp_client/gfx_handler.rs`

```rust
use ironrdp_egfx::client::{BitmapUpdate, GraphicsPipelineHandler, Surface};
use std::collections::HashMap;
use std::sync::mpsc::Sender;

/// Accumulates GFX frame updates for the session loop to drain
pub struct GfxFrameUpdate {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub data: Vec<u8>,  // RGBA → converted to BGRA on copy
}

/// Handler that receives decoded EGFX bitmap data and composites
/// it into the shared framebuffer.
pub struct RustConnGfxHandler {
    /// Surface ID → (origin_x, origin_y) mapping
    surface_mappings: HashMap<u16, (u32, u32)>,
    /// Dirty region accumulated within current frame
    dirty_rect: Option<DirtyRect>,
    /// Completed frame updates ready for the session loop
    pending_updates: Vec<GfxFrameUpdate>,
    /// Event sender (for error signaling)
    event_tx: Sender<super::RdpClientEvent>,
    /// Consecutive empty bitmap updates (decode failures)
    consecutive_empty: u32,
    /// Whether GFX pipeline is active (caps confirmed)
    active: bool,
}
```

**Key methods:**
- `new(event_tx)` — constructor
- `take_pending_updates() -> Vec<GfxFrameUpdate>` — drain for session loop
- `is_active() -> bool` — whether EGFX negotiation succeeded
- `reset()` — clear state on deactivation/reactivation

### 2. Feature Flag: `gfx-h264`

In `rustconn-core/Cargo.toml`:

```toml
ironrdp-egfx = { version = "0.3", features = ["openh264-libloading"], optional = true }

[features]
default = ["vnc-embedded", "rdp-embedded", "gfx-h264"]
gfx-h264 = ["dep:ironrdp-egfx", "rdp-embedded"]
```

The feature is enabled by default (bundled in Flatpak) but can be disabled for minimal builds.

### 3. Connection Registration (connection.rs changes)

```rust
// After existing DRDYNVC setup:
#[cfg(feature = "gfx-h264")]
{
    let gfx_handler = RustConnGfxHandler::new(event_tx.clone());
    let h264_decoder = try_load_openh264();  // Returns Option<Box<dyn H264Decoder>>
    
    let gfx_client = GraphicsPipelineClient::new(
        Box::new(gfx_handler),
        h264_decoder,  // None if library missing — EGFX still works without AVC
    );
    
    drdynvc = drdynvc.with_dynamic_channel(gfx_client);
    tracing::info!(
        h264_available = h264_decoder.is_some(),
        "EGFX pipeline registered"
    );
}
```

### 4. OpenH264 Loading (`gfx_handler.rs`)

```rust
/// Attempts to load OpenH264 at runtime via dlopen.
/// Returns None if the library is not found — EGFX will still work
/// with uncompressed/progressive codecs, just not H.264.
pub fn try_load_openh264() -> Option<Box<dyn ironrdp_egfx::decode::H264Decoder>> {
    match openh264::Decoder::new() {
        Ok(decoder) => {
            tracing::info!("OpenH264 loaded successfully — H.264 decoding enabled");
            Some(Box::new(OpenH264DecoderAdapter(decoder)))
        }
        Err(e) => {
            tracing::warn!(
                reason = %e,
                "OpenH264 not available — GFX pipeline will use non-AVC codecs"
            );
            None
        }
    }
}
```

### 5. Session Loop Changes (session.rs)

The handler uses an **mpsc channel** to deliver decoded frame updates to the session loop (the handler fires synchronously during `ActiveStage::process()` but cannot hold a mutable reference to `DecodedImage`):

```rust
// Setup (before session loop):
let (gfx_update_tx, gfx_update_rx) = std::sync::mpsc::channel::<GfxFrameUpdate>();
// gfx_update_tx is passed into RustConnGfxHandler

// In session loop, after ActiveStage::process() output handling:
#[cfg(feature = "gfx-h264")]
while let Ok(update) = gfx_update_rx.try_recv() {
    blit_rgba_to_bgra(&mut image, &update);
    let rect = RdpRect::new(update.x, update.y, update.width, update.height);
    let data = extract_region_data(&image, rect);
    let _ = event_tx.send(RdpClientEvent::FrameUpdate { rect, data });
}
```

### 6. Graphics Mode Selection (graphics.rs changes)

```rust
impl GraphicsMode {
    pub const fn is_supported(&self) -> bool {
        match self {
            Self::Auto | Self::Legacy | Self::RemoteFx => true,
            #[cfg(feature = "gfx-h264")]
            Self::Gfx | Self::GfxH264 | Self::GfxAvc444 => true,
            #[cfg(not(feature = "gfx-h264"))]
            _ => false,
        }
    }
}

impl ServerGraphicsCapabilities {
    fn auto_select(&self) -> GraphicsMode {
        #[cfg(feature = "gfx-h264")]
        {
            if self.supports_avc444 { return GraphicsMode::GfxAvc444; }
            if self.supports_h264 { return GraphicsMode::GfxH264; }
            if self.supports_gfx { return GraphicsMode::Gfx; }
        }
        if self.supports_remotefx {
            GraphicsMode::RemoteFx
        } else {
            GraphicsMode::Legacy
        }
    }
}
```

### 7. Flatpak OpenH264 Module

Added to `packaging/flatpak/io.github.totoshko88.RustConn.yml`:

```yaml
  - name: openh264
    buildsystem: meson
    config-opts:
      - -Dtests=disabled
    sources:
      - type: archive
        url: https://github.com/cisco/openh264/archive/refs/tags/v2.6.0.tar.gz
        sha256: <to-be-filled-at-implementation>
        x-checker-data:
          type: anitya
          project-id: 15752
          stable-only: true
          url-template: https://github.com/cisco/openh264/archive/refs/tags/v$version.tar.gz
```

### 8. RGBA → BGRA Conversion

`BitmapUpdate.data` is RGBA (4 bytes/pixel). Our framebuffer is BGRA (`BgrA32`). The blit function swaps R↔B:

```rust
fn blit_rgba_to_bgra(image: &mut DecodedImage, update: &GfxFrameUpdate) {
    let stride = image.width() as usize * 4;
    let data = image.data_mut();
    
    for row in 0..update.height as usize {
        let src_offset = row * update.width as usize * 4;
        let dst_offset = (update.y as usize + row) * stride + update.x as usize * 4;
        
        for px in 0..update.width as usize {
            let si = src_offset + px * 4;
            let di = dst_offset + px * 4;
            // RGBA → BGRA: swap R and B
            data[di]     = update.data[si + 2]; // B
            data[di + 1] = update.data[si + 1]; // G
            data[di + 2] = update.data[si];     // R
            data[di + 3] = update.data[si + 3]; // A
        }
    }
}
```

## Error Handling

```rust
#[derive(Debug, thiserror::Error)]
pub enum GfxError {
    #[error("OpenH264 library not available: {0}")]
    H264Unavailable(String),
    
    #[error("H.264 decode failed for surface {surface_id}: {reason}")]
    H264DecodeFailed { surface_id: u16, reason: String },
    
    #[error("Surface {surface_id} not mapped to output")]
    SurfaceNotMapped { surface_id: u16 },
    
    #[error("Persistent decode failure: {consecutive_failures} consecutive empty frames")]
    PersistentDecodeFailure { consecutive_failures: u32 },
}
```

## Files Changed

| File | Change |
|------|--------|
| `rustconn-core/Cargo.toml` | Add `ironrdp-egfx` dep + `gfx-h264` feature |
| `rustconn-core/src/rdp_client/gfx_handler.rs` | **NEW** — `GraphicsPipelineHandler` impl |
| `rustconn-core/src/rdp_client/mod.rs` | Add `pub mod gfx_handler` under `cfg(feature = "gfx-h264")` |
| `rustconn-core/src/rdp_client/client/connection.rs` | Register `GraphicsPipelineClient` on DVC |
| `rustconn-core/src/rdp_client/client/session.rs` | Drain GFX handler updates after `process()` |
| `rustconn-core/src/rdp_client/graphics.rs` | Update `is_supported()` and `auto_select()` |
| `packaging/flatpak/io.github.totoshko88.RustConn.yml` | Add `openh264` build module |

## Alternatives Considered

1. **Bundle OpenH264 via `openh264-bundled` feature (compile from C source at Rust build time)**
   - Pro: No runtime dependency, single binary
   - Con: Requires NASM + C compiler in cargo build; increases compile time by ~30s; no Cisco patent coverage
   - Decision: Use `openh264-libloading` for runtime dlopen — Flatpak builds it as a separate module (fast, cached), native users get it from distro packages

2. **Process EGFX outside ironrdp-session (custom DVC message parsing)**
   - Pro: Full control over decode timing
   - Con: Duplicates `ironrdp-egfx` logic; fragile to upstream changes
   - Decision: Use `ironrdp-egfx` as designed — implement handler trait only

3. **Shared framebuffer via `Arc<Mutex<DecodedImage>>`**
   - Pro: Handler writes directly to framebuffer
   - Con: Mutex contention between decode callback and session loop read; `DecodedImage` not Send
   - Decision: Channel-based update delivery — handler sends pixel data via mpsc, session loop blits into owned framebuffer

## Phasing

- **Phase 1 (this spec)**: AVC420 (GfxH264) — single H.264 stream, most common server config
- **Phase 2 (follow-up)**: AVC444 — dual-stream decode (luma full-res + chroma), requires upstream `ironrdp-egfx` AVC444 support verification
