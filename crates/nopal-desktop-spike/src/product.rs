//! Shared renderer-neutral composition entry for the native Nopal Field.

use std::time::Duration;

use nopal_native_lifecycle::application::{
    CoreFieldSnapshotSource, NativeApplicationStart, NativeApplicationStartError,
    NativeRestorePreferenceSource, PrimaryStartupRecovery, ResolvedNativeApplicationHostFactory,
    start_native_application,
};
use nopal_native_lifecycle::instance::InstancePlatform;
use nopal_native_lifecycle::state_root::NativeInstanceScope;
use nopal_native_lifecycle::supervisor::SecondaryActivationForwarder;

/// Complete renderer-neutral dependencies for launching one native Field product.
///
/// The future renderer adapter and headless acceptance tests both enter through
/// this type, keeping singleton, restore, Core, recovery, and host ordering in
/// one production composition boundary.
pub struct NativeFieldProduct<P, O, R, C, F> {
    scope: NativeInstanceScope,
    platform: P,
    recovery: O,
    restore: R,
    core: C,
    host_factory: F,
    secondary_connect_timeout: Duration,
}

impl<P, O, R, C, F> NativeFieldProduct<P, O, R, C, F> {
    /// Creates a product composition without performing startup work.
    pub fn new(
        scope: NativeInstanceScope,
        platform: P,
        recovery: O,
        restore: R,
        core: C,
        host_factory: F,
        secondary_connect_timeout: Duration,
    ) -> Self {
        Self {
            scope,
            platform,
            recovery,
            restore,
            core,
            host_factory,
            secondary_connect_timeout,
        }
    }

    /// Returns the exact state-root and release-channel identity to be launched.
    pub fn scope(&self) -> &NativeInstanceScope {
        &self.scope
    }
}

impl<P, O, R, C, F> NativeFieldProduct<P, O, R, C, F>
where
    P: InstancePlatform,
    P::Secondary: SecondaryActivationForwarder,
    O: PrimaryStartupRecovery,
    R: NativeRestorePreferenceSource,
    C: CoreFieldSnapshotSource,
    F: ResolvedNativeApplicationHostFactory,
{
    /// Launches the primary host or forwards activation to the existing primary.
    pub fn launch(
        &mut self,
    ) -> Result<NativeApplicationStart<P::Primary, F::Host>, NativeApplicationStartError> {
        start_native_application(
            &self.scope,
            &self.platform,
            &mut self.recovery,
            &self.restore,
            &self.core,
            &self.host_factory,
            self.secondary_connect_timeout,
        )
    }
}
