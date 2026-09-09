use std::fs::File;
use std::io::Read;
use std::path::Path;

pub const MIN_EXTERNAL_BIN_BYTES: u64 = 64 * 1024;

pub fn validate_external_binary(path: &Path, target: &str) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| format!("external binary {} is missing: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "external binary {} is not a regular file",
            path.display()
        ));
    }
    if metadata.len() < MIN_EXTERNAL_BIN_BYTES {
        return Err(format!(
            "external binary {} is only {} bytes; expected a real executable of at least {} bytes",
            path.display(),
            metadata.len(),
            MIN_EXTERNAL_BIN_BYTES
        ));
    }

    let mut prefix = [0_u8; 4];
    File::open(path)
        .and_then(|mut file| file.read_exact(&mut prefix))
        .map_err(|error| format!("read external binary {}: {error}", path.display()))?;

    let (expected, valid) = if target.contains("-windows-") {
        ("PE/COFF (MZ)", prefix.starts_with(b"MZ"))
    } else if target.contains("-linux-") {
        ("ELF", prefix == [0x7f, b'E', b'L', b'F'])
    } else if target.contains("-apple-darwin") {
        (
            "Mach-O or universal Mach-O",
            matches!(
                prefix,
                [0xfe, 0xed, 0xfa, 0xce]
                    | [0xce, 0xfa, 0xed, 0xfe]
                    | [0xfe, 0xed, 0xfa, 0xcf]
                    | [0xcf, 0xfa, 0xed, 0xfe]
                    | [0xca, 0xfe, 0xba, 0xbe]
                    | [0xbe, 0xba, 0xfe, 0xca]
                    | [0xca, 0xfe, 0xba, 0xbf]
                    | [0xbf, 0xba, 0xfe, 0xca]
            ),
        )
    } else {
        return Err(format!(
            "cannot validate external binary for unsupported target {target}"
        ));
    };

    if !valid {
        return Err(format!(
            "external binary {} has magic {:02x?}; target {target} requires {expected}",
            path.display(),
            prefix
        ));
    }
    Ok(())
}
