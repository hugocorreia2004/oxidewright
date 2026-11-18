//! Deserialization tolerates CDP payloads that do not match this protocol
//! revision exactly: missing fields that have a default, and enum values a
//! newer browser (or another CDP implementation) sends.

use chromiumoxide_cdp::cdp::CdpEventMessage;
use chromiumoxide_cdp::cdp::browser_protocol::network::{CookieParam, SetCookiesParams};
use chromiumoxide_cdp::cdp::browser_protocol::page::{
    AdFrameType, CrossOriginIsolatedContextType, Frame, GetFrameTreeReturns, SecureContextType,
};
use chromiumoxide_cdp::cdp::events::CdpEvent;
use serde_json::json;

/// mattsse/chromiumoxide#335: a CDP server that omits the experimental
/// `Page.Frame` fields must not break `Page.getFrameTree`.
#[test]
fn frame_tree_without_experimental_fields_deserializes() {
    let tree: GetFrameTreeReturns = serde_json::from_value(json!({
        "frameTree": {
            "frame": {
                "id": "main",
                "loaderId": "loader",
                "url": "https://example.test/",
                "domainAndRegistry": "example.test",
                "securityOrigin": "https://example.test",
                "mimeType": "text/html"
            }
        }
    }))
    .expect("missing experimental fields fall back to their defaults");

    let frame = tree.frame_tree.frame;
    assert_eq!(frame.id.as_ref(), "main");
    assert_eq!(frame.secure_context_type, SecureContextType::default());
    assert_eq!(
        frame.cross_origin_isolated_context_type,
        CrossOriginIsolatedContextType::default()
    );
    assert!(frame.gated_api_features.is_empty());
}

#[test]
fn unknown_enum_values_do_not_fail_the_payload() {
    let frame: Frame = serde_json::from_value(json!({
        "id": "main",
        "loaderId": "loader",
        "url": "https://example.test/",
        "domainAndRegistry": "example.test",
        "securityOrigin": "https://example.test",
        "mimeType": "text/html",
        "secureContextType": "SomeFutureContextType",
        "crossOriginIsolatedContextType": "Isolated",
        "gatedAPIFeatures": ["SharedArrayBuffers", "SomeFutureFeature"],
        "adFrameStatus": { "adFrameType": "SomeFutureAdFrameType" }
    }))
    .expect("unknown enum values are tolerated");

    // A required enum falls back to its default, a known value is kept.
    assert_eq!(frame.secure_context_type, SecureContextType::default());
    assert_eq!(
        frame.cross_origin_isolated_context_type,
        CrossOriginIsolatedContextType::Isolated
    );
    assert_eq!(frame.gated_api_features.len(), 2);
    assert_eq!(
        frame
            .ad_frame_status
            .expect("ad frame status")
            .ad_frame_type,
        AdFrameType::default()
    );
}

/// An event whose params cannot be read with this protocol revision keeps
/// its method and session and arrives as raw JSON.
#[test]
fn malformed_event_params_fall_back_to_raw_json() {
    let message: CdpEventMessage = serde_json::from_value(json!({
        "method": "Page.frameNavigated",
        "sessionId": "session",
        "params": { "frame": "not an object" }
    }))
    .expect("the message itself still parses");

    assert_eq!(message.method.as_ref(), "Page.frameNavigated");
    assert_eq!(message.session_id.as_deref(), Some("session"));
    match message.params {
        CdpEvent::Other(value) => assert_eq!(value["frame"], "not an object"),
        other => panic!("expected raw json, got {other:?}"),
    }
}

#[test]
fn well_formed_event_params_stay_typed() {
    let message: CdpEventMessage = serde_json::from_value(json!({
        "method": "Page.frameStoppedLoading",
        "params": { "frameId": "main" }
    }))
    .expect("event parses");
    assert!(matches!(
        message.params,
        CdpEvent::PageFrameStoppedLoading(_)
    ));
}

/// mattsse/chromiumoxide#293: a mandatory array is serialized even when it
/// is empty, otherwise the browser rejects the command.
#[test]
fn empty_mandatory_array_is_serialized() {
    let params = serde_json::to_value(SetCookiesParams::new(Vec::<CookieParam>::new()))
        .expect("params serialize");
    assert_eq!(params, json!({ "cookies": [] }));
}
