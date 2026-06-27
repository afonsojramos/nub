# Runs a target probe script under a genuinely NON-ELEVATED token, so the probes'
# "no elevation required" claim is really tested even on a runner whose default job token is
# elevated (GitHub Actions windows-latest runs jobs with a FULL admin token, IsElevated=True,
# and the runneradmin account has NO fetchable UAC linked split-token).
#
# De-elevation strategy (first that works wins; all logged; never blocks the mechanism verdict):
#   1. STANDARD USER (preferred) -- create a throwaway local user NOT in Administrators and
#      relaunch the probe as that user via CreateProcessWithLogonW (Secondary Logon service;
#      needs no SeImpersonate). This yields a real unprivileged token (IsElevated=False).
#   2. LUA TOKEN -- CreateRestrictedToken(LUA_TOKEN) + CreateProcessWithTokenW. Needs
#      SeImpersonate; observed to fail on this runner (kept as a secondary fallback only).
#   3. DIRECT (elevated) -- if neither de-elevation works, run directly so a MECHANISM verdict
#      is STILL produced (the AppContainer/lowbox child is restricted regardless of the parent's
#      elevation). The probe reports unprivileged=False and the harness stays red on the
#      unprivileged sub-claim, which is the honest signal.
#
# The relaunched child gets a fresh console; the inner probe redirects its whole output to a
# log we print here, and its real exit code is preserved.
#
# Usage: run-deelevated.ps1 <path-to-probe.ps1>   (exit code mirrors the target probe)

param([Parameter(Mandatory=$true)][string]$Target)
$ErrorActionPreference='Stop'
try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}

$id=[System.Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin=(New-Object System.Security.Principal.WindowsPrincipal($id)).IsInRole([System.Security.Principal.WindowsBuiltinRole]::Administrator)
Write-Host "[run-deelevated] parent IsElevated=$isAdmin target=$Target"

. "$PSScriptRoot\probe-common.ps1"
Ensure-ProbeRoot
Write-Host "[run-deelevated] prepared C:\probework (AC groups RX + user Modify)"

if (-not $isAdmin) {
    Write-Host "[run-deelevated] already non-elevated; running target directly"
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Target
    exit $LASTEXITCODE
}

Add-Type -Language CSharp -TypeDefinition @"
using System; using System.Runtime.InteropServices; using System.ComponentModel; using System.Text;
public static class DeElev {
    [DllImport("kernel32.dll", SetLastError=true)] static extern IntPtr GetCurrentProcess();
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr h, uint access, out IntPtr tok);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool GetTokenInformation(IntPtr tok, int cls, IntPtr buf, int len, out int ret);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool CreateRestrictedToken(IntPtr ExistingToken, uint Flags, uint DisableSidCount, IntPtr SidsToDisable, uint DeletePrivCount, IntPtr PrivsToDelete, uint RestrictedSidCount, IntPtr SidsToRestrict, out IntPtr NewToken);
    [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    static extern bool CreateProcessWithTokenW(IntPtr hToken, uint dwLogonFlags, string app, string cmd, uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    static extern bool CreateProcessWithLogonW(string user, string domain, string password, uint logonFlags, string app, string cmd, uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr h, out uint c);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr h);

    const uint TOKEN_QUERY=0x0008, TOKEN_DUPLICATE=0x0002, TOKEN_ASSIGN_PRIMARY=0x0001, TOKEN_ADJUST_DEFAULT=0x0080;
    const int TokenLinkedToken=19;
    const uint LUA_TOKEN=0x4;
    const uint CREATE_UNICODE_ENVIRONMENT=0x400;
    const uint LOGON_WITH_PROFILE=0x1;
    [StructLayout(LayoutKind.Sequential)] struct STARTUPINFO { public int cb; public string r1,desk,title; public int dwX,dwY,dwXS,dwYS,dwXC,dwYC,dwFill,dwFlags; public short wShow,cbR2; public IntPtr r2,hIn,hOut,hErr; }
    [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr hProcess,hThread; public int pid,tid; }

    static IntPtr OpenSelf(){
        IntPtr cur;
        if(!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY|TOKEN_DUPLICATE|TOKEN_ASSIGN_PRIMARY|TOKEN_ADJUST_DEFAULT, out cur))
            throw new Win32Exception(Marshal.GetLastWin32Error(),"OpenProcessToken");
        return cur;
    }
    public static IntPtr TryLinkedToken(){
        try {
            IntPtr cur=OpenSelf();
            int len; GetTokenInformation(cur, TokenLinkedToken, IntPtr.Zero, 0, out len);
            if(len<=0) return IntPtr.Zero;
            IntPtr buf=Marshal.AllocHGlobal(len);
            try {
                if(!GetTokenInformation(cur, TokenLinkedToken, buf, len, out len)) return IntPtr.Zero;
                return Marshal.ReadIntPtr(buf);
            } finally { Marshal.FreeHGlobal(buf); CloseHandle(cur); }
        } catch { return IntPtr.Zero; }
    }
    public static IntPtr CreateLuaToken(){
        IntPtr cur=OpenSelf(); IntPtr lua;
        bool ok=CreateRestrictedToken(cur, LUA_TOKEN, 0, IntPtr.Zero, 0, IntPtr.Zero, 0, IntPtr.Zero, out lua);
        CloseHandle(cur);
        if(!ok) throw new Win32Exception(Marshal.GetLastWin32Error(),"CreateRestrictedToken(LUA_TOKEN)");
        return lua;
    }
    public static uint RunUnderToken(IntPtr token, string cmd, string cwd){
        var si=new STARTUPINFO(); si.cb=Marshal.SizeOf(typeof(STARTUPINFO));
        PROCESS_INFORMATION pi;
        if(!CreateProcessWithTokenW(token, 0, null, cmd, CREATE_UNICODE_ENVIRONMENT, IntPtr.Zero, cwd, ref si, out pi))
            throw new Win32Exception(Marshal.GetLastWin32Error(),"CreateProcessWithTokenW");
        WaitForSingleObject(pi.hProcess, 300000);
        uint code; GetExitCodeProcess(pi.hProcess, out code);
        CloseHandle(pi.hProcess); CloseHandle(pi.hThread);
        return code;
    }
    // Launch cmd as a (standard) user via the Secondary Logon service. Needs no SeImpersonate.
    public static uint RunAsUser(string user, string domain, string password, string cmd, string cwd){
        var si=new STARTUPINFO(); si.cb=Marshal.SizeOf(typeof(STARTUPINFO));
        PROCESS_INFORMATION pi;
        if(!CreateProcessWithLogonW(user, domain, password, LOGON_WITH_PROFILE, null, cmd, CREATE_UNICODE_ENVIRONMENT, IntPtr.Zero, cwd, ref si, out pi))
            throw new Win32Exception(Marshal.GetLastWin32Error(),"CreateProcessWithLogonW");
        WaitForSingleObject(pi.hProcess, 300000);
        uint code; GetExitCodeProcess(pi.hProcess, out code);
        CloseHandle(pi.hProcess); CloseHandle(pi.hThread);
        return code;
    }
}
"@

$psExe=(Get-Command powershell.exe).Source

function Invoke-DirectElevated {
    Write-Host "[run-deelevated] running target DIRECTLY (elevated) -- probe reports unprivileged=False"
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Target
    return $LASTEXITCODE
}

# --- Strategy 1: real standard user via Secondary Logon (CreateProcessWithLogonW) -----------
function Invoke-AsStandardUser {
    $svc = Get-Service seclogon -ErrorAction SilentlyContinue
    if ($svc -and $svc.Status -ne 'Running') {
        try { Set-Service seclogon -StartupType Manual -ErrorAction Stop; Start-Service seclogon -ErrorAction Stop }
        catch { throw "seclogon not startable: $($_.Exception.Message)" }
    }
    $user = 'nubprobe' + (Get-Random -Maximum 999999)
    $pwPlain = 'Nub-' + ([guid]::NewGuid().ToString('N')) + '-aA9!'
    $pwSecure = ConvertTo-SecureString $pwPlain -AsPlainText -Force
    New-LocalUser -Name $user -Password $pwSecure -AccountNeverExpires -PasswordNeverExpires -UserMayNotChangePassword -ErrorAction Stop | Out-Null
    Add-LocalGroupMember -Group 'Users' -Member $user -ErrorAction SilentlyContinue
    try {
        # Grant the standard user Modify on the controlled root (propagate to existing children),
        # so it can read the copied scripts, compile the child, seed its work dirs, and write logs.
        & icacls $script:ProbeRoot /grant "${user}:(OI)(CI)(M)" /T /C | Out-Null
        # Copy the probe scripts into the controlled root so the standard user can read them
        # regardless of the repo-checkout ACLs. $PSScriptRoot resolves to this copy for the child.
        $copyDir = Join-Path $script:ProbeRoot 'scripts'
        if (-not (Test-Path $copyDir)) { New-Item -ItemType Directory -Path $copyDir -Force | Out-Null }
        Copy-Item -Path (Join-Path $PSScriptRoot '*') -Destination $copyDir -Force
        & icacls $copyDir /grant "${user}:(OI)(CI)(RX)" /T /C | Out-Null
        $copiedTarget = Join-Path $copyDir (Split-Path $Target -Leaf)

        $log = Join-Path $script:ProbeRoot ("stduser-" + [guid]::NewGuid().ToString('N') + ".log")
        $inner = "& '$copiedTarget' *> '$log'; exit `$LASTEXITCODE"
        $enc = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($inner))
        $cmd = "`"$psExe`" -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $enc"
        Write-Host "[run-deelevated] relaunching as standard user '$user' via CreateProcessWithLogonW; log=$log"
        $code = [DeElev]::RunAsUser($user, $env:COMPUTERNAME, $pwPlain, $cmd, $copyDir)
        if (-not (Test-Path $log)) {
            # The child never produced output -> it didn't actually run. Treat as a launch
            # failure so we fall back (LUA/direct) and still get the mechanism verdict (B).
            throw "standard-user child produced no log (did not run)"
        }
        Write-Host "[run-deelevated] ----- begin target output (standard user, non-elevated) -----"
        Get-Content -Raw $log | Write-Host; Remove-Item -Force $log -ErrorAction SilentlyContinue
        Write-Host "[run-deelevated] ----- end target output -----"
        Write-Host "[run-deelevated] target exit code (standard user): $code"
        return $code
    }
    finally {
        Remove-LocalUser -Name $user -ErrorAction SilentlyContinue
    }
}

# --- Strategy 2: LUA token via CreateProcessWithTokenW --------------------------------------
function Invoke-WithLuaToken {
    $log = Join-Path $env:TEMP ("deelev-" + [guid]::NewGuid().ToString('N') + ".log")
    $inner = "& '$Target' *> '$log'; exit `$LASTEXITCODE"
    $enc = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($inner))
    $cmd = "`"$psExe`" -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $enc"
    $cwd = (Split-Path $Target -Parent)
    $token=[IntPtr]::Zero; $kind='lua'
    $linked=[DeElev]::TryLinkedToken()
    if ($linked -ne [IntPtr]::Zero) { $token=$linked; $kind='linked' }
    else { $token=[DeElev]::CreateLuaToken() }
    Write-Host "[run-deelevated] relaunching under $kind token via CreateProcessWithTokenW; log=$log"
    $code = [DeElev]::RunUnderToken($token, $cmd, $cwd)
    Write-Host "[run-deelevated] ----- begin target output ($kind, non-elevated) -----"
    if (Test-Path $log) { Get-Content -Raw $log | Write-Host; Remove-Item -Force $log -ErrorAction SilentlyContinue }
    else { Write-Host "[run-deelevated] WARNING: no log produced" }
    Write-Host "[run-deelevated] ----- end target output -----"
    Write-Host "[run-deelevated] target exit code ($kind): $code"
    return $code
}

try {
    $code = Invoke-AsStandardUser
    exit $code
} catch {
    Write-Host "[run-deelevated] standard-user launch FAILED: $($_.Exception.Message)"
}
try {
    $code = Invoke-WithLuaToken
    exit $code
} catch {
    Write-Host "[run-deelevated] LUA-token launch FAILED: $($_.Exception.Message)"
}
exit (Invoke-DirectElevated)
