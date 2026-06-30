//! Minimal tar helpers for moving single files in and out of a container via
//! Docker's archive endpoints.

use std::io::Read;

use anyhow::{anyhow, Result};

/// Build a single entry tar containing `content` at `name` (relative path).
/// Docker's upload endpoint extracts this under a target directory.
pub fn tar_single_file(name: &str, content: &[u8], mode: u32) -> Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(mode);
    header.set_cksum();
    builder.append_data(&mut header, name, content)?;
    Ok(builder.into_inner()?)
}

/// Extract the first regular file from a tar archive (what Docker's download
/// endpoint returns when fetching a single path).
pub fn untar_first_file(archive: &[u8]) -> Result<Vec<u8>> {
    let mut ar = tar::Archive::new(archive);
    for entry in ar.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_file() {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    Err(anyhow!("tar archive contained no file entry"))
}

/// Split an absolute container path into (dir, basename) for the upload endpoint.
pub fn split_container_path(abs_path: &str) -> (String, String) {
    match abs_path.rfind('/') {
        Some(0) => ("/".to_string(), abs_path[1..].to_string()),
        Some(idx) => (abs_path[..idx].to_string(), abs_path[idx + 1..].to_string()),
        None => (".".to_string(), abs_path.to_string()),
    }
}
