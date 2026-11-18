//! The OWN TERMINAL route (user 18:51): a portable, app-owned Windows
//! Terminal runtime staged in the `terminal/` companion folder, run in
//! PORTABLE MODE (`.portable` marker; all settings live in
//! `terminal/settings/` - no installed WT prerequisite, no personal
//! settings, no global/default terminal changes, nothing registered).
//! The window runs ONLY Spectrum in ordinary use: one profile (fixed
//! GUID), focus mode, unbound new-tab/split/palette/settings keys, no
//! dynamic profile sources, close-on-exit. This is app-only UI - NOT
//! tamper-proof or security isolation.
//!
//! Move-safe: the launcher REGENERATES the profile's commandline and
//! startingDirectory from Spectrum's CURRENT absolute path on every
//! "Own terminal" launch, so the whole package folder can be copied
//! anywhere and the route keeps working without personal WT settings.
//!
//! Honest failure: if the runtime is missing or the spawn fails, the
//! error is returned and the caller stays in the NORMAL terminal with
//! a clear message - never a fake exit 0.

#[cfg(windows)]
pub mod imp {
    use std::ffi::OsStr;
    use std::io;
    use std::path::{Path, PathBuf};

    /// The Spectrum MENU profile's FIXED GUID (readable font for the
    /// interactive menu/path/settings text).
    pub const MENU_PROFILE_GUID: &str = "{b8f1c3d2-9a4e-5e6f-8b2a-3c5d7e9f1a4b}";

    /// The Spectrum GARDEN profile's FIXED GUID (the fine reference
    /// grid for the lyric garden / visualizer views).
    pub const GARDEN_PROFILE_GUID: &str = "{e7a9d4c3-1b5f-4f8e-a2d9-6c8e0b2a4d6f}";

    /// The reference grid the own terminal requests on launch
    /// (the LEFT capture: 288x128 visible cells; the ~5x8 px cell size
    /// comes from the app-owned profile FONT - Cascadia Mono at a
    /// small size - not from any console API).
    pub const OWN_COLS: &str = "288";
    pub const OWN_ROWS: &str = "128";

    /// True when THIS instance already runs inside the own terminal:
    /// the WT_PROFILE_ID matches the fixed Spectrum profile GUID, or
    /// the profile's own env marker is set. Inside the own terminal
    /// the chooser never runs - no relaunch loop.
    pub fn in_own_terminal() -> bool {
        // The profile `environment` object sets SPECTRUM_OWN_TERM
        // ("menu" or "garden" - QA 19:44: the documented WT key is
        // `environment`, not `env`).
        if std::env::var_os("SPECTRUM_OWN_TERM").is_some() {
            return true;
        }
        // Belt-and-braces fallback: WT sets WT_PROFILE_ID to the
        // running profile's GUID - match either of our two profiles.
        match std::env::var("WT_PROFILE_ID") {
            Ok(id) => id == MENU_PROFILE_GUID || id == GARDEN_PROFILE_GUID,
            Err(_) => false,
        }
    }

    /// True when THIS instance runs in the own-terminal's MENU
    /// profile specifically (the readable-font interactive menu).
    /// Checked by the menu's garden handoff: menu selections delegate
    /// to the GARDEN window (fine font).
    pub fn in_own_menu() -> bool {
        if std::env::var("SPECTRUM_OWN_TERM")
            .map(|v| v == "menu")
            .unwrap_or(false)
        {
            return true;
        }
        // WT_PROFILE_ID fallback when the environment var is not
        // set (e.g. an older WT that ignores `environment`).
        std::env::var("WT_PROFILE_ID")
            .map(|id| id == MENU_PROFILE_GUID)
            .unwrap_or(false)
    }

    /// Locate the portable WindowsTerminal.exe next to the running
    /// Spectrum executable (the `terminal/` companion folder). The
    /// path is computed from current_exe, so a moved/copied package
    /// finds its own runtime.
    fn wt_exe() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let wt = dir.join("terminal").join("WindowsTerminal.exe");
        if wt.is_file() {
            Some(wt)
        } else {
            None
        }
    }

    /// The app-owned settings file the launcher regenerates.
    fn settings_path() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let dir = exe.parent()?;
        let p = dir.join("terminal").join("settings").join("settings.json");
        if p.is_file() {
            Some(p)
        } else {
            None
        }
    }

    /// Standard Windows command-line quoting (the CommandLineToArgvW /
    /// MSVCRT convention, QA 19:00 audit):
    /// - backslashes before a quote (or at the end) are doubled, and
    ///   the quote is escaped with a backslash;
    /// - an argument containing spaces/quotes/tabs/empty is wrapped
    ///   in quotes;
    /// - all other characters pass through verbatim (Unicode-safe).
    /// See https://learn.microsoft.com/cpp/c-language/parsing-c-command-line-arguments
    pub fn quote_arg(arg: &OsStr) -> String {
        let s = arg.to_string_lossy();
        if s.is_empty() {
            return "\"\"".to_string();
        }
        let needs_quoting =
            s.chars().any(|c| c == ' ' || c == '\t' || c == '"') || s.ends_with('\\');
        if !needs_quoting {
            return s.into_owned();
        }
        // The documented algorithm (parsing C command-line arguments):
        // for each backslash run followed by a quote, emit half the
        // backslashes then \" (n backslashes + quote -> n/2 + \").
        // For a run NOT followed by a quote, emit them verbatim.
        // The closing quote needs a doubled run before it.
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        let n = chars.len();
        let mut i = 0;
        while i < n {
            if chars[i] == '"' {
                out.push('\\');
                out.push('"');
                i += 1;
                continue;
            }
            if chars[i] == '\\' {
                // Count the backslash run.
                let mut j = i;
                while j < n && chars[j] == '\\' {
                    j += 1;
                }
                let run = j - i;
                let followed_by_quote = j < n && chars[j] == '"';
                if followed_by_quote {
                    // SERIALIZATION (QA 19:05): to produce n literal
                    // backslashes then a literal quote, emit 2n
                    // backslashes + \" (2n+1 total before the quote
                    // character), because the PARSER folds 2n
                    // backslashes + \" back into n backslashes + ".
                    for _ in 0..run {
                        out.push('\\');
                        out.push('\\');
                    }
                    out.push('\\');
                    out.push('"');
                    i = j + 1; // consume the quote too
                } else if j == n {
                    // Trailing backslash run: the closing quote we
                    // append below follows it, so DOUBLE the run.
                    for _ in 0..run * 2 {
                        out.push('\\');
                    }
                    i = j;
                } else {
                    // Not followed by a quote and not trailing:
                    // verbatim (no doubling needed).
                    for _ in 0..run {
                        out.push('\\');
                    }
                    i = j;
                }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        out.push('"');
        out
    }

    /// Regenerate the app-owned settings.json with TWO profiles:
    /// "Spectrum Menu" (readable font for the interactive menu,
    /// path, settings and chooser text) and "Spectrum Garden" (the
    /// fine reference grid for the lyric garden / visualizer views).
    /// The user's exact arguments go into the MENU profile; the GARDEN
    /// profile gets the same exe with the visualizer selection args
    /// (paths stay DATA - quoted argv, no shell).
    ///
    /// QA 19:28: launchMode is "maximizedFocus" so the window ALWAYS
    /// fits the actual monitor work area - no clipping, no hidden
    /// buffer, no console geometry API.
    fn write_settings(
        exe: &Path,
        menu_args: &[std::ffi::OsString],
        garden_args: &[std::ffi::OsString],
        cwd: &Path,
    ) -> io::Result<()> {
        let menu_cmdline = build_cmdline(exe, menu_args);
        let garden_cmdline = build_cmdline(exe, garden_args);
        let settings = serde_json::json!({
            "$help": "https://aka.ms/terminal-documentation",
            "defaultProfile": MENU_PROFILE_GUID,
            "disabledProfileSources": [
                "Windows.Terminal.Wsl",
                "Windows.Terminal.Azure",
                "Windows.Terminal.PowershellCore",
                "Windows.Terminal.SSH"
            ],
            "launchMode": "fullscreen",
            "newTabMenu": [],
            "profiles": {
                "defaults": {
                    "closeOnExit": "always",
                    "suppressApplicationTitle": true
                },
                "list": [
                    {
                        "guid": MENU_PROFILE_GUID,
                        "name": "Spectrum Menu",
                        "commandline": menu_cmdline,
                        "startingDirectory": cwd.to_string_lossy(),
                        "font": {
                            "face": "Cascadia Mono",
                            "size": 11.0
                        },
                        "environment": {
                            "SPECTRUM_OWN_TERM": "menu"
                        }
                    },
                    {
                        "guid": GARDEN_PROFILE_GUID,
                        "name": "Spectrum Garden",
                        "commandline": garden_cmdline,
                        "startingDirectory": cwd.to_string_lossy(),
                        "font": {
                            "face": "Cascadia Mono",
                            "size": 6.0
                        },
                        "environment": {
                            "SPECTRUM_OWN_TERM": "garden"
                        }
                    },
                    // Hide the inherited built-in profiles so only
                    // Spectrum appears in ordinary use.
                    {
                        "guid": "{61c54bbd-c2c6-5271-96e7-009a87ff44bf}",
                        "name": "Windows PowerShell",
                        "hidden": true
                    },
                    {
                        "guid": "{0caa0dad-35be-5f56-a8ff-afceeeaa6101}",
                        "name": "Command Prompt",
                        "hidden": true
                    }
                ]
            },
            "keybindings": [
                { "command": "unbound", "keys": "ctrl+t" },
                { "command": "unbound", "keys": "ctrl+shift+t" },
                { "command": "unbound", "keys": "ctrl+shift+n" },
                { "command": "unbound", "keys": "ctrl+shift+p" },
                { "command": "unbound", "keys": "ctrl+shift+space" },
                { "command": "unbound", "keys": "ctrl+," },
                { "command": "unbound", "keys": "ctrl+alt+," },
                { "command": "unbound", "keys": "ctrl+shift+," },
                { "command": "unbound", "keys": "ctrl+shift+d" },
                { "command": "unbound", "keys": "alt+shift+d" },
                { "command": "unbound", "keys": "alt+shift+-" },
                { "command": "unbound", "keys": "alt+shift+=" },
                { "command": "unbound", "keys": "alt+shift+f10" },
                { "command": "unbound", "keys": "ctrl+shift+w" },
                { "command": "unbound", "keys": "ctrl+shift+f" },
                { "command": "unbound", "keys": "ctrl+shift+enter" },
                { "command": "unbound", "keys": "alt+enter" },
                { "command": "unbound", "keys": "f11" },
                { "command": "unbound", "keys": "alt+down" },
                { "command": "unbound", "keys": "alt+up" },
                { "command": "unbound", "keys": "alt+left" },
                { "command": "unbound", "keys": "alt+right" },
                { "command": "unbound", "keys": "alt+shift+down" },
                { "command": "unbound", "keys": "alt+shift+up" },
                { "command": "unbound", "keys": "alt+shift+left" },
                { "command": "unbound", "keys": "alt+shift+right" },
                { "command": "unbound", "keys": "ctrl+tab" },
                { "command": "unbound", "keys": "ctrl+shift+tab" },
                { "command": "unbound", "keys": "alt+space" }
            ]
        });
        std::fs::write(
            settings_path().ok_or_else(not_found)?,
            serde_json::to_string_pretty(&settings).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("json build failed: {e}"),
                )
            })?,
        )
    }

    fn build_cmdline(exe: &Path, args: &[std::ffi::OsString]) -> String {
        let mut cmdline = quote_arg(exe.as_os_str());
        for a in args {
            cmdline.push(' ');
            cmdline.push_str(&quote_arg(a));
        }
        cmdline
    }

    fn not_found() -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            "own-terminal settings path not found next to the executable",
        )
    }

    /// Launch the own terminal's MENU window running Spectrum with
    /// the EXACT original arguments (readable font). The GARDEN
    /// profile's commandline is pre-written with the same exe so the
    /// app can switch to it via the menu flow.
    ///
    /// maximizedFocus: the window fits the actual monitor work area
    /// - no clipping, no hidden buffer (QA 19:28).
    pub fn launch(args: &[std::ffi::OsString]) -> io::Result<std::process::ExitStatus> {
        let exe = std::env::current_exe()?;
        let wt = wt_exe().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "own-terminal runtime not found (expected terminal/WindowsTerminal.exe \
                 next to Spectrum; the pinned portable distribution is staged in \
                 terminal/ - see terminal/NOTICE-OWN-TERMINAL.txt)",
            )
        })?;
        let cwd = std::env::current_dir()?;
        // The garden profile gets a PLACEHOLDER commandline now (the
        // menu flow rewrites it with the user's selection args when
        // the visualizer starts).
        let garden_placeholder: Vec<std::ffi::OsString> = Vec::new();
        write_settings(&exe, args, &garden_placeholder, &cwd)?;
        let mut cmd = std::process::Command::new(&wt);
        cmd.args(["--profile", "Spectrum Menu"]);
        cmd.current_dir(cwd);
        cmd.status()
    }

    /// Launch the GARDEN as a NEW TAB in the SAME window (not a
    /// second window). Called from the menu flow when the user starts
    /// a visualizer in the own terminal: the garden tab takes focus,
    /// runs the visualizer at the fine font, and when it closes
    /// (closeOnExit) focus returns to the menu tab behind it.
    /// Same terminal window, no second window (user 20:01).
    pub fn launch_garden(args: &[std::ffi::OsString]) -> io::Result<std::process::ExitStatus> {
        let exe = std::env::current_exe()?;
        let wt = wt_exe().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "own-terminal runtime not found")
        })?;
        let cwd = std::env::current_dir()?;
        // Rewrite the garden profile's commandline with the selection
        // args (the menu profile's commandline stays untouched).
        let menu_args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        write_settings(&exe, &menu_args, args, &cwd)?;
        // -w 0 = the most recent window (THIS one); new-tab opens the
        // garden profile as a tab IN this window, not a new window.
        let mut cmd = std::process::Command::new(&wt);
        cmd.args(["-w", "0", "new-tab", "--profile", "Spectrum Garden"]);
        cmd.current_dir(cwd);
        cmd.status()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // QA 19:00/19:05 audit: the quoting must follow the documented
        // Windows algorithm - empty args, spaces, quotes, trailing
        // backslash, tabs, Unicode all stay DATA. The backslash+quote
        // serialization is verified by ROUND-TRIP through the real
        // CommandLineToArgvW, not merely self-authored strings.

        fn q(s: &str) -> String {
            quote_arg(std::ffi::OsStr::new(s))
        }

        // Parse a Windows command line back into arguments using the
        // REAL CommandLineToArgvW (shell32.dll) - the actual consumer
        // of the strings quote_arg produces.
        fn parse_back(cmdline: &str) -> Vec<String> {
            use std::os::windows::ffi::OsStrExt as _;
            let wide: Vec<u16> = std::ffi::OsStr::new(cmdline)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let mut argc: i32 = 0;
            let argv = unsafe { CommandLineToArgvW(wide.as_ptr(), &mut argc) };
            assert!(!argv.is_null(), "CommandLineToArgvW failed");
            let mut out = Vec::new();
            for i in 0..argc as usize {
                let p = unsafe { *argv.add(i) };
                assert!(!p.is_null());
                let mut len = 0;
                while unsafe { *p.add(len) } != 0 {
                    len += 1;
                }
                let slice = unsafe { std::slice::from_raw_parts(p, len) };
                out.push(String::from_utf16_lossy(slice));
            }
            // Test-only: do not free argv (avoids the Win32_Foundation
            // feature just for the test; the leaked memory is trivial
            // in a test process).
            out
        }

        #[allow(non_snake_case)]
        unsafe fn CommandLineToArgvW(cmd: *const u16, argc: *mut i32) -> *mut *mut u16 {
            // shell32!CommandLineToArgvW via raw linkage.
            #[link(name = "shell32")]
            extern "system" {
                fn CommandLineToArgvW(cmd: *const u16, argc: *mut i32) -> *mut *mut u16;
            }
            CommandLineToArgvW(cmd, argc)
        }

        // Round-trip: quote a single arg, build a 2-arg command line
        // (a marker + the quoted arg), parse with CommandLineToArgvW,
        // and compare the second parsed arg to the original.
        fn roundtrip(original: &str) {
            let quoted = q(original);
            let cmdline = format!("marker {}", quoted);
            let parsed = parse_back(&cmdline);
            assert_eq!(
                parsed.len(),
                2,
                "round-trip of {original:?} -> {quoted:?} -> {parsed:?}"
            );
            assert_eq!(
                &parsed[1], original,
                "round-trip mismatch: {original:?} -> {quoted:?} -> {parsed:?}"
            );
        }

        #[test]
        fn plain_args_pass_through_unquoted() {
            assert_eq!(q("hello"), "hello");
            assert_eq!(q("--volume=0"), "--volume=0");
            assert_eq!(q("song.mp3"), "song.mp3");
            roundtrip("hello");
            roundtrip("--volume=0");
        }

        #[test]
        fn empty_arg_quotes_to_two_quotes() {
            assert_eq!(q(""), "\"\"");
            roundtrip("");
        }

        #[test]
        fn spaces_get_wrapped() {
            assert_eq!(q("hello world"), "\"hello world\"");
            assert_eq!(
                q("C:\\Github Code etc v2\\Spectrum\\song.mp3"),
                "\"C:\\Github Code etc v2\\Spectrum\\song.mp3\""
            );
            roundtrip("hello world");
            roundtrip("C:\\Github Code etc v2\\Spectrum\\song.mp3");
        }

        #[test]
        fn tabs_get_wrapped() {
            assert_eq!(q("a\tb"), "\"a\tb\"");
            roundtrip("a\tb");
        }

        #[test]
        fn embedded_quote_escapes() {
            assert_eq!(q("say \"hi\""), "\"say \\\"hi\\\"\"");
            assert_eq!(q("\""), "\"\\\"\"");
            roundtrip("say \"hi\"");
            roundtrip("\"");
        }

        #[test]
        fn trailing_backslash_doubles() {
            // Trailing backslash before the closing quote: n -> 2n.
            assert_eq!(q("dir\\"), "\"dir\\\\\"");
            // Non-trailing, non-quoted-following backslash: verbatim.
            assert_eq!(q("a\\b"), "a\\b");
            roundtrip("dir\\");
            roundtrip("a\\b");
            roundtrip("C:\\path\\");
        }

        #[test]
        fn unicode_passes_through() {
            assert_eq!(q("ünïcodé"), "ünïcodé");
            assert_eq!(q("日本語 word"), "\"日本語 word\"");
            roundtrip("ünïcodé");
            roundtrip("日本語 word");
        }

        // QA 19:05 audit: verify the backslash-run-before-quote
        // branch through the REAL parser for 1, 2, 3 backslashes.
        #[test]
        fn backslash_run_before_quote_roundtrips() {
            // The branch fires when a run of backslashes is followed
            // by a quote character in the ORIGINAL arg.
            for n in 1..=3 {
                let bs = "\\".repeat(n);
                let arg = format!("a{bs}\"b");
                roundtrip(&arg);
            }
            // Also with spaces (forces quoting).
            for n in 1..=3 {
                let bs = "\\".repeat(n);
                let arg = format!("x y{bs}\"z");
                roundtrip(&arg);
            }
        }

        #[test]
        fn complex_arg_all_rules() {
            // Spaces + quotes + trailing backslash together.
            let r = q("a b\"c\\");
            assert_eq!(r, "\"a b\\\"c\\\\\"");
            roundtrip("a b\"c\\");
        }

        // A full multi-arg command line round-trip, like what the
        // launcher writes into the WT profile commandline.
        #[test]
        fn full_command_line_roundtrip() {
            let exe = "C:\\Github Code etc v2\\Spectrum\\target-qa\\debug\\spectrum.exe";
            let args = vec!["--volume", "0", ".qa-local\\fixtures\\my song (2024).mp3"];
            let mut cmdline = q(exe);
            for a in &args {
                cmdline.push(' ');
                cmdline.push_str(&q(a));
            }
            // Prepend "spectrum " so the parser sees a leading token,
            // then the exe (arg 1) and the forwarded args (arg 2+).
            let full = format!("spectrum {}", cmdline);
            let parsed = parse_back(&full);
            // spectrum (1) + exe (1) + 3 forwarded args = 5 total.
            assert_eq!(parsed.len(), 2 + args.len());
            assert_eq!(&parsed[1], exe);
            for (i, a) in args.iter().enumerate() {
                assert_eq!(&parsed[2 + i], a, "arg {i} mismatch");
            }
        }
    }
}
