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
using System.Diagnostics;
using System.Drawing;
using System.IO;
using System.Net.Sockets;
using System.Text;
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
        public const string Tip = "0845:{35F67E9D-A54D-4177-9697-8B0AB71A9E04}{9B4E7C21-3D5A-4F86-A2E1-6C0D8B7F5A13}";
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
                if (on)
                {
                    k.SetValue("LikhiLauncher", "\"" + Path.Combine(PimeDir, "PIMELauncher.exe") + "\"");
                    k.SetValue("LikhiEngine", "\"" + Path.Combine(AppDir, @"runtime\likhi-server.cmd") + "\"");
                }
                else
                {
                    try { k.DeleteValue("LikhiLauncher", false); } catch { }
                    try { k.DeleteValue("LikhiEngine", false); } catch { }
                }
            }
        }

        // Usage reporting is stored per user, so one person turning it off does not decide for
        // everyone on a shared machine, and so it needs no administrator. The engine merges this
        // over the installed config, which keeps the destination when reporting is switched off.
        public static bool ReportingOn()
        {
            try
            {
                string machine = Path.Combine(PimeDir, @"python\input_methods\likhi\config.json");
                bool on = ReadTelemetry(machine);
                string user = UserConfigPath;
                if (File.Exists(user)) on = ReadTelemetry(user);
                return on;
            }
            catch { return false; }
        }

        static bool ReadTelemetry(string path)
        {
            if (!File.Exists(path)) return false;
            // Deliberately not a JSON parser: this file has one line per key and we need one value.
            foreach (string line in File.ReadAllLines(path))
            {
                string t = line.Trim();
                if (t.StartsWith("\"telemetry\"") && t.Contains(":"))
                    return !t.Contains("\"off\"");
            }
            return false;
        }

        public static void SetReporting(bool on)
        {
            string path = UserConfigPath;
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            // Only the one key: everything else keeps coming from the installed config.
            File.WriteAllText(path,
                "{\n  \"telemetry\": \"" + (on ? "full" : "off") + "\"\n}\n",
                new UTF8Encoding(false));
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
        CheckBox autostart, reporting;
        TextBox tryHere;
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
            Text = "Likhi";
            ClientSize = new Size(520, 522);
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

            Refresh2();
        }

        // Named to avoid colliding with Form.Refresh().
        void Refresh2()
        {
            loading = true;
            try { RefreshInner(); } finally { loading = false; }
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
