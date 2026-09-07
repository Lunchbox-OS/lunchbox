//! The `Store` methods this wire carries, written down once.
//!
//! Three places need the same list: the [`StateRequest`](crate::StateRequest)
//! variant, the client method that builds it, and the server arm that turns it
//! back into a call. Keeping three hand-written copies in step was a class of
//! bug the compiler could not see — a variant whose arm called the *wrong*
//! store method typechecks perfectly, and only
//! `every_remaining_method_is_wired_to_the_right_store_call` stood between that
//! and a device silently writing the wrong row.
//!
//! So the list lives here and the three sites are generated from it. This is
//! the X-macro shape: [`with_store_methods`] takes the name of another macro
//! and hands it the table, so each site stays in its own file and none of them
//! restates the list.
//!
//! ## Why not derive it from the trait itself
//!
//! A proc macro on `trait Store` would remove the last copy, and it would put
//! this protocol in `shepherd-store` — a crate that has no business knowing
//! there is a socket. The list here is checked against the real trait anyway,
//! by the compiler: the generated `impl Store for RemoteStore` does not compile
//! if a method is missing, renamed, or has different types.
//!
//! ## Reading an entry
//!
//! ```text
//! GetUsage => get_usage(entry_id: rf EntryId, day: val NaiveDate) -> Duration;
//! ^variant    ^method    ^field     ^mode ^type                     ^returns
//! ```
//!
//! The mode says how an argument crosses the wire, which is the only part that
//! is not mechanical:
//!
//! | mode | the trait takes | the wire carries | client sends | server passes |
//! | --- | --- | --- | --- | --- |
//! | `val` | `T` | `T` | `x` | `x` |
//! | `rf` | `&T` | `T` | `x.clone()` | `&x` |
//! | `st` | `&str` | `String` | `x.to_string()` | `&x` |
//! | `bx` | `T` | `Box<T>` | `Box::new(x)` | `*x` |
//! | `rbx` | `&T` | `Box<T>` | `Box::new(x.clone())` | `&x` |
//!
//! The two boxed modes exist for `clippy::large_enum_variant`: an `AuditEvent`,
//! a `StateSnapshot` and an `AudioOutput` are big enough that carrying them
//! inline would set the size of every other variant too.

/// The wire type of one argument.
macro_rules! wire_ty {
    (val $t:ty) => { $t };
    (rf $t:ty) => { $t };
    (st) => { String };
    (bx $t:ty) => { Box<$t> };
    (rbx $t:ty) => { Box<$t> };
}

/// The type the *trait* uses for one argument.
macro_rules! trait_ty {
    (val $t:ty) => {
        $t
    };
    (rf $t:ty) => {
        &$t
    };
    (st) => {
        &str
    };
    (bx $t:ty) => {
        $t
    };
    (rbx $t:ty) => {
        &$t
    };
}

/// Turning a trait argument into a wire field, on the client.
macro_rules! wire_send {
    (val $n:ident) => {
        $n
    };
    (rf $n:ident) => {
        $n.clone()
    };
    (st $n:ident) => {
        $n.to_string()
    };
    (bx $n:ident) => {
        Box::new($n)
    };
    (rbx $n:ident) => {
        Box::new($n.clone())
    };
}

/// Turning a wire field back into a trait argument, on the server.
macro_rules! wire_recv {
    (val $n:ident) => {
        $n
    };
    (rf $n:ident) => {
        &$n
    };
    (st $n:ident) => {
        &$n
    };
    (bx $n:ident) => {
        *$n
    };
    (rbx $n:ident) => {
        &$n
    };
}

pub(crate) use {trait_ty, wire_recv, wire_send, wire_ty};

/// Hand the table to `$callback`.
///
/// Every entry is `Variant => method(field: mode Type, ...) -> Returns;`, and
/// the sections match the ones in `shepherd_store::Store` so the two read the
/// same way side by side.
macro_rules! with_store_methods {
    ($callback:ident) => {
        $callback! {
            // Audit log
            AppendAudit => append_audit(event: bx AuditEvent) -> ();
            GetRecentAudits => get_recent_audits(limit: val usize) -> Vec<AuditEvent>;

            // Usage accounting
            GetUsage => get_usage(entry_id: rf EntryId, day: val NaiveDate) -> Duration;
            AddUsage => add_usage(
                entry_id: rf EntryId, day: val NaiveDate, duration: val Duration
            ) -> ();
            GetUsageRange => get_usage_range(
                entry_id: rf EntryId, from: val NaiveDate, to: val NaiveDate
            ) -> Vec<(NaiveDate, Duration)>;
            GetAllUsageForDate => get_all_usage_for_date(
                date: val NaiveDate
            ) -> Vec<(EntryId, Duration)>;

            // Token balances (issue #8)
            GetTokenState => get_token_state(
                subject: rf LimitSubject, day: val NaiveDate, carry_over: val bool
            ) -> TokenState;
            AdjustTokenBalance => adjust_token_balance(
                subject: rf LimitSubject,
                day: val NaiveDate,
                carry_over: val bool,
                delta_secs: val i64
            ) -> TokenState;
            SetTokenRatchet => set_token_ratchet(
                subject: rf LimitSubject, day: val NaiveDate, carry_over: val bool
            ) -> ();

            // Cooldowns
            GetCooldownUntil => get_cooldown_until(
                subject: rf LimitSubject
            ) -> Option<DateTime<Local>>;
            SetCooldownUntil => set_cooldown_until(
                subject: rf LimitSubject, until: val DateTime<Local>
            ) -> ();
            ClearCooldown => clear_cooldown(subject: rf LimitSubject) -> ();

            // Session snapshot and health
            LoadSnapshot => load_snapshot() -> Option<StateSnapshot>;
            SaveSnapshot => save_snapshot(snapshot: rbx StateSnapshot) -> ();
            // `is_healthy` is deliberately absent: it returns a bare `bool`
            // rather than a `StoreResult`, so it has nowhere to report a broken
            // connection and cannot share the generated shape. It is written
            // out at all three sites, and it is the only one that is.

            // Daily overrides
            GetDailyOverride => get_daily_override(
                subject: rf LimitSubject, date: val NaiveDate
            ) -> Option<DailyOverride>;
            UpsertDailyOverride => upsert_daily_override(
                subject: rf LimitSubject,
                date: val NaiveDate,
                availability: val Option<bool>,
                quota_delta_seconds: val Option<i64>
            ) -> DailyOverride;
            ClearDailyOverride => clear_daily_override(
                subject: rf LimitSubject, date: val NaiveDate
            ) -> bool;
            ListDailyOverrides => list_daily_overrides(
                date: val NaiveDate
            ) -> Vec<DailyOverride>;

            // Audio outputs
            RecordAudioOutputSeen => record_audio_output_seen(output: rbx AudioOutput) -> ();
            SetAudioOutputLimits => set_audio_output_limits(
                output_key: st, max_volume: val Option<u8>, min_volume: val Option<u8>
            ) -> bool;
            GetAudioOutput => get_audio_output(output_key: st) -> Option<AudioOutputRecord>;
            ListAudioOutputs => list_audio_outputs() -> Vec<AudioOutputRecord>;
            ForgetAudioOutput => forget_audio_output(output_key: st) -> bool;

            // Settings
            GetSetting => get_setting(key: st) -> Option<String>;
            SetSetting => set_setting(key: st, value: st) -> ();
        }
    };
}

pub(crate) use with_store_methods;
