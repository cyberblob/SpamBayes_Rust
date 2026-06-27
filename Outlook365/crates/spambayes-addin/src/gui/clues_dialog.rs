//! Show Clues dialog — displays spam scoring evidence for a message.
//!
//! This is a 1-to-1 replacement of `show_clues_dialog.py`. Shows the
//! classifier's token scores in a scrollable monospace text view with
//! Copy to Clipboard and Close buttons.
//!
//! **Validates: Requirements 13.1–13.5**

use std::rc::Rc;

use gtk4::prelude::*;

/// Inner state for the clues dialog, shared via `Rc`.
struct CluesDialogInner {
    window: gtk4::Window,
}

/// The Show Clues dialog.
///
/// Displays:
/// - Title label: "Spam Clues: {subject}" (truncated to 50 chars)
/// - Scrollable monospace `TextView` with the clues text (read-only)
/// - "Copy to Clipboard" button
/// - "Close" button + Escape key binding
///
/// **Validates: Requirements 13.1, 13.2, 13.3, 13.4, 13.5**
#[derive(Clone)]
pub struct CluesDialog {
    inner: Rc<CluesDialogInner>,
}

impl CluesDialog {
    /// Create the clues dialog.
    ///
    /// # Arguments
    /// * `parent` - Optional parent window for transient-for/modal positioning
    /// * `subject` - The email subject (will be truncated to 50 chars in the title)
    /// * `clues_text` - The full clues/scoring text to display
    ///
    /// **Validates: Requirements 13.1, 13.5**
    pub fn new(parent: Option<&gtk4::Window>, subject: &str, clues_text: &str) -> Self {
        // Truncate subject to 50 characters (Requirement 13.1)
        let truncated_subject: String = subject.chars().take(50).collect();
        let title_text = format!("Spam Clues: {}", truncated_subject);

        // Build the window
        let window = gtk4::Window::builder()
            .title("Spam Clues")
            .default_width(700)
            .default_height(600)
            .modal(true)
            .resizable(true)
            .build();

        if let Some(parent_win) = parent {
            window.set_transient_for(Some(parent_win));
        }

        // Main vertical layout
        let vbox = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(10)
            .margin_top(16)
            .margin_bottom(16)
            .margin_start(16)
            .margin_end(16)
            .build();

        // Title label — bold, showing "Spam Clues: {subject}" (Requirement 13.1)
        // Use Pango markup for bold styling and larger font size.
        let markup = format!(
            "<span size=\"large\" weight=\"bold\">{}</span>",
            glib::markup_escape_text(&title_text)
        );
        let title_label = gtk4::Label::builder()
            .use_markup(true)
            .label(&markup)
            .halign(gtk4::Align::Start)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .build();

        // Scrollable text view with monospace font (Requirement 13.2)
        let text_view = gtk4::TextView::builder()
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk4::WrapMode::Word)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(8)
            .right_margin(8)
            .vexpand(true)
            .hexpand(true)
            .build();

        // Set the clues text into the buffer
        let buffer = text_view.buffer();
        buffer.set_text(clues_text);

        // Wrap in a scrolled window
        let scrolled_window = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Automatic)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .vexpand(true)
            .hexpand(true)
            .build();
        scrolled_window.set_child(Some(&text_view));

        // Button row
        let button_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(8)
            .halign(gtk4::Align::End)
            .build();

        // "Copy to Clipboard" button (Requirement 13.3)
        let copy_btn = gtk4::Button::builder()
            .label("Copy to Clipboard")
            .build();

        // "Close" button (Requirement 13.4)
        let close_btn = gtk4::Button::builder()
            .label("Close")
            .build();

        button_box.append(&copy_btn);
        button_box.append(&close_btn);

        // Assemble layout
        vbox.append(&title_label);
        vbox.append(&scrolled_window);
        vbox.append(&button_box);

        window.set_child(Some(&vbox));

        // --- Signal handlers ---

        // Copy to Clipboard: uses gdk4::Clipboard from the widget's display
        let clues_text_owned = clues_text.to_owned();
        let window_for_copy = window.clone();
        copy_btn.connect_clicked(move |_| {
            let display = gtk4::prelude::WidgetExt::display(&window_for_copy);
            let clipboard = display.clipboard();
            clipboard.set_text(&clues_text_owned);
        });

        // Close button closes the window
        let window_for_close = window.clone();
        close_btn.connect_clicked(move |_| {
            window_for_close.close();
        });

        // Escape key binding (Requirement 13.4)
        let key_controller = gtk4::EventControllerKey::new();
        let window_for_escape = window.clone();
        key_controller.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk4::gdk::Key::Escape {
                window_for_escape.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key_controller);

        Self {
            inner: Rc::new(CluesDialogInner { window }),
        }
    }

    /// Present (show) the dialog.
    ///
    /// **Validates: Requirement 13.5**
    pub fn present(&self) {
        self.inner.window.present();
    }

    /// Connect a callback that fires when the dialog window is closed.
    pub fn connect_close(&self, callback: impl FnOnce() + 'static) {
        use std::cell::Cell;
        let cb = Rc::new(Cell::new(Some(callback)));
        self.inner.window.connect_close_request(move |_| {
            if let Some(f) = cb.take() {
                f();
            }
            glib::Propagation::Proceed
        });
    }
}
