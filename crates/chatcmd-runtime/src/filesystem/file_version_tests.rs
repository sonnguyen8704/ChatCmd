use super::*;
use crate::{ApprovalDecision, BoxFuture, PolicyEngine};
use std::{
    collections::BTreeMap,
    sync::{Arc, Barrier},
};
use tempfile::TempDir;

struct Approve;

impl ApprovalDecision for Approve {
    fn request<'a>(&'a self, _context: &'a PolicyContext) -> BoxFuture<'a, RuntimeResult<bool>> {
        Box::pin(async { Ok(true) })
    }
}

fn service(root: &Path) -> WorkspaceService {
    WorkspaceService::new(
        &[root.to_path_buf()],
        PolicyEngine::new(
            Some(crate::ExecutionPolicy {
                default: crate::PolicyDecision::Allow,
                per_agent_tool: BTreeMap::new(),
                per_root: BTreeMap::new(),
            }),
            Arc::new(Approve),
        ),
    )
    .expect("workspace service")
}

fn request(path: &Path, strength: VersionStrength) -> FsStatRequest {
    FsStatRequest {
        path: path.to_path_buf(),
        version_strength: strength,
        hash_algorithm: None,
        budget: FsStatBudget::default(),
    }
}

#[tokio::test]
async fn metadata_token_is_stable_and_legacy_fields_remain() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("stable.txt");
    fs::write(&path, "stable").expect("write file");
    let workspace = service(directory.path());

    let first = workspace
        .stat_v2(None, &request(&path, VersionStrength::Metadata))
        .await
        .expect("first stat");
    let second = workspace
        .stat_v2(None, &request(&path, VersionStrength::Metadata))
        .await
        .expect("second stat");

    assert_eq!(first.version_token, second.version_token);
    assert_eq!(first.size, first.size_bytes);
    assert_eq!(first.content_hash, None);
    assert_eq!(first.hash_algorithm, None);
}

#[tokio::test]
async fn metadata_detects_same_size_change_and_atomic_replacement() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("changed.txt");
    fs::write(&path, "aaaa").expect("write file");
    let workspace = service(directory.path());
    let initial = workspace
        .stat_v2(None, &request(&path, VersionStrength::Metadata))
        .await
        .expect("initial stat");

    std::thread::sleep(Duration::from_millis(20));
    fs::write(&path, "bbbb").expect("same-size update");
    let changed = workspace
        .stat_v2(None, &request(&path, VersionStrength::Metadata))
        .await
        .expect("changed stat");
    assert_ne!(initial.version_token, changed.version_token);
    assert_eq!(
        workspace
            .verify_expected_version(&path, &initial.version_token, None)
            .await
            .expect_err("old metadata token must fail")
            .code,
        "versionMismatch"
    );

    let replacement = directory.path().join("replacement.tmp");
    fs::write(&replacement, "bbbb").expect("replacement");
    fs::remove_file(&path).expect("remove old file");
    fs::rename(&replacement, &path).expect("atomic rename");
    assert_eq!(
        workspace
            .verify_expected_version(&path, &changed.version_token, None)
            .await
            .expect_err("replacement identity must fail")
            .code,
        "targetReplaced"
    );
}

#[tokio::test]
async fn content_hash_matches_sha256_vector_and_streams_large_file() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("vector.bin");
    fs::write(&path, b"abc").expect("write vector");
    let workspace = service(directory.path());
    let vector = workspace
        .stat_v2(None, &request(&path, VersionStrength::Content))
        .await
        .expect("content stat");
    assert_eq!(
        vector.content_hash.as_deref(),
        Some("sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );

    let large = vec![0x5a_u8; HASH_CHUNK_BYTES * 5 + 17];
    fs::write(&path, &large).expect("write large file");
    let result = workspace
        .stat_v2(None, &request(&path, VersionStrength::Content))
        .await
        .expect("stream large file");
    assert_eq!(result.content_hash, Some(digest_hex(&large)));
}

#[tokio::test]
async fn tokens_are_tamper_evident_and_path_bound() {
    let directory = TempDir::new().expect("temp directory");
    let first_path = directory.path().join("a.txt");
    let second_path = directory.path().join("b.txt");
    fs::write(&first_path, "same").expect("first");
    fs::write(&second_path, "same").expect("second");
    let workspace = service(directory.path());
    let result = workspace
        .stat_v2(None, &request(&first_path, VersionStrength::Content))
        .await
        .expect("stat");

    let mut tampered = result.version_token.clone().into_bytes();
    let last = tampered.last_mut().expect("nonempty token");
    *last = if *last == b'A' { b'B' } else { b'A' };
    let tampered = String::from_utf8(tampered).expect("ASCII token");
    assert_eq!(
        workspace
            .verify_expected_version(&first_path, &tampered, None)
            .await
            .expect_err("tampered token")
            .code,
        "versionUnsupported"
    );
    assert_eq!(
        workspace
            .verify_expected_version(&second_path, &result.version_token, None)
            .await
            .expect_err("cross-path token")
            .code,
        "versionMismatch"
    );
    assert_eq!(
        workspace
            .verify_expected_version(&first_path, "v0:obsolete:token", None)
            .await
            .expect_err("old schema token")
            .code,
        "versionUnsupported"
    );
    fs::remove_file(&first_path).expect("remove versioned target");
    assert_eq!(
        workspace
            .verify_expected_version(&first_path, &result.version_token, None)
            .await
            .expect_err("missing target")
            .code,
        "targetMissing"
    );
}

#[tokio::test]
async fn hashing_honors_cancellation_and_byte_budget() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("bounded.bin");
    fs::write(&path, vec![1_u8; HASH_CHUNK_BYTES + 1]).expect("write file");
    let workspace = service(directory.path());
    let context = OperationContext::new("stat", "agent", "fs_stat");
    context.cancellation.cancel();
    assert_eq!(
        workspace
            .stat_v2(Some(&context), &request(&path, VersionStrength::Content))
            .await
            .expect_err("cancelled hash")
            .code,
        "operationCancelled"
    );

    let mut bounded = request(&path, VersionStrength::Content);
    bounded.budget.max_bytes_read = 1;
    assert_eq!(
        workspace
            .stat_v2(None, &bounded)
            .await
            .expect_err("byte budget")
            .code,
        "hashBudgetExceeded"
    );

    let mut timed_out = request(&path, VersionStrength::Content);
    timed_out.budget.timeout_ms = 0;
    assert_eq!(
        workspace
            .stat_v2(None, &timed_out)
            .await
            .expect_err("time budget")
            .code,
        "hashBudgetExceeded"
    );
}

#[tokio::test]
async fn file_changed_during_hash_returns_conflict() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("racing.bin");
    fs::write(&path, vec![1_u8; HASH_CHUNK_BYTES]).expect("write file");
    let workspace = service(directory.path());
    let gate = Arc::new((Barrier::new(2), Barrier::new(2)));
    *hash_test_hook().lock().expect("hook lock") =
        Some((path.canonicalize().expect("canonical path"), gate.clone()));
    let mutation_path = path.clone();
    let replacement_path = directory.path().join("racing-replacement.bin");
    fs::write(&replacement_path, vec![2_u8; HASH_CHUNK_BYTES + 1]).expect("write replacement");
    let mutation_gate = gate.clone();
    let mutation = std::thread::spawn(move || {
        mutation_gate.0.wait();
        fs::remove_file(&mutation_path).expect("remove during capture");
        fs::rename(&replacement_path, &mutation_path).expect("replace during capture");
        mutation_gate.1.wait();
    });

    let error = workspace
        .stat_v2(None, &request(&path, VersionStrength::Content))
        .await
        .expect_err("concurrent mutation");
    mutation.join().expect("mutation thread");
    assert_eq!(error.code, "fileChangedDuringHash");
}

#[test]
fn changed_snapshot_maps_to_hash_conflict() {
    let before = Snapshot {
        identity_fingerprint: "identity".into(),
        entry_type: "file".into(),
        size_bytes: 1,
        modified_at_ns: Some(1),
        changed_at_ns: Some(1),
    };
    let mut after = before.clone();
    after.size_bytes = 2;
    assert_eq!(
        ensure_unchanged(&before, &after)
            .expect_err("changed during hash")
            .code,
        "fileChangedDuringHash"
    );
}

#[cfg(unix)]
#[test]
fn unix_identity_uses_device_and_inode() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("identity");
    fs::write(&path, "one").expect("write");
    let first =
        fingerprint_identity(&path, &fs::metadata(&path).expect("metadata")).expect("identity");
    fs::rename(&path, directory.path().join("old-identity")).expect("preserve old identity");
    fs::write(&path, "two").expect("replace");
    let second =
        fingerprint_identity(&path, &fs::metadata(&path).expect("metadata")).expect("identity");
    assert_ne!(first, second);
}

#[cfg(windows)]
#[test]
fn windows_identity_uses_volume_and_file_index() {
    let directory = TempDir::new().expect("temp directory");
    let path = directory.path().join("identity");
    fs::write(&path, "one").expect("write");
    let first = windows_file_identity(&path).expect("identity");
    fs::rename(&path, directory.path().join("old-identity")).expect("preserve old identity");
    fs::write(&path, "two").expect("replace");
    let second = windows_file_identity(&path).expect("identity");
    assert_ne!(first, second);
}

fn digest_hex(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

type HashTestHook = (PathBuf, Arc<(std::sync::Barrier, std::sync::Barrier)>);

fn hash_test_hook() -> &'static std::sync::Mutex<Option<HashTestHook>> {
    static HOOK: std::sync::OnceLock<std::sync::Mutex<Option<HashTestHook>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

pub(super) fn wait_on_hash_test_hook(path: &Path) {
    let gate = hash_test_hook()
        .lock()
        .expect("hash hook lock")
        .as_ref()
        .filter(|(hook_path, _)| hook_path == path)
        .map(|(_, gate)| gate.clone());
    let Some(gate) = gate else {
        return;
    };
    gate.0.wait();
    gate.1.wait();
    *hash_test_hook().lock().expect("hash hook lock") = None;
}
