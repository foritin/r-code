#[path = "../build_support.rs"]
mod build_support;

use build_support::{validate_external_binary, MIN_EXTERNAL_BIN_BYTES};

fn executable_fixture(prefix: &[u8]) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut bytes = vec![0_u8; MIN_EXTERNAL_BIN_BYTES as usize];
    bytes[..prefix.len()].copy_from_slice(prefix);
    std::fs::write(file.path(), bytes).unwrap();
    file
}

#[test]
fn packaging_accepts_executable_magic_for_each_supported_platform() {
    let windows = executable_fixture(b"MZ\0\0");
    assert!(validate_external_binary(windows.path(), "x86_64-pc-windows-msvc").is_ok());

    let linux = executable_fixture(b"\x7fELF");
    assert!(validate_external_binary(linux.path(), "x86_64-unknown-linux-gnu").is_ok());

    let macos = executable_fixture(&[0xcf, 0xfa, 0xed, 0xfe]);
    assert!(validate_external_binary(macos.path(), "aarch64-apple-darwin").is_ok());
}

#[test]
fn packaging_rejects_placeholder_and_wrong_platform_binary() {
    let placeholder = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(placeholder.path(), b"# placeholder\n").unwrap();
    let error = validate_external_binary(placeholder.path(), "x86_64-pc-windows-msvc").unwrap_err();
    assert!(error.contains("expected a real executable"));

    let wrong_magic = executable_fixture(b"MZ\0\0");
    let error =
        validate_external_binary(wrong_magic.path(), "x86_64-unknown-linux-gnu").unwrap_err();
    assert!(error.contains("requires ELF"));
}
