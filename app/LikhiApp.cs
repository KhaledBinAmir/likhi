// The Likhi window: what people open from the Start menu after installing.
//
// It exists because installing a keyboard leaves nothing to click, and everyone looks for an app.
// Pilot users searched the Start menu, found nothing, and had no way to tell a working install from
// a broken one. So this answers three questions: is it working, how do I switch to it, and how do I
// turn the parts I don't want off.
//
// C# compiled with the csc.exe that ships in every .NET Framework install, rather than Python:
// the engine's embedded interpreter has no tkinter, and adding it costs about ten megabytes for one
// window. This is roughly twenty kilobytes, starts instantly, shows no console, and needs nothing
// installed. Built by scripts/build_app.py.

using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Net.Sockets;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.RegularExpressions;
using System.Windows.Forms;
using Microsoft.Win32;

namespace Likhi
{
    static class Program
    {
        [STAThread]
        static void Main()
        {
            Application.EnableVisualStyles();
            Application.SetCompatibleTextRenderingDefault(false);
            Application.Run(new MainForm());
        }
    }

    // Everything the window needs to know about this machine, read fresh each time it is shown.
    static class Env
    {
        // Must match shell/src/guids.rs. It did not after 0.2.0 replaced the text service, so the
        // window told everyone their keyboard was missing while it was installed and working.
        public const string Tip = "0845:{1D24C804-FAD0-4B32-AEDD-1317F4E6221E}{502AB3FE-5B7C-43E9-89D1-BE885846AE0D}";
        public const int EnginePort = 47123;

        // The application directory is wherever this executable sits; PIME is fixed, because its
        // text service DLL builds that path internally when it registers the keyboard.
        public static string AppDir
        {
            get { return Path.GetDirectoryName(Application.ExecutablePath); }
        }

        public static string PimeDir
        {
            get
            {
                string pf86 = Environment.GetEnvironmentVariable("ProgramFiles(x86)");
                if (string.IsNullOrEmpty(pf86)) pf86 = @"C:\Program Files (x86)";
                return Path.Combine(pf86, "PIME");
            }
        }

        public static string UserConfigPath
        {
            get
            {
                string local = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
                return Path.Combine(Path.Combine(local, "Likhi"), "config.json");
            }
        }

        public static string Version
        {
            get
            {
                try
                {
                    using (RegistryKey k = Registry.LocalMachine.OpenSubKey(
                        @"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{7F2E5B91-4C3A-4D8E-9A61-2B7D5E8C4F30}_is1"))
                    {
                        if (k != null)
                        {
                            object v = k.GetValue("DisplayVersion");
                            if (v != null) return v.ToString();
                        }
                    }
                }
                catch { }
                return "";
            }
        }

        // The keyboard is in this user's language list. Checked in the registry rather than through
        // PowerShell so it still works where Group Policy blocks scripts.
        public static bool KeyboardInstalled()
        {
            try
            {
                using (RegistryKey k = Registry.CurrentUser.OpenSubKey(
                    @"Control Panel\International\User Profile\bn-BD"))
                {
                    if (k == null) return false;
                    foreach (string name in k.GetValueNames())
                        if (string.Equals(name, Tip, StringComparison.OrdinalIgnoreCase)) return true;
                }
            }
            catch { }
            return false;
        }

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        static extern bool WaitNamedPipe(string name, uint timeoutMs);

        // Whether an engine is running for this Windows session, answered from its pipe without
        // connecting. Instant either way, where asking the socket costs the full connect timeout when
        // nothing is listening: Windows retries a refused loopback connection instead of failing it.
        public static bool EngineRunning()
        {
            string name = @"\\.\pipe\likhi-engine-s" + Process.GetCurrentProcess().SessionId;
            if (WaitNamedPipe(name, 1)) return true;
            const int ERROR_SEM_TIMEOUT = 121; // the pipe is there; every instance is busy
            return Marshal.GetLastWin32Error() == ERROR_SEM_TIMEOUT;
        }

        // Ask the engine directly. Anything else -- a running process, a registry value -- can be
        // true while the engine is wedged.
        public static bool EngineResponds()
        {
            if (!EngineRunning()) return false;
            try
            {
                using (TcpClient c = new TcpClient())
                {
                    IAsyncResult ar = c.BeginConnect("127.0.0.1", EnginePort, null, null);
                    if (!ar.AsyncWaitHandle.WaitOne(600)) return false;
                    c.EndConnect(ar);
                    using (NetworkStream s = c.GetStream())
                    {
                        s.ReadTimeout = 1500;
                        byte[] req = Encoding.UTF8.GetBytes("{\"op\":\"ping\"}\n");
                        s.Write(req, 0, req.Length);
                        byte[] buf = new byte[256];
                        int n = s.Read(buf, 0, buf.Length);
                        return n > 0 && Encoding.UTF8.GetString(buf, 0, n).Contains("\"ok\"");
                    }
                }
            }
            catch { return false; }
        }

        public static bool AutostartOn()
        {
            try
            {
                using (RegistryKey k = Registry.CurrentUser.OpenSubKey(
                    @"Software\Microsoft\Windows\CurrentVersion\Run"))
                {
                    return k != null && k.GetValue("LikhiEngine") != null;
                }
            }
            catch { return false; }
        }

        public static void SetAutostart(bool on)
        {
            using (RegistryKey k = Registry.CurrentUser.CreateSubKey(
                @"Software\Microsoft\Windows\CurrentVersion\Run"))
            {
                if (k == null) return;
                // One entry: the text service is a DLL Windows loads itself, so there is no
                // launcher to start any more. Any LikhiLauncher left by a PIME-era version is
                // removed either way, or it would run a program that is no longer installed.
                try { k.DeleteValue("LikhiLauncher", false); } catch { }
                if (on)
                    k.SetValue("LikhiEngine", "\"" + Path.Combine(AppDir, EngineExe) + "\"");
                else
                    try { k.DeleteValue("LikhiEngine", false); } catch { }
            }
        }

        // Settings live per user, so one person's choice does not decide for everyone on a shared
        // machine and none of it needs an administrator. Both the engine and the text service read
        // the installed config first and lay this file over it, key by key.
        //
        // Deliberately not a JSON library: this file holds a handful of flat values that only this
        // window writes, so a read-modify-write over the keys we own is simpler to audit than a
        // dependency, and cannot reformat or lose a key it does not understand -- it keeps every
        // line it did not come to change.
        public static Dictionary<string, string> ReadUserConfig()
        {
            var values = new Dictionary<string, string>();
            try
            {
                if (!File.Exists(UserConfigPath)) return values;
                foreach (string line in File.ReadAllLines(UserConfigPath))
                {
                    Match m = Regex.Match(line.Trim(), "^\"([^\"]+)\"\\s*:\\s*(.+?),?$");
                    if (m.Success) values[m.Groups[1].Value] = m.Groups[2].Value.Trim();
                }
            }
            catch { }
            return values;
        }

        public static void WriteUserConfig(Dictionary<string, string> values)
        {
            string path = UserConfigPath;
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            var sb = new StringBuilder();
            sb.Append("{\n");
            int i = 0;
            foreach (var kv in values)
            {
                sb.Append("  \"").Append(kv.Key).Append("\": ").Append(kv.Value);
                sb.Append(++i < values.Count ? ",\n" : "\n");
            }
            sb.Append("}\n");
            File.WriteAllText(path, sb.ToString(), new UTF8Encoding(false));
        }

        // Beside the executable when installed; the fixed install path as well, because this window
        // is also run straight out of a build directory during development and would otherwise read
        // no machine config at all and report every setting as its default.
        static string MachineConfigPath
        {
            get
            {
                foreach (string candidate in new[] {
                    Path.Combine(AppDir, "config.json"),
                    Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ProgramFiles), @"Likhi\config.json"),
                    Path.Combine(PimeDir, @"python\input_methods\likhi\config.json"),
                })
                    if (File.Exists(candidate)) return candidate;
                return Path.Combine(AppDir, "config.json");
            }
        }

        /// <summary>A setting's current value: the per-user file if it names it, else the machine one.</summary>
        public static string Setting(string key, string fallback)
        {
            var user = ReadUserConfig();
            if (user.ContainsKey(key)) return user[key].Trim('"');
            try
            {
                if (File.Exists(MachineConfigPath))
                {
                    foreach (string line in File.ReadAllLines(MachineConfigPath))
                    {
                        Match m = Regex.Match(line.Trim(), "^\"" + Regex.Escape(key) + "\"\\s*:\\s*(.+?),?$");
                        if (m.Success) return m.Groups[1].Value.Trim().Trim('"');
                    }
                }
            }
            catch { }
            return fallback;
        }

        public static void SetSetting(string key, string jsonValue)
        {
            var values = ReadUserConfig();
            values[key] = jsonValue;
            WriteUserConfig(values);
        }

        public static bool ReportingOn()
        {
            return Setting("telemetry", "off") != "off";
        }

        public static void SetReporting(bool on)
        {
            SetSetting("telemetry", "\"" + (on ? "full" : "off") + "\"");
        }

        // Its own setting, not part of usage reporting. A daily update check sends nothing about what
        // anyone types, but it does tell GitHub that a machine at this address runs Likhi, so it must
        // be possible to turn off on its own -- and turning reporting off must not quietly leave it on.
        // On unless someone has said otherwise, which is what the engine assumes too.
        public static bool UpdatesOn()
        {
            return Setting("check_updates", "true") != "false";
        }

        public static void SetUpdates(bool on)
        {
            // A bare JSON boolean: the engine reads this with as_bool, and a quoted "true" would read
            // as absent and fall back to on, so switching it off would silently do nothing.
            SetSetting("check_updates", on ? "true" : "false");
        }

        public static bool NextWordOn()
        {
            return Setting("next_word", "true") != "false";
        }

        public static void SetNextWord(bool on)
        {
            // Bare boolean, as above: the text service reads it as one.
            SetSetting("next_word", on ? "true" : "false");
        }

        // "Check now". The engine does the checking -- the same signed check the daily one does --
        // and puts up its own notification when it finds an update, so this only reports what
        // happened. Blocks for as long as the check takes; call it off the window's thread.
        public static void CheckForUpdates(out string message, out MessageBoxIcon icon)
        {
            icon = MessageBoxIcon.Information;
            string reply = null;
            if (EngineRunning())
            {
                try
                {
                    using (TcpClient c = new TcpClient())
                    {
                        IAsyncResult ar = c.BeginConnect("127.0.0.1", EnginePort, null, null);
                        if (ar.AsyncWaitHandle.WaitOne(3000))
                        {
                            c.EndConnect(ar);
                            using (NetworkStream s = c.GetStream())
                            {
                                // Minutes, not seconds: when there is an update the engine downloads
                                // and verifies the whole installer, about 40 MB, before it answers.
                                s.ReadTimeout = 300000;
                                byte[] req = Encoding.UTF8.GetBytes("{\"op\":\"update_check\"}\n");
                                s.Write(req, 0, req.Length);
                                reply = ReadLine(s);
                            }
                        }
                    }
                }
                catch { reply = null; }
            }
            if (reply == null)
            {
                icon = MessageBoxIcon.Warning;
                message = "The Likhi engine is not running, so it cannot check. Start it with Restart engine and try again.";
                return;
            }
            string current = JsonString(reply, "current");
            string version = JsonString(reply, "version");
            string error = JsonString(reply, "error");
            switch (JsonString(reply, "status"))
            {
                case "up_to_date":
                    message = "You have the newest version of Likhi (" + current + ").";
                    break;
                case "available":
                    message = "Likhi " + version + " is downloaded and verified.\n\n" +
                        "To install it, click the notification, or right-click the Likhi icon by the clock " +
                        "and choose Install update. Windows will ask for permission.";
                    break;
                case "dev":
                    message = "This is a development build. It does not update itself.";
                    break;
                default:
                    icon = MessageBoxIcon.Warning;
                    message = "Could not check for updates" + (error.Length > 0 ? ":\n\n" + error : ".");
                    break;
            }
        }

        static string ReadLine(NetworkStream s)
        {
            var bytes = new List<byte>();
            byte[] one = new byte[1];
            while (s.Read(one, 0, 1) == 1)
            {
                if (one[0] == (byte)'\n') break;
                bytes.Add(one[0]);
            }
            return bytes.Count == 0 ? null : Encoding.UTF8.GetString(bytes.ToArray());
        }

        // One string field of a flat JSON reply. The engine's replies are a handful of plain values,
        // so a pattern is enough and no JSON library has to ship with this window.
        static string JsonString(string json, string key)
        {
            Match m = Regex.Match(json, "\"" + Regex.Escape(key) + "\"\\s*:\\s*\"((?:[^\"\\\\]|\\\\.)*)\"");
            if (!m.Success) return "";
            try { return Regex.Unescape(m.Groups[1].Value); } catch { return m.Groups[1].Value; }
        }

        /// <summary>Installed families that actually contain Bengali, so the list cannot offer a
        /// font that would render every candidate as boxes.</summary>
        public static List<string> BanglaFonts()
        {
            var found = new List<string>();
            using (var probe = new Bitmap(1, 1))
            using (var g = Graphics.FromImage(probe))
            {
                foreach (FontFamily f in FontFamily.Families)
                {
                    try
                    {
                        using (var font = new Font(f, 14f))
                        {
                            // A family without Bengali coverage measures the string at the width of
                            // its fallback boxes; comparing against a known-good face is unreliable,
                            // so go by the families we ship plus the ones Windows is known to have.
                            if (Known(f.Name)) found.Add(f.Name);
                        }
                    }
                    catch { }
                }
            }
            found.Sort(StringComparer.OrdinalIgnoreCase);
            return found;
        }

        // There was a setting here to point Bangla at a chosen font across the whole machine. It is
        // gone because it did not work, and a switch that does nothing is worse than no switch.
        //
        // Windows has no "Bangla font" to change. An application asks for Segoe UI, that font has no
        // Bengali glyphs -- confirmed, it has no glyph for আ -- and something else supplies them.
        // The mechanism for choosing that something is font linking, under
        // HKLM\...\FontLink\SystemLink, and it is a GDI mechanism. Modern applications draw through
        // DirectWrite, which picks a fallback family itself and never reads those keys. Measured
        // rather than assumed: with the entries in place and the font correctly installed
        // machine-wide, Bangla under Segoe UI still rendered at Nirmala UI's exact metrics.
        //
        // The only thing that would actually work is replacing the system font file itself, which
        // means taking ownership of a Windows font, breaking servicing, and breaking the nine other
        // Indic scripts Nirmala UI carries. Not worth having.
        //
        // What does work is per-application: browsers and Office both let you choose a font per
        // script. installer/system_font.ps1 is kept, with its findings, for anyone who wants to
        // try the GDI path on older software.

        static bool Known(string name)
        {
            string[] families = {
                "Noto Sans Bengali", "Anek Bangla", "Hind Siliguri", "Tiro Bangla",
                "Nirmala UI", "Nirmala Text", "Shonar Bangla", "Vrinda", "Google Sans"
            };
            foreach (string f in families) if (string.Equals(f, name, StringComparison.OrdinalIgnoreCase)) return true;
            return false;
        }

        // The engine, relative to the install directory. It finds its own models beside it, so the
        // two move together or not at all.
        public const string EngineExe = @"engine\likhi-server.exe";

        public static void RestartEngine()
        {
            try
            {
                // "likhi-server", not "pythonw": the engine is its own binary since 0.3. A machine
                // upgraded from an older version may still have the Python one running, and that
                // one holds the port, so both names are stopped.
                foreach (string name in new string[] { "likhi-server", "pythonw" })
                {
                    foreach (Process p in Process.GetProcessesByName(name))
                    {
                        try
                        {
                            if (p.MainModule != null && p.MainModule.FileName != null &&
                                p.MainModule.FileName.StartsWith(AppDir, StringComparison.OrdinalIgnoreCase))
                                p.Kill();
                        }
                        catch { }
                    }
                }
            }
            catch { }
            StartEngine();
        }

        // Start the engine without stopping anything. Starting a second one is harmless -- it finds
        // the first and exits -- and starting it is also how someone undoes "Exit Likhi".
        public static void StartEngine()
        {
            try
            {
                ProcessStartInfo psi = new ProcessStartInfo(
                    Path.Combine(AppDir, EngineExe));
                psi.WindowStyle = ProcessWindowStyle.Hidden;
                psi.CreateNoWindow = true;
                psi.UseShellExecute = false;
                Process.Start(psi);
            }
            catch { }
        }
    }

    class MainForm : Form
    {
        readonly bool dark = IsDarkTheme();
        Label statusKeyboard, statusEngine;
        CheckBox autostart, reporting, updates, nextWord;
        Button checkNow;
        // Polls for the engine after this window started it, so the status line turns green when it
        // is ready rather than when a fixed sleep guessed it would be.
        System.Windows.Forms.Timer starting;
        DateTime startingSince;
        TextBox tryHere;
        ComboBox fontBox, sizeBox;
        // Set while the window writes its own controls. Without it, showing the current state fires
        // the change handlers, so simply opening the window would rewrite the autostart keys and
        // restart the engine -- an action the person never asked for.
        bool loading;

        static bool IsDarkTheme()
        {
            try
            {
                using (RegistryKey k = Registry.CurrentUser.OpenSubKey(
                    @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"))
                {
                    if (k != null)
                    {
                        object v = k.GetValue("AppsUseLightTheme");
                        if (v is int) return ((int)v) == 0;
                    }
                }
            }
            catch { }
            return false;
        }

        Color Bg { get { return dark ? Color.FromArgb(32, 32, 36) : Color.White; } }
        Color Fg { get { return dark ? Color.FromArgb(240, 240, 240) : Color.FromArgb(20, 20, 20); } }
        Color Dim { get { return dark ? Color.FromArgb(165, 165, 172) : Color.FromArgb(96, 96, 96); } }
        static Color Brand { get { return Color.FromArgb(16, 138, 95); } }

        public MainForm()
        {
            // Set for the whole of construction. Filling a drop-down selects its first item, which
            // raises the same change event a person clicking would, and that wrote a font nobody
            // had chosen into the settings file before the window was even on screen.
            loading = true;
            Text = "Likhi";
            // A placeholder: the real height is set at the end of construction, from where the
            // last line landed. Fixed heights were edited by hand every time a line was added.
            ClientSize = new Size(520, 700);
            FormBorderStyle = FormBorderStyle.FixedSingle;
            MaximizeBox = false;
            StartPosition = FormStartPosition.CenterScreen;
            BackColor = Bg;
            Font = new Font("Segoe UI", 9f);
            try { Icon = Icon.ExtractAssociatedIcon(Application.ExecutablePath); } catch { }

            int y = 16;

            Head("Likhi", 20f, Brand, 18, y); y += 36;
            Body("Bangla phonetic keyboard" +
                (Env.Version.Length > 0 ? "   ·   version " + Env.Version : ""), 18, y, Dim);
            y += 28;

            statusKeyboard = Body("", 18, y, Fg); y += 22;
            statusEngine = Body("", 18, y, Fg); y += 32;

            Head("How to type Bangla", 11f, Fg, 18, y); y += 28;
            string[] steps = {
                "1.   Press  Win + Space  and choose “Bangla (Bangladesh) — Likhi”.",
                "2.   Type the way the word sounds:   amar  →  আমার",
                "3.   Space or Enter accepts the highlighted word.",
                "4.   Press 1–5 to pick a different one, or ← → to move.",
                "5.   F12 switches to plain English without leaving Bangla mode.",
                "6.   After Space, a likely next word may appear:  Tab  types it.",
                "7.   Start the next word, and words that usually follow join the list."
            };
            foreach (string s in steps) { Body(s, 24, y, Fg, "Nirmala UI", 22); y += 24; }
            y += 14;

            Head("Suggestion font", 11f, Fg, 18, y); y += 26;
            fontBox = new ComboBox();
            fontBox.Location = new Point(18, y);
            fontBox.Size = new Size(320, 26);
            fontBox.DropDownStyle = ComboBoxStyle.DropDownList;
            foreach (string f in Env.BanglaFonts()) fontBox.Items.Add(f);
            Controls.Add(fontBox);
            sizeBox = new ComboBox();
            sizeBox.Location = new Point(346, y);
            sizeBox.Size = new Size(76, 26);
            sizeBox.DropDownStyle = ComboBoxStyle.DropDownList;
            foreach (int s in new[] { 12, 13, 14, 16, 18, 20, 24 }) sizeBox.Items.Add(s.ToString());
            Controls.Add(sizeBox);
            Body("px", 428, y + 4, Dim, "Segoe UI", 18);
            y += 30;
            Body("Applies to the suggestion list. Windows chooses the font for Bangla elsewhere.", 18, y, Dim, "Segoe UI", 18);
            y += 26;

            Head("Try it here", 11f, Fg, 18, y); y += 26;
            tryHere = new TextBox();
            tryHere.Location = new Point(18, y);
            tryHere.Size = new Size(484, 30);
            tryHere.Font = new Font("Nirmala UI", 12f);
            tryHere.BackColor = dark ? Color.FromArgb(48, 48, 54) : Color.FromArgb(250, 250, 250);
            tryHere.ForeColor = Fg;
            tryHere.BorderStyle = BorderStyle.FixedSingle;
            Controls.Add(tryHere);
            y += 42;

            nextWord = Check("Suggest the next word  (Tab after Space, and in the list as you type)", 18, y); y += 26;
            autostart = Check("Start Likhi when I sign in", 18, y); y += 26;
            reporting = Check("Share anonymous usage data to improve suggestions", 18, y); y += 24;
            Body("Counts only, plus words where the first suggestion was wrong. Never passwords,", 40, y, Dim, "Segoe UI", 18);
            y += 17;
            Body("numbers, or anything typed into a password box.", 40, y, Dim, "Segoe UI", 18);
            y += 26;
            updates = Check("Check for updates once a day", 18, y);
            // Works with the daily check switched off: that setting stops Likhi asking by itself,
            // and clicking this is asking.
            checkNow = Btn("Check now", 392, y - 3, 110);
            checkNow.Height = 26;
            checkNow.Click += delegate { CheckNow(); };
            y += 24;
            // Two lines, like the reporting note above: one would be clipped at this width.
            Body("Tells GitHub this computer runs Likhi. Every update is signed,", 40, y, Dim, "Segoe UI", 18);
            y += 17;
            Body("and nothing installs until you click it.", 40, y, Dim, "Segoe UI", 18);
            y += 30;

            Button diag = Btn("Run diagnostics", 18, y, 150);
            diag.Click += delegate { RunDiagnostics(); };
            Button restart = Btn("Restart engine", 180, y, 140);
            restart.Click += delegate { Env.RestartEngine(); WaitForEngine(); };
            Button site = Btn("Project page", 332, y, 170);
            site.Click += delegate { Open("https://github.com/KhaledBinAmir/likhi"); };
            y += 40;

            // Nirmala UI for this one line: it carries Bangla, and Segoe UI does not, so the
            // লিখি would come out as boxes on a machine without font linking.
            Body("লিখি  ·  made by Khaled Bin Amir  ·  free and open source, MIT",
                18, y, Dim, "Nirmala UI", 20);
            y += 28;

            // As tall as the content, but never taller than the screen: at 1366x768, still the
            // usual Bangladeshi laptop, the full window ran under the taskbar with its buttons out
            // of reach. Past that it scrolls, and is widened by the scroll bar so nothing is
            // covered.
            int chrome = Height - ClientSize.Height;
            int room = Screen.FromPoint(Cursor.Position).WorkingArea.Height - chrome - 16;
            if (y <= room)
                ClientSize = new Size(520, y);
            else
            {
                AutoScroll = true;
                ClientSize = new Size(520 + SystemInformation.VerticalScrollBarWidth, room);
            }

            nextWord.CheckedChanged += delegate
            {
                if (loading) return;
                // Picked up by the keyboard within a couple of seconds, in every application, with
                // nothing restarted.
                Env.SetNextWord(nextWord.Checked);
            };
            autostart.CheckedChanged += delegate
            {
                if (loading) return;
                Env.SetAutostart(autostart.Checked);
            };
            reporting.CheckedChanged += delegate
            {
                if (loading) return;
                Env.SetReporting(reporting.Checked);
                Env.RestartEngine();
            };
            // No restart: the engine reads this setting before every check, so the change takes
            // effect by the next one without interrupting anyone's typing.
            updates.CheckedChanged += delegate
            {
                if (loading) return;
                Env.SetUpdates(updates.Checked);
            };
            // The text service notices the file changing and rebuilds its text format within a
            // second, so there is nothing to restart and no need to switch keyboards.
            fontBox.SelectedIndexChanged += delegate
            {
                if (loading || fontBox.SelectedItem == null) return;
                Env.SetSetting("font_name", "\"" + fontBox.SelectedItem + "\"");
                PreviewFont();
            };
            sizeBox.SelectedIndexChanged += delegate
            {
                if (loading || sizeBox.SelectedItem == null) return;
                Env.SetSetting("font_size", sizeBox.SelectedItem.ToString());
                PreviewFont();
            };

            // Opening Likhi starts it. After "Exit Likhi" from the tray this is how it comes back --
            // what people do with any program they closed -- and after a failed start it is a retry.
            bool startedHere = false;
            if (!Env.EngineRunning())
            {
                Env.StartEngine();
                startedHere = true;
            }

            Refresh2();
            if (startedHere) WaitForEngine();
            // Construction is over: from here a change really is someone clicking.
            //
            // This has to be the last statement of the constructor, not the last of RefreshInner.
            // Refresh2 saves and restores the flag around its work, and during construction the
            // saved value is true -- so clearing it inside left the restore to put true straight
            // back, the flag never cleared, every handler returned early, and nothing anyone chose
            // in this window was ever written.
            loading = false;
        }

        // Named to avoid colliding with Form.Refresh().
        void Refresh2()
        {
            bool was = loading;
            loading = true;
            try { RefreshInner(); } finally { loading = was; }
        }

        void RefreshInner()
        {
            bool kb = Env.KeyboardInstalled();
            bool en = Env.EngineResponds();
            statusKeyboard.Text = (kb ? "✓  " : "✕  ") + (kb
                ? "Keyboard is installed for you"
                : "Keyboard is not in your language list — run “Set up the Likhi keyboard” from the Start menu");
            statusKeyboard.ForeColor = kb ? Brand : Color.FromArgb(200, 60, 60);
            statusEngine.Text = (en ? "✓  " : "✕  ") + (en
                ? "Suggestion engine is running"
                : "Suggestion engine is not responding — try Restart engine below");
            statusEngine.ForeColor = en ? Brand : Color.FromArgb(200, 60, 60);

            autostart.Checked = Env.AutostartOn();
            reporting.Checked = Env.ReportingOn();
            updates.Checked = Env.UpdatesOn();
            nextWord.Checked = Env.NextWordOn();

            string family = Env.Setting("font_name", "");
            if (family.Length == 0 || !fontBox.Items.Contains(family))
            {
                // Nothing chosen yet: show what the text service will actually use.
                foreach (string preferred in new[] { "Google Sans", "Noto Sans Bengali", "Nirmala UI" })
                    if (fontBox.Items.Contains(preferred)) { family = preferred; break; }
            }
            fontBox.SelectedItem = family;
            string size = Env.Setting("font_size", "14");
            if (!sizeBox.Items.Contains(size)) size = "14";
            sizeBox.SelectedItem = size;
            PreviewFont();
        }

        /// <summary>Say the engine is starting, and refresh once it answers -- or after ten seconds,
        /// when the status line then says it is not responding, which by then is true.</summary>
        void WaitForEngine()
        {
            statusEngine.Text = "…  Starting the suggestion engine";
            statusEngine.ForeColor = Dim;
            startingSince = DateTime.Now;
            if (starting == null)
            {
                starting = new System.Windows.Forms.Timer();
                starting.Interval = 400;
                starting.Tick += delegate
                {
                    if (Env.EngineResponds() || (DateTime.Now - startingSince).TotalSeconds > 10)
                    {
                        starting.Stop();
                        Refresh2();
                    }
                };
            }
            starting.Start();
        }

        void CheckNow()
        {
            checkNow.Enabled = false;
            checkNow.Text = "Checking…";
            var worker = new System.Threading.Thread(delegate ()
            {
                string message;
                MessageBoxIcon icon;
                Env.CheckForUpdates(out message, out icon);
                try
                {
                    BeginInvoke((MethodInvoker)delegate
                    {
                        checkNow.Text = "Check now";
                        checkNow.Enabled = true;
                        MessageBox.Show(this, message, "Likhi updates", MessageBoxButtons.OK, icon);
                    });
                }
                catch (InvalidOperationException) { } // the window was closed while checking
            });
            worker.IsBackground = true;
            worker.Start();
        }

        /// <summary>Show the chosen face in the try-it box, so the choice is visible before typing.</summary>
        void PreviewFont()
        {
            try
            {
                string family = fontBox.SelectedItem as string;
                float size;
                if (family == null || !float.TryParse(sizeBox.SelectedItem as string, out size)) return;
                Font old = tryHere.Font;
                // Points here, pixels in the candidate window: this box is ordinary UI text.
                tryHere.Font = new Font(family, size * 0.9f, FontStyle.Regular, GraphicsUnit.Pixel);
                if (old != null) old.Dispose();
            }
            catch { }
        }

        void RunDiagnostics()
        {
            try
            {
                ProcessStartInfo psi = new ProcessStartInfo("powershell.exe",
                    "-NoProfile -ExecutionPolicy Bypass -File \"" +
                    Path.Combine(Env.AppDir, "diagnose.ps1") + "\"");
                psi.UseShellExecute = true;
                Process.Start(psi);
            }
            catch (Exception ex)
            {
                MessageBox.Show(this, "Could not run the diagnostics: " + ex.Message,
                    "Likhi", MessageBoxButtons.OK, MessageBoxIcon.Warning);
            }
        }

        void Open(string url)
        {
            try { Process.Start(new ProcessStartInfo(url) { UseShellExecute = true }); } catch { }
        }

        Label Head(string text, float size, Color colour, int x, int y)
        {
            Label l = new Label();
            l.Text = text; l.AutoSize = true; l.Location = new Point(x, y);
            l.Font = new Font("Segoe UI", size, FontStyle.Bold);
            l.ForeColor = colour; l.BackColor = Color.Transparent;
            Controls.Add(l); return l;
        }

        // Height is explicit rather than AutoSize: these labels sit on a hand-laid grid, and an
        // auto-sized label reports a height that does not match the line spacing around it, which
        // is how the first build ended up with every line drawn over the one above.
        Label Body(string text, int x, int y, Color colour, string family = "Segoe UI", int h = 20)
        {
            Label l = new Label();
            l.Text = text; l.AutoSize = false;
            l.Location = new Point(x, y); l.Size = new Size(502 - x, h);
            l.Font = new Font(family, 9.5f);
            l.ForeColor = colour; l.BackColor = Color.Transparent;
            Controls.Add(l); return l;
        }

        CheckBox Check(string text, int x, int y)
        {
            CheckBox c = new CheckBox();
            c.Text = text; c.AutoSize = true; c.Location = new Point(x, y);
            c.ForeColor = Fg; c.BackColor = Color.Transparent;
            Controls.Add(c); return c;
        }

        Button Btn(string text, int x, int y, int w)
        {
            Button b = new Button();
            b.Text = text; b.Location = new Point(x, y); b.Size = new Size(w, 30);
            b.FlatStyle = FlatStyle.System;
            Controls.Add(b); return b;
        }
    }
}
