# Drive the installed Likhi keyboard in Notepad and check what it typed.
#
# Opens a scratch file in Notepad, switches the session to Likhi, types each case with SendInput --
# real keystrokes, through TSF and the text service exactly as a person's would -- and reads the
# document back through UI Automation. Afterwards it restores the input method that was active, the
# settings file byte for byte, and a running engine.
#
# It types into whatever window is in front, so it refuses to press a key unless Notepad is the
# foreground window, and stops the moment anything else is. Leave the machine alone while it runs.
#
# Usage reporting is switched off for the run, so test typing never reaches the pilot's numbers.
# The engine is restarted to apply that, and again at the end to put it back.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File scripts\type_test.ps1
#   ... -Case basic,predict_tab      # only these
#
# ASCII only, Bengali written as \u escapes: Windows PowerShell reads a file without a byte-order
# mark as the ANSI code page, and the repository does not allow byte-order marks.

param(
    [string[]]$Case = @(),
    [int]$KeyDelayMs = 45
)

$ErrorActionPreference = 'Stop'
# "-Case a,b" arrives as one string when the script is run with -File.
$Case = @($Case | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes

Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;

public static class Keys
{
    [DllImport("user32.dll")] static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] static extern bool SetForegroundWindow(IntPtr h);
    [DllImport("user32.dll")] static extern bool ShowWindow(IntPtr h, int cmd);
    [DllImport("user32.dll")] static extern bool IsIconic(IntPtr h);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] static extern bool AttachThreadInput(uint a, uint b, bool attach);
    [DllImport("kernel32.dll")] static extern uint GetCurrentThreadId();
    [DllImport("user32.dll", SetLastError = true)] static extern uint SendInput(uint n, INPUT[] inputs, int size);

    [StructLayout(LayoutKind.Sequential)]
    struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public IntPtr extra; }
    [StructLayout(LayoutKind.Sequential)]
    struct MOUSEINPUT { public int dx; public int dy; public uint data; public uint flags; public uint time; public IntPtr extra; }
    [StructLayout(LayoutKind.Explicit)]
    struct UNION { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; }
    [StructLayout(LayoutKind.Sequential)]
    struct INPUT { public uint type; public UNION u; }

    const uint KEYEVENTF_KEYUP = 0x2;
    const ushort VK_SHIFT = 0x10, VK_CONTROL = 0x11;

    public static IntPtr Target = IntPtr.Zero;

    static void Raw(ushort vk, bool up)
    {
        var i = new INPUT[1];
        i[0].type = 1;
        i[0].u.ki.wVk = vk;
        i[0].u.ki.dwFlags = up ? KEYEVENTF_KEYUP : 0;
        if (SendInput(1, i, Marshal.SizeOf(typeof(INPUT))) != 1)
            throw new Exception("SendInput failed: " + Marshal.GetLastWin32Error());
    }

    public static bool InFront() { return Target != IntPtr.Zero && GetForegroundWindow() == Target; }

    // Bring the target forward. Windows only lets the foreground process hand the foreground away,
    // so this borrows the foreground thread's input state for the call -- the usual test-harness
    // route, and harmless: nothing is typed until InFront() confirms it worked.
    public static bool Focus(IntPtr hwnd)
    {
        Target = hwnd;
        for (int attempt = 0; attempt < 10 && !InFront(); attempt++)
        {
            if (IsIconic(hwnd)) ShowWindow(hwnd, 9);
            uint pid;
            IntPtr front = GetForegroundWindow();
            if (front == IntPtr.Zero || attempt >= 3)
            {
                // Nothing to borrow from -- no window is in front, which is what is left after a
                // menu closes -- or borrowing is not working. A tapped Alt lifts the foreground lock
                // for the next call; it is the documented-by-folklore route every UI test uses.
                Raw(0x12, false);
                SetForegroundWindow(hwnd);
                Raw(0x12, true);
            }
            else
            {
                uint fg = GetWindowThreadProcessId(front, out pid);
                uint me = GetCurrentThreadId();
                AttachThreadInput(me, fg, true);
                SetForegroundWindow(hwnd);
                AttachThreadInput(me, fg, false);
            }
            Thread.Sleep(150);
        }
        return InFront();
    }

    [DllImport("user32.dll")] static extern bool SetCursorPos(int x, int y);

    // A real left click at (x, y), in the same coordinates UI Automation reports to this process.
    // Refuses unless the target is in front, like a keystroke.
    public static void Click(int x, int y)
    {
        if (!InFront()) throw new Exception("stopped: Notepad is no longer the foreground window");
        SetCursorPos(x, y);
        Thread.Sleep(50);
        var i = new INPUT[2];
        i[0].type = 0; i[0].u.mi.flags = 0x0002; // MOUSEEVENTF_LEFTDOWN
        i[1].type = 0; i[1].u.mi.flags = 0x0004; // MOUSEEVENTF_LEFTUP
        if (SendInput(2, i, Marshal.SizeOf(typeof(INPUT))) != 2)
            throw new Exception("SendInput (mouse) failed: " + Marshal.GetLastWin32Error());
        Thread.Sleep(200);
    }

    // One key, pressed and released, with modifiers held around it. Refuses unless the target is in
    // front: a keystroke sent anywhere else lands in somebody's real work.
    public static void Press(ushort vk, bool shift, bool ctrl, int delayMs)
    {
        if (!InFront()) throw new Exception("stopped: Notepad is no longer the foreground window");
        if (ctrl) Raw(VK_CONTROL, false);
        if (shift) Raw(VK_SHIFT, false);
        Raw(vk, false);
        Raw(vk, true);
        if (shift) Raw(VK_SHIFT, true);
        if (ctrl) Raw(VK_CONTROL, true);
        Thread.Sleep(delayMs);
    }
}

[ComImport, Guid("71C6E74C-0F28-11D8-A82A-00065B84435C"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface ITfInputProcessorProfileMgr
{
    [PreserveSig] int ActivateProfile(uint type, ushort langid, ref Guid clsid, ref Guid profile, IntPtr hkl, uint flags);
    [PreserveSig] int DeactivateProfile(uint type, ushort langid, ref Guid clsid, ref Guid profile, IntPtr hkl, uint flags);
    [PreserveSig] int GetProfile(uint type, ushort langid, ref Guid clsid, ref Guid profile, IntPtr hkl, out Profile p);
    [PreserveSig] int EnumProfiles(ushort langid, out IntPtr e);
    [PreserveSig] int ReleaseInputProcessor(ref Guid clsid, uint flags);
    [PreserveSig] int RegisterProfile(IntPtr a, IntPtr b, IntPtr c, IntPtr d, IntPtr e, IntPtr f, IntPtr g, IntPtr h, IntPtr i, IntPtr j);
    [PreserveSig] int UnregisterProfile(ref Guid clsid, ushort langid, ref Guid profile, uint flags);
    [PreserveSig] int GetActiveProfile(ref Guid category, out Profile p);
}

[StructLayout(LayoutKind.Sequential)]
public struct Profile
{
    public uint Type; public ushort LangId; public Guid Clsid; public Guid ProfileGuid; public Guid Category;
    public IntPtr HklSubstitute; public uint Caps; public IntPtr Hkl; public uint Flags;
}

public static class Ime
{
    static readonly Guid CLSID_TF_InputProcessorProfiles = new Guid("33C53A50-F456-4884-B049-85FD643ECFED");
    static readonly Guid GUID_TFCAT_TIP_KEYBOARD = new Guid("34745C63-B2F0-4784-8B67-5E12C8701A31");
    // shell/src/guids.rs
    static readonly Guid LikhiClsid = new Guid("1D24C804-FAD0-4B32-AEDD-1317F4E6221E");
    static readonly Guid LikhiProfile = new Guid("502AB3FE-5B7C-43E9-89D1-BE885846AE0D");
    const uint FOR_SESSION = 0x20000000, DONT_CARE_LANGUAGE = 0x00000004;

    static ITfInputProcessorProfileMgr Mgr()
    {
        return (ITfInputProcessorProfileMgr)Activator.CreateInstance(Type.GetTypeFromCLSID(CLSID_TF_InputProcessorProfiles));
    }

    public static Profile Current()
    {
        Guid cat = GUID_TFCAT_TIP_KEYBOARD;
        Profile p;
        int hr = Mgr().GetActiveProfile(ref cat, out p);
        if (hr != 0) throw new Exception("GetActiveProfile 0x" + hr.ToString("X8"));
        return p;
    }

    public static void Activate(Profile p)
    {
        Guid c = p.Clsid, g = p.ProfileGuid;
        int hr = Mgr().ActivateProfile(p.Type, p.LangId, ref c, ref g, p.Hkl, FOR_SESSION | DONT_CARE_LANGUAGE);
        if (hr != 0) throw new Exception("ActivateProfile 0x" + hr.ToString("X8"));
    }

    public static void ActivateLikhi()
    {
        Activate(new Profile { Type = 1, LangId = 0x0845, Clsid = LikhiClsid, ProfileGuid = LikhiProfile });
    }
}

public static class Wnd
{
    public delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc f, IntPtr l);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetClassName(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);

    // A top-level window of `cls` whose title contains `title`; visible ones only when asked.
    public static IntPtr Find(string cls, string title, bool visibleOnly)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate (IntPtr h, IntPtr l)
        {
            var c = new StringBuilder(256); GetClassName(h, c, 256);
            var t = new StringBuilder(512); GetWindowText(h, t, 512);
            if (c.ToString() == cls && t.ToString().Contains(title) && (!visibleOnly || IsWindowVisible(h))) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // The popup menu on screen that belongs to process `pid`, if any.
    public static IntPtr MenuOf(uint pid)
    {
        IntPtr found = IntPtr.Zero;
        EnumWindows(delegate (IntPtr h, IntPtr l)
        {
            var c = new StringBuilder(64); GetClassName(h, c, 64);
            uint p; GetWindowThreadProcessId(h, out p);
            if (c.ToString() == "#32768" && IsWindowVisible(h) && p == pid) { found = h; return false; }
            return true;
        }, IntPtr.Zero);
        return found;
    }

    // Whether a Likhi candidate window is on screen in the process that owns `hwnd`.
    public static bool CandidatesVisible(IntPtr hwnd)
    {
        uint owner; GetWindowThreadProcessId(hwnd, out owner);
        bool shown = false;
        EnumWindows(delegate (IntPtr h, IntPtr l)
        {
            var c = new StringBuilder(256); GetClassName(h, c, 256);
            uint pid; GetWindowThreadProcessId(h, out pid);
            if (c.ToString() == "LikhiCandidateWindow" && IsWindowVisible(h) && pid == owner) { shown = true; return false; }
            return true;
        }, IntPtr.Zero);
        return shown;
    }
}
'@

function U([string]$s) { [regex]::Unescape($s) }
$AMI = U '\u0986\u09AE\u09BF'   # "ami" -> the first-person pronoun
# From code points: an editing tool once turned \u escapes written here into the characters
# themselves, which Windows PowerShell then read as ANSI.
$TUMI = -join [char[]](0x09A4, 0x09C1, 0x09AE, 0x09BF)   # "tumi" -> you

# ----------------------------------------------------------------------------------- the machine

$likhiDir = 'C:\Program Files\Likhi'
$engineExe = Join-Path $likhiDir 'engine\likhi-server.exe'
$userConfig = Join-Path $env:LOCALAPPDATA 'Likhi\config.json'
# The personal dictionary learns from every word committed, test words included; it is put back
# exactly as it was, so a run teaches this person's keyboard nothing.
$personal = @('personal.sqlite', 'personal.sqlite-wal', 'personal.sqlite-shm') | ForEach-Object { Join-Path $env:LOCALAPPDATA "Likhi\$_" }
$marker = Join-Path $env:LOCALAPPDATA 'Likhi\exited'
$pipe = "\\.\pipe\likhi-engine-s$((Get-Process -Id $PID).SessionId)"

function Engine-Running { [bool](Get-Process likhi-server -ErrorAction SilentlyContinue) }

function Stop-Engine {
    Get-Process likhi-server -ErrorAction SilentlyContinue | ForEach-Object { $_.Kill(); $_.WaitForExit(5000) | Out-Null }
}

function Wait-Engine([int]$seconds = 15) {
    $deadline = (Get-Date).AddSeconds($seconds)
    while ((Get-Date) -lt $deadline) {
        try { if ((Ask-Engine '{"op":"ping"}') -match '"ok"') { return } } catch { }
        Start-Sleep -Milliseconds 250
    }
    throw 'the engine did not answer'
}

function Start-Engine {
    Start-Process $engineExe -WindowStyle Hidden | Out-Null
    Wait-Engine
}

function Ask-Engine([string]$json) {
    $c = New-Object System.Net.Sockets.TcpClient
    $ar = $c.BeginConnect('127.0.0.1', 47123, $null, $null)
    if (-not $ar.AsyncWaitHandle.WaitOne(500)) { $c.Close(); throw 'no engine' }
    try {
        $c.EndConnect($ar)
        $s = $c.GetStream(); $s.ReadTimeout = 10000
        $b = [Text.Encoding]::UTF8.GetBytes($json + "`n"); $s.Write($b, 0, $b.Length)
        return (New-Object IO.StreamReader($s, [Text.Encoding]::UTF8)).ReadLine()
    } finally { $c.Close() }
}

function Next-Word([string]$after) {
    $req = @{ op = 'next'; context = @($after); k = 1; min_share = 0.2 } | ConvertTo-Json -Compress
    $words = (Ask-Engine $req | ConvertFrom-Json).candidates
    if ($words) { return $words[0] } else { return $null }
}

# The text service's placement rule for first-letter predictions (with_predictions in
# shell/src/service.rs): up to two after the third entry, moved up if already lower.
function Merge-Predictions($base, $predicted, [int]$k = 5) {
    $list = New-Object System.Collections.Generic.List[string]
    $head = @($base | Select-Object -First 3)
    foreach ($w in $head) { $list.Add($w) }
    $extra = @($predicted | Where-Object { $head -notcontains $_ } | Select-Object -First 2)
    foreach ($w in $extra) { $list.Add($w) }
    foreach ($w in @($base | Select-Object -Skip 3)) { if ($extra -notcontains $w) { $list.Add($w) } }
    return @($list | Select-Object -First $k)
}

# One key of the per-user settings file, as the Likhi window writes it. The original bytes are
# put back at the end whatever happens.
function Set-UserSetting([string]$key, [string]$jsonValue) {
    $o = [ordered]@{}
    if (Test-Path $userConfig) {
        $parsed = [IO.File]::ReadAllText($userConfig) | ConvertFrom-Json
        foreach ($p in $parsed.PSObject.Properties) { $o[$p.Name] = $p.Value }
    }
    $o[$key] = $jsonValue | ConvertFrom-Json
    New-Item -ItemType Directory -Force (Split-Path $userConfig) | Out-Null
    [IO.File]::WriteAllText($userConfig, ($o | ConvertTo-Json), (New-Object Text.UTF8Encoding($false)))
}

# -------------------------------------------------------------------------------------- notepad

$vkFor = @{ ' ' = 0x20 }
foreach ($c in [char[]]'abcdefghijklmnopqrstuvwxyz') { $vkFor[[string]$c] = [int][char]::ToUpper($c) }
foreach ($d in 0..9) { $vkFor[[string]$d] = 0x30 + $d }
$named = @{ TAB = 0x09; ESC = 0x1B; BACK = 0x08; ENTER = 0x0D; F12 = 0x7B; LEFT = 0x25; RIGHT = 0x27
    HOME = 0x24; END = 0x23; DELETE = 0x2E }

# "ami {TAB}x" -> keystrokes. Braces name a key ({^END} holds Ctrl), {WAIT} pauses; everything else
# is typed as itself.
function Send-Keys([string]$text) {
    $i = 0
    while ($i -lt $text.Length) {
        if ($text[$i] -eq '{') {
            $end = $text.IndexOf('}', $i)
            $name = $text.Substring($i + 1, $end - $i - 1)
            $ctrl = $name.StartsWith('^')
            if ($ctrl) { $name = $name.Substring(1) }
            if ($name -eq 'WAIT') { Start-Sleep -Milliseconds 1500 }
            else { [Keys]::Press([uint16]$named[$name], $false, $ctrl, $KeyDelayMs) }
            $i = $end + 1
            continue
        }
        $ch = [string]$text[$i]
        if (-not $vkFor.ContainsKey($ch.ToLower())) { throw "no key for '$ch'" }
        [Keys]::Press([uint16]$vkFor[$ch.ToLower()], ($ch -cmatch '[A-Z]'), $false, $KeyDelayMs)
        $i++
    }
    Start-Sleep -Milliseconds 200
}

function Document {
    $root = [System.Windows.Automation.AutomationElement]::FromHandle($script:notepad)
    $cond = New-Object System.Windows.Automation.PropertyCondition(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Document)
    $doc = $root.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
    if (-not $doc) { throw 'no document element in Notepad' }
    return $doc.GetCurrentPattern([System.Windows.Automation.TextPattern]::Pattern)
}

function Text { ((Document).DocumentRange.GetText(-1) -replace "`r`n", "`n") -replace "`r", "`n" }

# A real mouse click just before the first character of the document.
function Click-DocumentStart {
    $rects = (Document).DocumentRange.GetBoundingRectangles()
    if (-not $rects -or $rects.Count -eq 0) { throw 'the document reported no text position to click' }
    $r = $rects[0]
    [Keys]::Click([int]($r.Left + 1), [int]($r.Top + $r.Height / 2))
}

# Put the caret at the start of the document without a keystroke. Not what a click does: an
# application does not end the word being typed for this, and a click it does.
function Move-CaretHome {
    $r = (Document).DocumentRange.Clone()
    $r.MoveEndpointByRange([System.Windows.Automation.Text.TextPatternRangeEndpoint]::End, $r,
        [System.Windows.Automation.Text.TextPatternRangeEndpoint]::Start)
    $r.Select()
    Start-Sleep -Milliseconds 150
}

function Clear-Document {
    Focus-Notepad
    [Keys]::Press(0x41, $false, $true, $KeyDelayMs)   # Ctrl+A
    [Keys]::Press(0x2E, $false, $false, $KeyDelayMs)  # Delete
    Start-Sleep -Milliseconds 100
}

function Focus-Notepad { if (-not [Keys]::Focus($script:notepad)) { throw 'could not bring Notepad to the front; stopped' } }

function List-Shown { [Wnd]::CandidatesVisible($script:notepad) }

# --------------------------------------------------------------------------------------- checks

$script:results = New-Object System.Collections.ArrayList
$script:current = ''

function Show([string]$s) { ($s -replace "`t", '\t') -replace "`n", '\n' }

function Expect-Text([string]$want) {
    $got = Text
    [void]$script:results.Add([pscustomobject]@{ Case = $script:current; Check = 'text'; Pass = ($got -ceq $want); Expected = (Show $want); Got = (Show $got) })
}

function Expect-List([bool]$want) {
    $got = List-Shown
    [void]$script:results.Add([pscustomobject]@{ Case = $script:current; Check = 'list shown'; Pass = ($got -eq $want); Expected = $want; Got = $got })
}

function Expect-True([string]$what, [bool]$value) {
    [void]$script:results.Add([pscustomobject]@{ Case = $script:current; Check = $what; Pass = $value; Expected = $true; Got = $value })
}

# A word that is followed by a next-word suggestion, found by typing candidates for real: the
# suggestion depends on the engine's tables and threshold, so it is looked up rather than assumed.
$script:predictable = $null
function Predictable {
    if ($script:predictable) { return $script:predictable }
    foreach ($roman in 'kemon', 'ami', 'tumi', 'valo', 'ki', 'amar', 'apni', 'ei', 'shuvo', 'dhonnobad', 'kothay', 'ekta') {
        Clear-Document
        Send-Keys "$roman "
        if (List-Shown) {
            $bangla = (Text).TrimEnd(' ')
            $script:predictable = @{ Roman = $roman; Bangla = $bangla; Next = (Next-Word $bangla) }
            return $script:predictable
        }
    }
    throw 'no candidate word produced a next-word suggestion'
}

# ---------------------------------------------------------------------------------------- cases

$cases = [ordered]@{
    basic = {
        Clear-Document; Send-Keys 'ami '
        Expect-Text "$AMI "
    }
    capital = {
        Clear-Document; Send-Keys 'Ami '
        Expect-Text "$AMI "
    }
    # Keys that end a word without being part of it commit the highlighted word first, then do
    # their own job. They used to leave the Latin behind.
    tab_commits = {
        Clear-Document; Send-Keys 'ami{TAB}'
        Expect-Text "$AMI`t"
    }
    home_commits = {
        Clear-Document; Send-Keys 'ami{HOME}ami '
        Expect-Text "$AMI $AMI"
    }
    delete_commits = {
        Clear-Document; Send-Keys 'ami{DELETE}'
        Expect-Text "$AMI"
    }
    ctrl_commits = {
        Clear-Document; Send-Keys 'ami{^END} '
        Expect-Text "$AMI "
    }
    # A click elsewhere ends the word without a key: the application ends it, and it must end as
    # the highlighted Bangla, not the Latin on screen.
    click_commits = {
        Clear-Document; Send-Keys 'ami tumi'
        Click-DocumentStart
        Expect-Text "$AMI $TUMI"
    }
    # The guess straight after Space is off unless configured.
    after_space_off_by_default = {
        $w = Predictable
        Set-UserSetting 'next_word_after_space' 'false'
        Start-Sleep -Milliseconds 2500
        Clear-Document; Send-Keys "$($w.Roman) "
        Expect-List $false
        Set-UserSetting 'next_word_after_space' 'true'
        Start-Sleep -Milliseconds 2500
    }
    # The suggestion appears after Space, and Tab types it followed by a space.
    predict_tab = {
        $w = Predictable
        Expect-True "engine suggests after '$($w.Bangla)'" ([bool]$w.Next)
        Clear-Document; Send-Keys "$($w.Roman) "
        Expect-List $true
        Send-Keys '{TAB}'
        Expect-Text "$($w.Bangla) $($w.Next) "
    }
    # Ignoring it costs nothing: the next word is typed exactly as it would have been.
    predict_ignored = {
        $w = Predictable
        Clear-Document; Send-Keys "$($w.Roman) ami "
        Expect-Text "$($w.Bangla) $AMI "
    }
    predict_escape = {
        $w = Predictable
        Clear-Document; Send-Keys "$($w.Roman) {ESC}"
        Expect-Text "$($w.Bangla) "
        Expect-List $false
    }
    # A key the keyboard lets through still ends the suggestion, and does its own job.
    predict_backspace = {
        $w = Predictable
        Clear-Document; Send-Keys "$($w.Roman) {BACK}"
        Expect-Text "$($w.Bangla)"
        Expect-List $false
    }
    predict_enter = {
        $w = Predictable
        Clear-Document; Send-Keys "$($w.Roman) {ENTER}"
        Expect-Text "$($w.Bangla) `n"
        Expect-List $false
    }
    predict_toggle = {
        $w = Predictable
        Clear-Document; Send-Keys "$($w.Roman) {F12}ami {F12}"
        Expect-Text "$($w.Bangla) ami "
        Expect-List $false
    }
    # First-letter prediction: after the previous word and one letter, a word that usually follows
    # is in the list at the position the placement rule gives -- fourth -- and its number types it.
    predict_letter = {
        $w = Predictable
        $plan = $null
        foreach ($letter in 'a', 'v', 'b', 'p', 'k', 's', 'e', 'o') {
            $options = @(); $predicted = @()
            foreach ($deadline in 0, 400) {
                $req = @{ op = 'suggest'; roman = $letter; context = @($w.Bangla); k = 5; deadline_ms = $deadline } | ConvertTo-Json -Compress
                $r = Ask-Engine $req | ConvertFrom-Json
                $predicted += @($r.next)
                $shown = Merge-Predictions @($r.candidates) @($r.next)
                if ($shown.Count -ge 4 -and @($r.next) -contains $shown[3]) { $options += $shown[3] }
            }
            if ($options.Count) { $plan = @{ Letter = $letter; Predicted = @($predicted | Select-Object -Unique) }; break }
        }
        Expect-True "engine predicts after '$($w.Bangla)' + a letter" ([bool]$plan)
        if (-not $plan) { return }
        Clear-Document; Send-Keys "$($w.Roman) $($plan.Letter)4"
        $got = (Text)
        # Any of the engine's predictions: which one lands fourth depends on whether the keyboard
        # was showing the fast answer or the full ranking, and the test cannot see which. What it
        # checks is the placement -- that the fourth entry is a prediction and its number types it.
        $ok = $false
        foreach ($p in $plan.Predicted) { if ($got -ceq "$($w.Bangla) $p") { $ok = $true } }
        [void]$script:results.Add([pscustomobject]@{ Case = $script:current; Check = 'a prediction typed'; Pass = $ok; Expected = (Show "$($w.Bangla) $($plan.Predicted -join '|')"); Got = (Show $got) })
    }
    # The caret moved without a key (a click): Tab must not drop the word somewhere else.
    predict_caret_moved = {
        $w = Predictable
        Clear-Document; Send-Keys "$($w.Roman) "
        Move-CaretHome
        Focus-Notepad
        Send-Keys '{TAB}'
        Expect-Text "$($w.Bangla) "
    }
    # Switched off in the Likhi window: no suggestion, and Tab is an ordinary Tab again.
    next_word_off = {
        $w = Predictable
        Set-UserSetting 'next_word' 'false'
        Start-Sleep -Milliseconds 2500
        Clear-Document; Send-Keys "$($w.Roman) "
        Expect-List $false
        Send-Keys '{TAB}'
        Expect-Text "$($w.Bangla) `t"
        Set-UserSetting 'next_word' 'true'
        Start-Sleep -Milliseconds 2500
    }
    # The engine died: the first word goes through as English while the keyboard restarts it, and
    # Bangla is back for the next.
    engine_restarts = {
        Stop-Engine
        Clear-Document; Send-Keys 'ami {WAIT}{WAIT}ami '
        Expect-Text "ami $AMI "
        Expect-True 'engine running again' (Engine-Running)
    }
    # "Exit Likhi" from the tray menu, driven through the menu itself.
    tray_exit = {
        Wait-Engine
        $tray = [Wnd]::Find('LikhiTray', 'Likhi', $false)
        Expect-True 'tray window exists' ($tray -ne [IntPtr]::Zero)
        $enginePid = [uint32](Get-Process likhi-server).Id
        [void][Wnd]::PostMessage($tray, 0x800B, [IntPtr]::Zero, [IntPtr]0x0205)   # WM_TRAY, right click
        $menu = [IntPtr]::Zero
        $deadline = (Get-Date).AddSeconds(5)
        while ($menu -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 150; $menu = [Wnd]::MenuOf($enginePid) }
        Expect-True 'menu opened' ($menu -ne [IntPtr]::Zero)
        if ($menu -eq [IntPtr]::Zero) { return }
        # Chosen with the keyboard, as a person could: Up wraps to the last item, Exit Likhi. UI
        # Automation sees no items in a menu whose owner is not the foreground window, and a test
        # cannot make the engine the foreground window.
        [void][Wnd]::PostMessage($menu, 0x0100, [IntPtr]0x26, [IntPtr]::Zero)   # WM_KEYDOWN Up
        Start-Sleep -Milliseconds 200
        [void][Wnd]::PostMessage($menu, 0x0100, [IntPtr]0x0D, [IntPtr]::Zero)   # WM_KEYDOWN Enter
        $deadline = (Get-Date).AddSeconds(5)
        while ((Engine-Running) -and (Get-Date) -lt $deadline) { Start-Sleep -Milliseconds 100 }
        Expect-True 'engine stopped' (-not (Engine-Running))
        Expect-True 'exit remembered' (Test-Path $marker)
        # The keyboard must not bring it back, and types plain English meanwhile.
        Clear-Document; Send-Keys 'ami {WAIT}ami '
        Expect-Text 'ami ami '
        Expect-List $false
        Expect-True 'still exited after typing' (-not (Engine-Running))
        # Opening Likhi starts it again.
        $app = Start-Process (Join-Path $likhiDir 'Likhi.exe') -PassThru
        Wait-Engine
        Start-Sleep -Milliseconds 500
        Expect-True 'exit forgotten once started' (-not (Test-Path $marker))
        $app.CloseMainWindow() | Out-Null
        Start-Sleep -Milliseconds 500
        Clear-Document; Send-Keys 'ami '
        Expect-Text "$AMI "
    }
}

# ------------------------------------------------------------------------------------------ run

$scratch = Join-Path $env:TEMP 'likhi-type-test.txt'
[IO.File]::WriteAllText($scratch, '')
$savedConfig = if (Test-Path $userConfig) { [IO.File]::ReadAllBytes($userConfig) } else { $null }
$savedPersonal = @{}
$before = [Ime]::Current()
$script:notepad = [IntPtr]::Zero
try {
    Set-UserSetting 'telemetry' '"off"'
    Set-UserSetting 'next_word' 'true'
    # Off by default since 0.5.2 but still shipped behind the setting, so still tested.
    Set-UserSetting 'next_word_after_space' 'true'
    Stop-Engine
    # With the engine stopped, so the dictionary's files are complete and not being written.
    foreach ($f in $personal) { $savedPersonal[$f] = if (Test-Path $f) { [IO.File]::ReadAllBytes($f) } else { $null } }
    Remove-Item $marker -ErrorAction SilentlyContinue
    Start-Engine

    Start-Process notepad.exe -ArgumentList "`"$scratch`"" | Out-Null
    $deadline = (Get-Date).AddSeconds(15)
    do { Start-Sleep -Milliseconds 250; $script:notepad = [Wnd]::Find('Notepad', 'likhi-type-test', $true) } while ($script:notepad -eq [IntPtr]::Zero -and (Get-Date) -lt $deadline)
    if ($script:notepad -eq [IntPtr]::Zero) { throw 'Notepad did not open' }
    Focus-Notepad
    Start-Sleep -Milliseconds 400
    [Ime]::ActivateLikhi()
    Start-Sleep -Milliseconds 400

    $names = if ($Case.Count) { $Case } else { @($cases.Keys) }
    foreach ($name in $names) {
        if (-not $cases.Contains($name)) { throw "no case named $name" }
        $script:current = $name
        try { & $cases[$name] }
        catch { [void]$script:results.Add([pscustomobject]@{ Case = $name; Check = 'ran'; Pass = $false; Expected = ''; Got = "$_" }) }
    }
}
finally {
    try { [Ime]::Activate($before) } catch { Write-Warning "could not restore the input method: $_" }
    if ($script:notepad -ne [IntPtr]::Zero -and [Keys]::Focus($script:notepad)) {
        try { Clear-Document; [Keys]::Press(0x53, $false, $true, 200); [Keys]::Press(0x57, $false, $true, 300) } catch { }
    }
    if ($savedConfig) { [IO.File]::WriteAllBytes($userConfig, $savedConfig) } else { Remove-Item $userConfig -ErrorAction SilentlyContinue }
    Remove-Item $marker -ErrorAction SilentlyContinue
    # Restarted so the restored settings -- usage reporting above all -- take effect, and stopped
    # first so the personal dictionary can be put back while nothing has it open.
    Stop-Engine
    if ($savedPersonal.Count) {
        foreach ($f in $personal) {
            if ($savedPersonal[$f]) { [IO.File]::WriteAllBytes($f, $savedPersonal[$f]) }
            elseif (Test-Path $f) { [IO.File]::Delete($f) }
        }
    }
    try { Start-Engine } catch { Write-Warning "the engine did not come back: $_" }
}
$script:results | Format-Table Case, Check, Pass, Expected, Got -AutoSize -Wrap
$failed = @($script:results | Where-Object { -not $_.Pass }).Count
"{0} checks, {1} failed" -f $script:results.Count, $failed
if ($failed) { exit 1 }
