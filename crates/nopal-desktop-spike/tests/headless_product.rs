#![cfg(unix)]

use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nopal_desktop_spike::core_source::CliCoreFieldSnapshotSource;
use nopal_desktop_spike::product::NativeFieldProduct;
use nopal_feed_client::field::FieldSnapshot;
use nopal_native_lifecycle::activation::ActivationDeadline;
use nopal_native_lifecycle::application::{
    NativeApplicationStart, OwnedResourceRecoveryReport, PrimaryStartupRecovery,
    ResolvedNativeApplicationHostFactory, RestorePreferenceNotice, ScopedRestorePreferenceSource,
};
use nopal_native_lifecycle::current_field::CurrentCoreFieldAuthority;
use nopal_native_lifecycle::instance::{InstanceAcquisition, InstancePlatform};
use nopal_native_lifecycle::preferences::{
    RestorePreferenceStore, RestorePreferenceUpdate, RestorePreferenceWriteOutcome,
};
use nopal_native_lifecycle::reconcile::{ExactSessionSelection, RestoreResolution};
use nopal_native_lifecycle::recovery::RecoveryReconcileOutcome;
use nopal_native_lifecycle::state_root::{CanonicalStateRoot, NativeInstanceScope, ReleaseChannel};
use nopal_native_lifecycle::supervisor::{
    NativeApplicationAck, NativeApplicationHost, NativeApplicationUnavailable,
    SecondaryActivationForwarder,
};

struct PrimaryOnlyPlatform;

impl InstancePlatform for PrimaryOnlyPlatform {
    type Primary = ();
    type Secondary = UnreachableSecondary;

    fn acquire(
        &self,
        _secondary_connect_timeout: Duration,
    ) -> io::Result<InstanceAcquisition<Self::Primary, Self::Secondary>> {
        Ok(InstanceAcquisition::Primary(()))
    }
}

struct UnreachableSecondary;

impl SecondaryActivationForwarder for UnreachableSecondary {
    fn forward(&self) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        Err(NativeApplicationUnavailable::new(
            "primary-only acceptance platform cannot forward",
        ))
    }
}

struct EmptyRecovery;

impl PrimaryStartupRecovery for EmptyRecovery {
    type Error = io::Error;

    fn reconcile_for_scope(
        &mut self,
        _scope: &NativeInstanceScope,
    ) -> Result<RecoveryReconcileOutcome, Self::Error> {
        Ok(RecoveryReconcileOutcome::Empty)
    }
}

#[derive(Clone)]
struct HeadlessFactory {
    restores: Arc<Mutex<Vec<RestoreResolution>>>,
}

struct HeadlessHost {
    _current_field: CurrentCoreFieldAuthority,
}

impl ResolvedNativeApplicationHostFactory for HeadlessFactory {
    type Host = HeadlessHost;

    fn create_host(
        &self,
        _field: &FieldSnapshot,
        restore: &RestoreResolution,
        _recovery_report: &OwnedResourceRecoveryReport,
        _preference_notice: Option<&RestorePreferenceNotice>,
        current_field: CurrentCoreFieldAuthority,
    ) -> Result<Self::Host, NativeApplicationUnavailable> {
        self.restores
            .lock()
            .map_err(|_| NativeApplicationUnavailable::new("restore recorder was poisoned"))?
            .push(restore.clone());
        Ok(HeadlessHost {
            _current_field: current_field,
        })
    }
}

impl NativeApplicationHost for HeadlessHost {
    fn activate(
        &mut self,
        _deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        Ok(NativeApplicationAck::Focused)
    }
}

#[test]
fn production_core_restore_and_headless_host_share_the_product_entry() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    let sandbox = tempfile::tempdir().expect("create headless product sandbox");
    let scope = NativeInstanceScope::new(
        CanonicalStateRoot::create(sandbox.path().join("state"))
            .expect("create canonical state root"),
        ReleaseChannel::Development,
    );
    let intended = ExactSessionSelection::new("plot-a", "session-a");
    let store = RestorePreferenceStore::new(scope.state_paths().restore_preference());
    assert_eq!(
        store
            .write(&RestorePreferenceUpdate::select(intended.clone()))
            .expect("persist exact restore intent"),
        RestorePreferenceWriteOutcome::Written
    );

    let executable = sandbox.path().join("fake-nopal");
    fs::write(
        &executable,
        concat!(
            "#!/bin/sh\n",
            "printf '%s' '",
            "{\"kind\":\"nopal.field/v1\",\"plots\":[",
            "{\"kind\":\"nopal.plot/v1\",\"plot_id\":\"plot-a\",",
            "\"sessions\":[{\"session_id\":\"session-a\"}]}],\"entries\":[]}",
            "'\n"
        ),
    )
    .expect("write bounded Core fixture executable");
    let mut permissions = fs::metadata(&executable)
        .expect("fixture executable metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&executable, permissions).expect("make Core fixture executable");

    let restores = Arc::new(Mutex::new(Vec::new()));
    let factory = HeadlessFactory {
        restores: Arc::clone(&restores),
    };
    let mut product = NativeFieldProduct::new(
        scope,
        PrimaryOnlyPlatform,
        EmptyRecovery,
        ScopedRestorePreferenceSource,
        CliCoreFieldSnapshotSource::production(executable),
        factory,
        Duration::from_millis(100),
    );

    let start = product.launch().expect("launch headless native Field");
    let NativeApplicationStart::Primary(application) = start else {
        panic!("primary-only platform unexpectedly forwarded activation");
    };
    assert_eq!(application.field().kind, "nopal.field/v1");
    assert_eq!(
        application.restore_resolution(),
        &RestoreResolution::Exact(intended.clone())
    );
    assert_eq!(
        restores.lock().expect("restore recorder").as_slice(),
        &[RestoreResolution::Exact(intended)]
    );
}
