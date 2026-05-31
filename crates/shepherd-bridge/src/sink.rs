//! The sink trait the bridge main loops drive.

use anyhow::Result;

use crate::event::OutputEvent;

/// An output backend that turns [`OutputEvent`]s into real synthetic input.
///
/// The driving loop calls [`dispatch`](OutputSink::dispatch) once per event,
/// [`frame`](OutputSink::frame) to mark a logical batch boundary (one input
/// "report"), and [`flush`](OutputSink::flush) to ensure pending events have
/// been delivered. `time` is a millisecond timestamp; backends that don't need
/// it (uinput stamps its own) may ignore it.
pub trait OutputSink {
    /// Queue one event for the current frame.
    fn dispatch(&mut self, event: OutputEvent, time: u32);

    /// Close the current batch of events into one input report.
    fn frame(&mut self);

    /// Ensure all queued events have been delivered to the OS.
    fn flush(&mut self) -> Result<()>;
}
