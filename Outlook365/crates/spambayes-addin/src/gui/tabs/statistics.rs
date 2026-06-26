//! Statistics tab — classification accuracy and training statistics.
//!
//! Displays total messages classified with breakdown by category, accuracy
//! metrics (correctly/incorrectly classified, false positives/negatives),
//! manual classification counts, and a reset button.
//!
//! **Validates: Requirements 4.1–4.6, 4.7**

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, Frame, Label, Orientation, ScrolledWindow, Separator,
};

use crate::gui::message_boxes::ask_question;
use crate::manager_dlg::{format_with_thousands, ManagerStats};
use crate::statistics::StatisticsManager;

// ─── StatisticsTab ───────────────────────────────────────────────────────────

/// The Statistics tab content.
///
/// Contains classification statistics, accuracy metrics, manual classification
/// counts, and a "Reset Statistics" button. All labels can be refreshed
/// dynamically via [`StatisticsTab::refresh`].
///
/// **Validates: Requirements 4.1–4.6, 4.7**
pub struct StatisticsTab {
    /// Outer scrollable container (the tab page widget).
    pub container: ScrolledWindow,

    // ─── Statistics Manager (for reset) ──────────────────────────────────
    /// Optional reference to the statistics manager for reset operations.
    #[allow(dead_code)]
    statistics_manager: Option<StatisticsManager>,

    // ─── Classification breakdown labels (Req 4.1) ───────────────────────
    /// "Messages classified: {total}"
    total_classified_label: Label,
    /// "Good: {count} ({pct}%)"
    good_label: Label,
    /// "Spam: {count} ({pct}%)"
    spam_label: Label,
    /// "Unsure: {count} ({pct}%)"
    unsure_label: Label,

    // ─── Accuracy labels (Req 4.2) ───────────────────────────────────────
    /// "Classified correctly: {count} ({pct}% of total)"
    correctly_classified_label: Label,
    /// "Classified incorrectly: {count} ({pct}% of total)"
    incorrectly_classified_label: Label,
    /// "False positives: {count} ({pct}% of total)"
    false_positives_label: Label,
    /// "False negatives: {count} ({pct}% of total)"
    false_negatives_label: Label,

    // ─── Manual classification labels (Req 4.3) ─────────────────────────
    /// "Manually classified as good: {count}"
    manually_good_label: Label,
    /// "Manually classified as spam: {count}"
    manually_spam_label: Label,

    // ─── Identification percentages (Req 4.4) ───────────────────────────
    /// "Spam correctly identified: {pct}% (+ {unsure_pct}% unsure)"
    spam_correct_pct_label: Label,
    /// "Good incorrectly identified: {pct}% (+ {unsure_pct}% unsure)"
    good_incorrect_pct_label: Label,

    // ─── Reset section (Req 4.5) ─────────────────────────────────────────
    /// "Last reset: {date}"
    last_reset_label: Label,
    /// "Reset Statistics" button — connect handler externally.
    pub reset_btn: Button,
}

impl StatisticsTab {
    /// Build the Statistics tab widget tree from the given stats.
    ///
    /// # Arguments
    ///
    /// * `stats` – Current classifier statistics
    /// * `statistics_manager` – Optional statistics manager for reset operations
    ///
    /// **Validates: Requirements 4.1–4.6, 4.7**
    #[must_use]
    pub fn new(stats: &ManagerStats, statistics_manager: Option<StatisticsManager>) -> Self {
        // ─── Main vertical layout ────────────────────────────────────────
        let content_box = GtkBox::new(Orientation::Vertical, 12);
        content_box.set_margin_top(16);
        content_box.set_margin_bottom(16);
        content_box.set_margin_start(16);
        content_box.set_margin_end(16);

        // ─── 1. Classification Breakdown (Req 4.1) ──────────────────────
        let classification_frame = Frame::new(Some("Messages Classified"));
        let classification_box = GtkBox::new(Orientation::Vertical, 4);
        classification_box.set_margin_top(8);
        classification_box.set_margin_bottom(8);
        classification_box.set_margin_start(12);
        classification_box.set_margin_end(12);

        let total_classified_label = Label::new(None);
        total_classified_label.set_halign(Align::Start);
        total_classified_label.add_css_class("section-title");
        classification_box.append(&total_classified_label);

        let good_label = Label::new(None);
        good_label.set_halign(Align::Start);
        good_label.set_margin_start(20);
        classification_box.append(&good_label);

        let spam_label = Label::new(None);
        spam_label.set_halign(Align::Start);
        spam_label.set_margin_start(20);
        classification_box.append(&spam_label);

        let unsure_label = Label::new(None);
        unsure_label.set_halign(Align::Start);
        unsure_label.set_margin_start(20);
        classification_box.append(&unsure_label);

        classification_frame.set_child(Some(&classification_box));
        content_box.append(&classification_frame);

        // ─── 2. Accuracy Metrics (Req 4.2) ──────────────────────────────
        let accuracy_frame = Frame::new(Some("Classification Accuracy"));
        let accuracy_box = GtkBox::new(Orientation::Vertical, 4);
        accuracy_box.set_margin_top(8);
        accuracy_box.set_margin_bottom(8);
        accuracy_box.set_margin_start(12);
        accuracy_box.set_margin_end(12);

        let correctly_classified_label = Label::new(None);
        correctly_classified_label.set_halign(Align::Start);
        accuracy_box.append(&correctly_classified_label);

        let incorrectly_classified_label = Label::new(None);
        incorrectly_classified_label.set_halign(Align::Start);
        accuracy_box.append(&incorrectly_classified_label);

        let false_positives_label = Label::new(None);
        false_positives_label.set_halign(Align::Start);
        false_positives_label.set_margin_start(20);
        accuracy_box.append(&false_positives_label);

        let false_negatives_label = Label::new(None);
        false_negatives_label.set_halign(Align::Start);
        false_negatives_label.set_margin_start(20);
        accuracy_box.append(&false_negatives_label);

        accuracy_frame.set_child(Some(&accuracy_box));
        content_box.append(&accuracy_frame);

        // ─── 3. Manual Classification (Req 4.3) ─────────────────────────
        let manual_frame = Frame::new(Some("Manual Classification"));
        let manual_box = GtkBox::new(Orientation::Vertical, 4);
        manual_box.set_margin_top(8);
        manual_box.set_margin_bottom(8);
        manual_box.set_margin_start(12);
        manual_box.set_margin_end(12);

        let manually_good_label = Label::new(None);
        manually_good_label.set_halign(Align::Start);
        manual_box.append(&manually_good_label);

        let manually_spam_label = Label::new(None);
        manually_spam_label.set_halign(Align::Start);
        manual_box.append(&manually_spam_label);

        manual_frame.set_child(Some(&manual_box));
        content_box.append(&manual_frame);

        // ─── 4. Identification Percentages (Req 4.4) ────────────────────
        let pct_frame = Frame::new(Some("Identification Rates"));
        let pct_box = GtkBox::new(Orientation::Vertical, 4);
        pct_box.set_margin_top(8);
        pct_box.set_margin_bottom(8);
        pct_box.set_margin_start(12);
        pct_box.set_margin_end(12);

        let spam_correct_pct_label = Label::new(None);
        spam_correct_pct_label.set_halign(Align::Start);
        pct_box.append(&spam_correct_pct_label);

        let good_incorrect_pct_label = Label::new(None);
        good_incorrect_pct_label.set_halign(Align::Start);
        pct_box.append(&good_incorrect_pct_label);

        pct_frame.set_child(Some(&pct_box));
        content_box.append(&pct_frame);

        // ─── 5. Separator ────────────────────────────────────────────────
        let sep = Separator::new(Orientation::Horizontal);
        sep.set_margin_top(4);
        sep.set_margin_bottom(4);
        content_box.append(&sep);

        // ─── 6. Reset Section (Req 4.5) ─────────────────────────────────
        let reset_box = GtkBox::new(Orientation::Horizontal, 12);
        reset_box.set_valign(Align::Center);

        let last_reset_label = Label::new(None);
        last_reset_label.set_halign(Align::Start);
        reset_box.append(&last_reset_label);

        // Spacer to push button to the right
        let spacer = GtkBox::new(Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        reset_box.append(&spacer);

        let reset_btn = Button::with_label("Reset Statistics");
        reset_btn.set_halign(Align::End);
        reset_box.append(&reset_btn);

        content_box.append(&reset_box);

        // ─── Reset button click handler (Req 4.6) ─────────────────────────
        // Clone labels and manager into the closure for the reset handler.
        let total_classified_lbl = total_classified_label.clone();
        let good_lbl = good_label.clone();
        let spam_lbl = spam_label.clone();
        let unsure_lbl = unsure_label.clone();
        let correctly_classified_lbl = correctly_classified_label.clone();
        let incorrectly_classified_lbl = incorrectly_classified_label.clone();
        let false_positives_lbl = false_positives_label.clone();
        let false_negatives_lbl = false_negatives_label.clone();
        let manually_good_lbl = manually_good_label.clone();
        let manually_spam_lbl = manually_spam_label.clone();
        let spam_correct_pct_lbl = spam_correct_pct_label.clone();
        let good_incorrect_pct_lbl = good_incorrect_pct_label.clone();
        let last_reset_lbl = last_reset_label.clone();
        let stats_mgr_clone = statistics_manager.clone();

        reset_btn.connect_clicked(move |_btn| {
            let confirmed = ask_question(
                None,
                "SpamBayes Manager",
                "Reset all lifetime statistics?",
            );
            if confirmed {
                if let Some(ref mgr) = stats_mgr_clone {
                    mgr.reset_lifetime();
                    mgr.reset_session();
                    let new_stats = ManagerStats::from_statistics(mgr);
                    // Update the last reset date to today (YYYY-MM-DD)
                    let today = Self::today_iso_date();
                    let new_stats = ManagerStats {
                        last_reset_date: Some(today),
                        ..new_stats
                    };
                    // Update all labels inline
                    Self::update_labels_static(
                        &new_stats,
                        &total_classified_lbl,
                        &good_lbl,
                        &spam_lbl,
                        &unsure_lbl,
                        &correctly_classified_lbl,
                        &incorrectly_classified_lbl,
                        &false_positives_lbl,
                        &false_negatives_lbl,
                        &manually_good_lbl,
                        &manually_spam_lbl,
                        &spam_correct_pct_lbl,
                        &good_incorrect_pct_lbl,
                        &last_reset_lbl,
                    );
                } else {
                    log::warn!("Reset Statistics: no StatisticsManager available");
                }
            }
        });

        // ─── 7. ScrolledWindow wrapper (Req 4.7) ────────────────────────
        let container = ScrolledWindow::new();
        container.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        container.set_child(Some(&content_box));
        container.set_vexpand(true);
        container.set_hexpand(true);

        let tab = Self {
            container,
            statistics_manager,
            total_classified_label,
            good_label,
            spam_label,
            unsure_label,
            correctly_classified_label,
            incorrectly_classified_label,
            false_positives_label,
            false_negatives_label,
            manually_good_label,
            manually_spam_label,
            spam_correct_pct_label,
            good_incorrect_pct_label,
            last_reset_label,
            reset_btn,
        };

        // Populate initial values
        tab.refresh(stats);

        tab
    }

    /// Update all labels from the given statistics snapshot.
    ///
    /// Call this when statistics change (e.g., after a reset or periodic refresh).
    pub fn refresh(&self, stats: &ManagerStats) {
        // ─── Classification breakdown ────────────────────────────────────
        let total = stats.total_ham_classified
            + stats.total_unsure_classified
            + stats.total_spam_classified;

        self.total_classified_label.set_text(&format!(
            "Messages classified: {}",
            format_with_thousands(total)
        ));

        let (good_pct, spam_pct, unsure_pct) = if total > 0 {
            (
                (stats.total_ham_classified as f64 / total as f64) * 100.0,
                (stats.total_spam_classified as f64 / total as f64) * 100.0,
                (stats.total_unsure_classified as f64 / total as f64) * 100.0,
            )
        } else {
            (0.0, 0.0, 0.0)
        };

        self.good_label.set_text(&format!(
            "Good:  {} ({:.1}%)",
            format_with_thousands(stats.total_ham_classified),
            good_pct
        ));
        self.spam_label.set_text(&format!(
            "Spam:  {} ({:.1}%)",
            format_with_thousands(stats.total_spam_classified),
            spam_pct
        ));
        self.unsure_label.set_text(&format!(
            "Unsure:  {} ({:.1}%)",
            format_with_thousands(stats.total_unsure_classified),
            unsure_pct
        ));

        // ─── Accuracy metrics ────────────────────────────────────────────
        let incorrect = stats.false_positives + stats.false_negatives;
        let correct = stats.correctly_classified;

        let (correct_pct, incorrect_pct, fp_pct, fn_pct) = if total > 0 {
            (
                (correct as f64 / total as f64) * 100.0,
                (incorrect as f64 / total as f64) * 100.0,
                (stats.false_positives as f64 / total as f64) * 100.0,
                (stats.false_negatives as f64 / total as f64) * 100.0,
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };

        self.correctly_classified_label.set_text(&format!(
            "Classified correctly:  {} ({:.1}% of total)",
            format_with_thousands(correct),
            correct_pct
        ));
        self.incorrectly_classified_label.set_text(&format!(
            "Classified incorrectly:  {} ({:.1}% of total)",
            format_with_thousands(incorrect),
            incorrect_pct
        ));
        self.false_positives_label.set_text(&format!(
            "False positives:  {} ({:.1}% of total)",
            format_with_thousands(stats.false_positives),
            fp_pct
        ));
        self.false_negatives_label.set_text(&format!(
            "False negatives:  {} ({:.1}% of total)",
            format_with_thousands(stats.false_negatives),
            fn_pct
        ));

        // ─── Manual classification ───────────────────────────────────────
        self.manually_good_label.set_text(&format!(
            "Manually classified as good:  {}",
            format_with_thousands(stats.manually_classified_good)
        ));
        self.manually_spam_label.set_text(&format!(
            "Manually classified as spam:  {}",
            format_with_thousands(stats.manually_classified_spam)
        ));

        // ─── Identification percentages ──────────────────────────────────
        // Spam correctly identified: of all actual spam (spam_classified + false_negatives),
        // what percentage was correctly caught?
        let total_actual_spam = stats.total_spam_classified + stats.false_negatives;
        let total_actual_ham = stats.total_ham_classified + stats.false_positives;

        let (spam_correct_pct, spam_unsure_pct) = if total_actual_spam > 0 {
            (
                (stats.total_spam_classified as f64 / total_actual_spam as f64) * 100.0,
                (stats.total_unsure_classified as f64 / total_actual_spam as f64) * 100.0,
            )
        } else {
            (0.0, 0.0)
        };

        let (good_incorrect_pct, good_unsure_pct) = if total_actual_ham > 0 {
            (
                (stats.false_positives as f64 / total_actual_ham as f64) * 100.0,
                (stats.total_unsure_classified as f64 / total_actual_ham as f64) * 100.0,
            )
        } else {
            (0.0, 0.0)
        };

        self.spam_correct_pct_label.set_text(&format!(
            "Spam correctly identified:  {:.1}% (+ {:.1}% unsure)",
            spam_correct_pct, spam_unsure_pct
        ));
        self.good_incorrect_pct_label.set_text(&format!(
            "Good incorrectly identified:  {:.1}% (+ {:.1}% unsure)",
            good_incorrect_pct, good_unsure_pct
        ));

        // ─── Last reset date ─────────────────────────────────────────────
        let reset_text = match &stats.last_reset_date {
            Some(date) => format!("Last reset:  {}", date),
            None => "Last reset:  Never".to_string(),
        };
        self.last_reset_label.set_text(&reset_text);
    }

    /// Static helper to update all labels from stats — used by the reset button
    /// closure which cannot call `&self` methods.
    ///
    /// **Validates: Requirement 4.6**
    #[allow(clippy::too_many_arguments)]
    fn update_labels_static(
        stats: &ManagerStats,
        total_classified_label: &Label,
        good_label: &Label,
        spam_label: &Label,
        unsure_label: &Label,
        correctly_classified_label: &Label,
        incorrectly_classified_label: &Label,
        false_positives_label: &Label,
        false_negatives_label: &Label,
        manually_good_label: &Label,
        manually_spam_label: &Label,
        spam_correct_pct_label: &Label,
        good_incorrect_pct_label: &Label,
        last_reset_label: &Label,
    ) {
        // ─── Classification breakdown ────────────────────────────────────
        let total = stats.total_ham_classified
            + stats.total_unsure_classified
            + stats.total_spam_classified;

        total_classified_label.set_text(&format!(
            "Messages classified: {}",
            format_with_thousands(total)
        ));

        let (good_pct, spam_pct, unsure_pct) = if total > 0 {
            (
                (stats.total_ham_classified as f64 / total as f64) * 100.0,
                (stats.total_spam_classified as f64 / total as f64) * 100.0,
                (stats.total_unsure_classified as f64 / total as f64) * 100.0,
            )
        } else {
            (0.0, 0.0, 0.0)
        };

        good_label.set_text(&format!(
            "Good:  {} ({:.1}%)",
            format_with_thousands(stats.total_ham_classified),
            good_pct
        ));
        spam_label.set_text(&format!(
            "Spam:  {} ({:.1}%)",
            format_with_thousands(stats.total_spam_classified),
            spam_pct
        ));
        unsure_label.set_text(&format!(
            "Unsure:  {} ({:.1}%)",
            format_with_thousands(stats.total_unsure_classified),
            unsure_pct
        ));

        // ─── Accuracy metrics ────────────────────────────────────────────
        let incorrect = stats.false_positives + stats.false_negatives;
        let correct = stats.correctly_classified;

        let (correct_pct, incorrect_pct, fp_pct, fn_pct) = if total > 0 {
            (
                (correct as f64 / total as f64) * 100.0,
                (incorrect as f64 / total as f64) * 100.0,
                (stats.false_positives as f64 / total as f64) * 100.0,
                (stats.false_negatives as f64 / total as f64) * 100.0,
            )
        } else {
            (0.0, 0.0, 0.0, 0.0)
        };

        correctly_classified_label.set_text(&format!(
            "Classified correctly:  {} ({:.1}% of total)",
            format_with_thousands(correct),
            correct_pct
        ));
        incorrectly_classified_label.set_text(&format!(
            "Classified incorrectly:  {} ({:.1}% of total)",
            format_with_thousands(incorrect),
            incorrect_pct
        ));
        false_positives_label.set_text(&format!(
            "False positives:  {} ({:.1}% of total)",
            format_with_thousands(stats.false_positives),
            fp_pct
        ));
        false_negatives_label.set_text(&format!(
            "False negatives:  {} ({:.1}% of total)",
            format_with_thousands(stats.false_negatives),
            fn_pct
        ));

        // ─── Manual classification ───────────────────────────────────────
        manually_good_label.set_text(&format!(
            "Manually classified as good:  {}",
            format_with_thousands(stats.manually_classified_good)
        ));
        manually_spam_label.set_text(&format!(
            "Manually classified as spam:  {}",
            format_with_thousands(stats.manually_classified_spam)
        ));

        // ─── Identification percentages ──────────────────────────────────
        let total_actual_spam = stats.total_spam_classified + stats.false_negatives;
        let total_actual_ham = stats.total_ham_classified + stats.false_positives;

        let (spam_correct_pct, spam_unsure_pct) = if total_actual_spam > 0 {
            (
                (stats.total_spam_classified as f64 / total_actual_spam as f64) * 100.0,
                (stats.total_unsure_classified as f64 / total_actual_spam as f64) * 100.0,
            )
        } else {
            (0.0, 0.0)
        };

        let (good_incorrect_pct, good_unsure_pct) = if total_actual_ham > 0 {
            (
                (stats.false_positives as f64 / total_actual_ham as f64) * 100.0,
                (stats.total_unsure_classified as f64 / total_actual_ham as f64) * 100.0,
            )
        } else {
            (0.0, 0.0)
        };

        spam_correct_pct_label.set_text(&format!(
            "Spam correctly identified:  {:.1}% (+ {:.1}% unsure)",
            spam_correct_pct, spam_unsure_pct
        ));
        good_incorrect_pct_label.set_text(&format!(
            "Good incorrectly identified:  {:.1}% (+ {:.1}% unsure)",
            good_incorrect_pct, good_unsure_pct
        ));

        // ─── Last reset date ─────────────────────────────────────────────
        let reset_text = match &stats.last_reset_date {
            Some(date) => format!("Last reset:  {}", date),
            None => "Last reset:  Never".to_string(),
        };
        last_reset_label.set_text(&reset_text);
    }

    /// Get today's date as an ISO 8601 string (YYYY-MM-DD) using GLib.
    fn today_iso_date() -> String {
        match glib::DateTime::now_local() {
            Ok(dt) => dt.format("%Y-%m-%d").map_or_else(
                |_| "Unknown".to_string(),
                |s| s.to_string(),
            ),
            Err(_) => "Unknown".to_string(),
        }
    }
}
