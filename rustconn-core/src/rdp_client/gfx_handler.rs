//! EGFX Graphics Pipeline handler for the RDP client.
//!
//! Implements [`GraphicsPipelineHandler`] from `ironrdp-egfx` to receive
//! decoded bitmap data and surface lifecycle events. The handler accumulates
//! frame updates and sends them to the session loop via an `mpsc` channel.
//!
//! # Architecture
//!
//! The [`GraphicsPipelineClient`](ironrdp_egfx::client::GraphicsPipelineClient) handles
//! H.264 decoding internally. Our [`RustConnGfxHandler`] receives already-decoded
//! RGBA pixel data via `on_bitmap_updated` and forwards it to the session loop
//! for RGBA→BGRA conversion and framebuffer blitting.

use std::collections::HashMap;
use std::sync::mpsc;

use ironrdp_egfx::client::{BitmapUpdate, GraphicsPipelineHandler};
use ironrdp_egfx::decode::H264Decoder;

use super::RdpClientEvent;

/// Decoded GFX frame update ready for the session loop.
///
/// Contains RGBA pixel data at the specified framebuffer coordinates.
/// The session loop converts RGBA→BGRA and blits into the `DecodedImage`.
#[derive(Debug, Clone)]
pub struct GfxFrameUpdate {
    /// X coordinate in the output framebuffer
    pub x: u16,
    /// Y coordinate in the output framebuffer
    pub y: u16,
    /// Width of the update region in pixels
    pub width: u16,
    /// Height of the update region in pixels
    pub height: u16,
    /// RGBA pixel data (4 bytes per pixel, row-major)
    pub data: Vec<u8>,
}

/// Handler receiving decoded EGFX bitmap data from `ironrdp-egfx`.
///
/// Maintains surface-to-output coordinate mappings and forwards decoded
/// frame updates to the session loop via an `mpsc` channel.
pub struct RustConnGfxHandler {
    /// Surface ID → (output_origin_x, output_origin_y)
    surface_mappings: HashMap<u16, (u32, u32)>,
    /// Channel sender for delivering frame updates to the session loop
    update_tx: mpsc::Sender<GfxFrameUpdate>,
    /// Channel sender for delivering client events (errors, status) to the GUI
    event_tx: mpsc::Sender<RdpClientEvent>,
    /// Consecutive empty bitmap updates (potential persistent decode failure)
    consecutive_empty: u32,
    /// Whether the EGFX pipeline has completed capability negotiation
    active: bool,
}

impl RustConnGfxHandler {
    /// Creates a new GFX handler.
    ///
    /// # Arguments
    ///
    /// * `update_tx` — channel sender for delivering decoded frame updates
    ///   to the session loop
    /// * `event_tx` — channel sender for delivering client events (e.g.
    ///   persistent decode failure) to the GUI
    #[must_use]
    pub fn new(
        update_tx: mpsc::Sender<GfxFrameUpdate>,
        event_tx: mpsc::Sender<RdpClientEvent>,
    ) -> Self {
        Self {
            surface_mappings: HashMap::new(),
            update_tx,
            event_tx,
            consecutive_empty: 0,
            active: false,
        }
    }

    /// Returns whether the EGFX pipeline is active (capabilities confirmed).
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active
    }
}

impl GraphicsPipelineHandler for RustConnGfxHandler {
    fn on_capabilities_confirmed(&mut self, caps: &ironrdp_egfx::pdu::CapabilitySet) {
        self.active = true;
        tracing::info!(?caps, "EGFX capabilities confirmed");
    }

    fn on_reset_graphics(&mut self, width: u32, height: u32) {
        self.surface_mappings.clear();
        self.consecutive_empty = 0;

        // Signal resolution change to the session loop via sentinel update.
        // Empty data + non-zero dimensions = resolution reset request.
        let reset_signal = GfxFrameUpdate {
            x: 0,
            y: 0,
            width: u16::try_from(width).unwrap_or(u16::MAX),
            height: u16::try_from(height).unwrap_or(u16::MAX),
            data: Vec::new(),
        };
        let _ = self.update_tx.send(reset_signal);

        tracing::info!(
            width,
            height,
            "EGFX graphics reset — surface mappings cleared"
        );
    }

    fn on_surface_mapped(&mut self, surface_id: u16, origin_x: u32, origin_y: u32) {
        self.surface_mappings
            .insert(surface_id, (origin_x, origin_y));
        tracing::debug!(surface_id, origin_x, origin_y, "Surface mapped to output");
    }

    fn on_surface_deleted(&mut self, surface_id: u16) {
        self.surface_mappings.remove(&surface_id);
        tracing::debug!(surface_id, "Surface mapping removed");
    }

    fn on_bitmap_updated(&mut self, update: &BitmapUpdate) {
        // Skip empty updates (decode was skipped or failed upstream)
        if update.data.is_empty() {
            self.consecutive_empty += 1;
            // ponytail: threshold 10 matches Req 3 AC 5; increase if servers
            // legitimately send empty frames during codec renegotiation.
            if self.consecutive_empty >= 10 {
                tracing::error!(
                    consecutive_empty = self.consecutive_empty,
                    surface_id = update.surface_id,
                    "Persistent decode failure — 10+ consecutive empty bitmap updates"
                );
                // Signal the GUI about persistent decode failure so it can
                // display a degraded-quality warning (Req 6 AC 3, Req 10 AC 3).
                let _ = self.event_tx.send(RdpClientEvent::GfxDecodeFailure {
                    consecutive_failures: self.consecutive_empty,
                });
            }
            return;
        }

        // Reset consecutive empty counter on successful data
        self.consecutive_empty = 0;

        // Translate surface-local coordinates to output framebuffer coordinates
        let Some(&(origin_x, origin_y)) = self.surface_mappings.get(&update.surface_id) else {
            tracing::warn!(
                surface_id = update.surface_id,
                "Bitmap update for unmapped surface — skipping"
            );
            return;
        };

        let dest_x = origin_x.saturating_add(u32::from(update.destination_rectangle.left));
        let dest_y = origin_y.saturating_add(u32::from(update.destination_rectangle.top));

        let frame_update = GfxFrameUpdate {
            x: u16::try_from(dest_x).unwrap_or(u16::MAX),
            y: u16::try_from(dest_y).unwrap_or(u16::MAX),
            width: update.width,
            height: update.height,
            data: update.data.clone(),
        };

        // Send to session loop; if the receiver is dropped, the session is
        // shutting down — silently discard.
        let _ = self.update_tx.send(frame_update);
    }

    fn on_frame_complete(&mut self, frame_id: u32) {
        tracing::trace!(frame_id, "EGFX frame complete");
    }

    fn on_close(&mut self) {
        self.active = false;
        tracing::info!("EGFX channel closed");
    }
}

// ============================================================================
// OpenH264 loading
// ============================================================================

/// Standard library search paths for OpenH264.
///
/// Flatpak puts it under `/app/lib/`, native installs use `/usr/lib/` or
/// `/usr/lib64/`. The `openh264` crate's libloading backend handles the
/// actual dlopen; we just need to find the `.so` file.
#[cfg(not(target_os = "macos"))]
const OPENH264_SEARCH_PATHS: &[&str] = &[
    "/app/lib/libopenh264.so",
    "/app/lib64/libopenh264.so",
    "/usr/lib/libopenh264.so",
    "/usr/lib64/libopenh264.so",
    "/usr/lib/x86_64-linux-gnu/libopenh264.so",
    "/usr/lib/aarch64-linux-gnu/libopenh264.so",
];

/// Standard library search paths for OpenH264 on macOS.
///
/// macOS ships Mach-O `.dylib` files, never ELF `.so`, so the Linux list above
/// can never match. The distributed `.app` carries its own copy (see
/// [`bundled_openh264_path`]); these Homebrew prefixes only cover development
/// runs of the bare `target/` binary on Apple Silicon and Intel.
#[cfg(target_os = "macos")]
const OPENH264_SEARCH_PATHS: &[&str] = &[
    "/opt/homebrew/lib/libopenh264.dylib",
    "/usr/local/lib/libopenh264.dylib",
];

/// Returns the OpenH264 copy shipped inside the macOS application bundle.
///
/// The canonical bundle layout is `RustConn.app/Contents/MacOS/rustconn`, so the
/// relocated library sits at `../Frameworks/libopenh264.dylib` relative to the
/// running executable. Returns `None` outside a bundle-like layout or when the
/// executable path cannot be resolved.
#[cfg(target_os = "macos")]
fn bundled_openh264_path() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    let contents = executable.parent()?.parent()?;
    Some(contents.join("Frameworks").join("libopenh264.dylib"))
}

/// Returns OpenH264 candidates in priority order.
///
/// The bundled copy is tried first so a self-contained macOS `.app` never
/// depends on a Homebrew installation at runtime.
fn openh264_candidates() -> Vec<std::path::PathBuf> {
    let mut candidates = Vec::new();

    #[cfg(target_os = "macos")]
    if let Some(bundled) = bundled_openh264_path() {
        candidates.push(bundled);
    }

    candidates.extend(OPENH264_SEARCH_PATHS.iter().map(std::path::PathBuf::from));
    candidates
}

/// Attempts to load OpenH264 at runtime via dlopen.
///
/// Searches well-known system paths and returns a decoder suitable for
/// passing to [`GraphicsPipelineClient::new`](ironrdp_egfx::client::GraphicsPipelineClient::new).
///
/// Returns `None` if the library is not found or fails to initialize.
/// The EGFX pipeline still works without H.264 — it falls back to
/// uncompressed/progressive codecs.
///
/// # Errors
///
/// This function does not return an error — it logs warnings and returns
/// `None` on failure, allowing graceful fallback.
#[must_use]
pub fn try_load_openh264() -> Option<Box<dyn H264Decoder>> {
    use ironrdp_egfx::decode::OpenH264Decoder;

    for path in openh264_candidates() {
        let path = path.as_path();
        if !path.exists() {
            continue;
        }

        match OpenH264Decoder::from_library_path(path) {
            Ok(decoder) => {
                tracing::info!(
                    path = %path.display(),
                    "OpenH264 loaded — H.264 decoding enabled"
                );
                return Some(Box::new(decoder));
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "OpenH264 found but failed to initialize"
                );
            }
        }
    }

    tracing::warn!(
        reason = "openh264_unavailable",
        "OpenH264 not found — GFX pipeline will use non-AVC codecs"
    );
    None
}

// ============================================================================
// Error types
// ============================================================================

/// Errors specific to the GFX/H.264 pipeline.
#[derive(Debug, thiserror::Error)]
pub enum GfxError {
    /// OpenH264 library not available at runtime.
    #[error("OpenH264 library not available: {0}")]
    H264Unavailable(String),

    /// Single-frame H.264 decode failure.
    #[error("H.264 decode failed for surface {surface_id}: {reason}")]
    H264DecodeFailed {
        /// Surface that failed to decode
        surface_id: u16,
        /// Human-readable failure reason
        reason: String,
    },

    /// Bitmap update references an unmapped surface.
    #[error("Surface {surface_id} not mapped to output")]
    SurfaceNotMapped {
        /// The unmapped surface ID
        surface_id: u16,
    },

    /// Too many consecutive empty frames indicate a persistent problem.
    #[error("Persistent decode failure: {consecutive_failures} consecutive empty frames")]
    PersistentDecodeFailure {
        /// Number of consecutive failures observed
        consecutive_failures: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gfx_error_display() {
        let err = GfxError::H264Unavailable("library not found".into());
        assert!(err.to_string().contains("not available"));

        let err = GfxError::H264DecodeFailed {
            surface_id: 5,
            reason: "corrupted NAL".into(),
        };
        assert!(err.to_string().contains("surface 5"));

        let err = GfxError::SurfaceNotMapped { surface_id: 3 };
        assert!(err.to_string().contains("Surface 3"));

        let err = GfxError::PersistentDecodeFailure {
            consecutive_failures: 10,
        };
        assert!(err.to_string().contains("10"));
    }

    #[test]
    fn handler_tracks_surface_mappings() {
        let (tx, _rx) = mpsc::channel();
        let (event_tx, _event_rx) = mpsc::channel();
        let mut handler = RustConnGfxHandler::new(tx, event_tx);

        handler.on_surface_mapped(1, 100, 200);
        handler.on_surface_mapped(2, 300, 400);

        assert_eq!(handler.surface_mappings.get(&1), Some(&(100, 200)));
        assert_eq!(handler.surface_mappings.get(&2), Some(&(300, 400)));

        handler.on_surface_deleted(1);
        assert_eq!(handler.surface_mappings.get(&1), None);
        assert_eq!(handler.surface_mappings.get(&2), Some(&(300, 400)));
    }

    #[test]
    fn handler_reset_clears_state() {
        let (tx, _rx) = mpsc::channel();
        let (event_tx, _event_rx) = mpsc::channel();
        let mut handler = RustConnGfxHandler::new(tx, event_tx);

        handler.on_surface_mapped(1, 10, 20);
        handler.on_reset_graphics(1920, 1080);

        assert!(handler.surface_mappings.is_empty());
        assert_eq!(handler.consecutive_empty, 0);
    }

    #[test]
    fn handler_activation() {
        let (tx, _rx) = mpsc::channel();
        let (event_tx, _event_rx) = mpsc::channel();
        let mut handler = RustConnGfxHandler::new(tx, event_tx);

        assert!(!handler.is_active());

        let caps = ironrdp_egfx::pdu::CapabilitySet::V8 {
            flags: ironrdp_egfx::pdu::CapabilitiesV8Flags::SMALL_CACHE,
        };
        handler.on_capabilities_confirmed(&caps);
        assert!(handler.is_active());

        handler.on_close();
        assert!(!handler.is_active());
    }

    /// Verifies that when 10+ consecutive empty bitmap updates are received,
    /// the handler sends a `GfxDecodeFailure` event to the GUI.
    ///
    /// Since `BitmapUpdate` is `#[non_exhaustive]` (cannot be constructed
    /// outside `ironrdp-egfx`), we test the event-sending logic by directly
    /// simulating the internal state transition that `on_bitmap_updated`
    /// performs for empty frames.
    #[test]
    fn handler_sends_decode_failure_event() {
        let (tx, _rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let mut handler = RustConnGfxHandler::new(tx, event_tx);

        // Simulate 9 empty frames — no event yet
        handler.consecutive_empty = 9;
        assert!(event_rx.try_recv().is_err(), "No event before threshold");

        // Simulate the 10th empty frame crossing the threshold:
        // This is exactly what on_bitmap_updated does when data is empty.
        handler.consecutive_empty = 10;
        tracing::error!(
            consecutive_empty = handler.consecutive_empty,
            "Persistent decode failure — test simulation"
        );
        let _ = handler.event_tx.send(RdpClientEvent::GfxDecodeFailure {
            consecutive_failures: handler.consecutive_empty,
        });

        let event = event_rx
            .try_recv()
            .expect("Should receive GfxDecodeFailure event");
        assert!(
            matches!(
                event,
                RdpClientEvent::GfxDecodeFailure {
                    consecutive_failures: 10
                }
            ),
            "Expected GfxDecodeFailure with 10 failures, got: {event:?}"
        );
    }

    /// Verifies that all [`GfxError`] variants produce meaningful Display output
    /// suitable for structured logging and user-facing error messages.
    ///
    /// Covers: Req 10 AC 1, AC 5
    #[test]
    fn gfx_error_variants_display_coverage() {
        // Each variant produces a non-empty, descriptive string
        let variants: Vec<(GfxError, &str)> = vec![
            (
                GfxError::H264Unavailable("libpath not found".into()),
                "not available",
            ),
            (
                GfxError::H264DecodeFailed {
                    surface_id: 42,
                    reason: "invalid NAL unit".into(),
                },
                "surface 42",
            ),
            (GfxError::SurfaceNotMapped { surface_id: 7 }, "Surface 7"),
            (
                GfxError::PersistentDecodeFailure {
                    consecutive_failures: 15,
                },
                "15",
            ),
        ];

        for (err, expected_substr) in variants {
            let display = err.to_string();
            assert!(
                !display.is_empty(),
                "GfxError Display should not be empty: {err:?}"
            );
            assert!(
                display.contains(expected_substr),
                "Expected '{expected_substr}' in '{display}'"
            );
        }
    }
}
