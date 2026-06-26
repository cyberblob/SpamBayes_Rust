//! Message box utility functions — standard dialogs for info/warning/error/question.
//!
//! This is a 1-to-1 replacement of `tkinter_messagebox.py`. All functions
//! default the title to "SpamBayes" when `None` is provided.
//!
//! Uses `gtk4::AlertDialog` (available since GTK 4.10, we target 4.12+) for
//! all standard message boxes. Dialogs run synchronously by spinning the
//! GLib main context until the user responds.
//!
//! **Validates: Requirements 12.1, 12.2, 12.3**

use std::cell::Cell;
use std::rc::Rc;

use gtk4::gio;

/// The kind of message box to display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgBoxKind {
    Info,
    Warning,
    Error,
    Question,
    OkCancel,
    RetryCancel,
    YesNoCancel,
}

/// Result from a message box interaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgBoxResult {
    Yes,
    No,
    Ok,
    Cancel,
    Retry,
}

/// Action chosen for calendar spam items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalendarAction {
    /// Train as ham (not spam).
    Ham,
    /// Delete the item.
    Delete,
    /// Move to spam folder.
    Move,
}

/// Default title used when none is provided.
const DEFAULT_TITLE: &str = "SpamBayes";

/// Resolve the title, defaulting to "SpamBayes" if empty.
fn resolve_title(title: &str) -> &str {
    if title.is_empty() {
        DEFAULT_TITLE
    } else {
        title
    }
}

/// Ask a Yes/No question. Returns `true` for Yes.
///
/// Title defaults to "SpamBayes" if empty.
///
/// **Validates: Requirement 12.1**
pub fn ask_question(parent: Option<&gtk4::Window>, title: &str, message: &str) -> bool {
    let title = resolve_title(title);
    let buttons = &["No", "Yes"];
    let response = show_alert_dialog(parent, title, message, buttons, None);
    // "Yes" is button index 1
    response == 1
}

/// Display an informational message.
///
/// Title defaults to "SpamBayes" if empty.
///
/// **Validates: Requirement 12.1**
pub fn report_information(parent: Option<&gtk4::Window>, title: &str, message: &str) {
    let title = resolve_title(title);
    let buttons = &["OK"];
    show_alert_dialog(parent, title, message, buttons, Some(0));
}

/// Display an error message.
///
/// Title defaults to "SpamBayes" if empty.
///
/// **Validates: Requirement 12.1**
pub fn report_error(parent: Option<&gtk4::Window>, title: &str, message: &str) {
    let title = resolve_title(title);
    let buttons = &["OK"];
    show_alert_dialog(parent, title, message, buttons, Some(0));
}

/// Display a warning message.
///
/// Title defaults to "SpamBayes" if empty.
///
/// **Validates: Requirement 12.1**
pub fn report_warning(parent: Option<&gtk4::Window>, title: &str, message: &str) {
    let title = resolve_title(title);
    let buttons = &["OK"];
    show_alert_dialog(parent, title, message, buttons, Some(0));
}

/// Ask OK/Cancel. Returns `true` for OK.
///
/// Title defaults to "SpamBayes" if empty.
///
/// **Validates: Requirement 12.1**
pub fn ask_ok_cancel(parent: Option<&gtk4::Window>, title: &str, message: &str) -> bool {
    let title = resolve_title(title);
    let buttons = &["Cancel", "OK"];
    let response = show_alert_dialog(parent, title, message, buttons, None);
    // "OK" is button index 1
    response == 1
}

/// Ask Retry/Cancel. Returns `true` for Retry.
///
/// Title defaults to "SpamBayes" if empty.
///
/// **Validates: Requirement 12.1**
pub fn ask_retry_cancel(parent: Option<&gtk4::Window>, title: &str, message: &str) -> bool {
    let title = resolve_title(title);
    let buttons = &["Cancel", "Retry"];
    let response = show_alert_dialog(parent, title, message, buttons, None);
    // "Retry" is button index 1
    response == 1
}

/// Ask Yes/No/Cancel. Returns `Some(true)` for Yes, `Some(false)` for No,
/// `None` for Cancel.
///
/// Title defaults to "SpamBayes" if empty.
///
/// **Validates: Requirement 12.1**
pub fn ask_yes_no_cancel(
    parent: Option<&gtk4::Window>,
    title: &str,
    message: &str,
) -> Option<bool> {
    let title = resolve_title(title);
    let buttons = &["Cancel", "No", "Yes"];
    let response = show_alert_dialog(parent, title, message, buttons, None);
    match response {
        2 => Some(true),  // Yes
        1 => Some(false), // No
        _ => None,        // Cancel or closed
    }
}

/// Custom 3-button dialog for calendar items classified as spam.
///
/// Buttons: "Train as Ham" / "Move to Trash" / "Move to Spam"
///
/// **Validates: Requirement 12.2**
pub fn ask_calendar_spam_action(
    parent: Option<&gtk4::Window>,
    subject: &str,
) -> CalendarAction {
    let title = "SpamBayes - Calendar Item";
    let message = format!(
        "A calendar invitation has been classified as spam:\n\n\
         \"{subject}\"\n\n\
         What would you like to do?"
    );
    let buttons = &["Move to Spam", "Move to Trash", "Train as Ham"];
    let response = show_alert_dialog(parent, title, &message, buttons, Some(0));
    match response {
        2 => CalendarAction::Ham,
        1 => CalendarAction::Delete,
        _ => CalendarAction::Move, // Default: move to spam (button 0 or dialog closed)
    }
}

/// Generic `ShowMessage` function matching the Python `ShowMessage` interface.
///
/// **Validates: Requirement 12.1**
pub fn show_message(
    parent: Option<&gtk4::Window>,
    title: &str,
    message: &str,
    kind: MsgBoxKind,
) -> MsgBoxResult {
    let title = resolve_title(title);
    match kind {
        MsgBoxKind::Info => {
            report_information(parent, title, message);
            MsgBoxResult::Ok
        }
        MsgBoxKind::Warning => {
            report_warning(parent, title, message);
            MsgBoxResult::Ok
        }
        MsgBoxKind::Error => {
            report_error(parent, title, message);
            MsgBoxResult::Ok
        }
        MsgBoxKind::Question => {
            if ask_question(parent, title, message) {
                MsgBoxResult::Yes
            } else {
                MsgBoxResult::No
            }
        }
        MsgBoxKind::OkCancel => {
            if ask_ok_cancel(parent, title, message) {
                MsgBoxResult::Ok
            } else {
                MsgBoxResult::Cancel
            }
        }
        MsgBoxKind::RetryCancel => {
            if ask_retry_cancel(parent, title, message) {
                MsgBoxResult::Retry
            } else {
                MsgBoxResult::Cancel
            }
        }
        MsgBoxKind::YesNoCancel => match ask_yes_no_cancel(parent, title, message) {
            Some(true) => MsgBoxResult::Yes,
            Some(false) => MsgBoxResult::No,
            None => MsgBoxResult::Cancel,
        },
    }
}

// ─── Internal AlertDialog Helper ─────────────────────────────────────────────

/// Show a GTK4 `AlertDialog` synchronously and return the button index chosen.
///
/// This function creates an `AlertDialog`, presents it, and spins the GLib
/// main context until the user responds. It must be called from the GTK
/// thread (within the GLib main loop).
///
/// # Arguments
/// * `parent` - Optional parent window for modality
/// * `title` - Dialog heading (shown as the bold message text)
/// * `message` - Dialog body text (shown as the detail)
/// * `buttons` - Button labels (indices returned as the response)
/// * `default_button` - Optional default/highlighted button index
///
/// # Returns
/// The index of the button pressed, or -1 if the dialog was closed without
/// choosing a button (e.g., via Escape or window close).
fn show_alert_dialog(
    parent: Option<&gtk4::Window>,
    title: &str,
    message: &str,
    buttons: &[&str],
    default_button: Option<i32>,
) -> i32 {
    let dialog = gtk4::AlertDialog::builder()
        .message(title)
        .detail(message)
        .modal(true)
        .buttons(buttons.to_vec())
        .build();

    if let Some(default) = default_button {
        dialog.set_default_button(default);
    }

    // Set the cancel button to the first button (index 0) which is typically
    // "Cancel" or "No" — this maps to the Escape key behavior.
    dialog.set_cancel_button(0);

    // Use choose() with a callback to get the response asynchronously, then
    // spin the main context to make it effectively synchronous from the
    // caller's perspective. This works because we're already on the GTK thread.
    let result: Rc<Cell<Option<i32>>> = Rc::new(Cell::new(None));
    let result_clone = Rc::clone(&result);

    let parent_window = parent.cloned();
    dialog.choose(
        parent_window.as_ref(),
        gio::Cancellable::NONE,
        move |response| {
            let index = match response {
                Ok(idx) => idx,
                Err(_) => -1, // Dialog was dismissed (Escape, close button)
            };
            result_clone.set(Some(index));
        },
    );

    // Spin the GLib main context until we get a response.
    // This is safe because we're already on the GTK thread inside the main loop.
    let context = glib::MainContext::default();
    while result.get().is_none() {
        context.iteration(true);
    }

    result.get().unwrap_or(-1)
}
