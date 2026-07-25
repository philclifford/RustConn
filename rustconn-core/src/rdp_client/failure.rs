//! Classification of embedded RDP connection failures.
//!
//! The embedded IronRDP client reports failures to the GUI as plain strings
//! (`RdpClientEvent::Error`), because the error crosses a thread channel and
//! upstream `ironrdp` error types are not `Clone`. The GUI has to decide what
//! to do with each failure:
//!
//! * retry with a different graphics mode,
//! * hand the session over to the external `FreeRDP` client,
//! * or surface an authentication error and stop.
//!
//! Historically that decision was a list of `msg.contains(..)` checks inlined
//! in the GTK layer, which silently broke every time an upstream error string
//! changed (issues [#199], [#234], [#235]). The matching now lives here as a
//! pure, unit-tested function so the GUI only switches on the resulting
//! [`RdpFailureClass`].
//!
//! [#199]: https://github.com/totoshko88/RustConn/issues/199
//! [#234]: https://github.com/totoshko88/RustConn/issues/234
//! [#235]: https://github.com/totoshko88/RustConn/issues/235

/// What kind of failure ended (or prevented) an embedded RDP session.
///
/// Ordered from "most specific" to "least specific"; [`classify_rdp_failure`]
/// returns the first matching class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RdpFailureClass {
    /// The server rejected the supplied credentials (CredSSP/NLA `NTSTATUS`
    /// logon failure, disabled or locked account, expired password).
    ///
    /// Retrying with the external client uses the same credentials, so a
    /// fallback is pointless — the GUI must report the auth error instead.
    Authentication,

    /// The GFX (EGFX) pipeline produced no decodable frame or failed to decode.
    ///
    /// Worth one retry with Legacy graphics before falling back.
    GraphicsPipeline,

    /// The server offers only a security protocol IronRDP does not implement
    /// (Standard RDP Security — the legacy RC4 "Encrypted" mode).
    ///
    /// Permanent incompatibility — the external `FreeRDP` client supports it.
    SecurityUnsupported,

    /// IronRDP and the server disagree on the wire protocol (unexpected PDU,
    /// connector state machine bug, undecodable update).
    ///
    /// The external `FreeRDP` client usually copes, so fall back.
    ProtocolIncompatible,

    /// Anything else: unreachable host, timeout, TLS failure, local error.
    /// No fallback — the message is shown to the user as-is.
    Other,
}

impl RdpFailureClass {
    /// Returns `true` when handing the session to external `FreeRDP` can help.
    #[must_use]
    pub const fn warrants_freerdp_fallback(self) -> bool {
        matches!(
            self,
            Self::SecurityUnsupported | Self::ProtocolIncompatible | Self::GraphicsPipeline
        )
    }
}

/// `NTSTATUS` codes CredSSP/NLA returns for credential problems.
///
/// The values arrive inside the `sspi` error debug output as
/// `nstatus: Some(NStatusCode(0xc000006d))`, so they are matched as text.
/// Kept lowercase — [`is_authentication_failure`] lowercases the haystack.
const AUTH_NSTATUS_CODES: &[&str] = &[
    "0xc000006d", // STATUS_LOGON_FAILURE — wrong user name or password
    "0xc000006a", // STATUS_WRONG_PASSWORD
    "0xc000006e", // STATUS_ACCOUNT_RESTRICTION
    "0xc000006f", // STATUS_INVALID_LOGON_HOURS
    "0xc0000070", // STATUS_INVALID_WORKSTATION
    "0xc0000071", // STATUS_PASSWORD_EXPIRED
    "0xc0000072", // STATUS_ACCOUNT_DISABLED
    "0xc000015b", // STATUS_LOGON_TYPE_NOT_GRANTED
    "0xc0000224", // STATUS_PASSWORD_MUST_CHANGE
    "0xc0000234", // STATUS_ACCOUNT_LOCKED_OUT
];

/// Symbolic `NTSTATUS` names and other markers of a credential rejection.
const AUTH_MARKERS: &[&str] = &[
    "Authentication failed",
    "STATUS_LOGON_FAILURE",
    "STATUS_WRONG_PASSWORD",
    "STATUS_PASSWORD_EXPIRED",
    "STATUS_PASSWORD_MUST_CHANGE",
    "STATUS_ACCOUNT_DISABLED",
    "STATUS_ACCOUNT_LOCKED_OUT",
    "STATUS_ACCOUNT_RESTRICTION",
    "STATUS_LOGON_TYPE_NOT_GRANTED",
    "AccessDenied",
];

/// Markers of a GFX/EGFX pipeline problem (see [`RdpFailureClass::GraphicsPipeline`]).
const GRAPHICS_MARKERS: &[&str] = &["no-frame-watchdog", "GFX pipeline decode failure"];

/// Markers of a security protocol IronRDP cannot speak.
///
/// `SSL_NOT_ALLOWED_BY_SERVER` is reported by `ironrdp-connector` as
/// "negotiation failure: server only supports Standard RDP Security"
/// (issue #235); the remaining codes cover the other negotiation refusals
/// that no amount of retrying inside IronRDP can satisfy.
const SECURITY_MARKERS: &[&str] = &[
    "Standard RDP Security",
    "SSL_NOT_ALLOWED_BY_SERVER",
    "SSL_CERT_NOT_ON_SERVER",
    "HYBRID_REQUIRED_BY_SERVER",
    "Unsupported security protocol",
];

/// Markers of an IronRDP/server protocol mismatch that external `FreeRDP`
/// is likely to survive.
const PROTOCOL_MARKERS: &[&str] = &[
    "ServerDemandActive",
    "ServerDeactivateAll",
    "connect_finalize",
    "Connection finalize failed",
    "invalid state (this is a bug)",
    "unexpected Share Control Pdu",
    "Unsupported PDU",
    "negotiation failure", // current ironrdp wording
    "negotiation failed",  // older/alternate wording
    "NegotiationError",
    "decode error",
    "unsupported fast-path update code",
];

/// Classifies an embedded RDP failure message into a [`RdpFailureClass`].
///
/// Authentication is checked first — a CredSSP logon failure also mentions
/// "Connection finalize failed", and treating it as a protocol problem caused
/// a pointless `FreeRDP` fallback with the very same credentials.
#[must_use]
pub fn classify_rdp_failure(msg: &str) -> RdpFailureClass {
    if is_authentication_failure(msg) {
        return RdpFailureClass::Authentication;
    }
    if GRAPHICS_MARKERS.iter().any(|m| msg.contains(m)) {
        return RdpFailureClass::GraphicsPipeline;
    }
    if SECURITY_MARKERS.iter().any(|m| msg.contains(m)) {
        return RdpFailureClass::SecurityUnsupported;
    }
    if PROTOCOL_MARKERS.iter().any(|m| msg.contains(m)) {
        return RdpFailureClass::ProtocolIncompatible;
    }
    RdpFailureClass::Other
}

/// Returns `true` when the message describes a rejected credential.
///
/// Exposed separately so the embedded client can map the failure to
/// `RdpClientError::AuthenticationFailed` at the source, before the error is
/// flattened into a string for the GUI channel.
#[must_use]
pub fn is_authentication_failure(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    AUTH_NSTATUS_CODES.iter().any(|c| lower.contains(c))
        || AUTH_MARKERS.iter().any(|m| msg.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real message from issue #235 (Windows Server 2022, NLA disabled).
    const ISSUE_235: &str = "Connection failed: Connection begin failed: \
         [negotiation failure @ ironrdp-connector-0.10.0/src/connection.rs:352] \
         negotiation failure: server only supports Standard RDP Security";

    /// Real message from a CredSSP `STATUS_LOGON_FAILURE`, with the connector
    /// error kind appended by the embedded client.
    const CREDSSP_LOGON_FAILURE: &str = "Connection failed: Connection finalize failed: \
         [CredSSP @ ironrdp-async-0.10.0/src/connector.rs:107] CredSSP \
         [kind: Credssp(Error { error_type: InvalidToken, \
         description: \"CredSSP server returned an error status\", \
         nstatus: Some(NStatusCode(0xc000006d)) })]";

    #[test]
    fn standard_rdp_security_is_security_unsupported() {
        assert_eq!(
            classify_rdp_failure(ISSUE_235),
            RdpFailureClass::SecurityUnsupported
        );
        assert!(
            classify_rdp_failure(ISSUE_235).warrants_freerdp_fallback(),
            "issue #235 must fall back to external FreeRDP"
        );
    }

    #[test]
    fn credssp_logon_failure_is_authentication() {
        assert_eq!(
            classify_rdp_failure(CREDSSP_LOGON_FAILURE),
            RdpFailureClass::Authentication
        );
        assert!(
            !classify_rdp_failure(CREDSSP_LOGON_FAILURE).warrants_freerdp_fallback(),
            "auth failures must not trigger a fallback with the same credentials"
        );
    }

    #[test]
    fn typed_authentication_error_display_is_detected() {
        // `RdpClientError::AuthenticationFailed` renders with this prefix.
        assert_eq!(
            classify_rdp_failure("Authentication failed: CredSSP rejected the credentials"),
            RdpFailureClass::Authentication
        );
    }

    #[test]
    fn nstatus_case_is_ignored() {
        assert!(is_authentication_failure(
            "nstatus: Some(NStatusCode(0xC000006D))"
        ));
    }

    #[test]
    fn gnome_remote_desktop_finalize_bug_is_protocol_incompatible() {
        let msg = "Connection failed: Connection finalize failed: invalid state (this is a bug)";
        assert_eq!(
            classify_rdp_failure(msg),
            RdpFailureClass::ProtocolIncompatible
        );
    }

    #[test]
    fn share_control_pdu_is_protocol_incompatible() {
        let msg = "Session error: unexpected Share Control Pdu (expected ServerDemandActive)";
        assert_eq!(
            classify_rdp_failure(msg),
            RdpFailureClass::ProtocolIncompatible
        );
    }

    #[test]
    fn no_frame_watchdog_is_graphics_pipeline() {
        assert_eq!(
            classify_rdp_failure("no-frame-watchdog: no decodable frame within 12s"),
            RdpFailureClass::GraphicsPipeline
        );
    }

    #[test]
    fn unreachable_host_is_other() {
        let msg = "Connection failed: failed to connect to 10.0.0.1:3389: \
                   Connection refused (os error 111)";
        assert_eq!(classify_rdp_failure(msg), RdpFailureClass::Other);
        assert!(!classify_rdp_failure(msg).warrants_freerdp_fallback());
    }

    #[test]
    fn timeout_is_other() {
        assert_eq!(
            classify_rdp_failure("Operation timed out"),
            RdpFailureClass::Other
        );
    }

    #[test]
    fn auth_wins_over_protocol_marker() {
        // Both markers present: "Connection finalize failed" (protocol) and
        // the logon-failure NTSTATUS (auth). Auth must win.
        let msg = "Connection finalize failed: CredSSP nstatus: Some(NStatusCode(0xc000006d))";
        assert_eq!(classify_rdp_failure(msg), RdpFailureClass::Authentication);
    }
}
