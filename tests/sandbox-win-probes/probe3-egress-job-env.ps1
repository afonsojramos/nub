# Probe 3 — AppContainer coarse egress + Job containment + env-scrub (all unprivileged)
#
# 3A EGRESS: an AppContainer WITHOUT the internetClient capability (S-1-15-3-1) cannot
#    make an outbound TCP connect (WSAEACCES); WITH it, it can.
#    NC: the WITH-internetClient leg connects (proves target reachable + block is the
#        missing capability). Block is asserted specifically as WSAEACCES (child exit 5),
#        NOT a generic timeout/failure (the native child inspects SocketErrorCode).
# 3B JOB: a Job with KILL_ON_JOB_CLOSE reaps the descendant tree on handle close.
#    NC: grandchild alive BEFORE close; gone AFTER.
# 3C ENV scrub: a child spawned with a cleared env block does NOT see a seeded *_TOKEN.
#    NC: a child inheriting the parent env DOES see it.
#    (Scoped honestly: this proves parent-side env withholding at the spawn boundary —
#     NOT an AppContainer property. A determined child could read env via other means.)
# All unprivileged. PASS = every sub-probe main+NC holds AND not elevated.

$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'
function Section($s){ Write-Host "`n=== $s ===" }
. "$PSScriptRoot\probe-common.ps1"

$id=[System.Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin=(New-Object System.Security.Principal.WindowsPrincipal($id)).IsInRole([System.Security.Principal.WindowsBuiltinRole]::Administrator)
Write-Host "Running as: $($id.Name)  IsElevated: $isAdmin"

$child = Build-ProbeChild
Write-Host "probe child: $child"
$psExe = (Get-Command powershell.exe).Source
$results = @{}

# ---- 3A: AppContainer coarse egress -------------------------------------------------
Section '3A: AppContainer coarse egress'
$acName='NubProbe3_'+([guid]::NewGuid().ToString('N').Substring(0,12))
$sidPtr=[IntPtr]::Zero
$hr=[AC]::CreateAppContainerProfile($acName,$acName,'nub probe3 egress',[IntPtr]::Zero,0,[ref]$sidPtr)
if($hr -ne 0){ throw "CreateAppContainerProfile hr=0x$("{0:X8}" -f $hr)" }
$sidStr=[AC]::SidToString($sidPtr); Write-Host "AppContainer SID: $sidStr"
$acAcct=New-Object System.Security.Principal.SecurityIdentifier($sidStr)
$work3 = New-ControlledDir 'probe3'
Grant-AcRx $work3 $acAcct
try {
    $cmd = "`"$child`" connect 1.1.1.1 443"
    Write-Host "--- 3A.NC: WITH internetClient (expect exit 0 CONNECT OK) ---"
    $codeWith=[AC]::LaunchWithCaps($sidPtr, @('S-1-15-3-1'), $cmd, $work3)
    Write-Host "with-internet raw exit: $codeWith"
    Write-Host "--- 3A.main: WITHOUT internetClient (expect exit 5 = WSAEACCES block) ---"
    $codeWithout=[AC]::LaunchWithCaps($sidPtr, @(), $cmd, $work3)
    Write-Host "no-internet raw exit: $codeWithout"
    if($codeWith -ne 0){ Write-Host "3A INCONCLUSIVE: NC failed — even WITH internetClient connect did not succeed (exit=$codeWith); runner network unreachable?"; $results['3A']='INCONCLUSIVE' }
    elseif($codeWithout -eq 5){ Write-Host "3A PASS: egress BLOCKED (WSAEACCES) w/o capability, allowed with it"; $results['3A']='PASS' }
    elseif($codeWithout -eq 0){ Write-Host "3A FAIL: egress NOT blocked without capability (LEAK)"; $results['3A']='FAIL' }
    elseif($codeWithout -eq 6){ Write-Host "3A INCONCLUSIVE: without-internet TIMED OUT (exit 6) rather than WSAEACCES — block manifests as silent drop, not access-denied"; $results['3A']='INCONCLUSIVE' }
    else { Write-Host "3A INCONCLUSIVE: without-internet exit=$codeWithout (expected 5)"; $results['3A']='INCONCLUSIVE' }
}
finally { [void][AC]::DeleteAppContainerProfile($acName) }

# ---- 3B: Job object containment -----------------------------------------------------
Add-Type -Language CSharp -TypeDefinition @"
using System; using System.Runtime.InteropServices; using System.ComponentModel;
public static class JobC {
    [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr CreateJobObject(IntPtr a, string n);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool SetInformationJobObject(IntPtr j, int infoClass, IntPtr info, uint len);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool AssignProcessToJobObject(IntPtr j, IntPtr p);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll", SetLastError=true)] public static extern bool GetExitCodeProcess(IntPtr h, out uint c);
    const int JobObjectExtendedLimitInformation=9;
    const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE=0x2000;
    [StructLayout(LayoutKind.Sequential)] struct JOBOBJECT_BASIC_LIMIT_INFORMATION { public long PerProcessUserTimeLimit, PerJobUserTimeLimit; public uint LimitFlags; public IntPtr MinimumWorkingSetSize, MaximumWorkingSetSize; public uint ActiveProcessLimit; public IntPtr Affinity; public uint PriorityClass, SchedulingClass; }
    [StructLayout(LayoutKind.Sequential)] struct IO_COUNTERS { public ulong r,w,o,rb,wb,ob; }
    [StructLayout(LayoutKind.Sequential)] struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION { public JOBOBJECT_BASIC_LIMIT_INFORMATION Basic; public IO_COUNTERS Io; public IntPtr ProcMemLimit, JobMemLimit, PeakProc, PeakJob; }
    public static IntPtr CreateKillOnCloseJob(){
        IntPtr j=CreateJobObject(IntPtr.Zero,null);
        if(j==IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(),"CreateJobObject");
        var info=new JOBOBJECT_EXTENDED_LIMIT_INFORMATION();
        info.Basic.LimitFlags=JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        IntPtr p=Marshal.AllocHGlobal(Marshal.SizeOf(typeof(JOBOBJECT_EXTENDED_LIMIT_INFORMATION)));
        Marshal.StructureToPtr(info,p,false);
        if(!SetInformationJobObject(j,JobObjectExtendedLimitInformation,p,(uint)Marshal.SizeOf(typeof(JOBOBJECT_EXTENDED_LIMIT_INFORMATION)))) throw new Win32Exception(Marshal.GetLastWin32Error(),"SetInfo");
        Marshal.FreeHGlobal(p); return j;
    }
    public static void Assign(IntPtr j, IntPtr p){ if(!AssignProcessToJobObject(j,p)) throw new Win32Exception(Marshal.GetLastWin32Error(),"Assign"); }
    public static void Close(IntPtr h){ CloseHandle(h); }
    public static bool IsAlive(int pid){
        IntPtr h=OpenProcess(0x1000,false,pid); // PROCESS_QUERY_LIMITED_INFORMATION
        if(h==IntPtr.Zero) return false;
        uint code; GetExitCodeProcess(h,out code); CloseHandle(h);
        return code==259; // STILL_ACTIVE (grandchild sleeps, exits 0 normally, so no ambiguity)
    }
}
"@
Section '3B: Job object containment'
try {
    $job=[JobC]::CreateKillOnCloseJob()
    $marker = Join-Path $work3 ('gc-'+[guid]::NewGuid().ToString('N')+'.pid')
    $childPs = @"
`$gc = Start-Process -FilePath '$psExe' -ArgumentList '-NoProfile','-Command','Start-Sleep -Seconds 120' -PassThru
Set-Content -Path '$marker' -Value `$gc.Id -NoNewline
Start-Sleep -Seconds 120
"@
    $b64j=[Convert]::ToBase64String([System.Text.Encoding]::Unicode.GetBytes($childPs))
    $proc = Start-Process -FilePath $psExe -ArgumentList '-NoProfile','-NonInteractive','-EncodedCommand',$b64j -PassThru
    [JobC]::Assign($job, $proc.Handle)
    $tries=0; while(-not (Test-Path $marker) -and $tries -lt 75){ Start-Sleep -Milliseconds 200; $tries++ }
    if(-not (Test-Path $marker)){ Write-Host "3B INCONCLUSIVE: grandchild never registered"; $results['3B']='INCONCLUSIVE' }
    else {
        $gcPid=[int](Get-Content -Raw $marker)
        Start-Sleep -Milliseconds 500
        $aliveBefore = [JobC]::IsAlive($gcPid)
        Write-Host "grandchild pid=$gcPid aliveBefore=$aliveBefore (NC: expect True)"
        [JobC]::Close($job)
        Start-Sleep -Seconds 2
        $aliveAfter = [JobC]::IsAlive($gcPid)
        Write-Host "grandchild aliveAfter=$aliveAfter (expect False); child exited=$($proc.HasExited)"
        if($aliveBefore -and (-not $aliveAfter)){ Write-Host "3B PASS: job reaped descendant tree on close"; $results['3B']='PASS' }
        elseif(-not $aliveBefore){ Write-Host "3B INCONCLUSIVE: grandchild not alive before close (NC failed)"; $results['3B']='INCONCLUSIVE' }
        else { Write-Host "3B FAIL: grandchild survived job close"; $results['3B']='FAIL' }
    }
    if(-not $proc.HasExited){ $proc.Kill() }
}
catch { Write-Host "3B ERROR: $($_.Exception.Message)"; $results['3B']='INCONCLUSIVE' }

# ---- 3C: env scrub at spawn ---------------------------------------------------------
Section '3C: env-scrub at spawn'
try {
    $env:NUB_PROBE_SECRET_TOKEN='sk-leak-me-3c'
    # NC: child inheriting parent env SEES the token (native child getenv -> exit 0 present)
    $ncProc = Start-Process -FilePath $child -ArgumentList 'getenv','NUB_PROBE_SECRET_TOKEN' -NoNewWindow -PassThru -Wait
    $ncCode = $ncProc.ExitCode
    Write-Host "NC (inherit env) getenv exit: $ncCode (expect 0 = present)"
    # main: scrubbed env via ProcessStartInfo (no token)
    $psi = New-Object System.Diagnostics.ProcessStartInfo
    $psi.FileName=$child
    $psi.UseShellExecute=$false
    [void]$psi.ArgumentList.Add('getenv'); [void]$psi.ArgumentList.Add('NUB_PROBE_SECRET_TOKEN')
    $psi.EnvironmentVariables.Clear()
    $psi.EnvironmentVariables['SystemRoot']=$env:SystemRoot
    $psi.EnvironmentVariables['windir']=$env:windir
    $psi.EnvironmentVariables['Path']=$env:Path
    $psi.EnvironmentVariables['TEMP']=$env:TEMP
    $psi.EnvironmentVariables['TMP']=$env:TEMP
    $p=[System.Diagnostics.Process]::Start($psi); $p.WaitForExit()
    $scrubCode=$p.ExitCode
    Write-Host "scrubbed child getenv exit: $scrubCode (expect 4 = absent)"
    if(($ncCode -eq 0) -and ($scrubCode -eq 4)){ Write-Host "3C PASS: token hidden from scrubbed child, visible to inherit-child (NC)"; $results['3C']='PASS' }
    elseif($ncCode -ne 0){ Write-Host "3C INCONCLUSIVE: NC failed — token not seen even by inherit-child (exit=$ncCode)"; $results['3C']='INCONCLUSIVE' }
    else { Write-Host "3C FAIL: scrubbed child still saw token (exit=$scrubCode)"; $results['3C']='FAIL' }
}
catch { Write-Host "3C ERROR: $($_.Exception.Message)"; $results['3C']='INCONCLUSIVE' }
finally { Remove-Item Env:\NUB_PROBE_SECRET_TOKEN -ErrorAction SilentlyContinue; Remove-Item -Recurse -Force $work3 -ErrorAction SilentlyContinue }

Section 'PROBE3 SUMMARY'
$results.GetEnumerator() | Sort-Object Name | ForEach-Object { Write-Host ("  {0}: {1}" -f $_.Name,$_.Value) }
Write-Host "IsElevated: $isAdmin (expect False)"
$allPass = ($results['3A'] -eq 'PASS') -and ($results['3B'] -eq 'PASS') -and ($results['3C'] -eq 'PASS') -and (-not $isAdmin)
if($allPass){ Write-Host "PROBE3 RESULT: PASS"; exit 0 } else { Write-Host "PROBE3 RESULT: NOT-ALL-PASS (see per-subprobe)"; exit 1 }
