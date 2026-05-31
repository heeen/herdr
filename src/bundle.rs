//! Multi-platform "fat" herdr bundle (issue #28).
//!
//! A normal herdr build produces a binary for one os/arch. An opt-in `just bundle`
//! step cross-compiles the other supported platforms and appends them to the native
//! binary as a compressed, indexed payload. Trailing bytes after a normal executable
//! image are ignored by the OS, so the file still runs natively while carrying the
//! sibling binaries as data — letting `herdr --remote <host>` seed a different-OS host
//! offline at exact version parity.
//!
//! On-disk layout of a fat binary:
//! ```text
//! [ executable image (native binary, unchanged) ]
//! [ deflate-compressed binary for platform 0 ]
//! [ deflate-compressed binary for platform 1 ]
//! ...
//! [ index JSON (UTF-8, uncompressed) ]
//! [ footer: 28 bytes, magic last ]
//! ```
//!
//! Footer (little-endian, magic at the very end so it can be found from EOF):
//! ```text
//! index_offset  u64     absolute offset of the index JSON
//! index_len     u64     length of the index JSON in bytes
//! format        u32     bundle format version (currently 1)
//! magic         [u8;8]  b"HERDRBND"
//! ```

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Trailing 8-byte marker identifying a herdr fat bundle.
const MAGIC: &[u8; 8] = b"HERDRBND";
/// Current bundle format version. Bumped on incompatible layout changes; readers
/// that see a newer version fall back cleanly (treat the file as un-bundled).
const FORMAT_VERSION: u32 = 1;
/// Fixed footer size: index_offset(8) + index_len(8) + format(4) + magic(8).
const FOOTER_LEN: u64 = 28;
/// deflate compression level used for appended binaries (matches the wire path).
const COMPRESSION_LEVEL: u8 = 6;

/// A single platform's binary carried inside a fat bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BundleEntry {
    pub os: String,
    pub arch: String,
    /// Absolute byte offset of this entry's deflate-compressed bytes.
    pub offset: u64,
    pub compressed_len: u64,
    pub uncompressed_len: u64,
    /// IEEE CRC-32 of the uncompressed bytes (integrity check on extract).
    pub crc32: u32,
}

impl BundleEntry {
    pub(crate) fn asset_key(&self) -> String {
        format!("{}-{}", self.os, self.arch)
    }
}

/// The index describing every platform carried by a fat bundle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BundleIndex {
    pub format: u32,
    /// herdr package version every carried binary was built at (e.g. "0.6.4").
    pub herdr_version: String,
    /// Build commit the bundle was packed at, when known (for exact parity).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,
    /// Length of the native executable image (where the appended payload starts).
    pub image_len: u64,
    pub entries: Vec<BundleEntry>,
}

impl BundleIndex {
    /// Find the carried binary for `os`/`arch`, if present.
    pub(crate) fn entry_for(&self, os: &str, arch: &str) -> Option<&BundleEntry> {
        self.entries
            .iter()
            .find(|entry| entry.os == os && entry.arch == arch)
    }
}

/// One platform's binary to pack into a fat bundle.
#[derive(Debug, Clone)]
pub(crate) struct PackInput {
    pub os: String,
    pub arch: String,
    pub binary: PathBuf,
}

/// The os/arch of the currently-running binary, in bundle asset-key terms
/// (e.g. `("macos", "aarch64")`). `None` on a platform herdr does not bundle.
pub(crate) fn local_os_arch() -> Option<(&'static str, &'static str)> {
    let os = if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else {
        return None;
    };
    let arch = if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        return None;
    };
    Some((os, arch))
}

/// IEEE CRC-32 (reflected, polynomial 0xEDB88320) of `data`.
pub(crate) fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Read the bundle index from `path`, or `Ok(None)` if the file carries no
/// (recognizable, current-format) bundle. Returns `Err` only when the file is
/// marked as a herdr bundle but its index cannot be read or parsed.
pub(crate) fn read_index(path: &Path) -> io::Result<Option<BundleIndex>> {
    let mut file = File::open(path)?;
    let file_len = file.metadata()?.len();
    if file_len < FOOTER_LEN {
        return Ok(None);
    }

    let mut footer = [0u8; FOOTER_LEN as usize];
    file.seek(SeekFrom::Start(file_len - FOOTER_LEN))?;
    file.read_exact(&mut footer)?;
    if &footer[20..28] != MAGIC {
        return Ok(None);
    }
    let format = u32::from_le_bytes(footer[16..20].try_into().unwrap());
    if format != FORMAT_VERSION {
        // Forward-compatible: a newer bundle format reads as "no usable bundle"
        // so callers fall back cleanly instead of misparsing.
        return Ok(None);
    }
    let index_offset = u64::from_le_bytes(footer[0..8].try_into().unwrap());
    let index_len = u64::from_le_bytes(footer[8..16].try_into().unwrap());
    if index_offset > file_len - FOOTER_LEN || index_len > file_len - FOOTER_LEN - index_offset {
        return Ok(None);
    }

    let mut index_bytes = vec![0u8; index_len as usize];
    file.seek(SeekFrom::Start(index_offset))?;
    file.read_exact(&mut index_bytes)?;
    let index: BundleIndex = serde_json::from_slice(&index_bytes)
        .map_err(|err| io::Error::other(format!("herdr bundle index is corrupt: {err}")))?;
    Ok(Some(index))
}

/// Read the bundle index carried by the currently-running executable.
pub(crate) fn read_self_index() -> io::Result<Option<BundleIndex>> {
    read_index(&std::env::current_exe()?)
}

/// Extract and decompress one carried binary, verifying its length and CRC-32.
pub(crate) fn extract_entry(path: &Path, entry: &BundleEntry) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(entry.offset))?;
    let mut compressed = vec![0u8; entry.compressed_len as usize];
    file.read_exact(&mut compressed)?;

    let bytes = miniz_oxide::inflate::decompress_to_vec(&compressed).map_err(|err| {
        io::Error::other(format!(
            "failed to decompress {} payload: {err:?}",
            entry.asset_key()
        ))
    })?;
    if bytes.len() as u64 != entry.uncompressed_len {
        return Err(io::Error::other(format!(
            "{} payload length mismatch: expected {}, got {}",
            entry.asset_key(),
            entry.uncompressed_len,
            bytes.len()
        )));
    }
    if crc32(&bytes) != entry.crc32 {
        return Err(io::Error::other(format!(
            "{} payload failed CRC-32 check",
            entry.asset_key()
        )));
    }
    Ok(bytes)
}

/// Bytes of the native executable image at the front of a (possibly already-fat)
/// carrier: if it already carries a bundle, the existing payload is dropped.
fn carrier_image_bytes(carrier_path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = fs::read(carrier_path)?;
    if let Some(index) = read_index(carrier_path)? {
        bytes.truncate(index.image_len as usize);
    }
    Ok(bytes)
}

/// Build a fat binary at `out_path` from `carrier_path` plus `inputs`. If `carrier_path`
/// already carries a bundle, its existing payload is stripped first (idempotent re-pack).
pub(crate) fn pack(
    carrier_path: &Path,
    inputs: &[PackInput],
    herdr_version: &str,
    build_commit: Option<&str>,
    out_path: &Path,
) -> io::Result<BundleIndex> {
    let mut output = carrier_image_bytes(carrier_path)?;
    let image_len = output.len() as u64;

    // Stable order so re-packing the same inputs yields an identical layout.
    let mut inputs = inputs.to_vec();
    inputs.sort_by(|a, b| (&a.os, &a.arch).cmp(&(&b.os, &b.arch)));

    let mut entries = Vec::with_capacity(inputs.len());
    for input in &inputs {
        let raw = fs::read(&input.binary)?;
        let compressed = miniz_oxide::deflate::compress_to_vec(&raw, COMPRESSION_LEVEL);
        let offset = output.len() as u64;
        entries.push(BundleEntry {
            os: input.os.clone(),
            arch: input.arch.clone(),
            offset,
            compressed_len: compressed.len() as u64,
            uncompressed_len: raw.len() as u64,
            crc32: crc32(&raw),
        });
        output.extend_from_slice(&compressed);
    }

    let index = BundleIndex {
        format: FORMAT_VERSION,
        herdr_version: herdr_version.to_string(),
        build_commit: build_commit.map(str::to_string),
        image_len,
        entries,
    };

    let index_json = serde_json::to_vec(&index).map_err(io::Error::other)?;
    let index_offset = output.len() as u64;
    output.extend_from_slice(&index_json);

    output.extend_from_slice(&index_offset.to_le_bytes());
    output.extend_from_slice(&(index_json.len() as u64).to_le_bytes());
    output.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    output.extend_from_slice(MAGIC);

    write_executable(out_path, &output)?;
    Ok(index)
}

/// Write `bytes` to `path` and mark it executable on unix.
fn write_executable(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = file.metadata()?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// Unique temp directory per test invocation; cleaned up by the OS / left for
    /// inspection on failure. Created fresh so parallel tests never collide.
    fn temp_dir() -> PathBuf {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("herdr-bundle-test-{}-{unique}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    fn sample_inputs(dir: &Path) -> (Vec<PackInput>, Vec<u8>, Vec<u8>) {
        let linux_bytes = b"this-is-the-linux-x86_64-herdr-binary-contents".repeat(50);
        let mac_bytes = b"different-bytes-for-the-macos-aarch64-build!!".repeat(40);
        let linux = write_file(dir, "herdr-linux", &linux_bytes);
        let mac = write_file(dir, "herdr-mac", &mac_bytes);
        let inputs = vec![
            PackInput {
                os: "linux".into(),
                arch: "x86_64".into(),
                binary: linux,
            },
            PackInput {
                os: "macos".into(),
                arch: "aarch64".into(),
                binary: mac,
            },
        ];
        (inputs, linux_bytes, mac_bytes)
    }

    #[test]
    fn crc32_matches_known_vector() {
        // Canonical IEEE CRC-32 check value for "123456789".
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn pack_then_read_index_round_trips() {
        let dir = temp_dir();
        let carrier_bytes = b"NATIVE-CARRIER-EXECUTABLE-IMAGE".repeat(20);
        let carrier = write_file(&dir, "herdr", &carrier_bytes);
        let (inputs, linux_bytes, mac_bytes) = sample_inputs(&dir);
        let out = dir.join("herdr-bundle");

        let returned = pack(&carrier, &inputs, "9.9.9", Some("deadbee"), &out).unwrap();

        let index = read_index(&out).unwrap().expect("bundle present");
        assert_eq!(index, returned);
        assert_eq!(index.format, FORMAT_VERSION);
        assert_eq!(index.herdr_version, "9.9.9");
        assert_eq!(index.build_commit.as_deref(), Some("deadbee"));
        assert_eq!(index.image_len, carrier_bytes.len() as u64);

        let linux = index.entry_for("linux", "x86_64").expect("linux entry");
        assert_eq!(linux.uncompressed_len, linux_bytes.len() as u64);
        assert_eq!(linux.crc32, crc32(&linux_bytes));
        let mac = index.entry_for("macos", "aarch64").expect("mac entry");
        assert_eq!(mac.uncompressed_len, mac_bytes.len() as u64);
        assert_eq!(mac.crc32, crc32(&mac_bytes));
    }

    #[test]
    fn extract_entry_returns_original_bytes() {
        let dir = temp_dir();
        let carrier = write_file(&dir, "herdr", b"CARRIER");
        let (inputs, linux_bytes, mac_bytes) = sample_inputs(&dir);
        let out = dir.join("herdr-bundle");
        let index = pack(&carrier, &inputs, "1.0.0", None, &out).unwrap();

        let linux = extract_entry(&out, index.entry_for("linux", "x86_64").unwrap()).unwrap();
        assert_eq!(linux, linux_bytes);
        let mac = extract_entry(&out, index.entry_for("macos", "aarch64").unwrap()).unwrap();
        assert_eq!(mac, mac_bytes);
    }

    #[test]
    fn carrier_image_prefix_is_unchanged() {
        // The "still runs natively" property: the executable image at the front of
        // the fat file is byte-for-byte the original carrier.
        let dir = temp_dir();
        let carrier_bytes = b"NATIVE-CARRIER-EXECUTABLE-IMAGE".repeat(20);
        let carrier = write_file(&dir, "herdr", &carrier_bytes);
        let (inputs, _, _) = sample_inputs(&dir);
        let out = dir.join("herdr-bundle");
        pack(&carrier, &inputs, "1.0.0", None, &out).unwrap();

        let fat = fs::read(&out).unwrap();
        assert_eq!(&fat[..carrier_bytes.len()], &carrier_bytes[..]);
        assert!(fat.len() > carrier_bytes.len());
    }

    #[test]
    fn read_index_returns_none_for_plain_file() {
        let dir = temp_dir();
        let plain = write_file(
            &dir,
            "plain",
            &b"just an ordinary binary with no bundle".repeat(10),
        );
        assert!(read_index(&plain).unwrap().is_none());

        let tiny = write_file(&dir, "tiny", b"abc");
        assert!(read_index(&tiny).unwrap().is_none());
    }

    #[test]
    fn repack_is_idempotent() {
        let dir = temp_dir();
        let carrier_bytes = b"NATIVE-CARRIER".repeat(30);
        let carrier = write_file(&dir, "herdr", &carrier_bytes);
        let (inputs, _, _) = sample_inputs(&dir);
        let fat1 = dir.join("fat1");
        let index1 = pack(&carrier, &inputs, "2.0.0", Some("abc"), &fat1).unwrap();

        // Re-pack the already-fat binary: the existing payload must be stripped, so
        // image_len and the leading image stay equal to the original carrier.
        let fat2 = dir.join("fat2");
        let index2 = pack(&fat1, &inputs, "2.0.0", Some("abc"), &fat2).unwrap();

        assert_eq!(index2.image_len, carrier_bytes.len() as u64);
        assert_eq!(index1.entries, index2.entries);
        let fat2_bytes = fs::read(&fat2).unwrap();
        assert_eq!(&fat2_bytes[..carrier_bytes.len()], &carrier_bytes[..]);
    }

    #[test]
    fn corrupted_payload_fails_extract() {
        let dir = temp_dir();
        let carrier = write_file(&dir, "herdr", b"CARRIER");
        let (inputs, _, _) = sample_inputs(&dir);
        let out = dir.join("herdr-bundle");
        let index = pack(&carrier, &inputs, "1.0.0", None, &out).unwrap();
        let entry = index.entry_for("linux", "x86_64").unwrap().clone();

        // Flip a byte in the middle of the compressed payload.
        let mut bytes = fs::read(&out).unwrap();
        let mid = (entry.offset + entry.compressed_len / 2) as usize;
        bytes[mid] ^= 0xFF;
        fs::write(&out, &bytes).unwrap();

        assert!(extract_entry(&out, &entry).is_err());
    }

    #[test]
    fn entry_for_finds_present_and_misses_absent() {
        let dir = temp_dir();
        let carrier = write_file(&dir, "herdr", b"CARRIER");
        let (inputs, _, _) = sample_inputs(&dir);
        let out = dir.join("herdr-bundle");
        let index = pack(&carrier, &inputs, "1.0.0", None, &out).unwrap();

        assert!(index.entry_for("linux", "x86_64").is_some());
        assert!(index.entry_for("linux", "aarch64").is_none());
        assert!(index.entry_for("windows", "x86_64").is_none());
    }

    #[test]
    fn unknown_format_version_reads_as_none() {
        let dir = temp_dir();
        let carrier = write_file(&dir, "herdr", b"CARRIER-IMAGE-BYTES");
        let (inputs, _, _) = sample_inputs(&dir);
        let out = dir.join("herdr-bundle");
        pack(&carrier, &inputs, "1.0.0", None, &out).unwrap();

        // Rewrite the footer's format field (offset 16 from end of footer) to a
        // future version. A current reader must fall back cleanly to None.
        let mut bytes = fs::read(&out).unwrap();
        let len = bytes.len();
        let format_at = len - FOOTER_LEN as usize + 16;
        bytes[format_at..format_at + 4].copy_from_slice(&999u32.to_le_bytes());
        fs::write(&out, &bytes).unwrap();

        assert!(read_index(&out).unwrap().is_none());
    }
}
