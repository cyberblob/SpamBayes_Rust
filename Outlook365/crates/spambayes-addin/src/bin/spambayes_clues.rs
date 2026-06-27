//! SpamBayes Clues Viewer — GTK4 dialog for displaying scoring evidence.
//!
//! Lightweight subprocess launched by the Outlook addin when the user clicks
//! "Show Clues". Receives subject as argv[1] and clues text on stdin, then
//! presents the GTK4 CluesDialog.
//!
//! This mirrors the Python version's approach of spawning a separate process
//! to avoid COM threading issues with modal dialogs.

#![windows_subsystem = "windows"]

use std::io::Read;

fn main() {
    // Get subject from arguments.
    let subject = std::env::args().nth(1).unwrap_or_else(|| "Unknown".to_string());

    // Read clues text from stdin.
    let mut clues_text = String::new();
    if std::io::stdin().read_to_string(&mut clues_text).is_err() || clues_text.is_empty() {
        spambayes_addin::gui::gtk_runtime::fallback_message_box(
            "SpamBayes",
            "No clues data received.",
        );
        return;
    }

    // Initialize GTK4 runtime.
    let runtime = match spambayes_addin::gui::gtk_runtime::GtkRuntime::init() {
        Ok(rt) => rt,
        Err(e) => {
            spambayes_addin::gui::gtk_runtime::fallback_message_box(
                "SpamBayes Clues",
                &format!("Failed to initialize GTK4:\n{e}"),
            );
            return;
        }
    };

    // Show the clues dialog and wait for it to close.
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    runtime.show_clues_and_wait(&subject, &clues_text, move || {
        let _ = done_tx.send(());
    });

    // Wait for the dialog to close.
    let _ = done_rx.recv();

    // Shut down GTK4 runtime.
    runtime.shutdown();
}
