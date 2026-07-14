//! Platform-neutral instance acquisition contracts.

use std::io;
use std::time::Duration;

/// The outcome of one launch's attempt to join the native application instance.
#[derive(Debug)]
pub enum InstanceAcquisition<P, S> {
    /// This launch owns the operating-system singleton lease.
    Primary(P),
    /// Another launch owns the lease and this launch connected to it.
    Secondary(S),
}

/// Platform boundary for acquiring a primary lease or connecting as a secondary.
pub trait InstancePlatform {
    type Primary;
    type Secondary;

    /// Attempts acquisition once. A launch that observes a held lease must never
    /// promote itself during this call, even if the holder subsequently exits.
    fn acquire(
        &self,
        secondary_connect_timeout: Duration,
    ) -> io::Result<InstanceAcquisition<Self::Primary, Self::Secondary>>;
}
