# Implementation Tasks

## Task 1: Add `ironrdp-egfx` dependency and `gfx-h264` feature flag

- [x] Add `ironrdp-egfx = { version = "0.3", features = ["openh264-libloading"], optional = true }` to `rustconn-core/Cargo.toml`
- [x] Add `gfx-h264 = ["dep:ironrdp-egfx", "rdp-embedded"]` to the `[features]` section
- [x] Add `"gfx-h264"` to the `default` feature list
- [x] Run `cargo update -p ironrdp-egfx` to resolve the dependency
- [x] Verify `cargo check -p rustconn-core --features gfx-h264` compiles cleanly

**Requirements:** Req 1 (AC 5), Req 2 (AC 6)

## Task 2: Create `gfx_handler.rs` — `GraphicsPipelineHandler` implementation

- [x] Create `rustconn-core/src/rdp_client/gfx_handler.rs`
- [x] Define `GfxFrameUpdate` struct (x, y, width, height, data: Vec<u8>)
- [x] Define `RustConnGfxHandler` struct with: surface_mappings (HashMap<u16, (u32, u32)>), update_tx (mpsc::Sender<GfxFrameUpdate>), consecutive_empty (u32), active (bool)
- [x] Implement `GraphicsPipelineHandler` trait:
  - `on_capabilities_confirmed` → set `active = true`, log negotiated caps
  - `on_reset_graphics(width, height)` → clear surface_mappings, signal resolution change
  - `on_surface_mapped(id, x, y)` → insert into surface_mappings
  - `on_surface_deleted(id)` → remove from surface_mappings
  - `on_bitmap_updated(update)` → translate coords via surface_mappings, send via update_tx; track consecutive_empty for error detection
  - `on_frame_complete(frame_id)` → log frame stats
  - `on_close()` → log channel close
- [x] Implement `try_load_openh264() -> Option<Box<dyn H264Decoder>>` function
- [x] Define `GfxError` enum with thiserror variants (H264Unavailable, H264DecodeFailed, SurfaceNotMapped, PersistentDecodeFailure)
- [x] Register module in `mod.rs` under `#[cfg(feature = "gfx-h264")]`

**Requirements:** Req 3, Req 4, Req 10

## Task 3: Register `GraphicsPipelineClient` in connection.rs

- [x] Import `ironrdp_egfx::client::GraphicsPipelineClient` under `#[cfg(feature = "gfx-h264")]`
- [x] Import `RustConnGfxHandler` and `try_load_openh264` from `gfx_handler`
- [x] After the existing `drdynvc` construction (DisplayControl + Echo), add a `#[cfg(feature = "gfx-h264")]` block that:
  - Calls `try_load_openh264()` to get `Option<Box<dyn H264Decoder>>`
  - Creates `RustConnGfxHandler::new(gfx_update_tx)`
  - Creates `GraphicsPipelineClient::new(Box::new(handler), h264_decoder)`
  - Chains `.with_dynamic_channel(gfx_client)` onto drdynvc
  - Logs H.264 availability status
- [x] Pass `gfx_update_rx` channel receiver through to session (add to `run_active_session` params or embed in a struct)
- [x] Verify `cargo check --all-targets` passes

**Requirements:** Req 1 (AC 1-4), Req 2

## Task 4: Integrate GFX updates into session loop

- [x] Add `gfx_update_rx: Option<mpsc::Receiver<GfxFrameUpdate>>` parameter to `run_active_session` (or pass via config struct)
- [x] After the `ActiveStage::process()` output handling loop, add a `#[cfg(feature = "gfx-h264")]` drain block:
  - `while let Ok(update) = gfx_update_rx.try_recv()` → blit RGBA→BGRA into image → send `FrameUpdate` event
- [x] Implement `blit_rgba_to_bgra(image: &mut DecodedImage, update: &GfxFrameUpdate)` — row-by-row pixel copy with R↔B swap
- [x] Handle `on_reset_graphics` resolution change signal (resize DecodedImage, notify GUI)
- [x] Verify existing RemoteFX/Legacy path is unchanged when `gfx-h264` is disabled

**Requirements:** Req 7 (AC 1-4), Req 3 (AC 2)

## Task 5: Update graphics mode selection

- [x] In `graphics.rs`, update `GraphicsMode::is_supported()` to return `true` for Gfx/GfxH264/GfxAvc444 when `#[cfg(feature = "gfx-h264")]`
- [x] Update `ServerGraphicsCapabilities::auto_select()` to prefer GFX modes when feature is enabled: GfxAvc444 > GfxH264 > Gfx > RemoteFX > Legacy
- [x] Add Performance_Mode mapping: Quality→GfxAvc444, Balanced→GfxH264, Speed→RemoteFX/Legacy
- [x] Update test `test_graphics_mode_supported` to reflect new behavior
- [x] Update test `test_server_capabilities_modern` auto_select expectation
- [x] Ensure tests still pass when `gfx-h264` feature is disabled (use `#[cfg]` on test assertions)

**Requirements:** Req 5, Req 6

## Task 6: Graceful fallback and error handling

- [x] In the GFX handler, when `consecutive_empty >= 10`: emit tracing error, send `RdpClientEvent` signaling persistent decode failure
- [x] In connection.rs, when `try_load_openh264()` returns None: emit `tracing::warn!(reason = "openh264_unavailable", ...)`
- [x] Verify that when EGFX DVC is registered without H.264 decoder, the server falls back to non-AVC codecs within the GFX channel (ironrdp-egfx handles this internally)
- [x] Verify that when EGFX is not registered at all (feature disabled or server doesn't support GFX), existing RemoteFX/Legacy path works identically to before
- [x] Add integration test or doc-test verifying GfxError variants implement Display correctly

**Requirements:** Req 6 (AC 1-5), Req 10 (AC 1-5)

## Task 7: Add OpenH264 module to Flatpak manifest

- [x] Add `openh264` module to `packaging/flatpak/io.github.totoshko88.RustConn.yml` before the `rustconn` module
- [x] Use meson buildsystem with `-Dtests=disabled`
- [x] Source: `https://github.com/cisco/openh264/archive/refs/tags/v2.6.0.tar.gz` (verify sha256)
- [x] Add x-checker-data for anitya project-id 15752
- [x] Verify that the built `libopenh264.so` lands in `/app/lib/` (covered by existing `LD_LIBRARY_PATH`)
- [x] Test `flatpak-builder` builds successfully with the new module

**Requirements:** Req 9 (AC 1-4)

## Task 8: Performance monitoring additions

- [x] Add `active_graphics_mode: GraphicsMode` field to `FrameStatistics`
- [x] Add `h264_decode_time_us: u64` field (exponential moving average)
- [x] In session loop GFX drain, measure blit time and update `h264_decode_time_us`
- [x] Add 5% frame drop rate warning (tracing::warn with structured fields) in `FrameStatistics::record_dropped()`
- [x] Expose `active_graphics_mode` in existing session statistics event path

**Requirements:** Req 8 (AC 1-3)

## Task 9: Update ON UPGRADE comment and documentation

- [x] Update the "ON UPGRADE" comment in `rustconn-core/Cargo.toml` to mark GFX/H.264 as implemented
- [x] Update `graphics.rs` module doc comment: "GFX/H.264: Supported via ironrdp-egfx (requires gfx-h264 feature)"
- [x] Update `rustconn-core/src/rdp_client/mod.rs` module doc to mention GFX pipeline
- [x] Remove `microphone_enabled` dead-field note from ON UPGRADE (still dead, but clarify it's unrelated to this change)

**Requirements:** Documentation accuracy

## Task 10: Quality check and final verification

- [x] Run `cargo fmt --all`
- [x] Run `cargo clippy --all-targets` — 0 warnings
- [x] Run `cargo test --workspace` — all pass
- [x] Run `cargo test -p rustconn-core --test property_tests` — all pass
- [x] Verify `cargo check --all-targets --no-default-features --features rdp-embedded` compiles (gfx-h264 disabled)
- [x] Verify `cargo check --all-targets` compiles (gfx-h264 enabled by default)
- [x] Run `cargo deny check` — no new advisories
- [x] Manual test with a real RDP server if available (Windows 10/11 with GFX enabled)

**Requirements:** All (integration verification)
