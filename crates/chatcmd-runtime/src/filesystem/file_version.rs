use super::*;
mod token;
use crate::{FsPermissions, FsStatBudget, FsStatRequest, FsStatResult, VersionStrength};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufReader, Read, Seek, SeekFrom},
    time::Instant,
};
use token::{decode_token, encode_token};
use tokio_util::sync::CancellationToken;

const TOKEN_VERSION: u8 = 1;
const HASH_CHUNK_BYTES: usize = 64 * 1024;
const SAMPLE_BYTES: usize = 64 * 1024;

/// Decoded, authenticated file version. Identity and path values are one-way fingerprints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct FileVersion {
    schema: u8,
    path_fingerprint: String,
    identity_fingerprint: String,
    entry_type: String,
    size_bytes: u64,
    modified_at_ns: Option<u64>,
    changed_at_ns: Option<u64>,
    strength: VersionStrength,
    content_hash: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Snapshot {
    identity_fingerprint: String,
    entry_type: String,
    size_bytes: u64,
    modified_at_ns: Option<u64>,
    changed_at_ns: Option<u64>,
}

impl WorkspaceService {
    /// Captures metadata and, when requested, a bounded streaming hash.
    pub async fn stat_v2(
        &self,
        context: Option<&OperationContext>,
        request: &FsStatRequest,
    ) -> RuntimeResult<FsStatResult> {
        validate_hash_algorithm(request)?;
        let resolved = self.existing(&request.path)?;
        resolved.revalidate()?;
        let path = resolved.canonical_path.clone();
        let root = resolved.root.clone();
        let key = self.version_key.clone();
        let request = request.clone();
        let cancellation =
            context.map_or_else(CancellationToken::new, |value| value.cancellation.clone());
        tokio::task::spawn_blocking(move || capture(&path, &root, &key, &request, &cancellation))
            .await
            .map_err(join_error)?
    }

    /// Authenticates and re-captures a token for mutation precondition checks.
    pub async fn verify_expected_version(
        &self,
        path: &Path,
        token: &str,
        context: Option<&OperationContext>,
    ) -> RuntimeResult<FileVersion> {
        self.verify_expected_version_with_budget(path, token, context, FsStatBudget::default())
            .await
    }

    pub(super) async fn verify_expected_version_with_budget(
        &self,
        path: &Path,
        token: &str,
        context: Option<&OperationContext>,
        budget: FsStatBudget,
    ) -> RuntimeResult<FileVersion> {
        let expected = decode_token(&self.version_key, token)?;
        let resolved = self.existing(path).map_err(|error| {
            if error.code == "not_found" {
                RuntimeError::new("targetMissing", "versioned target no longer exists")
            } else {
                error
            }
        })?;
        let request = FsStatRequest {
            path: resolved.canonical_path.clone(),
            version_strength: expected.strength,
            hash_algorithm: expected.content_hash.as_ref().map(|_| "sha256".to_owned()),
            budget,
        };
        let current_result = self.stat_v2(context, &request).await?;
        let current = decode_token(&self.version_key, &current_result.version_token)?;
        if current.path_fingerprint != expected.path_fingerprint {
            return Err(RuntimeError::new(
                "versionMismatch",
                "version token belongs to a different path or scope",
            ));
        }
        if current.identity_fingerprint != expected.identity_fingerprint {
            return Err(RuntimeError::new(
                "targetReplaced",
                "path now refers to a different filesystem entry",
            ));
        }
        if current != expected {
            return Err(RuntimeError::new(
                "versionMismatch",
                "filesystem entry changed since the version token was captured",
            ));
        }
        Ok(current)
    }

    pub(super) fn verify_expected_version_blocking(
        &self,
        path: &Path,
        token: &str,
        cancellation: &CancellationToken,
        budget: FsStatBudget,
    ) -> RuntimeResult<FileVersion> {
        let expected = decode_token(&self.version_key, token)?;
        let resolved = self.existing(path).map_err(|error| {
            if error.code == "not_found" {
                RuntimeError::new("targetMissing", "versioned target no longer exists")
            } else {
                error
            }
        })?;
        let request = FsStatRequest {
            path: resolved.canonical_path.clone(),
            version_strength: expected.strength,
            hash_algorithm: expected.content_hash.as_ref().map(|_| "sha256".to_owned()),
            budget,
        };
        let current_result = capture(
            &resolved.canonical_path,
            &resolved.root,
            &self.version_key,
            &request,
            cancellation,
        )?;
        let current = decode_token(&self.version_key, &current_result.version_token)?;
        if current.path_fingerprint != expected.path_fingerprint {
            return Err(RuntimeError::new(
                "versionMismatch",
                "version token belongs to a different path or scope",
            ));
        }
        if current.identity_fingerprint != expected.identity_fingerprint {
            return Err(RuntimeError::new(
                "targetReplaced",
                "path now refers to a different filesystem entry",
            ));
        }
        if current != expected {
            return Err(RuntimeError::new(
                "versionMismatch",
                "filesystem entry changed since the version token was captured",
            ));
        }
        Ok(current)
    }
}

fn capture(
    path: &Path,
    root: &Path,
    key: &[u8; 32],
    request: &FsStatRequest,
    cancellation: &CancellationToken,
) -> RuntimeResult<FsStatResult> {
    let started = Instant::now();
    let before_metadata = fs::symlink_metadata(path).map_err(io_error)?;
    reject_reparse_metadata(&before_metadata)?;
    let before = snapshot(path, &before_metadata)?;
    #[cfg(test)]
    tests::wait_on_hash_test_hook(path);
    let content_hash = match request.version_strength {
        VersionStrength::Metadata => None,
        VersionStrength::Sampled => {
            let bytes = before
                .size_bytes
                .min(u64::try_from(SAMPLE_BYTES).unwrap_or(u64::MAX))
                .saturating_mul(3);
            ensure_hash_size_within_budget(bytes, &request.budget)?;
            Some(hash_sampled(
                path,
                before.size_bytes,
                &request.budget,
                cancellation,
                started,
            )?)
        }
        VersionStrength::Content => {
            ensure_hash_size_within_budget(before.size_bytes, &request.budget)?;
            Some(hash_content(path, &request.budget, cancellation, started)?)
        }
    };
    let after_metadata = fs::symlink_metadata(path).map_err(io_error)?;
    let after = snapshot(path, &after_metadata)?;
    ensure_unchanged(&before, &after)?;
    let path_fingerprint = fingerprint_path(path, root);
    let version = FileVersion {
        schema: TOKEN_VERSION,
        path_fingerprint,
        identity_fingerprint: before.identity_fingerprint,
        entry_type: before.entry_type.clone(),
        size_bytes: before.size_bytes,
        modified_at_ns: before.modified_at_ns,
        changed_at_ns: before.changed_at_ns,
        strength: request.version_strength,
        content_hash: content_hash.clone(),
    };
    let token = encode_token(key, &version)?;
    Ok(FsStatResult {
        path: path.to_path_buf(),
        name: path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        ),
        entry_type: before.entry_type,
        size: before.size_bytes,
        size_bytes: before.size_bytes,
        readonly: after_metadata.permissions().readonly(),
        modified_at_ns: before.modified_at_ns,
        created_at_ns: system_time_ns(after_metadata.created().ok()),
        permissions: permissions(&after_metadata),
        version_token: token,
        version_strength: request.version_strength,
        content_hash,
        hash_algorithm: match request.version_strength {
            VersionStrength::Metadata => None,
            VersionStrength::Sampled | VersionStrength::Content => Some("sha256".to_owned()),
        },
        symlink: false,
    })
}

fn validate_hash_algorithm(request: &FsStatRequest) -> RuntimeResult<()> {
    if let Some(algorithm) = request.hash_algorithm.as_deref()
        && !algorithm.eq_ignore_ascii_case("sha256")
    {
        return Err(RuntimeError::new(
            "unsupportedHashAlgorithm",
            "fs_stat supports only sha256",
        ));
    }
    Ok(())
}

fn snapshot(path: &Path, metadata: &fs::Metadata) -> RuntimeResult<Snapshot> {
    let entry_type = if metadata.file_type().is_symlink() {
        "symlink"
    } else if metadata.is_file() {
        "file"
    } else if metadata.is_dir() {
        "directory"
    } else {
        "other"
    }
    .to_owned();
    Ok(Snapshot {
        identity_fingerprint: fingerprint_identity(path, metadata)?,
        entry_type,
        size_bytes: metadata.len(),
        modified_at_ns: system_time_ns(metadata.modified().ok()),
        changed_at_ns: changed_time_ns(metadata),
    })
}

fn fingerprint_path(path: &Path, root: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(relative.to_string_lossy().as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

fn fingerprint_identity(_path: &Path, _metadata: &fs::Metadata) -> RuntimeResult<String> {
    let mut hasher = Sha256::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        hasher.update(_metadata.dev().to_le_bytes());
        hasher.update(_metadata.ino().to_le_bytes());
    }
    #[cfg(windows)]
    {
        let (volume, index) = windows_file_identity(_path)?;
        hasher.update(volume.to_le_bytes());
        hasher.update(index.to_le_bytes());
    }
    #[cfg(not(any(unix, windows)))]
    {
        hasher.update(_metadata.len().to_le_bytes());
        if let Some(ns) = system_time_ns(_metadata.created().ok()) {
            hasher.update(ns.to_le_bytes());
        }
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[cfg(unix)]
fn changed_time_ns(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    let seconds = i128::from(metadata.ctime());
    let nanos = i128::from(metadata.ctime_nsec());
    u64::try_from(seconds.checked_mul(1_000_000_000)?.checked_add(nanos)?).ok()
}

#[cfg(not(unix))]
fn changed_time_ns(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(windows)]
fn windows_file_identity(path: &Path) -> RuntimeResult<(u32, u64)> {
    use std::{mem::MaybeUninit, os::windows::io::AsRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let file = File::open(path).map_err(io_error)?;
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
    // SAFETY: `information` points to writable storage for the duration of the call,
    // and `file` keeps the borrowed OS handle valid.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    // SAFETY: a successful Win32 call initialized the complete output structure.
    let information = unsafe { information.assume_init() };
    let index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok((information.dwVolumeSerialNumber, index))
}

fn permissions(_metadata: &fs::Metadata) -> FsPermissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        FsPermissions {
            mode: Some(format!("{:04o}", _metadata.permissions().mode() & 0o7777)),
        }
    }
    #[cfg(not(unix))]
    FsPermissions { mode: None }
}

fn system_time_ns(value: Option<SystemTime>) -> Option<u64> {
    value?
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok())
}

fn hash_content(
    path: &Path,
    budget: &FsStatBudget,
    cancellation: &CancellationToken,
    started: Instant,
) -> RuntimeResult<String> {
    let file = File::open(path).map_err(io_error)?;
    let mut reader = BufReader::with_capacity(HASH_CHUNK_BYTES, file);
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; HASH_CHUNK_BYTES];
    let mut bytes_read = 0_u64;
    loop {
        check_budget(budget, cancellation, started, bytes_read)?;
        let read = reader.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        check_budget(budget, cancellation, started, bytes_read)?;
        hash.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn hash_sampled(
    path: &Path,
    size: u64,
    budget: &FsStatBudget,
    cancellation: &CancellationToken,
    started: Instant,
) -> RuntimeResult<String> {
    let mut file = File::open(path).map_err(io_error)?;
    let sample = u64::try_from(SAMPLE_BYTES).unwrap_or(u64::MAX).min(size);
    let positions = [
        0,
        size.saturating_sub(sample) / 2,
        size.saturating_sub(sample),
    ];
    let mut hash = Sha256::new();
    hash.update(size.to_le_bytes());
    let mut bytes_read = 0_u64;
    let mut buffer = vec![0_u8; usize::try_from(sample).unwrap_or(SAMPLE_BYTES)];
    for position in positions {
        check_budget(budget, cancellation, started, bytes_read)?;
        file.seek(SeekFrom::Start(position)).map_err(io_error)?;
        let read = file.read(&mut buffer).map_err(io_error)?;
        bytes_read = bytes_read.saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        check_budget(budget, cancellation, started, bytes_read)?;
        hash.update(position.to_le_bytes());
        hash.update(&buffer[..read]);
    }
    Ok(format!("sha256:{:x}", hash.finalize()))
}

fn check_budget(
    budget: &FsStatBudget,
    cancellation: &CancellationToken,
    started: Instant,
    bytes_read: u64,
) -> RuntimeResult<()> {
    if cancellation.is_cancelled() {
        return Err(RuntimeError::new(
            "operationCancelled",
            "fs_stat hashing was cancelled",
        ));
    }
    if started.elapsed() > Duration::from_millis(budget.timeout_ms) {
        return Err(RuntimeError::new(
            "hashBudgetExceeded",
            "fs_stat hashing timed out",
        ));
    }
    if bytes_read > budget.max_bytes_read {
        return Err(RuntimeError::new(
            "hashBudgetExceeded",
            "fs_stat hashing exceeded maxBytesRead",
        ));
    }
    Ok(())
}

fn ensure_unchanged(before: &Snapshot, after: &Snapshot) -> RuntimeResult<()> {
    if before == after {
        Ok(())
    } else {
        Err(RuntimeError::new(
            "fileChangedDuringHash",
            "filesystem entry changed while its version was being captured",
        ))
    }
}

fn ensure_hash_size_within_budget(bytes: u64, budget: &FsStatBudget) -> RuntimeResult<()> {
    if bytes > budget.max_bytes_read {
        Err(RuntimeError::new(
            "hashBudgetExceeded",
            "fs_stat hashing would exceed maxBytesRead",
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
#[path = "file_version_tests.rs"]
mod tests;
