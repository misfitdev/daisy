//! Secure Enclave keys persisted in the Data Protection Keychain.

use anyhow::{Context, Result, bail};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::item::{ItemSearchOptions, KeyClass, Location, Reference, SearchResult};
use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey, Token};
use security_framework_sys::access_control::kSecAccessControlPrivateKeyUsage;

pub fn create() -> Result<Vec<u8>> {
    let mut id = [0; 32];
    getrandom::fill(&mut id)?;
    let label = format!(
        "dev.misfit.daisy.device.{}",
        id.iter().map(|byte| format!("{byte:02x}")).collect::<String>()
    );
    let access = SecAccessControl::create_with_protection(
        Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
        kSecAccessControlPrivateKeyUsage,
    )?;
    let mut options = GenerateKeyOptions::default();
    options
        .set_key_type(KeyType::ec_sec_prime_random())
        .set_size_in_bits(256)
        .set_token(Token::SecureEnclave)
        .set_location(Location::DataProtectionKeychain)
        .set_label(&label)
        .set_access_control(access);
    SecKey::new(&options).map_err(|error| anyhow::anyhow!("creating Secure Enclave identity: {error}"))?;
    Ok(label.into_bytes())
}

fn load(reference: &[u8]) -> Result<SecKey> {
    let label = std::str::from_utf8(reference).context("invalid device identity reference")?;
    if !label.starts_with("dev.misfit.daisy.device.") {
        bail!("invalid device identity reference");
    }
    let results = ItemSearchOptions::new()
        .key_class(KeyClass::private())
        .label(label)
        .ignore_legacy_keychains()
        .load_refs(true)
        .search()
        .context("looking up Secure Enclave identity")?;
    match results.into_iter().next() {
        Some(SearchResult::Ref(Reference::Key(key))) => {
            use core_foundation::{
                base::{TCFType, ToVoid},
                string::CFString,
            };
            use security_framework_sys::item::{kSecAttrTokenID, kSecAttrTokenIDSecureEnclave};
            let attributes = key.attributes();
            // SAFETY: Security returns a CFString for the token identifier, owned by attributes.
            let enclave = unsafe {
                attributes.find(kSecAttrTokenID.to_void()).is_some_and(|value| {
                    CFString::wrap_under_get_rule(value.cast())
                        == CFString::wrap_under_get_rule(kSecAttrTokenIDSecureEnclave)
                })
            };
            if !enclave {
                bail!("device identity is not a Secure Enclave key");
            }
            Ok(key)
        }
        _ => Err(security_framework::base::Error::from_code(security_framework_sys::base::errSecItemNotFound).into()),
    }
}

pub fn public(reference: &[u8]) -> Result<Vec<u8>> {
    let public = load(reference)?.public_key().context("device key has no public key")?;
    let bytes = public.external_representation().context("reading device public key")?;
    let key = p256::PublicKey::from_sec1_bytes(&bytes).context("invalid device public key")?;
    use p256::elliptic_curve::sec1::ToSec1Point;
    Ok(key.to_sec1_point(true).as_bytes().to_vec())
}

pub fn sign(reference: &[u8], message: &[u8]) -> Result<Vec<u8>> {
    load(reference)?
        .create_signature(Algorithm::ECDSASignatureMessageX962SHA256, message)
        .map_err(|error| anyhow::anyhow!("signing with Secure Enclave identity: {error}"))
}

pub fn delete(reference: &[u8]) -> Result<()> {
    match load(reference) {
        Ok(key) => key.delete().context("removing Secure Enclave identity"),
        Err(error)
            if error
                .downcast_ref::<security_framework::base::Error>()
                .is_some_and(|source| source.code() == security_framework_sys::base::errSecItemNotFound) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}
