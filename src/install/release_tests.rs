use super::*;
use std::cell::RefCell;
use std::collections::BTreeMap;

struct MemorySource {
    responses: BTreeMap<String, Vec<u8>>,
    requested: Vec<String>,
    fail_at: Option<usize>,
}

impl Source for MemorySource {
    fn get(&mut self, url: &str, _limit: usize) -> Result<Vec<u8>> {
        self.requested.push(url.to_owned());
        if self.fail_at == Some(self.requested.len()) {
            bail!("download failed");
        }
        self.responses.get(url).cloned().context("unknown test URL")
    }
}

fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer
            .start_file(*name, zip::write::SimpleFileOptions::default().unix_permissions(0o755))
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn setup() -> (MemorySource, Build) {
    let archive = archive(&[
        ("Daisy.app/Contents/MacOS/daisy", b"signed binary"),
        ("Daisy.app/Contents/Info.plist", b"signed metadata"),
    ]);
    let manifest = toml::to_string(&Manifest {
        format: 2,
        version: "0.6.0".into(),
        protocol: 7,
        supported_protocols: vec![7, 6],
        archive_sha256: digest(&archive),
    })
    .unwrap()
    .into_bytes();
    let arch = if std::env::consts::ARCH == "aarch64" {
        "arm64"
    } else {
        "x86_64"
    };
    let archive_name = format!("Daisy-0.6.0-macos-{arch}.zip");
    let mut responses = BTreeMap::new();
    let mut assets = Vec::new();
    for (name, bytes) in [
        ("Daisy-0.6.0-update.toml".to_owned(), manifest),
        (archive_name, archive),
    ] {
        let url = format!("{REPO}/releases/download/v0.6.0/{name}");
        assets.push(serde_json::json!({"name":name,"size":bytes.len(),"browser_download_url":url}));
        responses.insert(
            format!("{API}/attestations/sha256:{}?per_page=100", digest(&bytes)),
            serde_json::to_vec(&serde_json::json!({"hash":digest(&bytes),"name":name,"tag":"v0.6.0"})).unwrap(),
        );
        responses.insert(url, bytes);
    }
    let release =
        serde_json::to_vec(&serde_json::json!({"tag_name":"v0.6.0","draft":false,"prerelease":false,"assets":assets}))
            .unwrap();
    responses.insert(format!("{API}/releases/latest"), release.clone());
    responses.insert(format!("{API}/releases/tags/v0.6.0"), release);
    (
        MemorySource {
            responses,
            requested: Vec::new(),
            fail_at: None,
        },
        Build {
            version: "0.5.0".into(),
            protocol: 6,
            supported_protocols: vec![6],
        },
    )
}

// Orchestration uses a deterministic verifier seam. The public signed fixture
// tests exercise the production cryptographic verifier and root authorization.
fn trusted(bytes: &[u8], hash: &str, name: &str, tag: &str) -> Result<()> {
    let value: Value = serde_json::from_slice(bytes)?;
    ensure!(
        value["hash"] == hash && value["name"] == name && value["tag"] == tag,
        "unverified asset"
    );
    Ok(())
}

#[test]
fn release_manifest_must_cover_every_active_peer_protocol() {
    let (mut compatible, previous) = setup();
    assert!(
        fetch_verified_for_protocols(&mut compatible, &previous, None, &[6, 7], |bytes, hash, name, tag| {
            trusted(bytes, hash, name, tag)
        },)
        .is_ok()
    );

    let (mut incompatible, previous) = setup();
    assert!(
        fetch_verified_for_protocols(&mut incompatible, &previous, None, &[6, 5], |bytes, hash, name, tag| {
            trusted(bytes, hash, name, tag)
        },)
        .is_err()
    );
}

#[test]
fn both_assets_are_verified_before_a_candidate_can_be_extracted() {
    let (mut source, previous) = setup();
    let verified = RefCell::new(Vec::new());
    let release = fetch_verified(&mut source, &previous, None, |bytes, hash, name, tag| {
        trusted(bytes, hash, name, tag)?;
        verified.borrow_mut().push(name.to_owned());
        Ok(())
    })
    .unwrap();
    assert_eq!(verified.borrow().len(), 2);
    assert_eq!(
        release.build(),
        &Build {
            version: "0.6.0".into(),
            protocol: 7,
            supported_protocols: vec![7, 6],
        }
    );
    let directory = private_directory();
    let bundle = release.extract(directory.path()).unwrap();
    assert_eq!(fs::read(bundle.join("Contents/MacOS/daisy")).unwrap(), b"signed binary");
    assert_eq!(
        fs::metadata(bundle.join("Contents/MacOS/daisy"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn every_failed_download_or_attestation_blocks_installation() {
    for request in 1..=5 {
        let (mut source, previous) = setup();
        source.fail_at = Some(request);
        assert!(fetch_verified(&mut source, &previous, None, trusted).is_err());
    }
    for rejected in ["manifest", "archive"] {
        let (mut source, previous) = setup();
        assert!(
            fetch_verified(&mut source, &previous, None, |bytes, hash, name, tag| {
                ensure!(
                    !(rejected == "manifest" && name.ends_with(".toml")
                        || rejected == "archive" && name.ends_with(".zip")),
                    "untrusted artifact"
                );
                trusted(bytes, hash, name, tag)
            })
            .is_err()
        );
        if rejected == "manifest" {
            assert_eq!(source.requested.len(), 3);
        }
    }
}

fn change_release(source: &mut MemorySource, change: impl FnOnce(&mut Value)) {
    let url = format!("{API}/releases/latest");
    let mut release: Value = serde_json::from_slice(&source.responses[&url]).unwrap();
    change(&mut release);
    source.responses.insert(url, serde_json::to_vec(&release).unwrap());
}

#[test]
fn release_selection_and_asset_origin_fail_closed() {
    for kind in ["draft", "prerelease", "tag", "missing", "duplicate", "origin", "size"] {
        let (mut source, previous) = setup();
        change_release(&mut source, |release| match kind {
            "draft" => release["draft"] = true.into(),
            "prerelease" => release["prerelease"] = true.into(),
            "tag" => release["tag_name"] = "latest".into(),
            "missing" => release["assets"] = serde_json::json!([]),
            "duplicate" => {
                let duplicate = release["assets"][0].clone();
                release["assets"].as_array_mut().unwrap().push(duplicate);
            }
            "origin" => release["assets"][0]["browser_download_url"] = "https://other.invalid/manifest".into(),
            "size" => release["assets"][0]["size"] = (MANIFEST_LIMIT as u64 + 1).into(),
            _ => unreachable!(),
        });
        assert!(fetch_verified(&mut source, &previous, None, trusted).is_err(), "{kind}");
        assert_eq!(source.requested.len(), 1);
    }
    let (mut source, mut previous) = setup();
    previous.version = "0.6.0".into();
    assert!(fetch_verified(&mut source, &previous, None, trusted).is_err());
    let (mut source, previous) = setup();
    assert!(
        fetch_verified(
            &mut source,
            &previous,
            Some(&Version::parse("0.6.0-rc.1").unwrap()),
            trusted
        )
        .is_err()
    );
    assert!(source.requested.is_empty());
}

#[test]
fn verified_metadata_must_match_version_protocol_and_archive() {
    for kind in ["format", "version", "protocol", "hash", "malformed", "incomplete"] {
        let (mut source, previous) = setup();
        let url = format!("{REPO}/releases/download/v0.6.0/Daisy-0.6.0-update.toml");
        let mut manifest: Manifest = toml::from_str(std::str::from_utf8(&source.responses[&url]).unwrap()).unwrap();
        match kind {
            "format" => manifest.format = 3,
            "version" => manifest.version = "0.7.0".into(),
            "protocol" => manifest.protocol = 8,
            "hash" => manifest.archive_sha256 = "0".repeat(64),
            _ => {}
        }
        let bytes = match kind {
            "malformed" => b"not metadata".to_vec(),
            "incomplete" => Vec::new(),
            _ => toml::to_string(&manifest).unwrap().into_bytes(),
        };
        if kind != "incomplete" {
            change_release(&mut source, |release| release["assets"][0]["size"] = bytes.len().into());
            source.responses.insert(
                format!("{API}/attestations/sha256:{}?per_page=100", digest(&bytes)),
                serde_json::to_vec(
                    &serde_json::json!({"hash":digest(&bytes),"name":"Daisy-0.6.0-update.toml","tag":"v0.6.0"}),
                )
                .unwrap(),
            );
        }
        source.responses.insert(url, bytes);
        assert!(fetch_verified(&mut source, &previous, None, trusted).is_err(), "{kind}");
    }
}

#[test]
fn archive_paths_duplicates_links_and_incomplete_bundles_are_rejected() {
    for name in [
        "../outside",
        "/outside",
        "Daisy.app/../outside",
        "Daisy.app\\outside",
        "other/file",
    ] {
        let directory = private_directory();
        assert!(
            extract_archive(&archive(&[(name, b"bad")]), directory.path()).is_err(),
            "{name}"
        );
        assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
    }
    let directory = private_directory();
    let mut duplicate = archive(&[("Daisy.app/a", b"first"), ("Daisy.app/b", b"second")]);
    for start in 0..duplicate.len().saturating_sub(10) {
        if &duplicate[start..start + 11] == b"Daisy.app/b" {
            duplicate[start + 10] = b'a';
        }
    }
    assert!(extract_archive(&duplicate, directory.path()).is_err());
    assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    zip.add_symlink(
        "Daisy.app/link",
        "../../outside",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    assert!(extract_archive(&zip.finish().unwrap().into_inner(), directory.path()).is_err());
    assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
    assert!(
        extract_archive(
            &archive(&[("Daisy.app/Contents/Info.plist", b"metadata")]),
            directory.path()
        )
        .is_err()
    );
}

#[test]
fn extraction_requires_a_new_private_directory() {
    let bytes = archive(&[
        ("Daisy.app/Contents/MacOS/daisy", b"binary"),
        ("Daisy.app/Contents/Info.plist", b"metadata"),
    ]);
    let directory = private_directory();
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(extract_archive(&bytes, directory.path()).is_err());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(directory.path().join("kept"), b"existing").unwrap();
    assert!(extract_archive(&bytes, directory.path()).is_err());
    assert_eq!(fs::read(directory.path().join("kept")).unwrap(), b"existing");
}

fn private_directory() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}

#[test]
fn signed_claims_must_name_the_expected_workflow_and_subject() {
    let response: Value =
        serde_json::from_slice(include_bytes!("../../tests/fixtures/release-attestation.json")).unwrap();
    let payload = base64::engine::general_purpose::STANDARD
        .decode(
            response["attestations"][0]["bundle"]["dsseEnvelope"]["payload"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let statement: Value = serde_json::from_slice(&payload).unwrap();
    let hash = "ae7c85db71e530c324ee269df19bb988300246cee6a1d5098a770c0c7025fb9c";
    let name = "Daisy-0.6.0-macos-arm64.zip";
    let identity = format!("{REPO}/{WORKFLOW}@refs/tags/v0.6.0");
    authorize_statement(&statement, hash, name, "v0.6.0", &identity).unwrap();
    for pointer in [
        "/_type",
        "/predicateType",
        "/predicate/buildDefinition/buildType",
        "/predicate/buildDefinition/externalParameters/workflow/repository",
        "/predicate/buildDefinition/externalParameters/workflow/path",
        "/predicate/buildDefinition/externalParameters/workflow/ref",
        "/predicate/runDetails/builder/id",
        "/subject/1/name",
        "/subject/1/digest/sha256",
    ] {
        let mut changed = statement.clone();
        *changed.pointer_mut(pointer).unwrap() = "wrong".into();
        assert!(
            authorize_statement(&changed, hash, name, "v0.6.0", &identity).is_err(),
            "{pointer}"
        );
    }
}

#[test]
fn requested_version_and_actual_response_limits_are_enforced() {
    let (mut source, previous) = setup();
    let response = source.responses[&format!("{API}/releases/latest")].clone();
    source.responses.insert(format!("{API}/releases/tags/v0.7.0"), response);
    assert!(fetch_verified(&mut source, &previous, Some(&Version::parse("0.7.0").unwrap()), trusted).is_err());
    let (mut source, previous) = setup();
    let response = source.responses.get_mut(&format!("{API}/releases/latest")).unwrap();
    response.resize(METADATA_LIMIT + 1, b' ');
    assert!(fetch_verified(&mut source, &previous, None, trusted).is_err());
    assert_eq!(source.requested.len(), 1);
}

#[test]
fn declared_expanded_size_is_checked_before_extraction() {
    let mut bytes = archive(&[
        ("Daisy.app/Contents/MacOS/daisy", b"binary"),
        ("Daisy.app/Contents/Info.plist", b"metadata"),
    ]);
    let offsets: Vec<_> = bytes
        .windows(4)
        .enumerate()
        .filter_map(|(index, value)| {
            if value == b"PK\x03\x04" {
                Some(index + 22)
            } else if value == b"PK\x01\x02" {
                Some(index + 24)
            } else {
                None
            }
        })
        .collect();
    for offset in offsets {
        bytes[offset..offset + 4].copy_from_slice(&((EXPANDED_LIMIT + 1) as u32).to_le_bytes());
    }
    let directory = private_directory();
    assert!(extract_archive(&bytes, directory.path()).is_err());
    assert!(fs::read_dir(directory.path()).unwrap().next().is_none());
}

#[test]
fn latest_current_release_is_a_no_op_but_explicit_downgrades_are_refused() {
    let (mut source, mut previous) = setup();
    previous.version = "0.6.0".into();
    let error = fetch_verified(&mut source, &previous, None, trusted).err().unwrap();
    assert!(error.is::<AlreadyCurrent>());
    assert_eq!(source.requested.len(), 1);
    let (mut source, mut previous) = setup();
    previous.version = "0.7.0".into();
    let error = fetch_verified(&mut source, &previous, Some(&Version::parse("0.6.0").unwrap()), trusted)
        .err()
        .unwrap();
    assert!(!error.is::<AlreadyCurrent>());
    assert_eq!(source.requested.len(), 1);
}
