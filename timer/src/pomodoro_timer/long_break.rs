use std::marker::PhantomData;

use crate::{
    config::PomodoroTimerConfig,
    pomodoro_timer_action::PomodoroTimerAction,
    timer::{Paused, TimerStatus, TimerTickHandler},
    Timer, TimerAction,
};

use super::{
    interval::Interval,
    on_end_handler::OnEndHandler,
    on_tick_handler::{PomodoroActionHandler, PostponeHandlerConfig},
    postponed_long_break::PostponedLongBreak,
    state::{Callbacks, PomodoroState, PomodoroTimer, PomodoroTimerState, ViewState},
};

/// Pomodoro timer state designating a long break
#[derive(Debug, Copy, Clone)]
pub struct LongBreak {}

impl PomodoroState for LongBreak {}

struct LongBreakTickHandler {
    pomodoro_timer: PomodoroTimer<LongBreak>,
}

impl PomodoroActionHandler<LongBreak> for LongBreakTickHandler {
    fn get_timer(&self) -> PomodoroTimer<LongBreak> {
        self.pomodoro_timer.clone()
    }

    fn handle_action(&self, action: PomodoroTimerAction) -> Option<TimerAction> {
        let timer = self.get_timer();

        let PomodoroTimer {
            config,
            callbacks,
            shared_state: state,
            ..
        } = timer;

        let postpone_config = PostponeHandlerConfig {
            postpone_limit: config.postpone_limit,
            postponed_count: state.postponed_count,
        };

        match action {
            PomodoroTimerAction::PostponeBreak if Self::can_postpone(postpone_config) => {
                let state = PomodoroTimerState {
                    postponed_count: state.postponed_count + 1,
                    ..state
                };

                PomodoroTimer::<LongBreak>::postpone(config, callbacks, state);

                None
            }
            PomodoroTimerAction::PlayPause => Some(TimerAction::PlayPause),
            PomodoroTimerAction::Skip => Some(TimerAction::End),

            PomodoroTimerAction::ResetTimer => {
                PomodoroTimer::<Interval>::reset(config, callbacks).init();
                None
            }

            _ => None,
        }
    }
}

impl TimerTickHandler for LongBreakTickHandler {
    fn call(&mut self, status: TimerStatus) -> Option<TimerAction> {
        let callbacks = self.pomodoro_timer.callbacks.clone();
        let state = self.pomodoro_timer.shared_state;

        let result = (callbacks.on_tick)(ViewState {
            is_break: true,
            is_postponed: false,
            postpone_count: state.postponed_count,
            round: state.round,
            time: status.current_time.to_string(),
            is_paused: status.is_paused,
        });

        if let Some(action) = result {
            self.handle_action(action)
        } else {
            None
        }
    }
}

impl PomodoroTimer<LongBreak> {
    /// Starts the timer loop on a `PomodoroTimer<LongBreak>`
    pub fn init(self) {
        let next_shared_state = PomodoroTimerState {
            round: self.shared_state.round + 1,
            postponed_count: self.shared_state.postponed_count,
        };

        Timer::<Paused>::new(
            self.config.major_break,
            Some(OnEndHandler {
                on_timer_end: self.callbacks.on_timer_end.clone(),
                state: self.shared_state,
                notification: "Break is over",
            }),
            Some(LongBreakTickHandler {
                pomodoro_timer: self.clone(),
            }),
        )
        .init();

        Self::next(self.config, self.callbacks, next_shared_state)
    }

    fn postpone(
        config: PomodoroTimerConfig,
        callbacks: Callbacks,
        shared_state: PomodoroTimerState,
    ) {
        PomodoroTimer {
            shared_state,
            config,
            callbacks,
            marker: PhantomData::<PostponedLongBreak>,
        }
        .init();
    }

    fn next(config: PomodoroTimerConfig, callbacks: Callbacks, shared_state: PomodoroTimerState) {
        PomodoroTimer {
            shared_state,
            config,
            callbacks,
            marker: PhantomData::<Interval>,
        }
        .init();
    }
}
