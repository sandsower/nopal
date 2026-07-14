//! Canonical native application instance identity.

use sha2::{Digest, Sha256};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const SCOPE_FINGERPRINT_DOMAIN: &[u8] = b"nopal.native_instance_scope/v1";
const NATIVE_STATE_DIRECTORY: &str = "native";
const INSTANCE_LOCK_FILE: &str = "instance.lock";
const RESTORE_PREFERENCE_FILE: &str = "restore.json";
const CONTROL_FINGERPRINT_HEX_BYTES: usize = 32;

/// An existing, absolute state directory with filesystem aliases resolved.
///
/// Construction creates the directory first so a first launch and a reopened
/// launch derive identity from the same canonical filesystem object.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct CanonicalStateRoot {
    path: PathBuf,
}

impl CanonicalStateRoot {
    /// Creates the requested root and then resolves relative segments and
    /// symlinks into the one path used for native instance identity.
    pub fn create(path: impl AsRef<Path>) -> io::Result<Self> {
        std::fs::create_dir_all(path.as_ref())?;
        let path = std::fs::canonicalize(path.as_ref())?;
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native state root must be a directory",
            ));
        }
        if !path.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "canonical native state root must be absolute",
            ));
        }
        Ok(Self { path })
    }

    pub fn as_path(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for CanonicalStateRoot {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// The closed release vocabulary that partitions native application instances.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ReleaseChannel {
    Stable,
    Preview,
    Development,
}

impl ReleaseChannel {
    /// Stable lowercase value used in state layout and protocol identity.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Preview => "preview",
            Self::Development => "development",
        }
    }
}

impl fmt::Display for ReleaseChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// An input is not one of the exact supported release-channel wire values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidReleaseChannel;

impl fmt::Display for InvalidReleaseChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("release channel must be stable, preview, or development")
    }
}

impl std::error::Error for InvalidReleaseChannel {}

impl FromStr for ReleaseChannel {
    type Err = InvalidReleaseChannel;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "stable" => Ok(Self::Stable),
            "preview" => Ok(Self::Preview),
            "development" => Ok(Self::Development),
            _ => Err(InvalidReleaseChannel),
        }
    }
}

impl TryFrom<&str> for ReleaseChannel {
    type Error = InvalidReleaseChannel;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        value.parse()
    }
}

/// Identity of the one permitted native primary for a state root and channel.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct NativeInstanceScope {
    state_root: CanonicalStateRoot,
    release_channel: ReleaseChannel,
    fingerprint: String,
}

impl NativeInstanceScope {
    pub fn new(state_root: CanonicalStateRoot, release_channel: ReleaseChannel) -> Self {
        let fingerprint = scope_fingerprint(&state_root, release_channel);
        Self {
            state_root,
            release_channel,
            fingerprint,
        }
    }

    pub fn state_root(&self) -> &CanonicalStateRoot {
        &self.state_root
    }

    pub const fn release_channel(&self) -> ReleaseChannel {
        self.release_channel
    }

    /// Lowercase SHA-256 identity suitable for exact activation validation.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Derives persistent paths and a bounded leaf name for platform IPC.
    pub fn state_paths(&self) -> NativeStatePaths {
        NativeStatePaths::derive(self)
    }
}

/// Private-layout native state derived from one exact instance scope.
///
/// The activation endpoint is intentionally a leaf name rather than a full
/// platform path. Platform adapters choose their user-private control root and
/// never receive the potentially long canonical state root as socket input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeStatePaths {
    state_directory: PathBuf,
    instance_lock: PathBuf,
    restore_preference: PathBuf,
    activation_endpoint_name: String,
}

impl NativeStatePaths {
    fn derive(scope: &NativeInstanceScope) -> Self {
        let state_directory = scope
            .state_root()
            .as_path()
            .join(NATIVE_STATE_DIRECTORY)
            .join(scope.release_channel().as_str());
        let control_fingerprint = &scope.fingerprint()[..CONTROL_FINGERPRINT_HEX_BYTES];
        Self {
            instance_lock: state_directory.join(INSTANCE_LOCK_FILE),
            restore_preference: state_directory.join(RESTORE_PREFERENCE_FILE),
            state_directory,
            activation_endpoint_name: format!("nopal-{control_fingerprint}.sock"),
        }
    }

    pub fn state_directory(&self) -> &Path {
        &self.state_directory
    }

    pub fn instance_lock(&self) -> &Path {
        &self.instance_lock
    }

    pub fn restore_preference(&self) -> &Path {
        &self.restore_preference
    }

    /// A single bounded, separator-free component for a platform control root.
    pub fn activation_endpoint_name(&self) -> &str {
        &self.activation_endpoint_name
    }
}

fn scope_fingerprint(root: &CanonicalStateRoot, channel: ReleaseChannel) -> String {
    let root_bytes = root.as_path().as_os_str().as_encoded_bytes();
    let channel_bytes = channel.as_str().as_bytes();
    let mut hasher = Sha256::new();
    hasher.update(SCOPE_FINGERPRINT_DOMAIN);
    hasher.update((root_bytes.len() as u128).to_be_bytes());
    hasher.update(root_bytes);
    hasher.update((channel_bytes.len() as u128).to_be_bytes());
    hasher.update(channel_bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::{CanonicalStateRoot, NativeInstanceScope, ReleaseChannel};
    use std::path::Path;
    use std::str::FromStr;

    fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
        match result {
            Ok(value) => value,
            Err(error) => panic!("{context}: {error}"),
        }
    }

    #[test]
    fn relative_and_parent_segment_aliases_collapse_to_one_scope() {
        let cwd = must(std::env::current_dir(), "read current directory");
        let sandbox = must(tempfile::tempdir_in(&cwd), "create relative sandbox");
        let root = sandbox.path().join("state");
        let nested = root.join("alias-parent");
        must(std::fs::create_dir_all(&nested), "create alias fixture");

        let relative = must(root.strip_prefix(&cwd), "derive relative state root");
        let canonical = must(CanonicalStateRoot::create(relative), "open relative root");
        let parent_alias = must(
            CanonicalStateRoot::create(nested.join(Path::new(".."))),
            "open parent-segment alias",
        );

        let relative_scope = NativeInstanceScope::new(canonical, ReleaseChannel::Stable);
        let parent_scope = NativeInstanceScope::new(parent_alias, ReleaseChannel::Stable);

        assert_eq!(relative_scope, parent_scope);
        assert_eq!(relative_scope.fingerprint(), parent_scope.fingerprint());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_collapse_to_one_scope() {
        use std::os::unix::fs::symlink;

        let sandbox = must(tempfile::tempdir(), "create symlink sandbox");
        let root = sandbox.path().join("state");
        let alias = sandbox.path().join("state-link");
        must(std::fs::create_dir_all(&root), "create state root");
        must(symlink(&root, &alias), "create state-root symlink");

        let direct = NativeInstanceScope::new(
            must(CanonicalStateRoot::create(&root), "open direct root"),
            ReleaseChannel::Stable,
        );
        let linked = NativeInstanceScope::new(
            must(CanonicalStateRoot::create(&alias), "open symlink root"),
            ReleaseChannel::Stable,
        );

        assert_eq!(direct, linked);
        assert_eq!(direct.fingerprint(), linked.fingerprint());
    }

    #[test]
    fn release_channels_remain_distinct_for_the_same_canonical_root() {
        let sandbox = must(tempfile::tempdir(), "create channel sandbox");
        let root = must(
            CanonicalStateRoot::create(sandbox.path().join("state")),
            "open state root",
        );

        let stable = NativeInstanceScope::new(root.clone(), ReleaseChannel::Stable);
        let preview = NativeInstanceScope::new(root, ReleaseChannel::Preview);

        assert_ne!(stable, preview);
        assert_ne!(stable.fingerprint(), preview.fingerprint());
        assert_ne!(
            stable.state_paths().instance_lock(),
            preview.state_paths().instance_lock()
        );
        assert_ne!(
            stable.state_paths().activation_endpoint_name(),
            preview.state_paths().activation_endpoint_name()
        );
    }

    #[test]
    fn release_channel_wire_values_are_exact_and_path_safe() {
        for (value, expected) in [
            ("stable", ReleaseChannel::Stable),
            ("preview", ReleaseChannel::Preview),
            ("development", ReleaseChannel::Development),
        ] {
            assert_eq!(
                must(ReleaseChannel::from_str(value), "parse channel"),
                expected
            );
            assert_eq!(expected.to_string(), value);
        }

        for invalid in ["", "Stable", "stable ", "stable/preview", "..", "preview\n"] {
            assert!(
                ReleaseChannel::from_str(invalid).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn scope_fingerprint_and_control_name_are_bounded_and_unambiguous() {
        let sandbox = must(tempfile::tempdir(), "create path sandbox");
        let mut long_root = sandbox.path().to_path_buf();
        for index in 0..12 {
            long_root.push(format!("long-state-segment-{index:02}"));
        }
        let scope = NativeInstanceScope::new(
            must(CanonicalStateRoot::create(&long_root), "open long root"),
            ReleaseChannel::Development,
        );
        let paths = scope.state_paths();

        assert_eq!(scope.fingerprint().len(), 64);
        assert!(
            scope
                .fingerprint()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_eq!(paths.activation_endpoint_name().len(), 43);
        assert!(
            paths
                .activation_endpoint_name()
                .bytes()
                .all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'-'
                    || byte == b'.')
        );
        assert_eq!(
            paths.state_directory(),
            scope
                .state_root()
                .as_path()
                .join("native")
                .join("development")
        );
        assert_eq!(
            paths.instance_lock(),
            paths.state_directory().join("instance.lock")
        );
        assert_eq!(
            paths.restore_preference(),
            paths.state_directory().join("restore.json")
        );
        assert_ne!(paths.instance_lock(), paths.restore_preference());
    }
}
