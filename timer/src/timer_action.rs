//! Action enum that can be passed to the timer on each tick to interact with it

/// Various control actions to transition into new states
#[derive(Debug, Copy, Clone)]
pub enum TimerAction {
    /// Either start or pause the current timer
    PlayPause,

    /// Ends the currently blocking timer loop, such that the consuming code
    /// is able to continue
    End,
}
