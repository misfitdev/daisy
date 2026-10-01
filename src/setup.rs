//! The walkthrough that gets both macOS permissions granted.
//!
//! macOS only lets a person grant Accessibility and Input Monitoring in
//! System Settings, so Daisy asks for one at a time, opens the right pane,
//! and moves on as each is switched on.

use crate::permissions::Access;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Accessibility,
    InputMonitoring,
    /// Both are granted, but macOS applies Input Monitoring to Daisy only
    /// once it reopens.
    Reopen,
    Done,
}

/// The step to show. `reads_input` is whether macOS lets this process read
/// input right now, which can lag Input Monitoring reporting granted.
pub fn step(accessibility: Access, input_monitoring: Access, reads_input: bool) -> Step {
    match (accessibility, input_monitoring) {
        (Access::Granted, Access::Granted) if reads_input => Step::Done,
        (Access::Granted, Access::Granted) => Step::Reopen,
        (Access::Granted, _) => Step::InputMonitoring,
        _ => Step::Accessibility,
    }
}

/// How one permission's row looks at `step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    Allowed,
    /// The one to allow now.
    Current,
    /// Waits for the one before it.
    Later,
}

pub fn row(step: Step, permission: Step) -> Row {
    let order = |step| match step {
        Step::Accessibility => 0,
        Step::InputMonitoring => 1,
        Step::Reopen | Step::Done => 2,
    };
    match order(permission).cmp(&order(step)) {
        std::cmp::Ordering::Less => Row::Allowed,
        std::cmp::Ordering::Equal => Row::Current,
        std::cmp::Ordering::Greater => Row::Later,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Access::*;

    #[test]
    fn accessibility_comes_first() {
        assert_eq!(step(Denied, Undetermined, false), Step::Accessibility);
        assert_eq!(step(Denied, Granted, true), Step::Accessibility);
    }

    #[test]
    fn input_monitoring_follows_accessibility() {
        assert_eq!(step(Granted, Undetermined, false), Step::InputMonitoring);
        assert_eq!(step(Granted, Denied, false), Step::InputMonitoring);
    }

    #[test]
    fn granted_input_monitoring_that_does_not_apply_yet_asks_to_reopen() {
        assert_eq!(step(Granted, Granted, false), Step::Reopen);
        assert_eq!(step(Granted, Granted, true), Step::Done);
    }

    #[test]
    fn rows_show_what_is_allowed_and_what_is_next() {
        use Step::*;
        assert_eq!(row(Accessibility, Accessibility), Row::Current);
        assert_eq!(row(Accessibility, InputMonitoring), Row::Later);
        assert_eq!(row(InputMonitoring, Accessibility), Row::Allowed);
        assert_eq!(row(InputMonitoring, InputMonitoring), Row::Current);
        for done in [Reopen, Done] {
            assert_eq!(row(done, Accessibility), Row::Allowed);
            assert_eq!(row(done, InputMonitoring), Row::Allowed);
        }
    }
}
