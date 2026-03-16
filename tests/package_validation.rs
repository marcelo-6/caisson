mod common;

use caisson::app::{ValidatePackageRequest, ValidationApp};
use caisson::config::load_service_catalog;
use caisson::domain::ValidationStatus;
use caisson::persistence::FilesystemStore;
use semver::Version;

use common::{
    read_fixture, write_edgepkg_missing_image, write_edgepkg_with_duplicate_manifest,
    write_edgepkg_with_nested_entry, write_edgepkg_with_symlink, write_valid_edgepkg,
};

#[test]
fn accepts_a_valid_package_and_persists_the_record() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let store = FilesystemStore::new(&state_dir);
    let app = ValidationApp::filesystem(
        catalog,
        store.clone(),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path.clone(),
        })
        .expect("validation should succeed");

    assert_eq!(record.status, ValidationStatus::Accepted);
    assert!(
        record.issues.is_empty(),
        "expected no issues: {:?}",
        record.issues
    );
    assert_eq!(
        record.manifest.as_ref().expect("manifest").target.service,
        "frontend"
    );

    let staged_path = record.staged_path.as_ref().expect("staged path");
    assert!(staged_path.exists(), "staged package should exist");

    let persisted_record_path = store.validation_record_path(record.attempt_id);
    assert!(persisted_record_path.exists(), "record should be persisted");

    let audit_log_path = state_dir.join("audit").join("events.jsonl");
    let audit_log = std::fs::read_to_string(audit_log_path).expect("audit log");
    assert!(audit_log.contains("validation_started"));
    assert!(audit_log.contains("validation_accepted"));
}

#[test]
fn rejects_packages_with_missing_image_tar() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_edgepkg_missing_image(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "archive.missing_image")
    );
}

#[test]
fn rejects_packages_with_symlink_entries() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_edgepkg_with_symlink(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "archive.unsafe_entry_type")
    );
}

#[test]
fn rejects_packages_for_unknown_services() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/unknown-service.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "package.unknown_service")
    );
}

#[test]
fn rejects_service_revision_mismatches() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/mismatched-service-revision.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "package.service_revision_mismatch")
    );
}

#[test]
fn rejects_platform_mismatches() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/mismatched-platform.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "package.platform_mismatch")
    );
}

#[test]
fn rejects_duplicate_archive_entries() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_edgepkg_with_duplicate_manifest(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "archive.duplicate_entry")
    );
}

#[test]
fn rejects_nested_archive_entries() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_edgepkg_with_nested_entry(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "archive.nested_entry")
    );
}

#[test]
fn rejects_invalid_package_types() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/invalid-package-type.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "manifest.parse_failed")
    );
}

#[test]
fn rejects_packages_that_require_a_newer_updater() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/min-updater-too-new.toml"),
    );

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "package.updater_too_old")
    );
}

#[test]
fn rejects_malformed_manifest_toml() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(&package_path, &read_fixture("manifests/malformed.toml"));

    let record = app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");

    assert_eq!(record.status, ValidationStatus::Rejected);
    assert!(
        record
            .issues
            .iter()
            .any(|issue| issue.code == "manifest.parse_failed")
    );
}
