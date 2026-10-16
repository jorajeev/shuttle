pub mod continuation;

pub(crate) use continuation::switch_between_polls;
pub use continuation::{drop_scheduling_point, switch, FirstPollSwitch};
