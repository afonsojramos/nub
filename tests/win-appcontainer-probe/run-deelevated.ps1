# Runs the probe under a genuinely NON-ELEVATED token, so the "no elevation required" sub-claim
# (a standard user can create + AppContainer-grant a work dir under its OWN %TEMP%) is really
# tested. The GH Actions windows-latest job token is ELEVATED (IsElevated=True) with no fetchable
# UAC linked split-token, so we create a THROWAWAY standard user and relaunch via the Secondary
# Logon service (CreateProcessWithLogonW -- needs no SeImpersonate). Unlike the prior harness this
# needs NO C:\-owned root: the standard user works entirely under its own profile %TEMP%.
#
# Fallbacks (all logged): standard user -> LUA token -> direct (elevated, mechanism-only). The
# AppContainer mechanism axes hold regardless of parent elevation; only AXIS 2's unprivileged
# sub-claim needs the standard-user leg.
#
# Usage: run-deelevated.ps1 <path-to-probe.ps1>   (exit code mirrors the target probe)

param([Parameter(Mandatory=$true)][string]$Target)
$ErrorActionPreference='Stop'
try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}

$id=[System.Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin=(New-Object System.Security.Principal.WindowsPrincipal($id)).IsInRole([System.Security.Principal.WindowsBuiltinRole]::Administrator)
Write-Host "[run-deelevated] parent IsElevated=$isAdmin target=$Target"

if (-not $isAdmin) {
    Write-Host "[run-deelevated] already non-elevated; running target directly"
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Target
    exit $LASTEXITCODE
}

Add-Type -Language CSharp -TypeDefinition @"
using System; using System.Runtime.InteropServices; using System.ComponentModel;
public static class DeElev {
    [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)]
    static extern bool CreateProcessWithLogonW(string user, string domain, string password, uint logonFlags, string app, string cmd, uint flags, IntPtr env, string cwd, ref STARTUPINFO si, out PROCESS_INFORMATION pi);
    [DllImport("kernel32.dll", SetLastError=true)] static extern uint WaitForSingleObject(IntPtr h, uint ms);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool GetExitCodeProcess(IntPtr h, out uint c);
    [DllImport("kernel32.dll", SetLastError=true)] static extern bool CloseHandle(IntPtr h);
    const uint CREATE_UNICODE_ENVIRONMENT=0x400; const uint LOGON_WITH_PROFILE=0x1;
    [StructLayout(LayoutKind.Sequential)] struct STARTUPINFO { public int cb; public string r1,desk,title; public int dwX,dwY,dwXS,dwYS,dwXC,dwYC,dwFill,dwFlags; public short wShow,cbR2; public IntPtr r2,hIn,hOut,hErr; }
    [StructLayout(LayoutKind.Sequential)] struct PROCESS_INFORMATION { public IntPtr hProcess,hThread; public int pid,tid; }
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

function Invoke-AsStandardUser {
    $svc = Get-Service seclogon -ErrorAction SilentlyContinue
    if ($svc -and $svc.Status -ne 'Running') {
        try { Set-Service seclogon -StartupType Manual -ErrorAction Stop; Start-Service seclogon -ErrorAction Stop }
        catch { throw "seclogon not startable: $($_.Exception.Message)" }
    }
    $user = 'nubacp' + (Get-Random -Maximum 999999)
    $pwPlain = 'Nub-' + ([guid]::NewGuid().ToString('N')) + '-aA9!'
    $pwSecure = ConvertTo-SecureString $pwPlain -AsPlainText -Force
    New-LocalUser -Name $user -Password $pwSecure -AccountNeverExpires -PasswordNeverExpires -UserMayNotChangePassword -ErrorAction Stop | Out-Null
    Add-LocalGroupMember -Group 'Users' -Member $user -ErrorAction SilentlyContinue
    # Staging root the standard user can READ (scripts) and WRITE (log). NOT a confined work dir --
    # the probe's own work dirs live under the standard user's %TEMP%.
    $staging = Join-Path $env:SystemDrive ("nubac-staging-" + [guid]::NewGuid().ToString('N').Substring(0,10))
    New-Item -ItemType Directory -Path $staging -Force | Out-Null
    try {
        Copy-Item -Path (Join-Path $PSScriptRoot '*') -Destination $staging -Recurse -Force
        & icacls $staging /grant "${user}:(OI)(CI)(M)" /T /C | Out-Null
        $copiedTarget = Join-Path $staging (Split-Path $Target -Leaf)
        $log = Join-Path $staging ("stduser-" + [guid]::NewGuid().ToString('N') + ".log")
        $inner = "& '$copiedTarget' *> '$log'; exit `$LASTEXITCODE"
        $enc = [Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($inner))
        $cmd = "`"$psExe`" -NoProfile -NonInteractive -ExecutionPolicy Bypass -EncodedCommand $enc"
        Write-Host "[run-deelevated] relaunching as standard user '$user' via CreateProcessWithLogonW; log=$log"
        $code = [DeElev]::RunAsUser($user, $env:COMPUTERNAME, $pwPlain, $cmd, $staging)
        if (-not (Test-Path $log)) { throw "standard-user child produced no log (did not run)" }
        Write-Host "[run-deelevated] ----- begin target output (standard user, non-elevated) -----"
        Get-Content -Raw $log | Write-Host
        Write-Host "[run-deelevated] ----- end target output -----"
        Write-Host "[run-deelevated] target exit code (standard user): $code"
        return $code
    }
    finally {
        Remove-LocalUser -Name $user -ErrorAction SilentlyContinue
        Remove-Item -Recurse -Force $staging -ErrorAction SilentlyContinue
    }
}

try {
    $code = Invoke-AsStandardUser
    exit $code
} catch {
    Write-Host "[run-deelevated] standard-user launch FAILED: $($_.Exception.Message)"
    Write-Host "[run-deelevated] falling back to DIRECT (elevated) -- mechanism verdict still valid, unprivileged sub-claim not shown in this leg"
    & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File $Target
    exit $LASTEXITCODE
}
