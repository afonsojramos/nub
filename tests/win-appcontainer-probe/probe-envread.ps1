# Ascendant-env read probe: does an AppContainer (LowBox) child confined by nub's model
# reach a DE-ELEVATED (Medium-IL) PARENT's environment via OpenProcess(PROCESS_VM_READ)?
#
# The Codex comparison claimed nub may need a dedicated low-privilege account on Windows to
# close this; audit-windows found the LowBox already DENIES OpenProcess against same-user
# processes, but that ran with an ELEVATED parent. This confirms it with the parent
# de-elevated: run this whole script under run-deelevated.ps1 so THIS powershell (the parent
# the child targets) is a standard-user, Medium-IL process holding the secret.
#
# NEGATIVE CONTROL (so a "denied" can't be vacuous): an UNCONFINED child runs the SAME
# openparent against the SAME parent and MUST recover the secret -- proving the secret is in
# the parent's env and is readable absent the AppContainer confinement.

$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'
function Section($s){ Write-Host "`n========== $s ==========" }
. "$PSScriptRoot\probe-common.ps1"

$id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = (New-Object System.Security.Principal.WindowsPrincipal($id)).IsInRole([System.Security.Principal.WindowsBuiltinRole]::Administrator)
Write-Host "PARENT: $($id.Name)  IsElevated: $isAdmin  pid: $PID"

# The secret lives ONLY in the parent's (this process's) environment.
$secret = 'sk-ascendant-DO-NOT-LEAK-' + [guid]::NewGuid().ToString('N')
$env:NUB_ASCENDANT_SECRET = $secret
$parentPid = $PID

$AAP = 'S-1-15-2-1'
$stage = Join-Path $env:TEMP ("nubenv-" + [guid]::NewGuid().ToString('N').Substring(0,12))
New-Item -ItemType Directory -Path $stage -Force | Out-Null
$bin = Join-Path $stage 'bin'
$child = Build-ProbeChild $bin
& icacls $stage /grant "*${AAP}:(OI)(CI)(RX)" | Out-Null   # exe hosting: AC child can load its image
$diag = Join-Path $stage 'diag'; New-Item -ItemType Directory -Path $diag -Force | Out-Null
Write-Host "probe child: $child"

Section 'Create per-run AppContainer profile'
$acName = 'NubEnvRead_' + ([guid]::NewGuid().ToString('N').Substring(0,12))
$acSidPtr = [IntPtr]::Zero
$hr = [AC]::CreateAppContainerProfile($acName,$acName,'nub ascendant-env-read probe',[IntPtr]::Zero,0,[ref]$acSidPtr)
if ($hr -ne 0) { throw "CreateAppContainerProfile failed hr=0x$("{0:X8}" -f $hr)" }
$acSidStr = [AC]::SidToString($acSidPtr)
& icacls $diag /grant "*${acSidStr}:(OI)(CI)(M)" | Out-Null   # AC child can write its token dump
Write-Host "AppContainer SID: $acSidStr"

$inAC = $false
try {
    Section 'Confirm the AC child is genuinely a LowBox AppContainer'
    $dump = Join-Path $diag 'token.txt'
    [void][AC]::Launch($acSidPtr, "`"$child`" whoami `"$dump`"", $bin)
    if (Test-Path $dump) {
        $t = Get-Content -Raw $dump
        if ($t -match 'TokenIsAppContainer=1') { $inAC = $true }
        if ($t -match 'IntegrityLevelSid=(\S+)') { Write-Host "AC child IL: $($Matches[1])" }
    }
    Write-Host "AC child in AppContainer? $inAC"

    Section 'AC child: OpenProcess(PROCESS_VM_READ) the de-elevated parent + read its env'
    $acCode = [AC]::Launch($acSidPtr, "`"$child`" openparent $parentPid `"$secret`"", $bin)
    Write-Host "AC child openparent exit: $acCode  (5=OpenProcess DENIED=confined ; 0=SECRET LEAK ; 6=handle granted, read failed)"

    Section 'NEGATIVE CONTROL: UNCONFINED child does the SAME openparent (must leak)'
    $ncProc = Start-Process -FilePath $child -ArgumentList 'openparent',$parentPid,$secret -Wait -PassThru -NoNewWindow
    $ncCode = $ncProc.ExitCode
    Write-Host "unconfined openparent exit: $ncCode  (expect 0 = secret recovered = test is LIVE)"

    Section 'VERDICT'
    if (-not $inAC) {
        $verdict = 'INCONCLUSIVE'; $detail = "AC child was not a genuine AppContainer (TokenIsAppContainer!=1)"
    } elseif ($ncCode -ne 0) {
        $verdict = 'INCONCLUSIVE'; $detail = "negative control did NOT recover the secret (exit=$ncCode) -> the read path/PEB walk is broken, the AC result is meaningless"
    } elseif ($acCode -eq 5) {
        $verdict = 'CLOSED'; $detail = "AC child OpenProcess DENIED (exit 5) while the unconfined control leaked (exit 0) => LowBox blocks ascendant-env read of a DE-ELEVATED parent => dedicated low-priv account UNNECESSARY for this axis"
    } elseif ($acCode -eq 0) {
        $verdict = 'LEAK'; $detail = "AC child READ the parent's secret (exit 0) => ascendant-env read is OPEN => dedicated account NEEDED"
    } else {
        $verdict = 'PARTIAL/CONCERN'; $detail = "AC child OpenProcess was NOT denied (exit=$acCode, handle granted) => the OpenProcess boundary did not block it => needs deeper analysis / likely account NEEDED"
    }
    Write-Host "ENVREAD VERDICT: ${verdict}: $detail  [ac=$acCode nc=$ncCode inAC=$inAC parentElevated=$isAdmin]"
}
finally {
    [void][AC]::DeleteAppContainerProfile($acName)
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}

if ($verdict -eq 'CLOSED') { exit 0 } else { exit 1 }
