//! Public release fixture: signature checks run offline against bundled roots.

use base64::Engine;
use daisy::install::release::verify_attestations;
use serde_json::Value;

const RESPONSE: &[u8] = include_bytes!("fixtures/release-attestation.json");
const HASH: &str = "ae7c85db71e530c324ee269df19bb988300246cee6a1d5098a770c0c7025fb9c";
const NAME: &str = "Daisy-0.6.0-macos-arm64.zip";
const TAG: &str = "v0.6.0";

#[test]
fn published_release_signature_and_provenance_verify_offline() {
    verify_attestations(RESPONSE, HASH, NAME, TAG).unwrap();
}

#[test]
fn another_asset_digest_or_tag_cannot_authorize_the_release() {
    assert!(verify_attestations(RESPONSE, HASH, "different.zip", TAG).is_err());
    assert!(verify_attestations(RESPONSE, HASH, NAME, "v0.7.0").is_err());
    assert!(verify_attestations(RESPONSE, &"0".repeat(64), NAME, TAG).is_err());
    assert!(verify_attestations(br#"{"attestations":[]}"#, HASH, NAME, TAG).is_err());
}

#[test]
fn forged_provenance_or_signature_is_rejected() {
    let mut response: Value = serde_json::from_slice(RESPONSE).unwrap();
    let envelope = &mut response["attestations"][0]["bundle"]["dsseEnvelope"];
    let mut payload: Value = serde_json::from_slice(
        &base64::engine::general_purpose::STANDARD
            .decode(envelope["payload"].as_str().unwrap())
            .unwrap(),
    )
    .unwrap();
    payload["predicate"]["buildDefinition"]["externalParameters"]["workflow"]["repository"] =
        "https://github.com/other/project".into();
    envelope["payload"] = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_vec(&payload).unwrap())
        .into();
    assert!(verify_attestations(&serde_json::to_vec(&response).unwrap(), HASH, NAME, TAG).is_err());

    let mut response: Value = serde_json::from_slice(RESPONSE).unwrap();
    let signature = &mut response["attestations"][0]["bundle"]["dsseEnvelope"]["signatures"][0]["sig"];
    let mut bytes = base64::engine::general_purpose::STANDARD
        .decode(signature.as_str().unwrap())
        .unwrap();
    bytes[0] ^= 1;
    *signature = base64::engine::general_purpose::STANDARD.encode(bytes).into();
    assert!(verify_attestations(&serde_json::to_vec(&response).unwrap(), HASH, NAME, TAG).is_err());
}

#[test]
fn untrusted_transparency_timestamp_is_rejected() {
    let mut response: Value = serde_json::from_slice(RESPONSE).unwrap();
    let timestamp =
        &mut response["attestations"][0]["bundle"]["verificationMaterial"]["tlogEntries"][0]["integratedTime"];
    *timestamp = "1".into();
    assert!(verify_attestations(&serde_json::to_vec(&response).unwrap(), HASH, NAME, TAG).is_err());
}
