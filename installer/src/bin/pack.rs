use r_code_installer::{append_payload, hex_sha256};
use std::path::PathBuf;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: r-code-installer-pack <outer.exe> <payload.exe> <output.exe>");
        std::process::exit(2);
    }

    let outer = PathBuf::from(&args[0]);
    let payload = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    match append_payload(&outer, &payload, &output) {
        Ok(metadata) => {
            println!(
                "packed={} payload_bytes={} payload_sha256={}",
                output.display(),
                metadata.length,
                hex_sha256(&metadata.sha256)
            );
        }
        Err(error) => {
            eprintln!("failed to compose branded installer: {error}");
            std::process::exit(1);
        }
    }
}
