//! Configuration Wizard — a 4-page setup wizard for first-run configuration.
//!
//! This is a 1-to-1 replacement of `SimpleConfigWizard` from
//! `simple_config_wizard.py`. It uses a GTK4 `Stack` for page navigation
//! with Back/Next/Cancel buttons.
//!
//! **Validates: Requirements 9.1–9.9**

use gtk4::prelude::*;
use gtk4::{
    self, Align, Box as GtkBox, Button, CheckButton, CssProvider, Entry, Label, Orientation,
    Stack, StackTransitionType, Window,
};
use gtk4::gdk;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use spambayes_config::AppConfig;

use crate::gui::message_boxes;

/// CSS styles for the wizard window.
const WIZARD_CSS: &str = r#"
.wizard-title {
    font-family: "Segoe UI", sans-serif;
    font-size: 14pt;
    font-weight: bold;
    color: #2B579A;
}

.wizard-description {
    font-family: "Segoe UI", sans-serif;
    font-size: 10pt;
    color: #333333;
}

.wizard-section-title {
    font-family: "Segoe UI", sans-serif;
    font-size: 10pt;
    font-weight: bold;
    color: #444444;
}

.wizard-info-note {
    font-family: "Segoe UI", sans-serif;
    font-size: 9pt;
    font-style: italic;
    color: #666666;
}

.wizard-nav-bar {
    background-color: #F0F0F0;
    padding: 8px 12px;
    border-top: 1px solid #CCCCCC;
}
"#;

/// Result of the wizard completion.
pub enum WizardResult {
    /// User completed the wizard with the given folder names.
    Completed {
        spam_folder: String,
        unsure_folder: String,
    },
    /// User cancelled the wizard.
    Cancelled,
}

/// The Configuration Wizard window.
///
/// A 4-page dialog:
/// - Page 1 (Welcome): Preparation options (radio buttons)
/// - Page 2 (Folders): Spam/Unsure folder names
/// - Page 3 (Training): Context-dependent guidance
/// - Page 4 (Finish): Success message with next steps
///
/// **Validates: Requirements 9.1, 9.2, 9.3, 9.4, 9.5**
pub struct WizardWindow {
    /// The top-level GTK4 window.
    pub window: Window,
    /// Stack widget holding the 4 pages.
    pub stack: Stack,
    /// Current page index (0-based).
    pub current_page: Cell<usize>,
    // ─── Page 1 widgets ──────────────────────────────────────────────────
    /// Radio button: "I haven't prepared" (value=0)
    pub radio_no_prep: CheckButton,
    /// Radio button: "I have pre-sorted mail" (value=1)
    pub radio_presorted: CheckButton,
    /// Radio button: "I want to configure manually" (value=2)
    pub radio_manual: CheckButton,
    // ─── Page 2 widgets ──────────────────────────────────────────────────
    /// Entry for spam folder name.
    pub spam_folder_entry: Entry,
    /// Entry for unsure folder name.
    pub unsure_folder_entry: Entry,
    // ─── Page 3 widgets ──────────────────────────────────────────────────
    /// Label for training guidance text (updated dynamically based on Page 1 selection).
    pub training_text_label: Label,
    // ─── Navigation buttons ──────────────────────────────────────────────
    /// The "Back" button.
    pub back_btn: Button,
    /// The "Next" / "Finish" button.
    pub next_btn: Button,
    /// The "Cancel" button.
    pub cancel_btn: Button,
}

impl WizardWindow {
    /// Create and display the wizard.
    ///
    /// Builds all 4 pages, navigation buttons, and the window chrome.
    /// Signals are NOT connected here — that's handled by task 9.2.
    ///
    /// **Validates: Requirements 9.1, 9.2, 9.3, 9.4, 9.5**
    pub fn new(config: &AppConfig) -> Rc<Self> {
        let _ = config; // Will be used in task 9.2 for defaults

        // ─── Create the window ───────────────────────────────────────────
        let window = Window::new();
        window.set_title(Some("SpamBayes Configuration Wizard"));
        window.set_default_size(600, 450);
        window.set_resizable(true);
        window.set_modal(true);

        // ─── Main vertical layout ────────────────────────────────────────
        let main_vbox = GtkBox::new(Orientation::Vertical, 0);

        // ─── Stack for pages ─────────────────────────────────────────────
        let stack = Stack::new();
        stack.set_transition_type(StackTransitionType::SlideLeftRight);
        stack.set_transition_duration(200);
        stack.set_vexpand(true);
        stack.set_hexpand(true);

        // ─── Build Page 1: Welcome ───────────────────────────────────────
        let (page1, radio_no_prep, radio_presorted, radio_manual) = Self::build_page_welcome();
        stack.add_named(&page1, Some("page-1"));

        // ─── Build Page 2: Folders ───────────────────────────────────────
        let (page2, spam_folder_entry, unsure_folder_entry) = Self::build_page_folders();
        stack.add_named(&page2, Some("page-2"));

        // ─── Build Page 3: Training ──────────────────────────────────────
        let (page3, training_text_label) = Self::build_page_training();
        stack.add_named(&page3, Some("page-3"));

        // ─── Build Page 4: Finish ────────────────────────────────────────
        let page4 = Self::build_page_finish();
        stack.add_named(&page4, Some("page-4"));

        // Show Page 1 by default
        stack.set_visible_child_name("page-1");

        main_vbox.append(&stack);

        // ─── Navigation button bar ───────────────────────────────────────
        let nav_bar = GtkBox::new(Orientation::Horizontal, 8);
        nav_bar.add_css_class("wizard-nav-bar");
        nav_bar.set_margin_top(8);
        nav_bar.set_margin_bottom(8);
        nav_bar.set_margin_start(12);
        nav_bar.set_margin_end(12);

        let back_btn = Button::with_label("<< Back");
        back_btn.set_sensitive(false); // Disabled on page 1

        let next_btn = Button::with_label("Next >>");

        let cancel_btn = Button::with_label("Cancel");

        // Spacer to push Cancel to the right
        let spacer = GtkBox::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);

        nav_bar.append(&back_btn);
        nav_bar.append(&next_btn);
        nav_bar.append(&spacer);
        nav_bar.append(&cancel_btn);

        main_vbox.append(&nav_bar);

        // Set window content
        window.set_child(Some(&main_vbox));

        // ─── Build the WizardWindow Rc ───────────────────────────────────
        Rc::new(Self {
            window,
            stack,
            current_page: Cell::new(0),
            radio_no_prep,
            radio_presorted,
            radio_manual,
            spam_folder_entry,
            unsure_folder_entry,
            training_text_label,
            back_btn,
            next_btn,
            cancel_btn,
        })
    }

    /// Present the wizard window and load CSS.
    pub fn present(&self) {
        let display = gtk4::prelude::WidgetExt::display(&self.window);
        load_wizard_css(&display);
        self.window.present();
    }

    /// Get the currently selected preparation option (0, 1, or 2).
    pub fn preparation_selection(&self) -> u32 {
        if self.radio_presorted.is_active() {
            1
        } else if self.radio_manual.is_active() {
            2
        } else {
            0 // Default: radio_no_prep
        }
    }

    // ─── Page builders ───────────────────────────────────────────────────

    /// Build Page 1: Welcome page with title, description, and 3 radio buttons.
    ///
    /// **Validates: Requirement 9.2**
    fn build_page_welcome() -> (GtkBox, CheckButton, CheckButton, CheckButton) {
        let page = GtkBox::new(Orientation::Vertical, 8);
        page.set_margin_top(20);
        page.set_margin_bottom(20);
        page.set_margin_start(30);
        page.set_margin_end(30);

        // Title
        let title = Label::new(Some("Welcome to SpamBayes Configuration"));
        title.add_css_class("wizard-title");
        title.set_halign(Align::Start);
        page.append(&title);

        // Description
        let desc = Label::new(Some(
            "This wizard will help you configure SpamBayes for Outlook.\n\n\
             SpamBayes is a spam filter that learns from your email.\n\
             It needs to be trained with examples of spam and good mail.",
        ));
        desc.add_css_class("wizard-description");
        desc.set_halign(Align::Start);
        desc.set_wrap(true);
        desc.set_margin_top(12);
        page.append(&desc);

        // Section title
        let section_title = Label::new(Some("How have you prepared?"));
        section_title.add_css_class("wizard-section-title");
        section_title.set_halign(Align::Start);
        section_title.set_margin_top(16);
        page.append(&section_title);

        // Radio buttons (CheckButton in radio-group mode)
        let radio_no_prep =
            CheckButton::with_label("I haven't prepared — I'll let SpamBayes learn as it goes");
        radio_no_prep.set_margin_start(20);
        radio_no_prep.set_margin_top(8);
        radio_no_prep.set_active(true); // Default selection

        let radio_presorted =
            CheckButton::with_label("I have pre-sorted spam and good messages into folders");
        radio_presorted.set_margin_start(20);
        radio_presorted.set_margin_top(4);
        radio_presorted.set_group(Some(&radio_no_prep));

        let radio_manual =
            CheckButton::with_label("I want to configure manually using the Manager");
        radio_manual.set_margin_start(20);
        radio_manual.set_margin_top(4);
        radio_manual.set_group(Some(&radio_no_prep));

        page.append(&radio_no_prep);
        page.append(&radio_presorted);
        page.append(&radio_manual);

        (page, radio_no_prep, radio_presorted, radio_manual)
    }

    /// Build Page 2: Folders configuration page.
    ///
    /// **Validates: Requirement 9.3**
    fn build_page_folders() -> (GtkBox, Entry, Entry) {
        let page = GtkBox::new(Orientation::Vertical, 8);
        page.set_margin_top(20);
        page.set_margin_bottom(20);
        page.set_margin_start(30);
        page.set_margin_end(30);

        // Title
        let title = Label::new(Some("Configure Folders"));
        title.add_css_class("wizard-title");
        title.set_halign(Align::Start);
        page.append(&title);

        // Description
        let desc = Label::new(Some(
            "SpamBayes needs to know where to put spam and uncertain messages.",
        ));
        desc.add_css_class("wizard-description");
        desc.set_halign(Align::Start);
        desc.set_wrap(true);
        desc.set_margin_top(12);
        page.append(&desc);

        // Spam folder section
        let spam_section = GtkBox::new(Orientation::Vertical, 4);
        spam_section.set_margin_top(16);

        let spam_label = Label::new(Some("Spam folder name:"));
        spam_label.add_css_class("wizard-section-title");
        spam_label.set_halign(Align::Start);
        spam_section.append(&spam_label);

        let spam_folder_entry = Entry::new();
        spam_folder_entry.set_text("Junk E-Mail");
        spam_folder_entry.set_margin_start(20);
        spam_folder_entry.set_hexpand(true);
        spam_section.append(&spam_folder_entry);

        page.append(&spam_section);

        // Unsure folder section
        let unsure_section = GtkBox::new(Orientation::Vertical, 4);
        unsure_section.set_margin_top(16);

        let unsure_label = Label::new(Some("Unsure folder name:"));
        unsure_label.add_css_class("wizard-section-title");
        unsure_label.set_halign(Align::Start);
        unsure_section.append(&unsure_label);

        let unsure_folder_entry = Entry::new();
        unsure_folder_entry.set_text("Junk Suspects");
        unsure_folder_entry.set_margin_start(20);
        unsure_folder_entry.set_hexpand(true);
        unsure_section.append(&unsure_folder_entry);

        page.append(&unsure_section);

        // Info note
        let info_note = Label::new(Some("These folders will be created if they don't exist."));
        info_note.add_css_class("wizard-info-note");
        info_note.set_halign(Align::Start);
        info_note.set_margin_top(16);
        page.append(&info_note);

        (page, spam_folder_entry, unsure_folder_entry)
    }

    /// Build Page 3: Training guidance page.
    ///
    /// The text content is context-dependent based on Page 1 selection.
    /// The label text is updated dynamically when navigating to this page.
    ///
    /// **Validates: Requirement 9.4**
    fn build_page_training() -> (GtkBox, Label) {
        let page = GtkBox::new(Orientation::Vertical, 8);
        page.set_margin_top(20);
        page.set_margin_bottom(20);
        page.set_margin_start(30);
        page.set_margin_end(30);

        // Title
        let title = Label::new(Some("Training"));
        title.add_css_class("wizard-title");
        title.set_halign(Align::Start);
        page.append(&title);

        // Dynamic training text (will be updated on page navigation)
        let training_text_label = Label::new(Some(""));
        training_text_label.add_css_class("wizard-description");
        training_text_label.set_halign(Align::Start);
        training_text_label.set_valign(Align::Start);
        training_text_label.set_wrap(true);
        training_text_label.set_margin_top(16);
        training_text_label.set_vexpand(true);
        page.append(&training_text_label);

        (page, training_text_label)
    }

    /// Build Page 4: Finish page with success message.
    ///
    /// **Validates: Requirement 9.5**
    fn build_page_finish() -> GtkBox {
        let page = GtkBox::new(Orientation::Vertical, 8);
        page.set_margin_top(20);
        page.set_margin_bottom(20);
        page.set_margin_start(30);
        page.set_margin_end(30);

        // Title
        let title = Label::new(Some("Configuration Complete!"));
        title.add_css_class("wizard-title");
        title.set_halign(Align::Start);
        page.append(&title);

        // Success message with numbered next-steps
        let steps_text = Label::new(Some(
            "Click Finish to save your configuration.\n\n\
             After clicking Finish:\n\n\
             1. Create the folders you specified in Outlook\n\
             2. Look for the SpamBayes menu in Outlook\n\
             3. Use SpamBayes Manager to complete setup\n\n\
             The Manager will help you:\n\
             • Select the folders you created\n\
             • Choose which folders to watch (e.g., Inbox)\n\
             • Enable the spam filter\n\n\
             Click Finish to close this wizard.",
        ));
        steps_text.add_css_class("wizard-description");
        steps_text.set_halign(Align::Start);
        steps_text.set_valign(Align::Start);
        steps_text.set_wrap(true);
        steps_text.set_margin_top(16);
        page.append(&steps_text);

        page
    }

    /// Update the training page text based on the current preparation selection.
    ///
    /// Called when navigating to Page 3 to show context-dependent guidance.
    pub fn update_training_text(&self) {
        let text = match self.preparation_selection() {
            0 => {
                "You chose to let SpamBayes learn as it goes.\n\n\
                 SpamBayes will start filtering mail immediately.\n\
                 All uncertain mail will go to your Unsure folder.\n\n\
                 Use the 'Spam' and 'Not Spam' buttons to train SpamBayes\n\
                 as you review your mail."
            }
            1 => {
                "If you have pre-sorted mail, you can train SpamBayes now.\n\n\
                 To train SpamBayes:\n\n\
                 1. After completing this wizard, open SpamBayes Manager\n\
                 2. Go to the Training tab\n\
                 3. Select your folders with good and spam messages\n\
                 4. Click 'Start Training'"
            }
            _ => {
                // Manual config (value=2) — shouldn't normally reach this page
                "You selected manual configuration.\n\n\
                 Use the SpamBayes Manager to configure all settings."
            }
        };
        self.training_text_label.set_text(text);
    }

    // ─── Task 9.2: Wizard navigation and completion ──────────────────────

    /// Connect all navigation signals (Back, Next, Cancel, window close).
    ///
    /// The `on_close` callback is invoked exactly once when the wizard
    /// finishes or is cancelled, reporting the result back to the caller.
    ///
    /// **Validates: Requirements 9.6, 9.7, 9.8**
    pub fn connect_signals(
        self: &Rc<Self>,
        on_close: Option<Box<dyn FnOnce(WizardResult) + 'static>>,
    ) {
        let on_close = Rc::new(RefCell::new(on_close));

        // Back button
        {
            let wizard = Rc::clone(self);
            self.back_btn.connect_clicked(move |_| {
                wizard.go_back();
            });
        }

        // Next / Finish button
        {
            let wizard = Rc::clone(self);
            let on_close_next = Rc::clone(&on_close);
            self.next_btn.connect_clicked(move |_| {
                wizard.go_next(&on_close_next);
            });
        }

        // Cancel button
        {
            let wizard = Rc::clone(self);
            let on_close_cancel = Rc::clone(&on_close);
            self.cancel_btn.connect_clicked(move |_| {
                wizard.do_cancel(&on_close_cancel);
            });
        }

        // Window close-request (X button) — same as Cancel
        {
            let wizard = Rc::clone(self);
            let on_close_close = Rc::clone(&on_close);
            self.window.connect_close_request(move |_| {
                wizard.do_cancel(&on_close_close);
                glib::Propagation::Stop
            });
        }
    }

    /// Navigate to the previous page.
    fn go_back(&self) {
        let current = self.current_page.get();
        if current > 0 {
            self.navigate_to_page(current - 1);
        }
    }

    /// Validate the current page and advance to the next (or finish).
    fn go_next(
        &self,
        on_close: &Rc<RefCell<Option<Box<dyn FnOnce(WizardResult) + 'static>>>>,
    ) {
        let current = self.current_page.get();

        match current {
            0 => {
                // Page 1 → check if "configure manually" is selected
                if self.radio_manual.is_active() {
                    // Skip directly to page 4 (Finish)
                    self.navigate_to_page(3);
                } else {
                    self.navigate_to_page(1);
                }
            }
            1 => {
                // Page 2 (Folders) → advance to page 3 (Training)
                self.navigate_to_page(2);
            }
            2 => {
                // Page 3 (Training) → advance to page 4 (Finish)
                self.navigate_to_page(3);
            }
            3 => {
                // Page 4 → Finish
                self.finish(on_close);
            }
            _ => {}
        }
    }

    /// Handle cancel: show confirmation and close if accepted.
    fn do_cancel(
        &self,
        on_close: &Rc<RefCell<Option<Box<dyn FnOnce(WizardResult) + 'static>>>>,
    ) {
        // If callback already taken, we're in a re-entrant close — just destroy.
        if on_close.borrow().is_none() {
            self.window.destroy();
            return;
        }

        let confirmed = message_boxes::ask_question(
            Some(&self.window),
            "SpamBayes",
            "Are you sure you want to cancel the configuration wizard?\n\n\
             No settings will be saved.",
        );

        if confirmed {
            if let Some(callback) = on_close.borrow_mut().take() {
                callback(WizardResult::Cancelled);
            }
            self.window.destroy();
        }
    }

    /// Navigate to a specific page (0-based index) and update button states.
    fn navigate_to_page(&self, page: usize) {
        let page_name = match page {
            0 => "page-1",
            1 => "page-2",
            2 => "page-3",
            3 => "page-4",
            _ => return,
        };

        self.current_page.set(page);
        self.stack.set_visible_child_name(page_name);

        // Update training text when navigating to page 3
        if page == 2 {
            self.update_training_text();
        }

        // Update button states
        match page {
            0 => {
                self.back_btn.set_sensitive(false);
                self.next_btn.set_label("Next >>");
            }
            1 | 2 => {
                self.back_btn.set_sensitive(true);
                self.next_btn.set_label("Next >>");
            }
            3 => {
                self.back_btn.set_sensitive(true);
                self.next_btn.set_label("Finish");
            }
            _ => {}
        }
    }

    /// Complete the wizard: validate folders, save, show summary, and close.
    fn finish(
        &self,
        on_close: &Rc<RefCell<Option<Box<dyn FnOnce(WizardResult) + 'static>>>>,
    ) {
        let spam_folder = self.spam_folder_entry.text().to_string();
        let unsure_folder = self.unsure_folder_entry.text().to_string();

        // Validate: folders must not be empty
        if spam_folder.trim().is_empty() {
            message_boxes::report_error(
                Some(&self.window),
                "SpamBayes",
                "Please enter a name for the Spam folder.",
            );
            return;
        }
        if unsure_folder.trim().is_empty() {
            message_boxes::report_error(
                Some(&self.window),
                "SpamBayes",
                "Please enter a name for the Unsure folder.",
            );
            return;
        }

        // Show summary message
        message_boxes::report_information(
            Some(&self.window),
            "SpamBayes Configuration Complete",
            &format!(
                "Configuration saved successfully!\n\n\
                 Spam folder: {spam_folder}\n\
                 Unsure folder: {unsure_folder}\n\n\
                 Next steps:\n\
                 1. Create these folders in Outlook if they don't exist\n\
                 2. Use the SpamBayes Manager to complete setup\n\
                 3. Enable filtering from the SpamBayes menu",
            ),
        );

        // Invoke the completion callback (takes it so close-request won't re-ask)
        if let Some(callback) = on_close.borrow_mut().take() {
            callback(WizardResult::Completed {
                spam_folder,
                unsure_folder,
            });
        }

        // Use destroy() to avoid triggering close-request → do_cancel loop
        self.window.destroy();
    }
}

/// Register wizard CSS with the given display.
///
/// Should be called once when presenting the wizard window.
pub fn load_wizard_css(display: &gdk::Display) {
    let provider = CssProvider::new();
    provider.load_from_string(WIZARD_CSS);
    gtk4::style_context_add_provider_for_display(
        display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}
