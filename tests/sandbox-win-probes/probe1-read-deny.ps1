# Probe 1 — Unprivileged FS read-deny (THE KEY UNPROVEN CLAIM)
#
# From a NORMAL (non-elevated) token, create an AppContainer token, place a per-file
# Access-Denied ACE keyed to that AppContainer SID on a seeded secret, launch a child
# under it, and confirm the child CANNOT read the secret the PARENT CAN read — with NO
# elevation, no second account. No shipping tool does this on native Windows today.
#
# NEGATIVE CONTROLS (so a PASS cannot be vacuous):
#   NC-A: PARENT reads the secret (file exists + readable -> a child block is the ACE).
#   NC-B: the SAME AppContainer child reads an ALLOWED file in the same dir (child can
#         reach + read the FS -> the secret block is the deny-ACE, not blanket lockout).
#   NC-C: not elevated.
# PASS = NC-A ok, NC-B child read-allowed ok (exit 0), secret read BLOCKED (exit 5),
#        unelevated. Hardened per review: controlled dir chain + tiny native child + raw codes.

$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'
function Section($s){ Write-Host "`n=== $s ===" }
. "$PSScriptRoot\probe-common.ps1"

$id=[System.Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin=(New-Object System.Security.Principal.WindowsPrincipal($id)).IsInRole([System.Security.Principal.WindowsBuiltinRole]::Administrator)
Write-Host "Running as: $($id.Name)  IsElevated: $isAdmin"

$child = Build-ProbeChild
Write-Host "probe child: $child"

$work = New-ControlledDir 'probe1'
$secret  = Join-Path $work 'secret.env'
$allowed = Join-Path $work 'allowed.txt'
Set-Content -Path $secret -Value 'TOPSECRET_TOKEN=sk-do-not-leak-123' -NoNewline
Set-Content -Path $allowed -Value 'this-is-fine' -NoNewline
Write-Host "work dir: $work"

Section 'NC-A: parent reads secret'
$parentRead = Get-Content -Raw $secret
if ($parentRead -notlike '*TOPSECRET_TOKEN*') { throw "NC-A FAILED: parent could not read seeded secret" }
Write-Host "NC-A PASS: parent read secret OK"

Section 'Create AppContainer profile'
$acName = 'NubProbe1_' + ([guid]::NewGuid().ToString('N').Substring(0,12))
$acSidPtr = [IntPtr]::Zero
$hr = [AC]::CreateAppContainerProfile($acName,$acName,'nub probe1 read-deny',[IntPtr]::Zero,0,[ref]$acSidPtr)
if ($hr -ne 0) { throw "CreateAppContainerProfile failed hr=0x$("{0:X8}" -f $hr)" }
$acSidStr = [AC]::SidToString($acSidPtr)
Write-Host "AppContainer SID: $acSidStr"
$acAccount = New-Object System.Security.Principal.SecurityIdentifier($acSidStr)

$probe1 = 'INCONCLUSIVE'
try {
    Section 'Apply ACEs (RX work for AC SID; deny-read secret)'
    Grant-AcRx $work $acAccount
    $secAcl = Get-Acl $secret
    $secAcl.AddAccessRule((New-Object System.Security.AccessControl.FileSystemAccessRule($acAccount,'Read','None','None','Deny')))
    Set-Acl -Path $secret -AclObject $secAcl
    Write-Host "ACEs applied."

    Section 'Launch child: read ALLOWED file (NC-B, expect exit 0)'
    $codeAllowed = [AC]::Launch($acSidPtr, "`"$child`" read `"$allowed`"", $work)
    Write-Host "child(read allowed) raw exit: $codeAllowed"

    Section 'Launch child: read SECRET file (KEY, expect exit 5 = ACCESS_DENIED)'
    $codeSecret = [AC]::Launch($acSidPtr, "`"$child`" read `"$secret`"", $work)
    Write-Host "child(read secret) raw exit: $codeSecret"

    Section 'VERDICT'
    Write-Host "NC-B (allowed read) exit=$codeAllowed (expect 0); secret read exit=$codeSecret (expect 5)"
    if ($codeAllowed -ne 0) {
        Write-Host "INCONCLUSIVE: AppContainer child could not read the allowed file (exit=$codeAllowed) -> NC-B broken (traversal/launch, not a deny-ACE result)"
        $probe1='INCONCLUSIVE'
    } elseif ($codeSecret -eq 5) {
        if (-not $isAdmin) { Write-Host "PASS: unprivileged FS read-deny CONFIRMED (allowed readable, secret blocked, not elevated)"; $probe1='PASS' }
        else { Write-Host "read-deny held but process is ELEVATED -> cannot claim 'unprivileged'"; $probe1='INCONCLUSIVE(elevated)' }
    } elseif ($codeSecret -eq 0) {
        Write-Host "*** FAIL: SECRET LEAKED — read-deny DID NOT HOLD ***"; $probe1='FAIL'
    } else {
        Write-Host "INCONCLUSIVE: secret-read exit=$codeSecret (neither 0 nor 5)"; $probe1='INCONCLUSIVE'
    }
}
finally {
    [void][AC]::DeleteAppContainerProfile($acName)
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
Write-Host "PROBE1 RESULT: $probe1"
if ($probe1 -ne 'PASS') { exit 1 } else { exit 0 }
