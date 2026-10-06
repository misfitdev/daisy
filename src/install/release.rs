//! Verified release selection, provenance authorization and bounded extraction.

use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use base64::Engine;
use semver::Version;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::update::Build;
use crate::update::Manifest;

const API: &str = "https://api.github.com/repos/misfitdev/daisy";
const REPO: &str = "https://github.com/misfitdev/daisy";
const WORKFLOW: &str = ".github/workflows/release.yml";
const METADATA_LIMIT: usize = 1024 * 1024;
const MANIFEST_LIMIT: usize = 64 * 1024;
const ARCHIVE_LIMIT: usize = 256 * 1024 * 1024;
const EXPANDED_LIMIT: u64 = 512 * 1024 * 1024;

/// Sources must enforce the byte limit while reading, not after buffering.
pub trait Source {
    fn get(&mut self, url: &str, limit: usize) -> Result<Vec<u8>>;
}

#[derive(Debug, thiserror::Error)]
#[error("Daisy is already up to date")]
pub struct AlreadyCurrent;

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    size: u64,
}

/// Only the verified download path can construct an installation candidate.
pub struct VerifiedRelease {
    build: Build,
    archive: Vec<u8>,
}

impl VerifiedRelease {
    pub fn build(&self) -> &Build {
        &self.build
    }

    pub fn extract(&self, directory: &Path) -> Result<PathBuf> {
        extract_archive(&self.archive, directory)
    }
}

/// Select an exact stable release, or the latest stable release. Neither
/// release metadata nor a caller's requested version authorizes installation.
pub fn fetch(source: &mut impl Source, previous: &Build, requested: Option<&Version>) -> Result<VerifiedRelease> {
    fetch_verified(source, previous, requested, verify_attestations)
}

fn fetch_verified(
    source: &mut impl Source,
    previous: &Build,
    requested: Option<&Version>,
    verify: impl Fn(&[u8], &str, &str, &str) -> Result<()>,
) -> Result<VerifiedRelease> {
    if let Some(version) = requested {
        ensure!(version.pre.is_empty(), "prereleases cannot be installed by the updater");
    }
    let endpoint = requested.map_or_else(
        || format!("{API}/releases/latest"),
        |version| format!("{API}/releases/tags/v{version}"),
    );
    let release: Release =
        serde_json::from_slice(&read(source, &endpoint, METADATA_LIMIT)?).context("reading selected release")?;
    ensure!(
        !release.draft && !release.prerelease,
        "the selected release is not stable"
    );
    let version = Version::parse(release.tag_name.strip_prefix('v').unwrap_or(""))
        .context("the selected release has no canonical version tag")?;
    ensure!(
        release.tag_name == format!("v{version}"),
        "the selected release tag is not canonical"
    );
    ensure!(
        requested.is_none_or(|wanted| *wanted == version),
        "the release does not match the requested version"
    );
    let next = Build {
        version: version.to_string(),
        protocol: previous.protocol,
    };
    if requested.is_none() && Version::parse(&previous.version).is_ok_and(|current| current >= version) {
        bail!(AlreadyCurrent);
    }
    ensure!(
        previous.accepts(&next),
        "the selected release is not newer than this system's stable version"
    );
    let architecture = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x86_64",
        _ => bail!("no release archive is available for this architecture"),
    };
    let archive_name = format!("Daisy-{version}-macos-{architecture}.zip");
    let manifest_name = format!("Daisy-{version}-update.toml");
    let manifest_asset = asset(&release, &manifest_name, MANIFEST_LIMIT)?;
    let archive_asset = asset(&release, &archive_name, ARCHIVE_LIMIT)?;
    let manifest_bytes = read(source, &manifest_asset.browser_download_url, MANIFEST_LIMIT)?;
    ensure!(
        manifest_bytes.len() as u64 == manifest_asset.size,
        "the manifest download is incomplete"
    );
    verify_download(source, &manifest_bytes, &manifest_name, &release.tag_name, &verify)?;
    let manifest: Manifest =
        toml::from_str(std::str::from_utf8(&manifest_bytes)?).context("reading verified compatibility metadata")?;
    ensure!(
        manifest.format == 1 && manifest.version == version.to_string() && manifest.protocol == previous.protocol,
        "the release metadata does not prove compatibility with this system"
    );
    let archive = read(source, &archive_asset.browser_download_url, ARCHIVE_LIMIT)?;
    ensure!(
        archive.len() as u64 == archive_asset.size,
        "the archive download is incomplete"
    );
    ensure!(
        manifest.matches(&version, previous.protocol, &archive),
        "the release archive does not match its compatibility metadata"
    );
    verify_download(source, &archive, &archive_name, &release.tag_name, &verify)?;
    Ok(VerifiedRelease { build: next, archive })
}

fn asset<'a>(release: &'a Release, name: &str, limit: usize) -> Result<&'a Asset> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matches
        .next()
        .with_context(|| format!("the release is missing {name}; install from its DMG manually"))?;
    ensure!(matches.next().is_none(), "the release contains duplicate assets");
    ensure!(
        asset.size > 0 && asset.size <= limit as u64,
        "the release asset exceeds the download limit"
    );
    ensure!(
        asset.browser_download_url == format!("{REPO}/releases/download/{}/{name}", release.tag_name),
        "the release asset is not from the expected repository and tag"
    );
    Ok(asset)
}

fn read(source: &mut impl Source, url: &str, limit: usize) -> Result<Vec<u8>> {
    let bytes = source.get(url, limit)?;
    ensure!(bytes.len() <= limit, "the response exceeds the download limit");
    Ok(bytes)
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

fn verify_download(
    source: &mut impl Source,
    bytes: &[u8],
    name: &str,
    tag: &str,
    verify: &impl Fn(&[u8], &str, &str, &str) -> Result<()>,
) -> Result<()> {
    let hash = digest(bytes);
    let attestations = read(
        source,
        &format!("{API}/attestations/sha256:{hash}?per_page=100"),
        METADATA_LIMIT,
    )?;
    verify(&attestations, &hash, name, tag).with_context(|| format!("verifying release provenance for {name}"))
}

/// Verify the signature, CA chain, transparency evidence and exact release
/// workflow identity before consulting the signed provenance claims.
pub fn verify_attestations(bytes: &[u8], hash: &str, name: &str, tag: &str) -> Result<()> {
    use sigstore_verify::trust_root::{SIGSTORE_PRODUCTION_TRUSTED_ROOT, TrustedRoot};
    use sigstore_verify::types::{Bundle, Sha256Hash};
    use sigstore_verify::{VerificationPolicy, verify};

    ensure!(bytes.len() <= METADATA_LIMIT, "attestations exceed the metadata limit");
    let response: Value = serde_json::from_slice(bytes).context("reading attestations")?;
    let attestations = response["attestations"]
        .as_array()
        .context("attestations are missing")?;
    ensure!(
        !attestations.is_empty() && attestations.len() <= 100,
        "no usable release attestation was published"
    );
    let identity = format!("{REPO}/{WORKFLOW}@refs/tags/{tag}");
    let root = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)?;
    let policy = VerificationPolicy::new(identity.clone(), "https://token.actions.githubusercontent.com");
    let artifact = Sha256Hash::from_hex(hash)?;
    let mut last_error = None;
    for attestation in attestations {
        let result = (|| -> Result<()> {
            let bundle: Bundle = serde_json::from_value(attestation["bundle"].clone())?;
            verify(artifact, &bundle, &policy, &root)?;
            let encoded = attestation["bundle"]["dsseEnvelope"]["payload"]
                .as_str()
                .context("signed provenance is missing")?;
            let payload = base64::engine::general_purpose::STANDARD.decode(encoded)?;
            let statement: Value = serde_json::from_slice(&payload)?;
            authorize_statement(&statement, hash, name, tag, &identity)
        })();
        match result {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.context("no release attestation matched")?)
}

fn authorize_statement(statement: &Value, hash: &str, name: &str, tag: &str, identity: &str) -> Result<()> {
    ensure!(
        statement["_type"] == "https://in-toto.io/Statement/v1"
            && statement["predicateType"] == "https://slsa.dev/provenance/v1",
        "unsupported release provenance format"
    );
    let definition = &statement["predicate"]["buildDefinition"];
    let workflow = &definition["externalParameters"]["workflow"];
    ensure!(
        definition["buildType"] == "https://actions.github.io/buildtypes/workflow/v1"
            && workflow["repository"] == REPO
            && workflow["path"] == WORKFLOW
            && workflow["ref"] == format!("refs/tags/{tag}")
            && statement["predicate"]["runDetails"]["builder"]["id"] == identity,
        "provenance is not from the expected release workflow and tag"
    );
    ensure!(
        statement["subject"].as_array().is_some_and(|subjects| subjects
            .iter()
            .any(|subject| subject["name"] == name && subject["digest"]["sha256"] == hash)),
        "provenance does not bind the selected release asset and digest"
    );
    Ok(())
}

/// Bound metadata allocation before opening the ZIP. The ZIP reader keeps
/// only the last entry with a duplicated raw name, so inspect the original
/// central directory to reject duplicates rather than silently shadow files.
fn validate_directory(bytes: &[u8]) -> Result<()> {
    ensure!(bytes.len() <= ARCHIVE_LIMIT, "the archive exceeds its limit");
    let footer = (bytes.len().saturating_sub(65_557)..bytes.len().saturating_sub(21))
        .rev()
        .find(|&offset| {
            bytes.get(offset..offset + 4) == Some(b"PK\x05\x06")
                && bytes.get(offset + 20..offset + 22).is_some_and(|length| {
                    offset + 22 + u16::from_le_bytes([length[0], length[1]]) as usize == bytes.len()
                })
        })
        .context("the release ZIP has no complete directory footer")?;
    let short = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]) as usize;
    let integer =
        |offset: usize| u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("bounded ZIP field")) as usize;
    let count = short(footer + 10);
    ensure!(
        short(footer + 4) == 0 && short(footer + 6) == 0 && short(footer + 8) == count,
        "split ZIP archives are not supported"
    );
    ensure!(count > 0 && count <= 10_000, "the ZIP entry count exceeds its limit");
    let mut offset = integer(footer + 16);
    let end = offset
        .checked_add(integer(footer + 12))
        .context("the ZIP directory size overflowed")?;
    ensure!(
        end == footer && offset <= end,
        "the ZIP directory is incomplete or uses unsupported ZIP64 metadata"
    );
    let mut names = std::collections::HashSet::new();
    for _ in 0..count {
        ensure!(
            offset + 46 <= end && bytes.get(offset..offset + 4) == Some(b"PK\x01\x02"),
            "the ZIP central directory is malformed"
        );
        let name_end = offset + 46 + short(offset + 28);
        let next = name_end + short(offset + 30) + short(offset + 32);
        ensure!(
            name_end <= next && next <= end && short(offset + 34) == 0,
            "the ZIP directory entry is malformed"
        );
        ensure!(
            names.insert(&bytes[offset + 46..name_end]),
            "the archive contains duplicate paths"
        );
        offset = next;
    }
    ensure!(offset == end, "the ZIP entry count does not match its directory");
    Ok(())
}

fn extract_archive(bytes: &[u8], directory: &Path) -> Result<PathBuf> {
    ensure!(
        directory.is_dir() && fs::read_dir(directory)?.next().is_none(),
        "extraction requires an empty private directory"
    );
    let metadata = fs::symlink_metadata(directory)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
        "the extraction directory is not private"
    );
    validate_directory(bytes)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).context("opening release archive")?;
    ensure!(
        !zip.is_empty() && zip.len() <= 10_000,
        "the release archive has too many entries"
    );
    let mut expanded = 0u64;
    let mut seen = std::collections::HashSet::new();
    // Validate every path and size before writing anything, including metadata.
    for index in 0..zip.len() {
        let entry = zip.by_index(index)?;
        let name = entry.name();
        ensure!(!name.contains('\\'), "the archive contains an invalid path");
        let path = entry.enclosed_name().context("the archive contains an unsafe path")?;
        ensure!(
            path.components().all(|part| matches!(part, Component::Normal(_))),
            "the archive contains an ambiguous path"
        );
        ensure!(
            path.starts_with("Daisy.app") || path.starts_with("__MACOSX"),
            "the archive contains an unexpected top-level entry"
        );
        let mode = entry.unix_mode().unwrap_or(0);
        ensure!(
            mode & 0o170000 == 0 || mode & 0o170000 == 0o100000 || mode & 0o170000 == 0o040000,
            "the archive contains a link or special file"
        );
        ensure!(seen.insert(path), "the archive contains duplicate paths");
        expanded = expanded
            .checked_add(entry.size())
            .context("the archive size overflowed")?;
        ensure!(expanded <= EXPANDED_LIMIT, "the expanded archive exceeds its limit");
    }
    for index in 0..zip.len() {
        let mut entry = zip.by_index(index)?;
        let path = entry.enclosed_name().context("the archive contains an unsafe path")?;
        if path.starts_with("__MACOSX") {
            continue;
        }
        let destination = directory.join(path);
        if entry.is_dir() {
            fs::create_dir_all(&destination)?;
        } else {
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut output = File::options().write(true).create_new(true).open(&destination)?;
            let size = entry.size();
            let written = std::io::copy(&mut (&mut entry).take(size + 1), &mut output)?;
            ensure!(written == size, "the extracted file has an unexpected size");
            output.flush()?;
            fs::set_permissions(
                &destination,
                fs::Permissions::from_mode(if entry.unix_mode().unwrap_or(0) & 0o111 != 0 {
                    0o755
                } else {
                    0o644
                }),
            )?;
        }
    }
    let bundle = directory.join("Daisy.app");
    ensure!(
        bundle.join("Contents/MacOS/daisy").is_file() && bundle.join("Contents/Info.plist").is_file(),
        "the release archive is missing Daisy.app"
    );
    Ok(bundle)
}

#[cfg(test)]
#[path = "release_tests.rs"]
mod tests;
