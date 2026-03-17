mod common;

use caisson::app::{
    ImageImportApp, ImportValidatedImageRequest, ValidatePackageRequest, ValidationApp,
};
use caisson::config::load_service_catalog;
use caisson::docker::{DockerClientError, DockerImageClient};
use caisson::domain::{ImageImportStatus, ImportedImageMetadata, ValidationStatus};
use caisson::persistence::FilesystemStore;
use semver::Version;

use common::{read_fixture, write_valid_edgepkg};

#[derive(Debug, Clone)]
struct FakeDockerClient {
    load_error: Option<String>,
    inspect_error: Option<String>,
    imported_image: Option<ImportedImageMetadata>,
}

impl DockerImageClient for FakeDockerClient {
    fn load_image_archive(&self, _archive_path: &std::path::Path) -> Result<(), DockerClientError> {
        match self.load_error.as_ref() {
            Some(message) => Err(DockerClientError::ImportFailed(message.clone())),
            None => Ok(()),
        }
    }

    fn inspect_image(
        &self,
        _image_reference: &str,
    ) -> Result<ImportedImageMetadata, DockerClientError> {
        match self.inspect_error.as_ref() {
            Some(message) => Err(DockerClientError::ImportFailed(message.clone())),
            None => Ok(self
                .imported_image
                .clone()
                .expect("fake imported image should exist")),
        }
    }
}

#[test]
fn imports_a_validated_package_image_and_persists_candidate_release() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let validation_app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let validation_record = validation_app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");
    assert_eq!(validation_record.status, ValidationStatus::Accepted);

    let store = FilesystemStore::new(&state_dir);
    let import_app = ImageImportApp::filesystem(
        FakeDockerClient {
            load_error: None,
            inspect_error: None,
            imported_image: Some(ImportedImageMetadata {
                image_id: "sha256:frontend".into(),
                repo_tags: vec!["example/frontend:1.2.3".into()],
                repo_digests: vec!["example/frontend@sha256:frontend".into()],
                architecture: Some("amd64".into()),
                os: Some("linux".into()),
            }),
        },
        store.clone(),
    );

    let import_record = import_app
        .import_validated_image(ImportValidatedImageRequest { validation_record })
        .expect("image import should complete");

    assert_eq!(import_record.status, ImageImportStatus::Imported);
    assert!(
        import_record.issues.is_empty(),
        "unexpected issues: {:?}",
        import_record.issues
    );

    let import_record_path = store.image_import_record_path(import_record.import_id);
    assert!(
        import_record_path.exists(),
        "image import record should be persisted"
    );

    let candidate_release_id = import_record
        .candidate_release_id
        .expect("candidate release id should be set");
    let candidate_release_path = store.candidate_release_path(candidate_release_id);
    assert!(
        candidate_release_path.exists(),
        "candidate release record should be persisted"
    );

    let audit_log =
        std::fs::read_to_string(state_dir.join("audit").join("events.jsonl")).expect("audit log");
    assert!(audit_log.contains("image_import_started"));
    assert!(audit_log.contains("image_import_succeeded"));
}

#[test]
fn records_failed_image_imports_when_docker_load_fails() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let validation_app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let validation_record = validation_app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");
    assert_eq!(validation_record.status, ValidationStatus::Accepted);

    let store = FilesystemStore::new(&state_dir);
    let import_app = ImageImportApp::filesystem(
        FakeDockerClient {
            load_error: Some("tarball rejected".into()),
            inspect_error: None,
            imported_image: Some(ImportedImageMetadata {
                image_id: "unused".into(),
                repo_tags: Vec::new(),
                repo_digests: Vec::new(),
                architecture: None,
                os: None,
            }),
        },
        store.clone(),
    );

    let import_record = import_app
        .import_validated_image(ImportValidatedImageRequest { validation_record })
        .expect("image import should complete");

    assert_eq!(import_record.status, ImageImportStatus::Failed);
    assert!(
        import_record
            .issues
            .iter()
            .any(|issue| issue.code == "docker.load_failed")
    );
    assert!(import_record.candidate_release_id.is_none());
    assert!(
        store
            .image_import_record_path(import_record.import_id)
            .exists(),
        "failed image import should still be persisted"
    );

    let audit_log =
        std::fs::read_to_string(state_dir.join("audit").join("events.jsonl")).expect("audit log");
    assert!(audit_log.contains("image_import_failed"));
}

#[test]
fn records_failed_image_imports_when_docker_inspect_fails() {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let state_dir = temp_dir.path().join("state");
    let package_path = temp_dir.path().join("frontend.edgepkg");
    let catalog = load_service_catalog(common::fixtures_dir().join("services.valid.toml"))
        .expect("catalog should load");
    let validation_app = ValidationApp::filesystem(
        catalog,
        FilesystemStore::new(&state_dir),
        Version::parse("0.1.0-alpha").expect("version"),
    );

    write_valid_edgepkg(
        &package_path,
        &read_fixture("manifests/valid-frontend.toml"),
    );

    let validation_record = validation_app
        .validate_package(ValidatePackageRequest {
            package_path: package_path,
        })
        .expect("validation should complete");
    assert_eq!(validation_record.status, ValidationStatus::Accepted);

    let store = FilesystemStore::new(&state_dir);
    let import_app = ImageImportApp::filesystem(
        FakeDockerClient {
            load_error: None,
            inspect_error: Some("image not visible after load".into()),
            imported_image: None,
        },
        store.clone(),
    );

    let import_record = import_app
        .import_validated_image(ImportValidatedImageRequest { validation_record })
        .expect("image import should complete");

    assert_eq!(import_record.status, ImageImportStatus::Failed);
    assert!(
        import_record
            .issues
            .iter()
            .any(|issue| issue.code == "docker.inspect_failed")
    );
    assert!(import_record.candidate_release_id.is_none());
    assert!(
        store
            .image_import_record_path(import_record.import_id)
            .exists(),
        "failed image import should still be persisted"
    );
}
