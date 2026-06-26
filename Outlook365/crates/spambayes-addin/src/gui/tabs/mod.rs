//! Tab modules for the Manager window.
//!
//! Each tab is a separate module that builds its widget tree and provides
//! methods to read values for saving and validate user input.

pub mod general;
pub mod filtering;
pub mod training;
pub mod statistics;
pub mod notifications;
pub mod calendar;
pub mod advanced;

pub use general::GeneralTab;
pub use filtering::FilteringTab;
pub use training::TrainingTab;
pub use training::{TrainingExecutor, TrainingResult};
pub use statistics::StatisticsTab;
pub use notifications::NotificationsTab;
pub use calendar::CalendarTab;
pub use advanced::AdvancedTab;
