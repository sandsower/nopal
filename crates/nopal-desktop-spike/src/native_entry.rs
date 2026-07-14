//! Production native Field composition and eframe process entry.

#[cfg(unix)]
mod unix {
    use std::ffi::OsString;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    use nopal_native_lifecycle::activation::{ActivationDeadline, ActivationRequestValidator};
    use nopal_native_lifecycle::application::{
        NativeApplicationStart, NativePrimaryApplication, ScopedOwnedResourceRecovery,
        ScopedRestorePreferenceSource,
    };
    use nopal_native_lifecycle::instance::{InstanceAcquisition, InstancePlatform};
    use nopal_native_lifecycle::platform::unix::{UnixInstanceCoordinator, UnixPrimaryLease};
    use nopal_native_lifecycle::recovery::{
        ExactRecoveryAdapter, FilesystemRecoveryRecipe, RecoveryAdapterError, RecoveryDeadline,
        RecoveryDisposition, VerifiedProcessRecoveryRecipe,
    };
    use nopal_native_lifecycle::state_root::{
        CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
    };
    use nopal_native_lifecycle::supervisor::{
        NativeApplicationAck, NativeApplicationUnavailable, SerializedPrimaryActivation,
    };
    use nopal_native_lifecycle::transport::{UnixActivationForwarder, serve_unix_activation};

    use crate::core_source::CliCoreFieldSnapshotSource;
    use crate::eframe_host::{EframeHostFactory, EframeNativeHost, EframeUiBridge};
    use crate::eframe_shell::EframeFieldApp;
    use crate::product::NativeFieldProduct;
    use crate::session_runtime::drain_native_background_tasks;

    const SECONDARY_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
    const ACTIVATION_TIMEOUT: Duration = Duration::from_secs(3);
    const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(8);

    /// Runs the native Field or activates the already-resident instance.
    pub fn run() -> Result<(), String> {
        let arguments = std::env::args_os().collect::<Vec<_>>();
        let state_dir = state_dir(&arguments)?;
        let release_channel = release_channel()?;
        let (scope, core_state_dir) = scope_and_core_state_dir(&state_dir, release_channel)?;
        let scope_fingerprint = scope.fingerprint().to_owned();
        let coordinator = UnixInstanceCoordinator::with_default_control_root(scope.clone())
            .map_err(|error| format!("cannot prepare native instance coordination: {error}"))?;
        let platform = UnixEframePlatform {
            coordinator,
            scope_fingerprint: scope.fingerprint().to_owned(),
        };
        let recovery = ScopedOwnedResourceRecovery::new(FailClosedRecoveryAdapter);
        let core =
            CliCoreFieldSnapshotSource::production(nopal_binary()).with_state_dir(core_state_dir);
        let host_factory = EframeHostFactory::with_model_recents(
            scope.state_paths().model_recents().to_path_buf(),
        );
        let bridge = host_factory.bridge();
        let mut product = NativeFieldProduct::new(
            scope,
            platform,
            recovery,
            ScopedRestorePreferenceSource,
            core,
            host_factory.clone(),
            SECONDARY_CONNECT_TIMEOUT,
        );

        match product.launch().map_err(|error| error.to_string())? {
            NativeApplicationStart::Secondary { .. } => Ok(()),
            NativeApplicationStart::Primary(application) => {
                let seed = host_factory.take_seed().ok_or_else(|| {
                    "native lifecycle started without preparing an eframe application".to_owned()
                })?;
                run_primary(application, seed, bridge, scope_fingerprint)
            }
        }
    }

    type PrimaryApplication = NativePrimaryApplication<UnixPrimaryLease, EframeNativeHost>;

    fn run_primary(
        application: Box<PrimaryApplication>,
        seed: crate::eframe_host::EframeAppSeed,
        bridge: EframeUiBridge,
        scope_fingerprint: String,
    ) -> Result<(), String> {
        let listener = application
            .lease()
            .listener()
            .try_clone()
            .map_err(|error| format!("cannot clone native activation listener: {error}"))?;
        listener
            .set_nonblocking(true)
            .map_err(|error| format!("cannot configure native activation listener: {error}"))?;
        let activation_bridge = bridge.clone();
        let service = Arc::new(SerializedPrimaryActivation::new(
            application,
            activate_primary_application,
        ));
        let activation_thread = thread::Builder::new()
            .name("nopal-native-activation".to_owned())
            .spawn(move || {
                serve_activation_loop(listener, &scope_fingerprint, activation_bridge, service)
            })
            .map_err(|error| format!("cannot start native activation service: {error}"))?;

        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_app_id("nopal-field-native")
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([900.0, 600.0]),
            ..Default::default()
        };
        let app_bridge = bridge.clone();
        let renderer_result = eframe::run_native(
            "Nopal Field",
            options,
            Box::new(move |creation| {
                Ok(Box::new(EframeFieldApp::new(
                    seed,
                    app_bridge,
                    &creation.egui_ctx,
                )))
            }),
        )
        .map_err(|error| format!("native renderer failed: {error}"));
        bridge.shutdown();
        let activation_result = activation_thread
            .join()
            .unwrap_or_else(|_| Err("native activation service panicked".to_owned()));
        let terminal_cleanup_result = drain_native_background_tasks();
        renderer_result
            .and(activation_result)
            .and(terminal_cleanup_result)
    }

    fn activate_primary_application(
        application: &mut Box<PrimaryApplication>,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        application.activate(deadline)
    }

    fn serve_activation_loop(
        listener: std::os::unix::net::UnixListener,
        scope_fingerprint: &str,
        bridge: EframeUiBridge,
        service: Arc<SerializedPrimaryActivation<Box<PrimaryApplication>>>,
    ) -> Result<(), String> {
        let mut validator = ActivationRequestValidator::new(scope_fingerprint)
            .map_err(|error| format!("cannot validate native activation scope: {error}"))?;
        while !bridge.is_shutdown() {
            match listener.accept() {
                Ok((stream, _)) => {
                    if let Err(error) = serve_unix_activation(
                        stream,
                        &mut validator,
                        service.as_ref(),
                        ACTIVATION_TIMEOUT,
                    ) {
                        eprintln!("native activation request failed: {error}");
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(ACCEPT_POLL_INTERVAL);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(format!("native activation listener failed: {error}")),
            }
        }
        Ok(())
    }

    struct UnixEframePlatform {
        coordinator: UnixInstanceCoordinator,
        scope_fingerprint: String,
    }

    impl InstancePlatform for UnixEframePlatform {
        type Primary = UnixPrimaryLease;
        type Secondary = UnixActivationForwarder;

        fn acquire(
            &self,
            secondary_connect_timeout: Duration,
        ) -> io::Result<InstanceAcquisition<Self::Primary, Self::Secondary>> {
            match self.coordinator.acquire(secondary_connect_timeout)? {
                InstanceAcquisition::Primary(lease) => Ok(InstanceAcquisition::Primary(lease)),
                InstanceAcquisition::Secondary(stream) => UnixActivationForwarder::new(
                    stream,
                    &self.scope_fingerprint,
                    ACTIVATION_TIMEOUT,
                )
                .map(InstanceAcquisition::Secondary)
                .map_err(io::Error::other),
            }
        }
    }

    /// The current product owns no durable resources yet. Existing entries block startup
    /// until a platform exact-identity adapter is deliberately selected.
    struct FailClosedRecoveryAdapter;

    impl ExactRecoveryAdapter for FailClosedRecoveryAdapter {
        fn recover_filesystem_exact(
            &mut self,
            _recipe: &FilesystemRecoveryRecipe,
            _deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            Err(RecoveryAdapterError::new(
                "native filesystem recovery requires an exact platform adapter",
            ))
        }

        fn recover_process_exact(
            &mut self,
            _recipe: &VerifiedProcessRecoveryRecipe,
            _deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            Err(RecoveryAdapterError::new(
                "native process recovery requires an exact platform adapter",
            ))
        }
    }

    fn state_dir(arguments: &[OsString]) -> Result<PathBuf, String> {
        let explicit = argument_value(arguments, "--state-dir")?;
        Ok(resolve_state_dir(
            explicit,
            std::env::var_os("NOPAL_STATE_DIR").map(PathBuf::from),
            std::env::var_os("BEISLID_STATE_DIR").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        ))
    }

    fn resolve_state_dir(
        explicit: Option<PathBuf>,
        nopal_state_dir: Option<PathBuf>,
        beislid_state_dir: Option<PathBuf>,
        home: Option<PathBuf>,
    ) -> PathBuf {
        explicit
            .or(nopal_state_dir)
            // Preserve the former environment override for existing launchers,
            // but do not let it outrank Nopal's own Core state-root contract.
            .or(beislid_state_dir)
            .unwrap_or_else(|| {
                home.unwrap_or_else(|| PathBuf::from("."))
                    .join(".local")
                    .join("state")
                    .join("nopal")
            })
    }

    fn argument_value(arguments: &[OsString], flag: &str) -> Result<Option<PathBuf>, String> {
        let mut values = arguments.iter().skip(1);
        let mut result = None;
        while let Some(argument) = values.next() {
            if argument != flag {
                return Err(format!(
                    "unsupported native Field argument: {}",
                    argument.to_string_lossy()
                ));
            }
            let value = values
                .next()
                .map(PathBuf::from)
                .ok_or_else(|| format!("{flag} requires a path"))?;
            if result.replace(value).is_some() {
                return Err(format!("{flag} may be specified only once"));
            }
        }
        Ok(result)
    }

    fn release_channel() -> Result<ReleaseChannel, String> {
        std::env::var("NOPAL_RELEASE_CHANNEL")
            .unwrap_or_else(|_| "development".to_owned())
            .parse()
            .map_err(|error| format!("invalid NOPAL_RELEASE_CHANNEL: {error}"))
    }

    fn scope_and_core_state_dir(
        state_dir: &Path,
        release_channel: ReleaseChannel,
    ) -> Result<(NativeInstanceScope, PathBuf), String> {
        let canonical_state_root = CanonicalStateRoot::create(state_dir)
            .map_err(|error| format!("cannot prepare native state root: {error}"))?;
        let core_state_dir = canonical_state_root.as_path().to_path_buf();
        Ok((
            NativeInstanceScope::new(canonical_state_root, release_channel),
            core_state_dir,
        ))
    }

    fn nopal_binary() -> PathBuf {
        if let Some(binary) = std::env::var_os("NOPAL_BIN") {
            return PathBuf::from(binary);
        }
        std::env::current_exe()
            .ok()
            .and_then(|current| current.parent().map(Path::to_path_buf))
            .map(|parent| parent.join(format!("nopal{}", std::env::consts::EXE_SUFFIX)))
            .filter(|candidate| candidate.is_file())
            .unwrap_or_else(|| PathBuf::from("nopal"))
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::symlink;

        use super::*;

        #[test]
        fn native_arguments_accept_one_state_root_and_reject_ambiguous_input() {
            let accepted = ["nopal-field-native", "--state-dir", "/state"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>();
            assert_eq!(
                argument_value(&accepted, "--state-dir"),
                Ok(Some(PathBuf::from("/state")))
            );

            for rejected in [
                vec!["nopal-field-native", "--unknown"],
                vec!["nopal-field-native", "--state-dir"],
                vec![
                    "nopal-field-native",
                    "--state-dir",
                    "/one",
                    "--state-dir",
                    "/two",
                ],
            ] {
                let rejected = rejected.into_iter().map(OsString::from).collect::<Vec<_>>();
                assert!(argument_value(&rejected, "--state-dir").is_err());
            }
        }

        #[test]
        fn native_state_root_matches_core_nopal_defaults_and_precedence() {
            let home = PathBuf::from("/home/nopal-user");
            let expected_default = home.join(".local").join("state").join("nopal");

            assert_eq!(
                resolve_state_dir(None, None, None, Some(home)),
                expected_default
            );
            assert_eq!(
                resolve_state_dir(
                    None,
                    Some(PathBuf::from("/nopal-state")),
                    Some(PathBuf::from("/legacy-state")),
                    None,
                ),
                PathBuf::from("/nopal-state")
            );
            assert_eq!(
                resolve_state_dir(
                    Some(PathBuf::from("/explicit-state")),
                    Some(PathBuf::from("/nopal-state")),
                    Some(PathBuf::from("/legacy-state")),
                    None,
                ),
                PathBuf::from("/explicit-state")
            );
        }

        #[test]
        fn core_inspection_uses_the_same_canonical_root_as_singleton_identity() {
            let sandbox = tempfile::tempdir().expect("create state-root sandbox");
            let target = sandbox.path().join("state");
            std::fs::create_dir(&target).expect("create target state root");
            let alias = sandbox.path().join("state-alias");
            symlink(&target, &alias).expect("create state-root alias");

            let (scope, core_state_dir) =
                scope_and_core_state_dir(&alias, ReleaseChannel::Development)
                    .expect("canonicalize native scope");
            let expected = std::fs::canonicalize(&target).expect("canonical target state root");

            assert_eq!(scope.state_root().as_path(), expected);
            assert_eq!(core_state_dir, expected);
        }
    }
}

#[cfg(unix)]
pub use unix::run;

#[cfg(not(unix))]
pub fn run() -> Result<(), String> {
    Err("native Field lifecycle is not implemented for this operating system yet".to_owned())
}
