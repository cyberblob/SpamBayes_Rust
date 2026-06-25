//! SpamBayes Manager Launcher

#![windows_subsystem = "windows"]

use std::process::Command;
use std::path::Path;

fn main() {
    let script = r"D:\My Apps\SpamBayes_Rust\Outlook2000\launch_manager.py";
    let python = r"c:\python\pythonw.exe";
    let python_fallback = r"c:\python\python.exe";
    let spambayes_root = r"D:\My Apps\SpamBayes_Rust";

    let py = if Path::new(python).exists() {
        python
    } else {
        python_fallback
    };

    if Path::new(py).exists() && Path::new(script).exists() {
        use std::os::windows::process::CommandExt;
        let _ = Command::new(py)
            .arg(script)
            .current_dir(r"D:\My Apps\SpamBayes_Rust\Outlook2000")
            .env("PYTHONPATH", spambayes_root)
            .creation_flags(0x08000000) // CREATE_NO_WINDOW
            .spawn();
    }
}

