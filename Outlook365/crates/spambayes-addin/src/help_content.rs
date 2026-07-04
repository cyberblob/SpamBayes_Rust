//! Help content module for the SpamBayes add-in dialogs.
//!
//! Stores all help text as structured constants, organized by section.
//! This module is separate from dialog logic to enable future localization
//! or external help file loading.
//!
//! **Validates: Requirements 4.1, 4.2, 4.3, 4.4**

/// A structured help entry for a dialog section or wizard page.
///
/// Each entry contains a title, a description paragraph, and an optional
/// tips/notes section with practical guidance.
pub struct HelpEntry {
    /// Short title identifying the help topic (e.g., "Filter Settings").
    pub title: &'static str,
    /// Description paragraph explaining the section's purpose and behavior.
    pub description: &'static str,
    /// Optional tips or notes providing practical guidance.
    pub tips: Option<&'static str>,
}

/// Tooltip text associated with a specific dialog control.
///
/// Each tooltip maps a control ID to a concise description of that
/// control's effect.
pub struct TooltipText {
    /// The Win32 control identifier (IDC_*) this tooltip applies to.
    pub control_id: u16,
    /// Brief description of the control's purpose (one or two sentences).
    pub text: &'static str,
}

/// Help entries organized by dialog section.
///
/// Each constant provides structured help content for a logical section
/// of the Manager dialog or a Wizard page.
pub mod sections {
    use super::HelpEntry;

    /// Help content for the Filter Settings section of the Manager dialog.
    pub const FILTER_SETTINGS: HelpEntry = HelpEntry {
        title: "Filter Settings",
        description: "Configure how SpamBayes classifies incoming mail. The spam threshold \
            sets the score above which messages are treated as spam, while the unsure threshold \
            marks the boundary for uncertain messages. You can also choose what action to take \
            for each classification (move to folder, delete, or leave in place) and enable or \
            disable filtering entirely.",
        tips: Some(
            "• Start with default thresholds (90/15) and adjust after reviewing a week of results\n\
             • Lowering the spam threshold catches more spam but risks false positives\n\
             • Disable filtering temporarily if you need to troubleshoot missed messages"
        ),
    };

    /// Help content for the Folder Configuration section of the Manager dialog.
    pub const FOLDER_CONFIG: HelpEntry = HelpEntry {
        title: "Folder Configuration",
        description: "Select which folders SpamBayes monitors and where it moves classified \
            messages. The watched folder is checked for new mail, spam is moved to the spam \
            folder, and uncertain messages go to the unsure folder. Training folders tell \
            SpamBayes where to find confirmed ham and spam for learning.",
        tips: Some(
            "• Use a dedicated spam folder rather than Deleted Items so you can review catches\n\
             • Check the unsure folder regularly and train those messages to improve accuracy\n\
             • Training folders can be the same as your spam/unsure folders for convenience"
        ),
    };

    /// Help content for the Training section of the Manager dialog.
    pub const TRAINING: HelpEntry = HelpEntry {
        title: "Training",
        description: "Train SpamBayes to recognize your ham and spam. Use Train Now to perform \
            a full batch retrain from your training folders, or rebuild the database from scratch \
            if results have degraded. Incremental training happens automatically when you use the \
            Spam and Not Spam buttons on individual messages.",
        tips: Some(
            "• Train with at least 20 ham and 20 spam messages for reasonable initial accuracy\n\
             • Rebuild the database if you notice accuracy declining after many months of use\n\
             • Incremental training on mistakes is the fastest way to improve classification"
        ),
    };

    /// Help content for the Notification section of the Manager dialog.
    pub const NOTIFICATION: HelpEntry = HelpEntry {
        title: "Notification",
        description: "Control how SpamBayes alerts you about classified messages. You can \
            enable sounds for each classification type (ham, spam, unsure). The accumulate \
            timer batches rapid arrivals into a single notification instead of playing a sound \
            for every message in a burst.",
        tips: Some(
            "• Enable the ham sound only if you want confirmation that good mail arrived\n\
             • Increase the accumulate timer if you receive many messages at once"
        ),
    };

    /// Help content for the Cleanup section of the Manager dialog.
    pub const CLEANUP: HelpEntry = HelpEntry {
        title: "Cleanup",
        description: "Automatically remove old spam messages to keep your mailbox tidy. When \
            enabled, SpamBayes deletes confirmed spam older than the configured retention \
            period. This only affects messages in the spam folder that were classified by \
            SpamBayes.",
        tips: Some(
            "• Set retention to at least 7 days so you can recover false positives\n\
             • Review the spam folder before enabling auto-delete for the first time\n\
             • Cleanup runs once per Outlook session at startup"
        ),
    };

    // -- Wizard pages --

    /// Help entry for the Configuration Wizard Welcome page.
    pub const WIZARD_WELCOME: HelpEntry = HelpEntry {
        title: "Welcome to SpamBayes",
        description: "This wizard guides you through the initial setup of SpamBayes. \
            SpamBayes uses statistical analysis to classify incoming email as ham (good), \
            spam (bad), or unsure, and automatically moves messages to the appropriate folder.",
        tips: Some(
            "• You can re-run this wizard later from the SpamBayes Manager\n\
             • The wizard only takes a minute — most defaults work well out of the box"
        ),
    };

    /// Help entry for the Watch Folders wizard page.
    pub const WIZARD_WATCH_FOLDERS: HelpEntry = HelpEntry {
        title: "Watch Folders",
        description: "Select which Outlook folders SpamBayes should monitor for new incoming \
            mail. When a message arrives in a watched folder, SpamBayes scores it and moves \
            spam or unsure messages to their designated folders.",
        tips: Some(
            "• The default watched folder is Junk Email, where Exchange places suspected spam\n\
             • You can watch multiple folders if you receive mail in more than one location\n\
             • Only add folders that receive new mail — avoid sent or archive folders"
        ),
    };

    /// Help entry for the Spam Folder wizard page.
    pub const WIZARD_SPAM_FOLDER: HelpEntry = HelpEntry {
        title: "Spam Folder",
        description: "Choose the destination folder for messages SpamBayes classifies as spam. \
            Messages scoring above the spam threshold are moved here automatically. You can \
            review this folder periodically and permanently delete its contents.",
        tips: Some(
            "• A dedicated spam folder keeps spam out of your Inbox without losing messages\n\
             • Check the spam folder occasionally for false positives, especially during initial training"
        ),
    };

    /// Help entry for the Unsure Folder wizard page.
    pub const WIZARD_UNSURE_FOLDER: HelpEntry = HelpEntry {
        title: "Unsure Folder",
        description: "Choose the destination folder for messages SpamBayes is not confident \
            about. These are messages that score between the unsure and spam thresholds. \
            Reviewing unsure messages and training on them improves classification accuracy.",
        tips: Some(
            "• Training on unsure messages is the fastest way to improve SpamBayes accuracy\n\
             • If very few messages land here, your thresholds may be well tuned\n\
             • Use the Not Spam or Delete As Spam buttons to train on unsure messages"
        ),
    };

    /// Help entry for the Training Folders wizard page.
    pub const WIZARD_TRAINING_FOLDERS: HelpEntry = HelpEntry {
        title: "Training Folders",
        description: "Select folders containing known good mail (ham) and known spam for \
            initial training. SpamBayes learns word patterns from these messages to build \
            its classifier database. More training data generally means better accuracy.",
        tips: Some(
            "• Aim for at least 20-50 messages of each type for a reasonable starting point\n\
             • Use your Inbox or Sent Items as a ham source\n\
             • A balanced mix of ham and spam produces the best classification results"
        ),
    };

    /// Help entry for the Wizard Finish page.
    pub const WIZARD_FINISH: HelpEntry = HelpEntry {
        title: "Setup Complete",
        description: "Your SpamBayes configuration is ready. After closing this wizard, \
            SpamBayes will begin monitoring your selected folders and classifying incoming \
            mail. Classification improves over time as you train on misclassified messages.",
        tips: Some(
            "• Use the toolbar buttons to correct any mistakes — this trains the classifier\n\
             • Open the SpamBayes Manager to adjust thresholds or re-train later"
        ),
    };
}

/// Tooltip text for dialog controls.
///
/// Organized by dialog: Manager tooltips and Wizard tooltips.
///
/// **Validates: Requirements 1.1, 1.3**
pub mod tooltips {
    use super::TooltipText;

    // ─── Wizard Dialog Control IDs ───────────────────────────────────────────
    // These must match the IDC_* constants in wizard.rs and the .rc file.

    const IDC_WIZARD_BACK: u16 = 2001;
    const IDC_WIZARD_NEXT: u16 = 2002;
    const IDC_WIZARD_CANCEL: u16 = 2003;
    const IDC_WIZARD_FINISH: u16 = 2004;

    /// Tooltip text for Wizard dialog navigation controls.
    ///
    /// The wizard has fewer interactive controls than the Manager, so tooltips
    /// are provided for the navigation buttons that appear on every page.
    pub const WIZARD_TOOLTIPS: &[TooltipText] = &[
        TooltipText {
            control_id: IDC_WIZARD_BACK,
            text: "Go back to the previous setup step to review or change your selections.",
        },
        TooltipText {
            control_id: IDC_WIZARD_NEXT,
            text: "Continue to the next setup step. Your current selections are saved.",
        },
        TooltipText {
            control_id: IDC_WIZARD_CANCEL,
            text: "Cancel the wizard without saving. SpamBayes will ask again next time Outlook starts.",
        },
        TooltipText {
            control_id: IDC_WIZARD_FINISH,
            text: "Complete setup and apply your configuration. SpamBayes will begin filtering.",
        },
    ];

    // ─── Manager Dialog Control IDs ──────────────────────────────────────────
    // These must match the IDC_* constants in manager_dlg.rs and the .rc file.

    // Filter settings controls
    const IDC_SPAM_THRESHOLD: u16 = 3010;
    const IDC_UNSURE_THRESHOLD: u16 = 3011;
    const IDC_SPAM_ACTION: u16 = 3012;
    const IDC_UNSURE_ACTION: u16 = 3013;
    const IDC_HAM_ACTION: u16 = 3014;
    const IDC_ENABLE_FILTERING: u16 = 3015;

    // Browse buttons for folder pickers
    const IDC_BROWSE_WATCH: u16 = 3030;
    const IDC_BROWSE_SPAM_FOLDER: u16 = 3031;
    const IDC_BROWSE_UNSURE_FOLDER: u16 = 3032;
    const IDC_BROWSE_HAM_TRAIN: u16 = 3033;
    const IDC_BROWSE_SPAM_TRAIN: u16 = 3034;

    // Action buttons
    const IDC_TRAIN_NOW: u16 = 3040;
    const IDC_FILTER_NOW: u16 = 3041;

    // Cleanup controls (must match .rc file when authored)
    const IDC_CLEANUP_ENABLE: u16 = 3050;
    const IDC_CLEANUP_DAYS: u16 = 3051;

    // Notification controls (must match .rc file when authored)
    const IDC_NOTIFY_SOUND_ENABLE: u16 = 3060;
    const IDC_NOTIFY_ACCUMULATE: u16 = 3061;

    /// Tooltip text for all Manager dialog controls.
    ///
    /// Each entry maps a control ID to a concise, actionable description
    /// of what the control does.
    pub const MANAGER_TOOLTIPS: &[TooltipText] = &[
        // ─── Filter Threshold Controls ───────────────────────────────────────
        TooltipText {
            control_id: IDC_SPAM_THRESHOLD,
            text: "Set the score above which messages are classified as spam. \
                   Higher values are more strict (fewer false positives, more missed spam).",
        },
        TooltipText {
            control_id: IDC_UNSURE_THRESHOLD,
            text: "Set the score above which messages are classified as unsure. \
                   Messages scoring between this and the spam threshold land in the unsure folder.",
        },
        // ─── Action Combo Boxes ──────────────────────────────────────────────
        TooltipText {
            control_id: IDC_SPAM_ACTION,
            text: "Choose what happens to messages classified as spam: \
                   move to the spam folder, delete permanently, or do nothing.",
        },
        TooltipText {
            control_id: IDC_UNSURE_ACTION,
            text: "Choose what happens to messages classified as unsure: \
                   move to the unsure folder, leave in place, or do nothing.",
        },
        TooltipText {
            control_id: IDC_HAM_ACTION,
            text: "Choose what happens to messages classified as good (ham): \
                   leave in place, move to a specific folder, or do nothing.",
        },
        // ─── Enable Filtering ────────────────────────────────────────────────
        TooltipText {
            control_id: IDC_ENABLE_FILTERING,
            text: "Turn message filtering on or off. \
                   When disabled, new messages are not classified or moved.",
        },
        // ─── Folder Browse Buttons ───────────────────────────────────────────
        TooltipText {
            control_id: IDC_BROWSE_WATCH,
            text: "Open the folder picker to select which folders SpamBayes monitors \
                   for incoming messages.",
        },
        TooltipText {
            control_id: IDC_BROWSE_SPAM_FOLDER,
            text: "Open the folder picker to choose where spam messages are moved.",
        },
        TooltipText {
            control_id: IDC_BROWSE_UNSURE_FOLDER,
            text: "Open the folder picker to choose where unsure messages are moved.",
        },
        TooltipText {
            control_id: IDC_BROWSE_HAM_TRAIN,
            text: "Open the folder picker to select the folder containing known good messages \
                   used for training.",
        },
        TooltipText {
            control_id: IDC_BROWSE_SPAM_TRAIN,
            text: "Open the folder picker to select the folder containing known spam messages \
                   used for training.",
        },
        // ─── Action Buttons ──────────────────────────────────────────────────
        TooltipText {
            control_id: IDC_TRAIN_NOW,
            text: "Re-train the classifier using the messages in your ham and spam training folders. \
                   This rebuilds the database from scratch.",
        },
        TooltipText {
            control_id: IDC_FILTER_NOW,
            text: "Apply the spam filter to all messages currently in your watched folders. \
                   Messages are classified and moved according to your action settings.",
        },
        // ─── Cleanup Controls ────────────────────────────────────────────────
        TooltipText {
            control_id: IDC_CLEANUP_ENABLE,
            text: "Enable automatic deletion of old spam. \
                   When active, messages in the spam folder older than the specified days are removed.",
        },
        TooltipText {
            control_id: IDC_CLEANUP_DAYS,
            text: "Set the number of days to keep spam before automatic deletion. \
                   Messages older than this are permanently removed during cleanup.",
        },
        // ─── Notification Controls ───────────────────────────────────────────
        TooltipText {
            control_id: IDC_NOTIFY_SOUND_ENABLE,
            text: "Enable sound notifications when messages are classified. \
                   A different sound plays for ham, unsure, and spam results.",
        },
        TooltipText {
            control_id: IDC_NOTIFY_ACCUMULATE,
            text: "Set how long to wait before playing a notification sound. \
                   Groups rapid arrivals into a single notification.",
        },
    ];
}

/// Error guidance messages for validation errors.
///
/// Each message explains what went wrong and tells the user what to do.
///
/// **Validates: Requirements 5.1, 5.2, 5.3**
pub mod errors {
    /// Guidance when a threshold value is outside the valid range or
    /// the unsure threshold exceeds the spam threshold.
    pub const THRESHOLD_INVALID: &str = "\
The threshold value is out of range.\n\n\
Both the spam threshold and the unsure threshold must be whole numbers \
between 0 and 100. The unsure threshold must be less than or equal to \
the spam threshold.\n\n\
To fix this, enter a number between 0 and 100 for each threshold and \
make sure the unsure value does not exceed the spam value. Typical \
defaults are 90 for spam and 15 for unsure.";

    /// Guidance when the cleanup days value is not a valid positive integer.
    pub const CLEANUP_DAYS_INVALID: &str = "\
The cleanup days value is not valid.\n\n\
The number of days to keep spam before automatic deletion must be a \
positive whole number (1 or greater).\n\n\
To fix this, enter a number like 7 or 30 to set how many days spam is \
kept before SpamBayes removes it. If you do not want automatic cleanup, \
disable the feature instead of entering zero.";

    /// Guidance when a configured folder no longer exists in Outlook.
    pub const FOLDER_NOT_FOUND: &str = "\
A configured folder could not be found in Outlook.\n\n\
The folder may have been renamed, moved, or deleted. SpamBayes cannot \
filter or train without valid folder paths.\n\n\
To fix this, open the SpamBayes Manager, go to Folder Configuration, \
and use the Browse button to select a new folder for the affected role. \
If the folder was accidentally deleted, you can recreate it in Outlook \
and then re-select it.";

    /// Guidance when the user attempts to filter without a trained classifier.
    pub const TRAINING_REQUIRED: &str = "\
SpamBayes needs training before it can filter messages.\n\n\
The classifier has not been trained with any examples yet. Without \
training data, SpamBayes cannot distinguish spam from good mail and \
all messages will be scored as unsure.\n\n\
To fix this, open the SpamBayes Manager and click Train Now. You need \
at least a few examples of both good mail (ham) and spam in the \
designated training folders. The more examples you provide, the more \
accurate filtering will be.";
}
