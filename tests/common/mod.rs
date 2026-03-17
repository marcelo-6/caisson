#![allow(dead_code)]

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use tar::{Builder, EntryType, Header};

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn read_fixture(path: &str) -> String {
    fs::read_to_string(fixtures_dir().join(path)).expect("fixture should exist")
}

pub fn write_valid_edgepkg(path: &Path, manifest: &str) {
    let file = fs::File::create(path).expect("create edgepkg");
    let mut builder = Builder::new(file);

    append_regular_file(&mut builder, "manifest.toml", manifest.as_bytes());
    append_regular_file(&mut builder, "image.tar", b"placeholder image tar bytes");
    builder.finish().expect("finish tar");
}

pub fn write_edgepkg_missing_image(path: &Path, manifest: &str) {
    let file = fs::File::create(path).expect("create edgepkg");
    let mut builder = Builder::new(file);

    append_regular_file(&mut builder, "manifest.toml", manifest.as_bytes());
    builder.finish().expect("finish tar");
}

pub fn write_edgepkg_with_duplicate_manifest(path: &Path, manifest: &str) {
    let file = fs::File::create(path).expect("create edgepkg");
    let mut builder = Builder::new(file);

    append_regular_file(&mut builder, "manifest.toml", manifest.as_bytes());
    append_regular_file(&mut builder, "manifest.toml", manifest.as_bytes());
    append_regular_file(&mut builder, "image.tar", b"placeholder image tar bytes");
    builder.finish().expect("finish tar");
}

pub fn write_edgepkg_with_symlink(path: &Path, manifest: &str) {
    let file = fs::File::create(path).expect("create edgepkg");
    let mut builder = Builder::new(file);

    append_regular_file(&mut builder, "manifest.toml", manifest.as_bytes());
    append_regular_file(&mut builder, "image.tar", b"placeholder image tar bytes");

    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Symlink);
    header.set_size(0);
    header.set_mode(0o777);
    header.set_cksum();

    builder
        .append_link(&mut header, "notes-link", "README.txt")
        .expect("append symlink");
    builder.finish().expect("finish tar");
}

pub fn write_edgepkg_with_nested_entry(path: &Path, manifest: &str) {
    let file = fs::File::create(path).expect("create edgepkg");
    let mut builder = Builder::new(file);

    append_regular_file(&mut builder, "nested/manifest.toml", manifest.as_bytes());
    append_regular_file(&mut builder, "image.tar", b"placeholder image tar bytes");
    builder.finish().expect("finish tar");
}

fn append_regular_file(builder: &mut Builder<std::fs::File>, name: &str, bytes: &[u8]) {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(0o644);
    header.set_size(bytes.len() as u64);
    header.set_cksum();

    builder
        .append_data(&mut header, name, Cursor::new(bytes))
        .expect("append file");
}
