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
    Authentication,
    /// The MS-TSGU tunnel to the RD Gateway could not be established.
    GatewayFailure,
    /// The GFX pipeline produced no decodable frame or failed to decode.
    GraphicsPipeline,
    /// The server offers only a security protocol IronRDP does not implement.
    SecurityUnsupported,
    /// IronRDP and the server disagree on the wire protocol.
    ProtocolIncompatible,
    /// Anything else: unreachable host, timeout, TLS failure, local error.
    Other,
}

impl RdpFailureClass {
    /// Returns `true` when handing the session to external `FreeRDP` can help.
    #[must_use]
    pub const fn warrants_freerdp_fallback(self) -> bool {
        matches!(
            self,
            Self::SecurityUnsupported
                | Self::ProtocolIncompatible
                | Self::GraphicsPipeline
                | Self::GatewayFailure
        )
    }

    /// Returns `true` when fallback would weaken the negotiated security.
    #[must_use]
    pub const fn requires_explicit_consent(self) -> bool {
        matches!(self, Self::SecurityUnsupported)
    }
}

/// `NTSTATUS` codes CredSSP/NLA returns for credential problems.
const AUTH_NSTATUS_CODES: &[&str] = &[
    "0xc0000064", // STATUS_NO_SUCH_USER
    "0xc000006d", // STATUS_LOGON_FAILURE
    "0xc000006a", // STATUS_WRONG_PASSWORD
    "0xc000006e", // STATUS_ACCOUNT_RESTRICTION
    "0xc000006f", // STATUS_INVALID_LOGON_HOURS
    "0xc0000070", // STATUS_INVALID_WORKSTATION
    "0xc0000071", // STATUS_PASSWORD_EXPIRED
    "0xc0000072", // STATUS_ACCOUNT_DISABLED
    "0xc000015b", // STATUS_LOGON_TYPE_NOT_GRANTED
    "0xc0000193", // STATUS_ACCOUNT_EXPIRED
    "0xc0000224", // STATUS_PASSWORD_MUST_CHANGE
    "0xc0000234", // STATUS_ACCOUNT_LOCKED_OUT
];

/// Symbolic `NTSTATUS` names and other markers of a credential rejection.
const AUTH_MARKERS: &[&str] = &[
    "authentication failed",
    "status_no_such_user",
    "status_logon_failure",
    "status_wrong_password",
    "status_password_expired",
    "status_password_must_change",
    "status_account_disabled",
    "status_account_expired",
    "status_account_locked_out",
    "status_account_restriction",
    "status_logon_type_not_granted",
    "accessdenied",
];

/// Markers of a failed MS-TSGU tunnel to the RD Gateway.
///
/// Produced by the gateway branch of the embedded connect path; the external
/// FreeRDP client speaks the same protocol with a wider set of authentication
/// methods (`ironrdp-mstsgu` only offers HTTP Basic), so it is worth a try.
const GATEWAY_MARKERS: &[&str] = &["rd gateway connection failed"];

/// TLS, certificate, and transport failures that must not trigger fallback.
const NON_FALLBACK_MARKERS: &[&str] = &[
    "tls",
    "certificate",
    "ssl_cert_not_on_server",
    "unknown issuer",
    "x509",
    "transport",
    "connection refused",
    "connection reset",
    "connection timed out",
    "operation timed out",
    "dns_name_not_found",
    "host not found",
    "network is unreachable",
    "no route to host",
];

/// Markers of a GFX/EGFX pipeline problem.
///
/// `gfx unsupported codec` covers the case where the server sends surface
/// content in a codec `ironrdp-egfx` cannot decode. Retrying without GFX is the
/// right response: the RemoteFX/bitmap path has no such gap (issue [#262]).
///
/// [#262]: https://github.com/totoshko88/RustConn/issues/262
const GRAPHICS_MARKERS: &[&str] = &[
    "no-frame-watchdog",
    "gfx pipeline decode failure",
    "gfx unsupported codec",
];

/// Explicit security protocols IronRDP cannot speak.
const SECURITY_MARKERS: &[&str] = &[
    "standard rdp security",
    "ssl_not_allowed_by_server",
    "hybrid_required_by_server",
    "unsupported security protocol",
];

/// Specific IronRDP/server protocol mismatches that justify fallback.
/// Generic finalize and negotiation wrappers are intentionally excluded.
const PROTOCOL_MARKERS: &[&str] = &[
    "serverdemandactive",
    "serverdeactivateall",
    "invalid state (this is a bug)",
    "unexpected share control pdu",
    "unsupported pdu",
    "decode error",
    "unsupported fast-path update code",
];

/// Classifies an embedded RDP failure message into a [`RdpFailureClass`].
///
/// Authentication is checked first, then RD Gateway tunnel failures. TLS,
/// certificate, and transport roots take precedence over fallback-worthy
/// protocol markers.
#[must_use]
pub fn classify_rdp_failure(msg: &str) -> RdpFailureClass {
    let lower = msg.to_ascii_lowercase();

    if is_authentication_failure_lower(&lower) {
        return RdpFailureClass::Authentication;
    }
    // Checked before the transport markers on purpose: a gateway failure wraps
    // the underlying cause ("TCP connect", "TLS connect", "WS Upgrade"), and
    // those words would otherwise classify the whole message as a plain
    // transport error and strand the session (issue #246). The external client
    // implements the same tunnel with more authentication methods, so handing
    // it over is worthwhile whatever the inner cause was.
    if GATEWAY_MARKERS.iter().any(|m| lower.contains(m)) {
        return RdpFailureClass::GatewayFailure;
    }
    if NON_FALLBACK_MARKERS.iter().any(|m| lower.contains(m)) {
        return RdpFailureClass::Other;
    }
    if GRAPHICS_MARKERS.iter().any(|m| lower.contains(m)) {
        return RdpFailureClass::GraphicsPipeline;
    }
    if SECURITY_MARKERS.iter().any(|m| lower.contains(m)) {
        return RdpFailureClass::SecurityUnsupported;
    }
    if PROTOCOL_MARKERS.iter().any(|m| lower.contains(m)) {
        return RdpFailureClass::ProtocolIncompatible;
    }
    RdpFailureClass::Other
}

/// Returns `true` when the message describes a rejected credential.
#[must_use]
pub fn is_authentication_failure(msg: &str) -> bool {
    is_authentication_failure_lower(&msg.to_ascii_lowercase())
}

fn is_authentication_failure_lower(lower: &str) -> bool {
    AUTH_NSTATUS_CODES.iter().any(|c| lower.contains(c))
        || AUTH_MARKERS.iter().any(|m| lower.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ISSUE_235: &str = "Connection failed: Connection begin failed: \
         negotiation failure: server only supports Standard RDP Security";

    const CREDSSP_LOGON_FAILURE: &str = "Connection failed: Connection finalize failed: \
         CredSSP server returned an error status; \
         nstatus: Some(NStatusCode(0xc000006d))";

    #[test]
    fn standard_rdp_security_is_security_unsupported() {
        let class = classify_rdp_failure(ISSUE_235);
        assert_eq!(class, RdpFailureClass::SecurityUnsupported);
        assert!(class.warrants_freerdp_fallback());
    }

    #[test]
    fn classification_is_case_insensitive() {
        assert_eq!(
            classify_rdp_failure("UNEXPECTED SHARE CONTROL PDU"),
            RdpFailureClass::ProtocolIncompatible
        );
        assert_eq!(
            classify_rdp_failure("GfX PiPeLiNe DeCoDe FaIlUrE"),
            RdpFailureClass::GraphicsPipeline
        );
        assert_eq!(
            classify_rdp_failure("SERVER ONLY SUPPORTS STANDARD RDP SECURITY"),
            RdpFailureClass::SecurityUnsupported
        );
    }

    #[test]
    fn credssp_logon_failure_is_authentication() {
        let class = classify_rdp_failure(CREDSSP_LOGON_FAILURE);
        assert_eq!(class, RdpFailureClass::Authentication);
        assert!(!class.warrants_freerdp_fallback());
    }

    #[test]
    fn missing_account_statuses_are_authentication() {
        for marker in [
            "nstatus: Some(NStatusCode(0xC0000064))",
            "nstatus: Some(NStatusCode(0xc0000193))",
            "STATUS_NO_SUCH_USER",
            "status_account_expired",
        ] {
            assert_eq!(
                classify_rdp_failure(marker),
                RdpFailureClass::Authentication,
                "marker was not classified as authentication: {marker}"
            );
        }
    }

    #[test]
    fn tls_and_certificate_failures_never_fall_back() {
        for msg in [
            "Connection finalize failed: TLS handshake failed: invalid peer certificate",
            "Unexpected Share Control Pdu after certificate verification failed",
            "SSL_CERT_NOT_ON_SERVER during negotiation",
        ] {
            let class = classify_rdp_failure(msg);
            assert_eq!(class, RdpFailureClass::Other, "message: {msg}");
            assert!(!class.warrants_freerdp_fallback(), "message: {msg}");
        }
    }

    #[test]
    fn transport_failures_never_fall_back() {
        for msg in [
            "negotiation failed: ERRCONNECT_CONNECT_TRANSPORT_FAILED",
            "ServerDemandActive: connection reset by peer",
            "Connection refused (os error 111)",
        ] {
            let class = classify_rdp_failure(msg);
            assert_eq!(class, RdpFailureClass::Other, "message: {msg}");
            assert!(!class.warrants_freerdp_fallback(), "message: {msg}");
        }
    }

    #[test]
    fn generic_finalize_and_negotiation_are_not_protocol_markers() {
        for msg in [
            "Connection finalize failed",
            "connect_finalize failed",
            "negotiation failure",
            "NegotiationError",
        ] {
            assert_eq!(classify_rdp_failure(msg), RdpFailureClass::Other);
        }
    }

    #[test]
    fn specific_protocol_and_graphics_markers_still_fall_back() {
        assert_eq!(
            classify_rdp_failure("invalid state (this is a bug)"),
            RdpFailureClass::ProtocolIncompatible
        );
        assert_eq!(
            classify_rdp_failure("NO-FRAME-WATCHDOG: no decodable frame"),
            RdpFailureClass::GraphicsPipeline
        );
    }

    /// Exact message built by the GFX unsupported-codec branch of the GUI event
    /// loop; it must reach the Legacy retry rather than being reported as-is.
    #[test]
    fn unsupported_gfx_codec_retries_without_gfx() {
        let class =
            classify_rdp_failure("gfx unsupported codec: Avc444v2 (5 surface updates dropped)");
        assert_eq!(class, RdpFailureClass::GraphicsPipeline);
        assert!(class.warrants_freerdp_fallback());
        assert!(!class.requires_explicit_consent());
    }

    #[test]
    fn gateway_tunnel_failure_falls_back_to_external_client() {
        // Exact message built by the gateway branch of `establish_connection`.
        let class = classify_rdp_failure("RD Gateway connection failed: WS Upgrade error");
        assert_eq!(class, RdpFailureClass::GatewayFailure);
        assert!(class.warrants_freerdp_fallback());
        assert!(!class.requires_explicit_consent());
    }

    #[test]
    fn gateway_failure_wins_over_wrapped_transport_cause() {
        // The wrapped cause carries transport/TLS wording; the gateway class
        // must still win so the session reaches the external client (#246).
        for msg in [
            "RD Gateway connection failed: TCP connect: connection refused (os error 111)",
            "RD Gateway connection failed: TLS connect: invalid peer certificate",
            "RD Gateway connection failed: custom error: host not found",
        ] {
            let class = classify_rdp_failure(msg);
            assert_eq!(class, RdpFailureClass::GatewayFailure, "message: {msg}");
            assert!(class.warrants_freerdp_fallback(), "message: {msg}");
        }
    }

    #[test]
    fn rejected_gateway_credentials_stay_authentication() {
        // Credential rejection outranks the gateway marker: the external client
        // would be refused by the same account.
        let class = classify_rdp_failure(
            "RD Gateway connection failed: nstatus: Some(NStatusCode(0xc000006d))",
        );
        assert_eq!(class, RdpFailureClass::Authentication);
        assert!(!class.warrants_freerdp_fallback());
    }

    #[test]
    fn direct_transport_failures_are_not_gateway_failures() {
        assert_eq!(
            classify_rdp_failure("Failed to connect to host.internal:3389: connection refused"),
            RdpFailureClass::Other
        );
    }

    #[test]
    fn only_legacy_security_fallback_requires_explicit_consent() {
        assert!(RdpFailureClass::SecurityUnsupported.requires_explicit_consent());
        for class in [
            RdpFailureClass::Authentication,
            RdpFailureClass::GatewayFailure,
            RdpFailureClass::GraphicsPipeline,
            RdpFailureClass::ProtocolIncompatible,
            RdpFailureClass::Other,
        ] {
            assert!(!class.requires_explicit_consent(), "class: {class:?}");
        }
    }

    #[test]
    fn auth_wins_over_non_fallback_and_protocol_markers() {
        let msg = "TLS handshake; unexpected Share Control Pdu; \
                   nstatus: Some(NStatusCode(0xc000006d))";
        assert_eq!(classify_rdp_failure(msg), RdpFailureClass::Authentication);
    }
}
