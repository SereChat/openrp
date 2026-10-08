//! Operating-system integration through the tools each platform ships with:
//! opening URLs and folders, the native file pickers (open and save), and
//! the keychain.
//! No shell is involved: every argument is passed to the program directly,
//! and text that is not ours never becomes part of a script.
//!
//! The pickers block until the user answers, so call them on a worker thread.
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

/// Shows a native error dialog with `title` and `message` and waits until
/// the user closes it. For failures the window cannot show (it failed to
/// start, or the app is crashing); safe from any thread. Does nothing where
/// no dialog helper is installed.
pub fn alert(title: &str, message: &str) {
    #[cfg(target_os = "windows")]
    {
        // `MessageBoxW` from user32, which every Windows GUI process loads.
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(owner: *mut std::ffi::c_void, text: *const u16, caption: *const u16, kind: u32) -> i32;
        }
        /// `MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST`.
        const KIND: u32 = 0x10 | 0x1_0000 | 0x4_0000;
        let (text, caption) = (wide(message), wide(title));
        // SAFETY: both strings are NUL-terminated UTF-16 that outlive the
        // call, and a null owner is allowed.
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), KIND);
        }
    }
    #[cfg(target_os = "macos")]
    {
        // The text goes in as arguments, never into the script itself.
        let mut command = Command::new("osascript");
        command.args(["-e", "on run argv", "-e", "display alert (item 1 of argv) message (item 2 of argv) as critical", "-e", "end run", title, message]);
        let _ = command.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let mut zenity = Command::new("zenity");
        zenity.args(["--error", "--no-markup", "--title", title, "--text", message]);
        let shown = zenity.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok();
        if !shown {
            let mut kdialog = Command::new("kdialog");
            kdialog.args(["--title", title, "--error", message]);
            let _ = kdialog.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
        }
    }
}

/// `s` as NUL-terminated UTF-16 for Win32, without inner NULs.
#[cfg(target_os = "windows")]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().filter(|&c| c != 0).chain([0]).collect()
}

/// The keychain service OpenRP's secrets are kept under.
const KEYCHAIN_SERVICE: &str = "OpenRP";

/// Credential Manager, through `advapi32`, which every Windows process loads.
#[cfg(target_os = "windows")]
mod keychain {
    use std::ffi::c_void;
    use std::io;

    use super::{KEYCHAIN_SERVICE, wide};

    /// `CREDENTIALW` from `wincred.h`.
    #[repr(C)]
    struct Credential {
        flags: u32,
        kind: u32,
        target_name: *mut u16,
        comment: *mut u16,
        last_written: [u32; 2],
        blob_size: u32,
        blob: *mut u8,
        persist: u32,
        attribute_count: u32,
        attributes: *mut c_void,
        target_alias: *mut u16,
        user_name: *mut u16,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn CredReadW(target: *const u16, kind: u32, flags: u32, credential: *mut *mut Credential) -> i32;
        fn CredWriteW(credential: *const Credential, flags: u32) -> i32;
        fn CredDeleteW(target: *const u16, kind: u32, flags: u32) -> i32;
        fn CredFree(buffer: *mut c_void);
    }

    /// `CRED_TYPE_GENERIC`.
    const GENERIC: u32 = 1;
    /// `CRED_PERSIST_LOCAL_MACHINE`: this user on this computer, not roaming.
    const LOCAL_MACHINE: u32 = 2;
    /// `ERROR_NOT_FOUND`.
    const NOT_FOUND: i32 = 1168;

    fn target(account: &str) -> Vec<u16> {
        wide(&format!("{KEYCHAIN_SERVICE}/{account}"))
    }

    /// See [`super::secret`].
    pub(super) fn secret(account: &str) -> io::Result<Option<String>> {
        let target = target(account);
        let mut credential: *mut Credential = std::ptr::null_mut();
        // SAFETY: `target` is NUL-terminated UTF-16 that outlives the call.
        if unsafe { CredReadW(target.as_ptr(), GENERIC, 0, &raw mut credential) } == 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(NOT_FOUND) { Ok(None) } else { Err(e) };
        }
        // SAFETY: the read succeeded, so `credential` points at a credential
        // whose blob holds `blob_size` bytes until `CredFree`.
        let bytes = unsafe {
            let found = &*credential;
            let bytes = if found.blob.is_null() { Vec::new() } else { std::slice::from_raw_parts(found.blob, found.blob_size as usize).to_vec() };
            CredFree(credential.cast());
            bytes
        };
        String::from_utf8(bytes).map(Some).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the saved secret is not text"))
    }

    /// See [`super::keep_secret`].
    pub(super) fn keep_secret(account: &str, secret: Option<&str>) -> io::Result<()> {
        let mut target = target(account);
        let Some(secret) = secret else {
            // SAFETY: as in `secret`.
            if unsafe { CredDeleteW(target.as_ptr(), GENERIC, 0) } != 0 {
                return Ok(());
            }
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(NOT_FOUND) { Ok(()) } else { Err(e) };
        };
        let (mut user, mut blob) = (wide(account), secret.as_bytes().to_vec());
        let credential = Credential {
            flags: 0,
            kind: GENERIC,
            target_name: target.as_mut_ptr(),
            comment: std::ptr::null_mut(),
            last_written: [0; 2],
            blob_size: u32::try_from(blob.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?,
            blob: blob.as_mut_ptr(),
            persist: LOCAL_MACHINE,
            attribute_count: 0,
            attributes: std::ptr::null_mut(),
            target_alias: std::ptr::null_mut(),
            user_name: user.as_mut_ptr(),
        };
        // SAFETY: every pointer in `credential` is valid for the call, which
        // only reads through them.
        if unsafe { CredWriteW(&raw const credential, 0) } == 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
    }
}

/// The login keychain through `security` on macOS; elsewhere the Secret
/// Service (GNOME Keyring, `KWallet`) through `secret-tool`.
#[cfg(unix)]
mod keychain {
    use std::io::{self, Write as _};
    use std::process::{Command, Stdio};

    use super::KEYCHAIN_SERVICE as SERVICE;

    /// See [`super::secret`].
    pub(super) fn secret(account: &str) -> io::Result<Option<String>> {
        let mut command = if cfg!(target_os = "macos") {
            tool("security", &["find-generic-password", "-s", SERVICE, "-a", account, "-w"])
        } else {
            tool("secret-tool", &["lookup", "service", SERVICE, "account", account])
        };
        Ok(run(&mut command, None)?.filter(|found| !found.is_empty()))
    }

    /// See [`super::keep_secret`].
    pub(super) fn keep_secret(account: &str, secret: Option<&str>) -> io::Result<()> {
        let macos = cfg!(target_os = "macos");
        let (mut command, input) = match secret {
            // ponytail: the secret is on `security`'s command line for a
            // moment, as it reads no stdin; link Security.framework's SecItem
            // calls to avoid that. Items `security` makes open without a
            // prompt after an update, which an unsigned app's own would not.
            Some(secret) if macos => (tool("security", &["add-generic-password", "-U", "-s", SERVICE, "-a", account, "-l", SERVICE, "-w", secret]), None),
            Some(secret) => (tool("secret-tool", &["store", "--label", SERVICE, "service", SERVICE, "account", account]), Some(secret)),
            None if macos => (tool("security", &["delete-generic-password", "-s", SERVICE, "-a", account]), None),
            None => (tool("secret-tool", &["clear", "service", SERVICE, "account", account]), None),
        };
        match run(&mut command, input)? {
            None if secret.is_some() => Err(io::Error::other("the keychain did not take it")),
            _ => Ok(()),
        }
    }

    fn tool(program: &str, args: &[&str]) -> Command {
        let mut command = Command::new(program);
        command.args(args);
        command
    }

    /// Runs a keychain helper, writing `input` to it: what it printed, or
    /// `None` when it found nothing (`security` exits 44, `secret-tool`
    /// fails without a word).
    fn run(command: &mut Command, input: Option<&str>) -> io::Result<Option<String>> {
        let spawned = command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn();
        let mut child = spawned.map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => io::Error::new(e.kind(), "secret-tool is not installed (it comes with libsecret)"),
            _ => e,
        })?;
        if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
            stdin.write_all(input.as_bytes())?;
        }
        let output = child.wait_with_output()?;
        let printed = String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_owned();
        let complaint = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        match output.status.code() {
            Some(0) => Ok(Some(printed)),
            Some(44) => Ok(None),
            _ if complaint.is_empty() => Ok(None),
            _ => Err(io::Error::other(complaint)),
        }
    }
}

/// The secret kept in the OS keychain as `account`, if any: Credential
/// Manager on Windows, the login keychain on macOS, the Secret Service
/// elsewhere. Can wait for the user to unlock the keychain.
///
/// # Errors
/// The keychain could not be read, or there is none.
pub fn secret(account: &str) -> io::Result<Option<String>> {
    keychain::secret(account)
}

/// Keeps `secret` in the OS keychain as `account` (see [`secret`]),
/// replacing what was there; `None` removes it.
///
/// # Errors
/// The keychain refused, or there is none.
pub fn keep_secret(account: &str, secret: Option<&str>) -> io::Result<()> {
    keychain::keep_secret(account, secret)
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

/// Runs `script` in PowerShell with `vars` in its environment. Text that is
/// not ours (a story's title) goes in as a variable, never into the script.
fn powershell(script: &str, vars: &[(&str, &str)]) -> io::Result<Vec<String>> {
    // Windows PowerShell 5 is always present as a fallback for PowerShell 7.
    let run = |program: &str| {
        let mut command = Command::new(program);
        command.args(["-NoProfile", "-NonInteractive", "-STA", "-Command", &format!("{PS_PRELUDE} {script}")]);
        command.envs(vars.iter().copied());
        run_picker(command)
    };
    run("pwsh").or_else(|_| run("powershell"))
}

/// Kinds of file a picker offers: a name and the extensions, without dots.
pub type Filter<'a> = (&'a str, &'a [&'a str]);

/// The portrait picker's file types.
const IMAGES: Filter<'static> = ("Images", &["png", "jpg", "jpeg"]);

/// Asks the user for a PNG or JPEG image. `None` when cancelled.
///
/// # Errors
/// No dialog helper could be started.
pub fn pick_image() -> io::Result<Option<PathBuf>> {
    Ok(pick_files("Choose a portrait", IMAGES, false)?.into_iter().next())
}

/// Asks the user for files of the kinds `filter` names (several when
/// `multiple`); none when cancelled.
///
/// # Errors
/// No dialog helper could be started.
pub fn pick_files(title: &str, (kind, extensions): Filter<'_>, multiple: bool) -> io::Result<Vec<PathBuf>> {
    let lines = if cfg!(target_os = "windows") {
        let filter = format!("{kind}|{}", extensions.iter().map(|e| format!("*.{e}")).collect::<Vec<_>>().join(";"));
        let multiple = if multiple { "$true" } else { "$false" };
        powershell(
            &format!(
                "$d = New-Object System.Windows.Forms.OpenFileDialog -Property @{{Title = $env:OPENRP_TITLE; Filter = $env:OPENRP_FILTER; Multiselect = {multiple}}}; \
                 if ($d.ShowDialog($owner) -eq 'OK') {{ $d.FileNames }}"
            ),
            &[("OPENRP_TITLE", title), ("OPENRP_FILTER", &filter)],
        )?
    } else if cfg!(target_os = "macos") {
        // Type identifiers, from our own list only: never text from elsewhere.
        let types: Vec<String> = extensions.iter().map(|e| format!("\"{}\"", uti(e))).collect();
        let several = if multiple { " with multiple selections allowed" } else { "" };
        let script = format!(
            "set picked to choose file with prompt (item 1 of argv) of type {{{}}}{several}\n\
             if class of picked is not list then set picked to {{picked}}\n\
             set out to \"\"\n\
             repeat with f in picked\n\
             set out to out & POSIX path of f & linefeed\n\
             end repeat\n\
             return out",
            types.join(", ")
        );
        let mut command = Command::new("osascript");
        command.args(["-e", "on run argv", "-e", &script, "-e", "end run", title]);
        run_picker(command)?
    } else {
        let patterns: Vec<String> = extensions.iter().map(|e| format!("*.{e}")).collect();
        let mut command = Command::new("zenity");
        command.args(["--file-selection", &format!("--title={title}"), &format!("--file-filter={kind} | {}", patterns.join(" "))]);
        if multiple {
            command.args(["--multiple", "--separator=\n"]);
        }
        run_picker(command).or_else(|_| {
            let mut command = Command::new("kdialog");
            command.args(["--getopenfilename", ".", &format!("{}|{kind}", patterns.join(" ")), "--title", title]);
            if multiple {
                command.args(["--multiple", "--separate-output"]);
            }
            run_picker(command)
        })?
    };
    Ok(lines.into_iter().map(PathBuf::from).collect())
}

/// Asks the user where to save a file, suggesting `name` (made safe for a
/// file name) of the kind `filter` names. `None` when cancelled.
///
/// # Errors
/// No dialog helper could be started.
pub fn save_file(title: &str, name: &str, (kind, extensions): Filter<'_>) -> io::Result<Option<PathBuf>> {
    let extension = extensions.first().copied().unwrap_or("txt");
    let name = format!("{}.{extension}", file_name(name));
    let lines = if cfg!(target_os = "windows") {
        let filter = format!("{kind}|*.{extension}");
        powershell(
            "$d = New-Object System.Windows.Forms.SaveFileDialog -Property @{Title = $env:OPENRP_TITLE; Filter = $env:OPENRP_FILTER; FileName = $env:OPENRP_NAME; OverwritePrompt = $true}; \
             if ($d.ShowDialog($owner) -eq 'OK') { $d.FileName }",
            &[("OPENRP_TITLE", title), ("OPENRP_FILTER", &filter), ("OPENRP_NAME", &name)],
        )?
    } else if cfg!(target_os = "macos") {
        let mut command = Command::new("osascript");
        let script = "POSIX path of (choose file name with prompt (item 1 of argv) default name (item 2 of argv))";
        command.args(["-e", "on run argv", "-e", script, "-e", "end run", title, &name]);
        run_picker(command)?
    } else {
        let mut command = Command::new("zenity");
        command.args(["--file-selection", "--save", &format!("--title={title}"), &format!("--filename={name}")]);
        command.arg(format!("--file-filter={kind} | *.{extension}"));
        run_picker(command).or_else(|_| {
            let mut command = Command::new("kdialog");
            command.args(["--getsavefilename", &name, &format!("*.{extension}|{kind}"), "--title", title]);
            run_picker(command)
        })?
    };
    Ok(lines.into_iter().next().map(PathBuf::from))
}

/// macOS's type identifier for a file extension.
fn uti(extension: &str) -> &'static str {
    match extension {
        "png" => "public.png",
        "jpg" | "jpeg" => "public.jpeg",
        "json" => "public.json",
        _ => "public.data",
    }
}

/// `name` as a file name every platform accepts: no path separators or
/// reserved characters, not too long, never empty.
fn file_name(name: &str) -> String {
    let safe: String = name.chars().map(|c| if c.is_control() || "<>:\"/\\|?*".contains(c) { ' ' } else { c }).take(80).collect();
    // Words of only dots (`..`) go: no climbing out of the folder.
    let words: Vec<&str> = safe.split_whitespace().filter(|w| !w.chars().all(|c| c == '.')).collect();
    let safe = words.join(" ");
    let safe = safe.trim_end_matches('.');
    if safe.is_empty() { "Untitled".to_owned() } else { safe.to_owned() }
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn suggested_file_names_are_safe() {
        assert_eq!(file_name("Mira: the barkeep?"), "Mira the barkeep");
        assert_eq!(file_name("../../etc/passwd"), "etc passwd");
        assert_eq!(file_name("  \n "), "Untitled");
        assert_eq!(file_name(&"x".repeat(200)).len(), 80);
    }
}
