use std::collections::BTreeMap;
use std::fmt;

use crate::identifier::ProfileName;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A nonnegative byte count, never a filesystem block count.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Bytes(u64);

impl Bytes {
    pub const fn new(bytes: u64) -> Self {
        Self(bytes)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub(super) fn add(self, other: Self) -> std::io::Result<Self> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or_else(|| std::io::Error::other("generated Cargo byte accounting overflowed"))
    }
}

impl fmt::Display for Bytes {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Generated worker storage admission; pressure never overrides lifetime protection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Policy {
    pub max_bytes: Bytes,
    pub reservation_bytes: Bytes,
    pub profile_reservations: BTreeMap<ProfileName, Bytes>,
    pub min_free_bytes: Bytes,
    pub max_age_secs: u64,
}

impl Default for Policy {
    fn default() -> Self {
        const GIB: u64 = 1024 * 1024 * 1024;
        Self {
            max_bytes: Bytes::new(256 * GIB),
            reservation_bytes: Bytes::new(GIB / 2),
            profile_reservations: BTreeMap::new(),
            min_free_bytes: Bytes::new(16 * GIB),
            max_age_secs: 7 * 24 * 60 * 60,
        }
    }
}

/// One request estimate paired with the unchanged shared storage policy.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    pub(in crate::workspace::cargo) policy: &'a Policy,
    pub(in crate::workspace::cargo) reservation: Bytes,
}

impl Policy {
    pub fn request(&self, profile: &ProfileName) -> Request<'_> {
        Request {
            policy: self,
            reservation: self
                .profile_reservations
                .get(profile)
                .copied()
                .unwrap_or(self.reservation_bytes),
        }
    }

    pub const fn default_request(&self) -> Request<'_> {
        Request {
            policy: self,
            reservation: self.reservation_bytes,
        }
    }

    pub(super) fn unknown_reservation(&self) -> Bytes {
        self.profile_reservations
            .values()
            .copied()
            .fold(self.reservation_bytes, std::cmp::max)
    }

    pub(super) const fn max_age(&self) -> Duration {
        Duration::from_secs(self.max_age_secs)
    }
}
