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

        // Ask the engine directly. Anything else -- a running process, a registry value -- can be
        // true while the engine is wedged.
        public static bool EngineResponds()
        {
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
                    k.SetValue("LikhiEngine", "\"" + Path.Combine(AppDir, @"runtime\likhi-server.cmd") + "\"");
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

        /// <summary>Whether Bangla has been pointed at a chosen font everywhere on this machine.
        /// The backup file exists only while that is in force, so it is the honest test.</summary>
        public static bool SystemFontApplied()
        {
            return File.Exists(Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.CommonApplicationData),
                @"Likhi\system-font-backup.json"));
        }

        /// <summary>Apply or undo the machine-wide setting. Raises a UAC prompt: it is machine-wide,
        /// so it cannot be done from this window's own rights, and should not be.</summary>
        public static bool SetSystemFont(string family, bool apply)
        {
            string script = Path.Combine(AppDir, "system_font.ps1");
            if (!File.Exists(script)) return false;
            string args = "-NoProfile -ExecutionPolicy Bypass -File \"" + script + "\" " +
                (apply ? "-Apply \"" + family + "\"" : "-Restore");
            try
            {
                var psi = new ProcessStartInfo("powershell.exe", args);
                psi.Verb = "runas";
                psi.UseShellExecute = true;
                psi.WindowStyle = ProcessWindowStyle.Hidden;
                Process p = Process.Start(psi);
                p.WaitForExit();
                return p.ExitCode == 0;
            }
            catch { return false; }   // the person declined the prompt, which is an answer
        }

        static bool Known(string name)
        {
            string[] families = {
                "Noto Sans Bengali", "Anek Bangla", "Hind Siliguri", "Tiro Bangla",
                "Nirmala UI", "Nirmala Text", "Shonar Bangla", "Vrinda", "Google Sans"
            };
            foreach (string f in families) if (string.Equals(f, name, StringComparison.OrdinalIgnoreCase)) return true;
            return false;
        }

        public static void RestartEngine()
        {
            try
            {
                foreach (Process p in Process.GetProcessesByName("pythonw"))
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
            catch { }
            try
            {
                ProcessStartInfo psi = new ProcessStartInfo(
                    Path.Combine(AppDir, @"runtime\likhi-server.cmd"));
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
        CheckBox autostart, reporting, systemFont;
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
            ClientSize = new Size(520, 630);
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
                "5.   F12 switches to plain English without leaving Bangla mode."
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
            y += 32;
            systemFont = Check("Use this font for Bangla in all apps", 18, y); y += 22;
            Body("Changes what Bangla looks like everywhere, not just here. Reversible.", 40, y, Dim, "Segoe UI", 18);
            y += 30;

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

            autostart = Check("Start Likhi when I sign in", 18, y); y += 26;
            reporting = Check("Share anonymous usage data to improve suggestions", 18, y); y += 24;
            Body("Counts only, plus words where the first suggestion was wrong. Never passwords,", 40, y, Dim, "Segoe UI", 18);
            y += 17;
            Body("numbers, or anything typed into a password box.", 40, y, Dim, "Segoe UI", 18);
            y += 30;

            Button diag = Btn("Run diagnostics", 18, y, 150);
            diag.Click += delegate { RunDiagnostics(); };
            Button restart = Btn("Restart engine", 180, y, 140);
            restart.Click += delegate { Env.RestartEngine(); System.Threading.Thread.Sleep(1500); Refresh2(); };
            Button site = Btn("Project page", 332, y, 170);
            site.Click += delegate { Open("https://github.com/KhaledBinAmir/likhi"); };

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
            systemFont.CheckedChanged += delegate
            {
                if (loading) return;
                string family = fontBox.SelectedItem as string;
                bool wanted = systemFont.Checked;
                if (wanted && string.IsNullOrEmpty(family)) { systemFont.Checked = false; return; }
                if (!Env.SetSystemFont(family, wanted))
                {
                    // Declined the prompt, or the script failed: put the box back rather than
                    // leaving it claiming something that did not happen.
                    loading = true;
                    systemFont.Checked = !wanted;
                    loading = false;
                    return;
                }
                MessageBox.Show(this,
                    wanted
                        ? "Bangla will use " + family + " everywhere.\n\nApplications read this when they start, so restart the ones you have open."
                        : "Bangla is back to the system font everywhere.\n\nRestart open applications to see it.",
                    "Likhi", MessageBoxButtons.OK, MessageBoxIcon.Information);
            };

            Refresh2();
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
            systemFont.Checked = Env.SystemFontApplied();
            PreviewFont();
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
