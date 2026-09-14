//! Stable path leases shared by catalogs using the same private state home.
//! Lock files are never unlinked: the kernel releases ownership on process exit.
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::AgentError;

pub(super) struct FileLeases {
    _guards: Vec<LeaseFile>,
}

struct LeaseFile(File);

impl Drop for LeaseFile {
    fn drop(&mut self) {
        // An unrelated thread can fork between open and close. Explicit unlock
        // releases our ownership even while the child briefly inherits the FD.
        if let Err(error) = self.0.unlock() {
            eprintln!("failed to release worker file lease; closing descriptor: {error}");
        }
    }
}

fn identity(path: &Path) -> std::io::Result<PathBuf> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return Err(std::io::Error::other(
            "worker write target must be an absolute normalized path",
        ));
    }
    let mut ancestor = path;
    let mut suffix = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut resolved) => {
                for name in suffix.into_iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(ancestor.file_name().ok_or(error)?);
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| std::io::Error::other("worker path has no existing ancestor"))?;
            }
            Err(error) => return Err(error),
        }
    }
}

impl FileLeases {
    pub(super) fn acquire(home: &Path, targets: &BTreeSet<PathBuf>) -> Result<Self, AgentError> {
        Self::try_acquire(home, targets).map_err(|error| {
            AgentError::Subagent(format!("cannot acquire worker write leases: {error}"))
        })
    }

    fn try_acquire(home: &Path, targets: &BTreeSet<PathBuf>) -> std::io::Result<Self> {
        let directory = home.join("file-leases");
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&directory)?;
        if !std::fs::symlink_metadata(&directory)?.is_dir() {
            return Err(std::io::Error::other(
                "worker lease directory is not a directory",
            ));
        }
        let identities = targets
            .iter()
            .map(|target| identity(target))
            .collect::<std::io::Result<BTreeSet<_>>>()?;
        let mut guards = Vec::new();
        for path in identities {
            let key = format!("{:x}", Sha256::digest(path.as_os_str().as_encoded_bytes()));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let guard = options.open(directory.join(format!("{key}.lock")))?;
            if !guard.metadata()?.is_file() {
                return Err(std::io::Error::other("worker lease is not a regular file"));
            }
            guard.try_lock().map_err(|error| match error {
                std::fs::TryLockError::WouldBlock => std::io::Error::other(format!(
                    "another worker owns {}; wait until it exits",
                    path.display()
                )),
                std::fs::TryLockError::Error(error) => error,
            })?;
            guards.push(LeaseFile(guard));
        }
        Ok(Self { _guards: guards })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let root = std::env::temp_dir().join(format!("worker-leases-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn independent_claims_conflict_and_failed_batch_releases_earlier_paths() {
        let root = root();
        let a = BTreeSet::from([root.join("a")]);
        let b = BTreeSet::from([root.join("b")]);
        let held = FileLeases::acquire(&root, &b).unwrap();
        assert!(FileLeases::acquire(&root, &a.union(&b).cloned().collect()).is_err());
        let earlier = FileLeases::acquire(&root, &a).unwrap();
        assert!(FileLeases::acquire(&root, &b).is_err());
        drop(held);
        let released = FileLeases::acquire(&root, &b).unwrap();
        drop((earlier, released));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn inherited_descriptor_does_not_extend_the_owner_lifetime() {
        let root = root();
        let targets = BTreeSet::from([root.join("target")]);
        let held = FileLeases::acquire(&root, &targets).unwrap();
        // dup shares the same open-file description, as an inherited fork FD does.
        let inherited = held._guards[0].0.try_clone().unwrap();
        drop(held);
        let replacement = FileLeases::acquire(&root, &targets).unwrap();
        drop(inherited);
        assert!(FileLeases::acquire(&root, &targets).is_err());
        drop(replacement);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn catalog_refuses_owned_target_before_calling_provider_and_can_retry_after_release() {
        use crate::subagent::{SpawnAgentArgs, SubagentCatalog, builtin_profiles};
        use std::sync::{Arc, Mutex};
        let root = root();
        let home = root.join("state");
        let targets = BTreeSet::from([root.join("code.rs")]);
        let held = FileLeases::acquire(&home, &targets).unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(super::super::test_support::ModelProvider {
            model: "local-test".into(),
            seen: seen.clone(),
        });
        let mut profiles = builtin_profiles(provider);
        let profile = profiles
            .iter_mut()
            .find(|profile| profile.id == "implementer")
            .unwrap();
        profile.worktree = crate::subagent_worktree::SubagentWorktreePolicy::Shared;
        let catalog = SubagentCatalog::new(
            &root,
            profiles,
            Arc::new(crate::background::BackgroundTaskRegistry::default()),
        )
        .with_state_home(&home);
        let args = || SpawnAgentArgs {
            profile: Some("implementer".into()),
            prompt: "Inspect the approved target".into(),
            task: Some(crate::subagent::TaskPacket {
                goal: "Inspect the approved target".into(),
                verifier: Some(crate::subagent::TaskVerifier {
                    command: "true".into(),
                    expected_exit_code: None,
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let error = catalog
            .run(args(), Some(targets.clone()))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("another worker owns"), "{error}");
        assert!(seen.lock().unwrap().is_empty());
        drop(held);
        catalog.run(args(), Some(targets)).await.unwrap();
        assert_eq!(seen.lock().unwrap().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_parent_and_new_file_share_the_same_lease() {
        let root = root();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("alias")).unwrap();
        let held = FileLeases::acquire(&root, &BTreeSet::from([root.join("real/new")])).unwrap();
        assert!(FileLeases::acquire(&root, &BTreeSet::from([root.join("alias/new")])).is_err());
        std::fs::write(root.join("real/new"), "created while owning lease").unwrap();
        assert!(FileLeases::acquire(&root, &BTreeSet::from([root.join("real/new")])).is_err());
        drop(held);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "child process entry invoked by process_exit_releases_lease"]
    fn lease_child() {
        let Some(root) = std::env::var_os("WILLDEEP_TEST_FILE_LEASE") else {
            return;
        };
        let root = PathBuf::from(root);
        let _held = FileLeases::acquire(&root, &BTreeSet::from([root.join("target")])).unwrap();
        std::fs::write(root.join("ready"), "ready").unwrap();
        std::thread::sleep(std::time::Duration::from_secs(30));
    }

    #[test]
    fn process_exit_releases_lease() {
        let root = root();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "subagent::file_leases::tests::lease_child",
                "--ignored",
            ])
            .env("WILLDEEP_TEST_FILE_LEASE", &root)
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !root.join("ready").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let ready = root.join("ready").exists();
        let conflict = FileLeases::acquire(&root, &BTreeSet::from([root.join("target")])).is_err();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            ready && conflict,
            "live child must hold an exclusive path lease"
        );
        let released = FileLeases::acquire(&root, &BTreeSet::from([root.join("target")])).unwrap();
        drop(released);
        std::fs::remove_dir_all(root).unwrap();
    }
}
