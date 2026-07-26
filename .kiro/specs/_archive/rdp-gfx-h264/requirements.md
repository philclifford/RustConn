# Requirements Document

## Introduction

This feature integrates the RDP Graphics Pipeline (RDPGFX / MS-RDPEGFX) with H.264/AVC decoding into RustConn's embedded RDP client. The EGFX pipeline provides dramatically better image quality and lower bandwidth usage compared to RemoteFX or legacy bitmap updates, making the embedded client viable for WAN connections. The implementation uses `ironrdp-egfx` 0.3 for DVC processing and OpenH264 (Cisco prebuilt) for H.264 decoding via `dlopen` at runtime, ensuring Flatpak compatibility without linking against external C libraries at build time.

## Glossary

- **EGFX_Processor**: The `ironrdp-egfx` 0.3 DVC processor that handles RDPGFX channel protocol messages, surface commands, and codec dispatch
- **H264_Decoder**: The OpenH264 decoder instance (provided by the `openh264` crate) that decodes H.264/AVC NAL units into raw YUV frames
- **GFX_Pipeline**: The complete rendering path from EGFX surface commands through H.264 decoding to BGRA framebuffer output
- **Surface**: An RDPGFX surface — a server-managed rectangular region that receives codec-compressed frame updates
- **Framebuffer**: The in-memory BGRA pixel buffer (`DecodedImage`) that the GUI reads for display
- **DVC**: Dynamic Virtual Channel — the transport layer for EGFX messages within the RDP session
- **GraphicsMode_Selector**: The logic in `ServerGraphicsCapabilities::auto_select()` that picks the best available graphics pipeline
- **OpenH264_Library**: The Cisco-licensed `libopenh264.so` shared library loaded at runtime via `dlopen`
- **Performance_Mode**: The user-configurable Quality/Balanced/Speed setting (`RdpPerformanceMode`) that influences codec selection
- **Connection_Module**: The `connection.rs` module responsible for RDP handshake, capability exchange, and DVC registration
- **Session_Loop**: The `session.rs` active session loop that processes `ActiveStageOutput` variants and dispatches events to the GUI
- **Flatpak_Manifest**: The Flatpak build manifest (`io.github.*.yml`) that declares runtime dependencies and extensions

## Requirements

### Requirement 1: EGFX DVC Registration

**User Story:** As the RDP client, I want to register the EGFX dynamic virtual channel processor during connection setup, so that the server can negotiate GFX pipeline usage.

#### Acceptance Criteria

1. WHEN the `rdp-embedded` feature and the `gfx-h264` feature are both enabled at compile time, THE Connection_Module SHALL attempt to initialize the H264_Decoder and, on success, register the `GraphicsPipelineClient` (from `ironrdp-egfx`) as a dynamic virtual channel on the `DrdynvcClient` alongside the existing DisplayControl and Echo channels
2. WHEN the `GraphicsPipelineClient` is registered, IT SHALL advertise RDPGFX capability versions (V10.7 for AVC444, V8.1 for AVC420, V8 as fallback) via its built-in `capabilities()` method — AVC versions are automatically filtered if no H.264 decoder was provided
3. IF the OpenH264_Library fails to load at runtime, THEN THE Connection_Module SHALL construct the `GraphicsPipelineClient` with `h264_decoder: None`, which disables AVC codec advertisement and falls back to uncompressed/RFX-progressive surface updates within the GFX channel
4. THE Connection_Module SHALL register the `GraphicsPipelineClient` without requiring any `unsafe` code in `rustconn-core`
5. IF the `gfx-h264` feature is disabled at compile time, THEN THE Connection_Module SHALL not include any EGFX registration logic in the compiled binary — the session uses RemoteFX/Legacy as before

### Requirement 2: OpenH264 Runtime Loading

**User Story:** As a Flatpak user, I want the H.264 decoder to load the OpenH264 shared library at runtime via `dlopen`, so that decoding works inside the sandbox without build-time linking.

#### Acceptance Criteria

1. WHEN the GFX pipeline initializes, THE H264_Decoder SHALL attempt to load `libopenh264.so` using the `openh264` crate's dynamic loading mechanism via `dlopen`
2. WHILE running inside a Flatpak sandbox, THE H264_Decoder SHALL search for `libopenh264.so` in directories provided by the `org.freedesktop.Platform.openh264` runtime extension and in directories listed in the `LD_LIBRARY_PATH` environment variable, in that order
3. IF `libopenh264.so` is not found in any searched path, THEN THE GFX_Pipeline SHALL set its decoder status to `H264Unavailable` and SHALL log a warning-level message indicating the library was not found
4. IF `libopenh264.so` is found but fails to initialize (symbol resolution failure, ABI incompatibility, or decoder creation error), THEN THE GFX_Pipeline SHALL set its decoder status to `H264Unavailable` and SHALL log an error-level message indicating the initialization failure reason
5. WHILE the decoder status is `H264Unavailable`, THE GraphicsMode_Selector SHALL exclude `GfxH264` and `GfxAvc444` from automatic mode selection and SHALL reject explicit user requests for these modes by returning a mode-unavailable error
6. THE H264_Decoder SHALL not require any `unsafe` code in `rustconn-core` (the `openh264` crate handles FFI internally)

### Requirement 3: H.264 Frame Decoding

**User Story:** As a user connected to an RDP server, I want H.264 frames decoded efficiently, so that I see smooth, high-quality remote desktop output.

#### Acceptance Criteria

1. THE `GraphicsPipelineClient` SHALL handle H.264 decoding internally — our `GraphicsPipelineHandler` implementation receives already-decoded RGBA pixel data via `on_bitmap_updated(&BitmapUpdate)`, so no manual YUV→BGRA conversion is needed in `rustconn-core`
2. WHEN `on_bitmap_updated` is called, THE handler SHALL copy the RGBA data from `BitmapUpdate.data` into the Framebuffer at the coordinates specified by `BitmapUpdate.surface_id` + that surface's mapped output position
3. WHEN `on_frame_complete` is called, THE handler SHALL emit a single `GraphicsUpdate` event covering the bounding rectangle of all bitmap updates received since the last frame completion
4. IF the H264_Decoder encounters a corrupted NAL unit, THE `GraphicsPipelineClient` logs it internally and delivers an empty `BitmapUpdate.data` — THE handler SHALL skip empty updates without disconnecting the session
5. IF 10 or more consecutive `on_bitmap_updated` calls deliver empty data, THEN THE handler SHALL emit a tracing error and signal the GUI that H.264 decoding is failing persistently
6. WHEN `on_reset_graphics` is called (resolution change), THE handler SHALL resize the Framebuffer to the new dimensions and clear all accumulated surface state

### Requirement 4: Surface Management

**User Story:** As the rendering engine, I want to manage EGFX surfaces correctly, so that frame updates are applied to the right regions of the framebuffer.

#### Acceptance Criteria

1. THE `GraphicsPipelineClient` manages surfaces internally (create/delete/map) — our handler receives lifecycle callbacks (`on_surface_created`, `on_surface_mapped`, `on_surface_deleted`) for bookkeeping only
2. WHEN `on_surface_mapped` is called with (surface_id, origin_x, origin_y), THE handler SHALL record the mapping so that `on_bitmap_updated` can translate surface-local coordinates to Framebuffer coordinates
3. WHEN `on_surface_deleted` is called, THE handler SHALL remove the surface mapping from its bookkeeping
4. WHEN the Framebuffer is resized (via `on_reset_graphics`), THE handler SHALL clear all surface mappings and await fresh `on_surface_mapped` calls from the server
5. IF `on_bitmap_updated` references a surface_id that has no recorded mapping, THE handler SHALL log a tracing warning and skip the update without disconnecting

### Requirement 5: Graphics Mode Selection with GFX Support

**User Story:** As a user, I want the client to automatically select the best graphics pipeline including GFX/H.264 when available, so that I get optimal image quality without manual configuration.

#### Acceptance Criteria

1. WHEN the GraphicsMode is set to Auto, THE GraphicsMode_Selector SHALL select the highest-priority supported mode in the following order: GfxAvc444 > GfxH264 > Gfx > RemoteFX > Legacy, where a mode is considered supported only if both the server advertises the corresponding capability and the client has the required decoder (H264_Decoder for GfxAvc444 and GfxH264)
2. WHEN the Performance_Mode is set to Quality, THE GraphicsMode_Selector SHALL prefer GfxAvc444 if both server and client support it, falling back to the next mode in the Auto priority order that is supported
3. WHEN the Performance_Mode is set to Balanced, THE GraphicsMode_Selector SHALL prefer GfxH264 if both server and client support it, falling back to the next mode in the Auto priority order that is supported
4. WHEN the Performance_Mode is set to Speed, THE GraphicsMode_Selector SHALL prefer RemoteFX if the server supports it, otherwise Legacy, skipping all H.264-based modes regardless of availability
5. WHEN the user explicitly selects a GraphicsMode other than Auto and the server supports the requested mode, THE GraphicsMode_Selector SHALL use the requested mode
6. IF the user explicitly selects a GraphicsMode other than Auto and the server does not support the requested mode, THEN THE GraphicsMode_Selector SHALL fall back to Auto priority selection and indicate to the caller that the requested mode was unavailable
7. THE GraphicsMode::is_supported() method SHALL return true for GfxH264 and GfxAvc444 only when the H264_Decoder library is linked and can be successfully instantiated at runtime
8. WHEN the server does not advertise GFX support, THE GraphicsMode_Selector SHALL select RemoteFX if the server supports it, otherwise Legacy, regardless of Performance_Mode setting

### Requirement 6: Graceful Fallback

**User Story:** As a user, I want the client to fall back gracefully to RemoteFX or Legacy when H.264 is not available, so that connections always succeed regardless of decoder availability.

#### Acceptance Criteria

1. IF the OpenH264_Library is not installed, THEN THE GraphicsMode_Selector SHALL select RemoteFX as the graphics mode when the server supports RemoteFX, or Legacy when the server does not support RemoteFX
2. IF the server does not advertise GFX/H.264 support in its capabilities, THEN THE GFX_Pipeline SHALL not be activated and the session SHALL proceed using the previously negotiated RemoteFX or Legacy rendering path without modification to frame output
3. WHEN a fallback occurs due to missing OpenH264 library, THEN THE GFX_Pipeline SHALL emit a tracing warning at `warn` level with a structured field `reason` indicating "openh264_unavailable"
4. WHEN a fallback occurs due to unsupported server capabilities, THEN THE GFX_Pipeline SHALL emit a tracing warning at `warn` level with a structured field `reason` indicating "server_gfx_unsupported"
5. THE fallback behavior SHALL produce identical pixel output and frame timing as a session that was configured for RemoteFX or Legacy from the start (no additional latency, no color depth reduction, no resolution change)

### Requirement 7: Session Loop Integration

**User Story:** As the session runtime, I want EGFX output to integrate seamlessly with the existing `ActiveStageOutput` processing, so that frame updates reach the GUI through the established event channel.

#### Acceptance Criteria

1. THE EGFX pipeline operates entirely within the DVC layer — the `GraphicsPipelineHandler` callbacks (`on_bitmap_updated`, `on_frame_complete`) fire during `ActiveStage::process()` when DVC data arrives, and the handler accumulates updates into a shared buffer
2. AFTER `ActiveStage::process()` returns, THE Session_Loop SHALL check the handler's pending updates buffer and, if non-empty, extract the BGRA pixel data and send it as `RdpClientEvent::FrameUpdate` to the GUI
3. WHEN the EGFX_Processor produces DVC response frames (frame acknowledgments), they are returned as `ActiveStageOutput::ResponseFrame` by the existing DVC routing in `ironrdp-session` — THE Session_Loop handles them via the same write path already used for other response frames
4. WHEN a `DeactivateAll` occurs, THE handler SHALL clear its surface mappings and pending-update buffer, and the `GraphicsPipelineClient`'s internal state resets on the next capability exchange

### Requirement 8: Performance Monitoring

**User Story:** As a user, I want to monitor GFX pipeline performance, so that I can verify H.264 decoding is working efficiently.

#### Acceptance Criteria

1. WHILE the GFX_Pipeline is active, THE FrameStatistics SHALL track H.264 decode time separately from total frame processing time, recording both values in microseconds per frame using exponential moving average
2. THE FrameStatistics SHALL expose the active graphics pipeline mode (one of: GfxH264, GfxAvc444, RemoteFX, or Legacy) as a queryable field in the session statistics
3. WHEN the frame drop rate (frames_dropped / frames_received × 100) exceeds 5% measured over a sliding window of at least 100 frames, THE GFX_Pipeline SHALL log a tracing warning at `warn` level with structured fields: `drop_rate_percent`, `avg_decode_time_us`, and `current_fps`

### Requirement 9: Flatpak Packaging

**User Story:** As a Flatpak packager, I want H.264 decoding to work inside the sandbox, so that GFX pipeline functions for Flatpak users.

#### Acceptance Criteria

1. THE Flatpak_Manifest SHALL build OpenH264 from source as a module (the `org.freedesktop.Platform.openh264` extension was removed from Freedesktop SDK 23.08+ due to a security vulnerability and is NOT available in GNOME 50 runtime) — this produces a local `libopenh264.so` under `/app/lib/` without Cisco patent coverage (acceptable for open-source redistribution)
2. WHEN the Flatpak is built, THE rustconn binary SHALL NOT link against `libopenh264.so` at compile time — the `ironrdp-egfx` crate's `openh264-libloading` feature uses `dlopen` exclusively at runtime
3. THE Flatpak `LD_LIBRARY_PATH` (already set to `/app/lib64:/app/lib`) SHALL allow `dlopen("libopenh264.so")` to resolve the bundled library without additional environment configuration
4. FOR non-Flatpak installations (native packages, AppImage), THE application SHALL search standard system library paths for `libopenh264.so` — the user installs it via their distro package manager (`openh264` on Fedora, `libopenh264-7` on Debian/Ubuntu)

### Requirement 10: Error Handling

**User Story:** As a developer, I want GFX pipeline errors to be well-typed and recoverable, so that failures can be diagnosed and handled without crashing the session.

#### Acceptance Criteria

1. THE GraphicsError enum SHALL include at minimum the following variants for H.264-specific failures: `H264Unavailable` (library not loaded), `H264DecodeFailed` (single frame decode error with source context), and `SurfaceError` (surface creation or management failure with surface ID)
2. WHEN a non-fatal GFX error occurs (single frame decode failure), THE GFX_Pipeline SHALL log the error at `warn` level with structured fields identifying the failed frame and continue processing subsequent frames without interrupting the session
3. WHEN a fatal GFX error occurs (decoder initialization failure or repeated decode failures exceeding 10 consecutive frames), THE GFX_Pipeline SHALL attempt fallback to RemoteFX rendering and emit a tracing error at `error` level with the failure variant and original error source
4. IF fallback to RemoteFX also fails after a fatal GFX error, THEN THE GFX_Pipeline SHALL propagate the error to the session loop as a terminal `GraphicsError`, allowing the session to disconnect gracefully
5. THE GFX_Pipeline SHALL propagate errors using `thiserror::Error` derive and SHALL NOT use `unwrap()` or `expect()` on any fallible operation in the graphics pipeline code path
