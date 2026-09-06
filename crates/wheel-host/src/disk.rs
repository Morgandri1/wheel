//! How much room is left on the volume the host writes to.
//!
//! This module exists because of one afternoon. `/data` reached 100%, and sqlite reported it as
//! "disk I/O error ... trying to resize an existing shared-memory segment" while growing a WAL
//! index. The whole team spent ninety minutes on journal modes and filesystem capabilities; the
//! platform's dashboard said 127 MB of 5000 MB, and `df` inside the container said 4.5G of 4.6G.
//! The number that ended it was one `df`, and nothing in the product had ever reported it.
//!
//! So the host measures its own volume, puts the figure on its health check, and refuses to start a
//! sandbox onto a volume with no room in it rather than letting the failure surface as a story
//! about shared memory.

use anyhow::{Context, Result};
use std::path::Path;

const MB: u64 = 1024 * 1024;

/// What a filesystem has left, as the caller who writes to it cares about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    /// Bytes available to an unprivileged writer, which is what every child of the host is.
    pub free_bytes: u64,
    pub total_bytes: u64,
}

impl Space {
    pub fn free_mb(&self) -> u64 {
        self.free_bytes / MB
    }

    pub fn total_mb(&self) -> u64 {
        self.total_bytes / MB
    }

    /// Rounded up, so 99.4% full reads as 100 rather than as room to spare.
    pub fn used_percent(&self) -> u64 {
        if self.total_bytes == 0 {
            return 100;
        }
        let used = self.total_bytes.saturating_sub(self.free_bytes);
        (used * 100).div_ceil(self.total_bytes).min(100)
    }
}

impl std::fmt::Display for Space {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} MB free of {} MB ({}% used)",
            self.free_mb(),
            self.total_mb(),
            self.used_percent()
        )
    }
}

/// Measure the filesystem holding `path`.
// statvfs field widths differ by platform: 64-bit on Linux, 32-bit for the block counts on macOS.
// `u64::from` is therefore a no-op on one and load-bearing on the other, and clippy objects to
// whichever spelling is not needed where it is running. Truncating a block count would report a
// full volume as an empty one, so the conversions stay and the lint is answered here instead.
#[allow(clippy::useless_conversion)]
pub fn space(path: impl AsRef<Path>) -> Result<Space> {
    let path = path.as_ref();
    let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .with_context(|| format!("{} is not a path we can measure", path.display()))?;

    // SAFETY: `stat` is written only by statvfs, and only on success; `c_path` is a valid C string
    // that outlives the call.
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    let rc = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("measuring the volume at {}", path.display()));
    }
    let stat = unsafe { stat.assume_init() };

    // f_frsize is the fragment size the block counts are in; some platforms leave it zero and mean
    // f_bsize. Getting this wrong scales the answer by a factor of thousands, silently.
    let unit = if stat.f_frsize > 0 {
        u64::from(stat.f_frsize)
    } else {
        u64::from(stat.f_bsize)
    };
    Ok(Space {
        // f_bavail, not f_bfree: the host's children run as unprivileged project uids and cannot
        // touch the reserve, so counting it would promise room that nothing may use.
        free_bytes: u64::from(stat.f_bavail).saturating_mul(unit),
        total_bytes: u64::from(stat.f_blocks).saturating_mul(unit),
    })
}

/// Is there room to run something that will write?
///
/// The floor is not what a build needs — it is what one engine needs to open its database and take
/// a message without corrupting it. Below that, refuse and say so: an engine started onto a full
/// volume fails on its first write, in the vocabulary of whatever it was doing at the time.
pub fn check_room(path: impl AsRef<Path>, floor_mb: u64) -> Result<()> {
    let space = space(&path)?;
    if space.free_mb() < floor_mb {
        anyhow::bail!("the volume is full enough to break a database: {space}, under the {floor_mb} MB a project needs to start");
    }
    if is_filling(&space) {
        tracing::warn!(
            free_mb = space.free_mb(),
            used_percent = space.used_percent(),
            floor_mb,
            "the volume is filling; starts are refused below the floor"
        );
    }
    Ok(())
}

/// How full is too full to stay quiet about.
///
/// The floor is where a start is refused, and by then the disk is already broken for anything that
/// writes — the first symptom was a sqlite error about shared memory. A warning band exists so the
/// log says "the volume is filling" while there is still room to act: production went from 141 MB
/// to 2.5 GB in an hour on node_modules, twice, which is faster than anyone reads a dashboard.
const WARN_PERCENT: u64 = 85;

/// Separate from the logging so the threshold is testable without capturing a subscriber.
pub fn is_filling(space: &Space) -> bool {
    space.used_percent() >= WARN_PERCENT
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The warning has to arrive while there is still room to act on it, not at the floor.
    #[test]
    fn a_volume_filling_up_is_warned_about_before_it_is_refused() {
        let comfortable = Space {
            free_bytes: 50,
            total_bytes: 100,
        };
        assert!(!is_filling(&comfortable));

        let filling = Space {
            free_bytes: 10,
            total_bytes: 100,
        };
        assert!(is_filling(&filling), "90% full read as comfortable");

        // And the band is above the floor, not at it: a volume that is filling still starts
        // projects, which is the whole point of hearing about it early.
        let dir = std::env::temp_dir();
        assert!(
            check_room(&dir, 1).is_ok(),
            "warning must not become refusing"
        );
    }

    #[test]
    fn a_real_filesystem_measures() {
        let s = space(std::env::temp_dir()).expect("the temp dir is on a filesystem");
        assert!(s.total_bytes > 0, "a volume with no size: {s:?}");
        assert!(
            s.free_bytes <= s.total_bytes,
            "more free than exists: {s:?}"
        );
        assert!(s.used_percent() <= 100);
    }

    #[test]
    fn a_path_that_is_not_there_is_an_error_naming_it() {
        let missing = std::env::temp_dir().join("wheel-no-such-volume-2f7a1c");
        let e = format!("{:#}", space(&missing).unwrap_err());
        assert!(e.contains("wheel-no-such-volume-2f7a1c"), "{e}");
    }

    #[test]
    fn a_volume_with_room_passes_and_one_without_does_not() {
        let dir = std::env::temp_dir();
        check_room(&dir, 0).expect("a floor of zero can always be met");

        let e = format!("{:#}", check_room(&dir, u64::MAX / MB).unwrap_err());
        assert!(
            e.contains("the volume is full"),
            "the refusal has to name the disk: {e}"
        );
    }

    #[test]
    fn nearly_full_does_not_round_down_to_comfortable() {
        // 99.4% used. Rounding this to 99 would be honest; reporting the 0.6% as headroom is how a
        // volume gets to 100% with nobody noticing.
        let s = Space {
            free_bytes: 6 * MB,
            total_bytes: 1000 * MB,
        };
        assert_eq!(s.used_percent(), 100);
        assert_eq!(s.free_mb(), 6);
    }

    #[test]
    fn an_empty_volume_is_not_reported_as_full() {
        let s = Space {
            free_bytes: 1000 * MB,
            total_bytes: 1000 * MB,
        };
        assert_eq!(s.used_percent(), 0);
    }

    /// A zero-sized filesystem is not a volume with infinite headroom.
    #[test]
    fn a_volume_of_no_size_reads_as_full() {
        let s = Space {
            free_bytes: 0,
            total_bytes: 0,
        };
        assert_eq!(s.used_percent(), 100);
    }
}
