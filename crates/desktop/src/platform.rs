//! Operating-system integration through the tools each platform ships with:
//! opening URLs and folders, and the native image picker. No shell is
//! involved: every argument is passed to the program directly.
//!
//! The picker blocks until the user answers, so call it on a worker thread.
//!
//! ponytail: the picker runs a helper process (PowerShell / `osascript` /
//! `zenity`/`kdialog`) instead of linking the platform dialog APIs; it takes
//! a moment to appear. Swap in direct Win32/Cocoa/portal calls if that grates.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Configures `command` to open no console window (Windows only; a GUI app
/// spawning console programs would otherwise flash one).
pub fn no_window(command: &mut Command) -> &mut Command {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Starts `command` detached from our stdio and reaps it in the background.
fn launch(mut command: Command) -> io::Result<()> {
    let mut child = no_window(&mut command).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}

/// Opens `url` in the default browser.
///
/// # Errors
/// The launcher could not be started.
pub fn open_url(url: &str) -> io::Result<()> {
    let command = if cfg!(target_os = "windows") {
        let mut c = Command::new("rundll32");
        c.args(["url.dll,FileProtocolHandler", url]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(url);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(url);
        c
    };
    launch(command)
}

/// Shows `path` in the file manager.
///
/// # Errors
/// The file manager could not be started.
pub fn open_folder(path: &Path) -> io::Result<()> {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut command = Command::new(program);
    command.arg(path);
    launch(command)
}

/// Runs a picker helper and returns the non-empty lines it printed.
fn run_picker(mut command: Command) -> io::Result<Vec<String>> {
    let output = no_window(&mut command).stdin(Stdio::null()).stderr(Stdio::null()).output()?;
    // A cancelled dialog exits non-zero or prints nothing; both mean "none".
    Ok(String::from_utf8_lossy(&output.stdout).lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

/// PowerShell prelude: an invisible topmost owner so the dialog appears in
/// front of our window, and UTF-8 output.
const PS_PRELUDE: &str = "Add-Type -AssemblyName System.Windows.Forms; \
    [Console]::OutputEncoding = [Text.Encoding]::UTF8; \
    $owner = New-Object System.Windows.Forms.Form -Property @{TopMost = $true; ShowInTaskbar = $false; Opacity = 0}; \
    $owner.Show(); $owner.Activate();";

fn powershell(script: &str) -> io::Result<Vec<String>> {
    // Windows PowerShell 5 is always present as a fallback for PowerShell 7.
    let run = |program: &str| {
        let mut command = Command::new(program);
        command.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &format!("{PS_PRELUDE} {script}")]);
        run_picker(command)
    };
    run("pwsh").or_else(|_| run("powershell"))
}

/// Asks the user for a PNG or JPEG image. `None` when cancelled.
///
/// # Errors
/// No dialog helper could be started.
pub fn pick_image() -> io::Result<Option<PathBuf>> {
    let lines = if cfg!(target_os = "windows") {
        powershell(
            "$d = New-Object System.Windows.Forms.OpenFileDialog -Property @{Title = 'Choose a portrait'; Filter = 'Images|*.png;*.jpg;*.jpeg'}; \
             if ($d.ShowDialog($owner) -eq 'OK') { $d.FileName }",
        )?
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("osascript");
        command.args(["-e", "POSIX path of (choose file with prompt \"Choose a portrait\" of type {\"public.png\", \"public.jpeg\"})"]);
        run_picker(command)?
    } else {
        let mut command = Command::new("zenity");
        command.args(["--file-selection", "--title=Choose a portrait", "--file-filter=Images | *.png *.jpg *.jpeg"]);
        run_picker(command).or_else(|_| {
            let mut command = Command::new("kdialog");
            command.args(["--getopenfilename", ".", "*.png *.jpg *.jpeg|Images", "--title", "Choose a portrait"]);
            run_picker(command)
        })?
    };
    Ok(lines.into_iter().next().map(PathBuf::from))
}
