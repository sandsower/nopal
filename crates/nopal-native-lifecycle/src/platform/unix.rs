//! Unix advisory-lock and local-socket instance coordination.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;

use crate::instance::{InstanceAcquisition, InstancePlatform};
use crate::state_root::NativeInstanceScope;

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const CONNECT_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const DEFAULT_CONTROL_PARENT: &str = "/tmp";
// The smallest common sockaddr_un path capacity among supported Unix targets
// has room for 103 path bytes plus its terminating NUL.
const MAX_SOCKET_PATH_BYTES: usize = 103;

/// Coordinates the one native primary for a Unix instance scope.
#[derive(Debug)]
pub struct UnixInstanceCoordinator {
    state_directory: PathBuf,
    lock_path: PathBuf,
    control_root: PathBuf,
    endpoint: PathBuf,
}

impl UnixInstanceCoordinator {
    /// Uses a stable, short, user-private control directory suitable for
    /// production launch on macOS and other Unix systems. This intentionally
    /// does not use `TMPDIR`: its macOS value commonly leaves too little room
    /// for a portable `sockaddr_un` endpoint.
    pub fn with_default_control_root(scope: NativeInstanceScope) -> io::Result<Self> {
        let owner = std::fs::metadata(scope.state_root().as_path())?.uid();
        let control_root = Path::new(DEFAULT_CONTROL_PARENT).join(format!("nopal-native-{owner}"));
        Self::new(scope, control_root)
    }

    /// Prepares private persistent and control directories for one scope.
    pub fn new(scope: NativeInstanceScope, control_root: impl AsRef<Path>) -> io::Result<Self> {
        let paths = scope.state_paths();
        ensure_private_directory(paths.state_directory())?;
        ensure_private_directory(control_root.as_ref())?;
        let control_root = std::fs::canonicalize(control_root.as_ref())?;
        let state_owner = std::fs::metadata(paths.state_directory())?.uid();
        let control_owner = std::fs::metadata(&control_root)?.uid();
        if state_owner != control_owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native state and control directories must have the same owner",
            ));
        }
        let endpoint = control_root.join(paths.activation_endpoint_name());
        if endpoint.as_os_str().as_encoded_bytes().len() > MAX_SOCKET_PATH_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "native activation socket path exceeds the Unix platform limit",
            ));
        }
        Ok(Self {
            state_directory: paths.state_directory().to_owned(),
            lock_path: paths.instance_lock().to_owned(),
            control_root,
            endpoint,
        })
    }

    pub fn state_directory(&self) -> &Path {
        &self.state_directory
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }

    pub fn control_root(&self) -> &Path {
        &self.control_root
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    fn open_lock_file(&self) -> io::Result<File> {
        let state_owner = std::fs::metadata(&self.state_directory)?.uid();
        let effective_owner = effective_user_id();
        if state_owner != effective_owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native lifecycle state directory must be owned by the effective user",
            ));
        }

        let mut options = OpenOptions::new();
        options
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(PRIVATE_FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW);
        let file = options.open(&self.lock_path).map_err(|error| {
            if std::fs::symlink_metadata(&self.lock_path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                lock_file_error("native lifecycle lock must not be a symbolic link")
            } else {
                error
            }
        })?;
        self.validate_lock_file(&file)?;
        file.set_permissions(std::fs::Permissions::from_mode(PRIVATE_FILE_MODE))?;
        Ok(file)
    }

    fn validate_lock_file(&self, file: &File) -> io::Result<FileIdentity> {
        let state_owner = std::fs::metadata(&self.state_directory)?.uid();
        let effective_owner = effective_user_id();
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(lock_file_error(
                "native lifecycle lock descriptor must be a regular file",
            ));
        }
        if metadata.uid() != state_owner || metadata.uid() != effective_owner {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native lifecycle lock must be owned by the state directory owner and effective user",
            ));
        }
        if metadata.nlink() != 1 {
            return Err(lock_file_error(
                "native lifecycle lock must have exactly one filesystem link",
            ));
        }
        let path_metadata = std::fs::symlink_metadata(&self.lock_path)?;
        if path_metadata.file_type().is_symlink()
            || FileIdentity::from_metadata(&path_metadata) != FileIdentity::from_metadata(&metadata)
        {
            return Err(lock_file_error(
                "native lifecycle lock path changed while it was opened",
            ));
        }
        Ok(FileIdentity::from_metadata(&metadata))
    }

    fn become_primary(&self, lock_file: File) -> io::Result<UnixPrimaryLease> {
        match std::fs::remove_file(&self.endpoint) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }

        let listener = UnixListener::bind(&self.endpoint)?;
        std::fs::set_permissions(
            &self.endpoint,
            std::fs::Permissions::from_mode(PRIVATE_FILE_MODE),
        )?;
        let lock_identity = FileIdentity::from_metadata(&lock_file.metadata()?);
        let endpoint_identity = FileIdentity::from_metadata(&std::fs::metadata(&self.endpoint)?);
        Ok(UnixPrimaryLease {
            _lock_file: lock_file,
            lock_path: self.lock_path.clone(),
            listener,
            endpoint: self.endpoint.clone(),
            lock_identity,
            endpoint_identity,
        })
    }

    fn connect_secondary(&self, timeout: Duration) -> io::Result<UnixStream> {
        let started = Instant::now();
        loop {
            match UnixStream::connect(&self.endpoint) {
                Ok(stream) => return Ok(stream),
                Err(error) if is_transient_connect_error(&error) => {
                    let elapsed = started.elapsed();
                    if elapsed >= timeout {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!(
                                "primary lease is held but activation endpoint remained unavailable for {elapsed:?}"
                            ),
                        ));
                    }
                    thread::sleep(CONNECT_RETRY_INTERVAL.min(timeout.saturating_sub(elapsed)));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl InstancePlatform for UnixInstanceCoordinator {
    type Primary = UnixPrimaryLease;
    type Secondary = UnixStream;

    fn acquire(
        &self,
        secondary_connect_timeout: Duration,
    ) -> io::Result<InstanceAcquisition<Self::Primary, Self::Secondary>> {
        let lock_file = self.open_lock_file()?;
        match FileExt::try_lock_exclusive(&lock_file) {
            Ok(()) => {
                // The path can be renamed between open and lock acquisition.
                // Revalidate only after authority is held and immediately
                // before the endpoint is replaced.
                self.validate_lock_file(&lock_file)?;
                self.become_primary(lock_file)
                    .map(InstanceAcquisition::Primary)
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.validate_lock_file(&lock_file)?;
                self.connect_secondary(secondary_connect_timeout)
                    .map(InstanceAcquisition::Secondary)
            }
            Err(error) => Err(error),
        }
    }
}

/// The sole authority to accept activation traffic for one Unix instance.
#[derive(Debug)]
pub struct UnixPrimaryLease {
    // Field order is intentional. The Drop body removes the guarded endpoint,
    // then Rust closes the listener before it closes the advisory lock file.
    // A replacement primary therefore cannot acquire authority while the old
    // listener is still alive.
    listener: UnixListener,
    endpoint: PathBuf,
    lock_path: PathBuf,
    lock_identity: FileIdentity,
    endpoint_identity: FileIdentity,
    _lock_file: File,
}

impl UnixPrimaryLease {
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }

    pub fn endpoint(&self) -> &Path {
        &self.endpoint
    }

    /// Accepts one untyped transport. Activation framing and validation belong
    /// to the platform-neutral activation protocol.
    pub fn accept(&self) -> io::Result<UnixStream> {
        self.listener.accept().map(|(stream, _address)| stream)
    }
}

impl Drop for UnixPrimaryLease {
    fn drop(&mut self) {
        let still_names_our_lock = std::fs::metadata(&self.lock_path)
            .map(|metadata| FileIdentity::from_metadata(&metadata) == self.lock_identity)
            .unwrap_or(false);
        let still_names_our_endpoint = std::fs::metadata(&self.endpoint)
            .map(|metadata| FileIdentity::from_metadata(&metadata) == self.endpoint_identity)
            .unwrap_or(false);
        if still_names_our_lock && still_names_our_endpoint {
            let _cleanup_result = std::fs::remove_file(&self.endpoint);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }
}

fn ensure_private_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path)?;
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native lifecycle directory must not be a symbolic link",
        ));
    }
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native lifecycle path must be a directory",
        ));
    }
    std::fs::set_permissions(
        path,
        std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
    )
}

fn effective_user_id() -> u32 {
    // SAFETY: libc's `geteuid` takes no arguments and has no preconditions.
    unsafe { libc::geteuid() }
}

fn lock_file_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn is_transient_connect_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
    )
}

#[cfg(test)]
mod tests {
    use std::fs::OpenOptions;
    use std::io::{self, Read, Write};
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::process::{Child, Command, ExitStatus};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use fs2::FileExt;

    use super::UnixInstanceCoordinator;
    use crate::instance::{InstanceAcquisition, InstancePlatform};
    use crate::state_root::{CanonicalStateRoot, NativeInstanceScope, ReleaseChannel};

    fn scope(root: &std::path::Path, channel: ReleaseChannel) -> NativeInstanceScope {
        NativeInstanceScope::new(
            CanonicalStateRoot::create(root).expect("create canonical state root"),
            channel,
        )
    }

    fn sandbox() -> tempfile::TempDir {
        tempfile::tempdir_in("/tmp").expect("create short Unix socket sandbox")
    }

    struct KillOnDropChild {
        child: Option<Child>,
    }

    impl KillOnDropChild {
        fn new(child: Child) -> Self {
            Self { child: Some(child) }
        }

        fn wait_bounded(mut self, timeout: Duration) -> io::Result<ExitStatus> {
            let deadline = Instant::now() + timeout;
            loop {
                let child = self.child.as_mut().expect("child remains owned");
                if let Some(status) = child.try_wait()? {
                    self.child = None;
                    return Ok(status);
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    self.child = None;
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "launch contender did not exit before the test deadline",
                    ));
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Drop for KillOnDropChild {
        fn drop(&mut self) {
            if let Some(child) = &mut self.child {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[test]
    fn held_lock_with_dead_endpoint_fails_bounded_without_promotion() {
        let sandbox = sandbox();
        let scope = scope(&sandbox.path().join("state"), ReleaseChannel::Stable);
        let coordinator = UnixInstanceCoordinator::new(scope, sandbox.path().join("control"))
            .expect("create coordinator");
        let lock_path = coordinator.lock_path().to_owned();
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)
            .expect("open lock");
        lock.lock_exclusive().expect("hold primary lock");

        let started = Instant::now();
        let error = coordinator
            .acquire(Duration::from_millis(80))
            .expect_err("secondary must not promote without a live endpoint");

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(60));
        assert!(started.elapsed() < Duration::from_secs(1));
        fs2::FileExt::unlock(&lock).expect("unlock fixture");
    }

    #[test]
    fn concurrent_launch_has_exactly_one_primary_and_one_connected_secondary() {
        let sandbox = sandbox();
        let coordinator = Arc::new(
            UnixInstanceCoordinator::new(
                scope(&sandbox.path().join("state"), ReleaseChannel::Stable),
                sandbox.path().join("control"),
            )
            .expect("create coordinator"),
        );
        let start = Arc::new(Barrier::new(3));
        let (roles_tx, roles_rx) = mpsc::channel();
        let mut launches = Vec::new();
        for _ in 0..2 {
            let coordinator = Arc::clone(&coordinator);
            let start = Arc::clone(&start);
            let roles_tx = roles_tx.clone();
            launches.push(thread::spawn(move || {
                start.wait();
                match coordinator
                    .acquire(Duration::from_secs(1))
                    .expect("acquire launch role")
                {
                    InstanceAcquisition::Primary(primary) => {
                        roles_tx.send("primary").expect("report primary");
                        let mut stream = primary.accept().expect("accept secondary");
                        let mut byte = [0_u8; 1];
                        stream.read_exact(&mut byte).expect("read activation byte");
                        assert_eq!(byte, [0x58]);
                    }
                    InstanceAcquisition::Secondary(mut stream) => {
                        roles_tx.send("secondary").expect("report secondary");
                        stream.write_all(&[0x58]).expect("send activation byte");
                    }
                }
            }));
        }
        drop(roles_tx);

        start.wait();
        for launch in launches {
            launch.join().expect("launch thread completed");
        }
        let mut roles: Vec<_> = roles_rx.into_iter().collect();
        roles.sort_unstable();
        assert_eq!(roles, ["primary", "secondary"]);
    }

    #[test]
    fn concurrent_processes_have_exactly_one_primary() {
        let sandbox = sandbox();
        let start = sandbox.path().join("start");
        let executable = std::env::current_exe().expect("find current test executable");
        let mut children = Vec::new();
        let mut role_paths = Vec::new();
        for index in 0..2 {
            let role_path = sandbox.path().join(format!("role-{index}"));
            let child = Command::new(&executable)
                .arg("--exact")
                .arg("platform::unix::tests::singleton_subprocess_probe")
                .arg("--nocapture")
                .env("NOPAL_INSTANCE_TEST_STATE", sandbox.path().join("state"))
                .env(
                    "NOPAL_INSTANCE_TEST_CONTROL",
                    sandbox.path().join("control"),
                )
                .env("NOPAL_INSTANCE_TEST_START", &start)
                .env("NOPAL_INSTANCE_TEST_ROLE", &role_path)
                .spawn()
                .expect("spawn launch contender");
            children.push(KillOnDropChild::new(child));
            role_paths.push(role_path);
        }

        std::fs::write(&start, b"go").expect("release launch contenders");
        for child in children {
            assert!(
                child
                    .wait_bounded(Duration::from_secs(5))
                    .expect("wait for launch contender")
                    .success()
            );
        }
        let mut roles: Vec<_> = role_paths
            .iter()
            .map(|path| std::fs::read_to_string(path).expect("read contender role"))
            .collect();
        roles.sort_unstable();
        assert_eq!(roles, ["primary", "secondary"]);
    }

    #[test]
    fn singleton_subprocess_probe() {
        let Some(state) = std::env::var_os("NOPAL_INSTANCE_TEST_STATE") else {
            return;
        };
        let control = std::env::var_os("NOPAL_INSTANCE_TEST_CONTROL")
            .expect("subprocess control root is configured");
        let start = std::env::var_os("NOPAL_INSTANCE_TEST_START")
            .expect("subprocess start marker is configured");
        let role = std::env::var_os("NOPAL_INSTANCE_TEST_ROLE")
            .expect("subprocess role output is configured");
        let deadline = Instant::now() + Duration::from_secs(2);
        while !std::path::Path::new(&start).exists() {
            assert!(Instant::now() < deadline, "start marker timed out");
            thread::sleep(Duration::from_millis(5));
        }

        let coordinator = UnixInstanceCoordinator::new(
            scope(std::path::Path::new(&state), ReleaseChannel::Stable),
            control,
        )
        .expect("create subprocess coordinator");
        match coordinator
            .acquire(Duration::from_secs(2))
            .expect("acquire subprocess role")
        {
            InstanceAcquisition::Primary(primary) => {
                std::fs::write(role, "primary").expect("record primary role");
                let mut stream = primary.accept().expect("accept subprocess secondary");
                let mut byte = [0_u8; 1];
                stream
                    .read_exact(&mut byte)
                    .expect("read subprocess activation");
                assert_eq!(byte, [0x5a]);
            }
            InstanceAcquisition::Secondary(mut stream) => {
                std::fs::write(role, "secondary").expect("record secondary role");
                stream
                    .write_all(&[0x5a])
                    .expect("send subprocess activation");
            }
        }
    }

    #[test]
    fn secondary_retries_while_primary_listener_starts() {
        let sandbox = sandbox();
        let coordinator = Arc::new(
            UnixInstanceCoordinator::new(
                scope(&sandbox.path().join("state"), ReleaseChannel::Stable),
                sandbox.path().join("control"),
            )
            .expect("create coordinator"),
        );
        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(coordinator.lock_path())
            .expect("open lock");
        lock.lock_exclusive().expect("hold primary lock");

        let secondary_coordinator = Arc::clone(&coordinator);
        let secondary = thread::spawn(move || {
            match secondary_coordinator
                .acquire(Duration::from_millis(500))
                .expect("secondary connects after listener startup")
            {
                InstanceAcquisition::Secondary(mut stream) => {
                    stream.write_all(&[0x59]).expect("send activation byte");
                }
                InstanceAcquisition::Primary(_) => panic!("secondary promoted while lock held"),
            }
        });

        thread::sleep(Duration::from_millis(50));
        let listener = UnixListener::bind(coordinator.endpoint()).expect("start primary listener");
        let (mut stream, _) = listener.accept().expect("accept retried secondary");
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).expect("read activation byte");
        assert_eq!(byte, [0x59]);
        secondary.join().expect("secondary completed");
        drop(listener);
        std::fs::remove_file(coordinator.endpoint()).expect("clean listener endpoint");
        fs2::FileExt::unlock(&lock).expect("unlock fixture");
    }

    #[test]
    fn only_lock_holder_recovers_stale_socket() {
        let sandbox = sandbox();
        let scope = scope(&sandbox.path().join("state"), ReleaseChannel::Stable);
        let coordinator = UnixInstanceCoordinator::new(scope, sandbox.path().join("control"))
            .expect("create coordinator");
        let stale = UnixListener::bind(coordinator.endpoint()).expect("bind stale socket");
        drop(stale);
        let stale_identity = std::fs::metadata(coordinator.endpoint())
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .expect("inspect stale endpoint");

        let lock = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(coordinator.lock_path())
            .expect("open lock");
        lock.lock_exclusive().expect("hold primary lock");
        match coordinator.acquire(Duration::from_millis(40)) {
            Ok(InstanceAcquisition::Secondary(stream)) => drop(stream),
            Err(error) => assert_eq!(error.kind(), io::ErrorKind::TimedOut),
            Ok(InstanceAcquisition::Primary(_)) => {
                panic!("non-holder promoted while the primary lock was held")
            }
        }
        let after = std::fs::metadata(coordinator.endpoint())
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .expect("stale endpoint was preserved");
        assert_eq!(after, stale_identity);
        fs2::FileExt::unlock(&lock).expect("unlock fixture");

        let primary = match coordinator
            .acquire(Duration::from_millis(40))
            .expect("lock holder recovers stale socket")
        {
            InstanceAcquisition::Primary(primary) => primary,
            InstanceAcquisition::Secondary(_) => panic!("expected primary"),
        };
        let client =
            UnixStream::connect(primary.endpoint()).expect("connect to recovered endpoint");
        let server = primary.accept().expect("accept recovered connection");
        drop((client, server));
    }

    #[test]
    fn primary_drop_does_not_delete_replacement_endpoint() {
        let sandbox = sandbox();
        let scope = scope(&sandbox.path().join("state"), ReleaseChannel::Stable);
        let coordinator = UnixInstanceCoordinator::new(scope, sandbox.path().join("control"))
            .expect("create coordinator");
        let primary = match coordinator
            .acquire(Duration::from_millis(40))
            .expect("acquire primary")
        {
            InstanceAcquisition::Primary(primary) => primary,
            InstanceAcquisition::Secondary(_) => panic!("expected primary"),
        };
        std::fs::remove_file(primary.endpoint()).expect("unlink original endpoint");
        let replacement =
            UnixListener::bind(primary.endpoint()).expect("bind replacement endpoint");
        let replacement_identity = std::fs::metadata(primary.endpoint())
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .expect("inspect replacement endpoint");

        drop(primary);

        let after = std::fs::metadata(coordinator.endpoint())
            .map(|metadata| (metadata.dev(), metadata.ino()))
            .expect("replacement endpoint survives old lease drop");
        assert_eq!(after, replacement_identity);
        drop(replacement);
        std::fs::remove_file(coordinator.endpoint()).expect("clean replacement endpoint");
    }

    #[test]
    fn scope_paths_are_private_and_endpoint_is_bounded() {
        let sandbox = sandbox();
        let mut long_root = sandbox.path().join("state");
        for index in 0..10 {
            long_root.push(format!("very-long-state-root-segment-{index}"));
        }
        let scope = scope(&long_root, ReleaseChannel::Development);
        let state_directory = scope.state_paths().state_directory().to_owned();
        let control_root = sandbox.path().join("control");
        std::fs::create_dir_all(&state_directory).expect("precreate state directory");
        std::fs::create_dir_all(&control_root).expect("precreate control directory");
        std::fs::set_permissions(&state_directory, std::fs::Permissions::from_mode(0o777))
            .expect("make state directory permissive");
        std::fs::set_permissions(&control_root, std::fs::Permissions::from_mode(0o777))
            .expect("make control directory permissive");
        let coordinator =
            UnixInstanceCoordinator::new(scope, &control_root).expect("create coordinator");
        assert_eq!(
            std::fs::metadata(coordinator.control_root())
                .expect("inspect control root")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(coordinator.state_directory())
                .expect("inspect state directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            coordinator
                .endpoint()
                .file_name()
                .expect("endpoint leaf")
                .as_encoded_bytes()
                .len(),
            43
        );
        let primary = match coordinator
            .acquire(Duration::from_millis(40))
            .expect("acquire primary")
        {
            InstanceAcquisition::Primary(primary) => primary,
            InstanceAcquisition::Secondary(_) => panic!("expected primary"),
        };
        assert_eq!(
            std::fs::metadata(coordinator.lock_path())
                .expect("inspect lock file")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(primary.endpoint())
                .expect("inspect endpoint")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn long_control_root_is_rejected_before_socket_binding() {
        let sandbox = sandbox();
        let mut control_root = sandbox.path().join("control");
        for index in 0..6 {
            control_root.push(format!("long-control-segment-{index}"));
        }
        let error = UnixInstanceCoordinator::new(
            scope(&sandbox.path().join("state"), ReleaseChannel::Stable),
            control_root,
        )
        .expect_err("long Unix control path must fail before bind");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("socket path"));
    }

    #[test]
    fn preplanted_lock_symlink_is_rejected_without_mutating_target() {
        use std::os::unix::fs::symlink;

        let sandbox = sandbox();
        let coordinator = UnixInstanceCoordinator::new(
            scope(&sandbox.path().join("state"), ReleaseChannel::Stable),
            sandbox.path().join("control"),
        )
        .expect("create coordinator");
        let target = sandbox.path().join("do-not-touch");
        std::fs::write(&target, b"sentinel").expect("write target");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))
            .expect("set target mode");
        symlink(&target, coordinator.lock_path()).expect("preplant lock symlink");

        let error = coordinator
            .acquire(Duration::from_millis(40))
            .expect_err("lock symlink must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(&target).unwrap(), b"sentinel");
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert!(
            std::fs::symlink_metadata(coordinator.lock_path())
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn multiply_linked_lock_file_is_rejected_without_changing_its_mode() {
        let sandbox = sandbox();
        let coordinator = UnixInstanceCoordinator::new(
            scope(&sandbox.path().join("state"), ReleaseChannel::Stable),
            sandbox.path().join("control"),
        )
        .expect("create coordinator");
        std::fs::write(coordinator.lock_path(), b"preplanted").expect("preplant lock file");
        std::fs::set_permissions(
            coordinator.lock_path(),
            std::fs::Permissions::from_mode(0o644),
        )
        .expect("set preplanted mode");
        std::fs::hard_link(
            coordinator.lock_path(),
            sandbox.path().join("second-lock-link"),
        )
        .expect("create second link");

        let error = coordinator
            .acquire(Duration::from_millis(40))
            .expect_err("multiply linked lock must be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("exactly one filesystem link"));
        assert_eq!(
            std::fs::metadata(coordinator.lock_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o644
        );
    }

    #[test]
    fn lock_path_is_revalidated_after_authority_is_acquired() {
        let sandbox = sandbox();
        let coordinator = UnixInstanceCoordinator::new(
            scope(&sandbox.path().join("state"), ReleaseChannel::Stable),
            sandbox.path().join("control"),
        )
        .expect("create coordinator");
        let lock = coordinator.open_lock_file().expect("open lock");
        lock.try_lock_exclusive()
            .expect("acquire fixture authority");
        let displaced = sandbox.path().join("displaced-lock");
        std::fs::rename(coordinator.lock_path(), &displaced).expect("rename locked inode");
        std::fs::write(coordinator.lock_path(), b"replacement").expect("install replacement lock");

        let error = coordinator
            .validate_lock_file(&lock)
            .expect_err("path replacement after open must fail closed");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("path changed"));
        assert!(!coordinator.endpoint().exists());
    }

    #[test]
    fn separate_roots_and_channels_hold_independent_primary_leases() {
        let sandbox = sandbox();
        let shared_root = sandbox.path().join("shared-state");
        let stable = UnixInstanceCoordinator::new(
            scope(&shared_root, ReleaseChannel::Stable),
            sandbox.path().join("control"),
        )
        .expect("create stable coordinator");
        let preview = UnixInstanceCoordinator::new(
            scope(&shared_root, ReleaseChannel::Preview),
            sandbox.path().join("control"),
        )
        .expect("create preview coordinator");
        let other_root = UnixInstanceCoordinator::new(
            scope(&sandbox.path().join("other-state"), ReleaseChannel::Stable),
            sandbox.path().join("control"),
        )
        .expect("create other-root coordinator");

        let mut primaries = Vec::new();
        for coordinator in [&stable, &preview, &other_root] {
            match coordinator
                .acquire(Duration::from_millis(40))
                .expect("acquire independent primary")
            {
                InstanceAcquisition::Primary(primary) => primaries.push(primary),
                InstanceAcquisition::Secondary(_) => panic!("independent scope collided"),
            }
        }
        assert_eq!(primaries.len(), 3);
    }
}
