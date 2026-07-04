//! The shared "serve fresh, else fetch, else fall back to stale" cache policy.
//!
//! Several caches across the media front-ends follow the same rule: a fresh
//! cached value is used directly; otherwise a live fetch is attempted; if that
//! fetch fails but a *stale* cached value exists, the stale value is served as
//! an offline fallback (so a device offline past the TTL keeps working). This
//! module expresses that decision once, generic over the cached value type, so
//! the poster-bytes cache and the playlist-metadata cache share it rather than
//! each re-deriving the branch.
//!
//! The caller owns all I/O: it decides freshness (from an mtime/timestamp),
//! supplies the `fetch`, and — on a [`Resolution::Fetched`] — writes the value
//! back. This keeps the module free of filesystem and network specifics.

/// Whether a cached value is still within its freshness window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Within the TTL — safe to use directly.
    Fresh,
    /// Past the TTL — refresh if possible, but usable as an offline fallback.
    Stale,
}

/// The outcome of resolving a cached value against a live fetch. The caller
/// logs each case as it sees fit and writes back on [`Resolution::Fetched`].
pub enum Resolution<T> {
    /// Fresh cache hit — no fetch was attempted.
    Fresh(T),
    /// A live fetch succeeded; the caller should persist this value.
    Fetched(T),
    /// The fetch failed but a stale value existed; serving it. Carries the error.
    Stale(T, String),
    /// No value: a miss (or stale-but-discarded) plus a failed fetch.
    Miss(String),
}

/// Apply the fresh/fetch/stale-fallback policy.
///
/// `cached` is the current cache state, if any, paired with its [`Freshness`].
/// `fetch` is only invoked on a miss or a stale entry, and returns `Ok(value)`
/// or `Err(message)`.
pub fn resolve<T>(
    cached: Option<(T, Freshness)>,
    fetch: impl FnOnce() -> Result<T, String>,
) -> Resolution<T> {
    if let Some((value, Freshness::Fresh)) = cached {
        return Resolution::Fresh(value);
    }
    let stale = cached.map(|(value, _)| value);
    match fetch() {
        Ok(value) => Resolution::Fetched(value),
        Err(e) => match stale {
            Some(value) => Resolution::Stale(value, e),
            None => Resolution::Miss(e),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_is_served_without_fetching() {
        let r = resolve(Some((1, Freshness::Fresh)), || panic!("no fetch on fresh"));
        assert!(matches!(r, Resolution::Fresh(1)));
    }

    #[test]
    fn stale_prefers_a_successful_fetch() {
        let r = resolve(Some((1, Freshness::Stale)), || Ok(2));
        assert!(matches!(r, Resolution::Fetched(2)));
    }

    #[test]
    fn stale_falls_back_when_fetch_fails() {
        let r = resolve(Some((1, Freshness::Stale)), || Err("offline".into()));
        assert!(matches!(r, Resolution::Stale(1, _)));
    }

    #[test]
    fn miss_with_failed_fetch_is_a_miss() {
        let r = resolve(None::<(i32, Freshness)>, || Err("offline".into()));
        assert!(matches!(r, Resolution::Miss(_)));
    }
}
