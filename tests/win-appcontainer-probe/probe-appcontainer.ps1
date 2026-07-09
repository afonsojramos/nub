# nub Windows AppContainer file-access model -- definitive ground-truth probe.
#
# Verifies (or refutes) the two "catastrophic" claims baked into crates/nub-sandbox on the
# sandbox-primitives branch:
#   CLAIM 1 (windows.rs ancestor_traverse_dirs): "A LowBox token does NOT bypass traverse
#           checking ... every ancestor up to the drive root needs a traverse grant" -> hence
#           confined work dirs "must live under a nub-owned store" at C:\ (LIMITATIONS.md).
#   CLAIM 2 (windows.rs derive_grants): a generous-read-minus-secrets ("deny-inside-allow")
#           policy is inexpressible under the AppContainer allowlist model.
#
# The prior sandbox-win-probes harness never tested these: it hardcoded C:\probework + granted
# ALL APPLICATION PACKAGES (AAP) on the root, ASSUMING %TEMP% ancestors don't grant traverse.
# This probe isolates the variables with airtight ACL evidence and negative controls.
#
# Every AC read/write returns: 0 = allowed, 5 = ACCESS_DENIED, 9 = other error (e.g. not-found).
# All secret/sibling files are SEEDED by the parent so a block is a genuine deny (5), never a 9.

$ErrorActionPreference = 'Stop'; $ProgressPreference = 'SilentlyContinue'
function Section($s){ Write-Host "`n========== $s ==========" }
function Note($s){ Write-Host "  $s" }
. "$PSScriptRoot\probe-common.ps1"

$fail = New-Object System.Collections.Generic.List[string]
$summary = New-Object System.Collections.Generic.List[string]
function Record($axis, $ok, $detail){
    $tag = if($ok){'PASS'}else{'FAIL'}
    $summary.Add("$axis : ${tag}: $detail")
    Write-Host "RESULT $axis : ${tag}: $detail"
    if(-not $ok){ $fail.Add("$axis : $detail") }
}
# Informational: records an observed behavior that does NOT gate the verdict (e.g. confirming a
# known mechanism limitation). $asExpected just annotates whether the observation matched the model.
function Info($axis, $asExpected, $detail){
    $tag = if($asExpected){'CONFIRMED'}else{'UNEXPECTED'}
    $summary.Add("$axis : [info ${tag}]: $detail")
    Write-Host "INFO $axis : ${tag}: $detail"
}

$id = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$isAdmin = (New-Object System.Security.Principal.WindowsPrincipal($id)).IsInRole([System.Security.Principal.WindowsBuiltinRole]::Administrator)
$me = "$($env:USERDOMAIN)\$($env:USERNAME)"
Write-Host "Running as: $($id.Name)  IsElevated: $isAdmin  (mechanism axes are child-token properties, elevation-independent)"

# AAP + the running user, used throughout.
$AAP = 'S-1-15-2-1'   # ALL APPLICATION PACKAGES

# ---- Stage under the user's OWN %TEMP% (an ORDINARY user-writable profile location; NOT C:\) ----
$stage = Join-Path $env:TEMP ("nubac-" + [guid]::NewGuid().ToString('N').Substring(0,12))
New-Item -ItemType Directory -Path $stage -Force | Out-Null
Write-Host "stage (under %TEMP%, NOT a C:\-owned store): $stage"
Write-Host "stage is under the user profile: $([bool]($stage -like "$env:USERPROFILE*"))"

# Host the child exe in <stage>\bin, granted AAP RX so launch is bulletproof and never
# masquerades as a traverse failure. Axis chains break inheritance to get controlled ACLs.
$bin = Join-Path $stage 'bin'
$child = Build-ProbeChild $bin
& icacls $stage /grant "*${AAP}:(OI)(CI)(RX)" | Out-Null   # stage tree AC-readable (exe hosting only)
Write-Host "probe child: $child"

# AXIS 2 first-half evidence: can THIS token icacls-grant an AppContainer SID on a %TEMP% dir?
Section 'Create per-run AppContainer profile (unique AC SID; never AAP-only for confinement)'
$acName = 'NubAcProbe_' + ([guid]::NewGuid().ToString('N').Substring(0,12))
$acSidPtr = [IntPtr]::Zero
$hr = [AC]::CreateAppContainerProfile($acName,$acName,'nub appcontainer file-access probe',[IntPtr]::Zero,0,[ref]$acSidPtr)
if ($hr -ne 0) { throw "CreateAppContainerProfile failed hr=0x$("{0:X8}" -f $hr)" }
$acSidStr = [AC]::SidToString($acSidPtr)
Write-Host "per-run AppContainer SID: $acSidStr"

try {
    # A diag dir the AC SID can WRITE (for the child token/priv dump). Inherits AAP RX from stage;
    # we additionally grant the AC SID Modify.
    $diag = Join-Path $stage 'diag'
    New-Item -ItemType Directory -Path $diag -Force | Out-Null
    & icacls $diag /grant "*${acSidStr}:(OI)(CI)(M)" | Out-Null
    $dump = Join-Path $diag 'token.txt'

    # =====================================================================================
    Section 'AXIS 1 -- traverse bypass: leaf-only AC grant, DEEP chain under %TEMP%, ancestors grant NOTHING'
    # trav\a\b\c\work : inheritance broken at trav (user-only), so a,b,c,work carry NO AAP and NO AC SID.
    $trav = Join-Path $stage 'trav'
    New-Item -ItemType Directory -Path $trav -Force | Out-Null
    & icacls $trav /inheritance:r /grant:r "${me}:(OI)(CI)(F)" | Out-Null   # strip inherited AAP; user-only
    $chain = $trav
    foreach($seg in 'a','b','c','work'){ $chain = Join-Path $chain $seg; New-Item -ItemType Directory -Path $chain -Force | Out-Null }
    $work = $chain
    $allowed = Join-Path $work 'allowed.txt'; Set-Content -Path $allowed -Value 'leaf-allowed' -NoNewline
    $sibling = Join-Path $trav 'a\notgranted-secret.txt'; Set-Content -Path $sibling -Value 'SIB_SECRET' -NoNewline

    # Grant the AC SID RX on ONLY the leaf 'work'. NO ancestor grant. This is what CLAIM 1 says
    # is insufficient. icacls SUCCESS here also proves AXIS 2 (a normal token can AC-grant a %TEMP% dir).
    $ic = (& icacls $work /grant "*${acSidStr}:(OI)(CI)(RX)" 2>&1); $icRc = $LASTEXITCODE
    Note "icacls leaf-grant rc=$icRc : $ic"
    $axis2ok = ($icRc -eq 0)
    Record 'AXIS2 write-DAC-on-%TEMP% unelevated' $axis2ok "icacls /grant AppContainer SID on a %TEMP% dir rc=$icRc (elevated=$isAdmin; de-elevated leg proves the unprivileged sub-claim)"

    Note "--- ACL EVIDENCE: ancestors of the leaf grant NEITHER AAP NOR the AC SID ---"
    foreach($p in @($trav, (Join-Path $trav 'a'), (Join-Path $trav 'a\b'), (Join-Path $trav 'a\b\c'))){
        $acl = (& icacls $p 2>&1 | Out-String)
        $hasAAP = $acl -match [regex]::Escape($AAP) -or $acl -match 'APPLICATION PACKAGES'
        $hasAC  = $acl -match [regex]::Escape($acSidStr)
        Note "$p  AAP=$hasAAP AC_SID=$hasAC"
        if($hasAAP -or $hasAC){ Note "   full: $acl" }
    }

    Section 'AXIS 1 -- child token + PRIVILEGE dump (is SeChangeNotifyPrivilege retained + enabled?)'
    $codeWhoami = [AC]::Launch($acSidPtr, "`"$child`" whoami `"$dump`"", $bin)
    Note "child(whoami) raw exit: $codeWhoami"
    $inAC = $false; $hasChangeNotify = $false; $changeNotifyEnabled = $false
    if (Test-Path $dump) {
        $dumpText = Get-Content -Raw $dump
        Write-Host $dumpText
        if ($dumpText -match 'TokenIsAppContainer=1') { $inAC = $true }
        if ($dumpText -match 'SeChangeNotifyPrivilege') {
            $hasChangeNotify = $true
            if ($dumpText -match 'SeChangeNotifyPrivilege enabled=True') { $changeNotifyEnabled = $true }
        }
    } else { Note "no token dump produced (child could not write diag)" }
    Note "inAppContainer=$inAC  hasSeChangeNotify=$hasChangeNotify  enabled=$changeNotifyEnabled"

    Section 'AXIS 1 -- AC child reads the leaf (ancestors ungranted) + negative control'
    $codeLeaf = [AC]::Launch($acSidPtr, "`"$child`" read `"$allowed`"", $bin)
    Note "child(read leaf under ungranted ancestors) exit: $codeLeaf  (0 => traverse BYPASSED => CLAIM 1 FALSE)"
    $codeSib = [AC]::Launch($acSidPtr, "`"$child`" read `"$sibling`"", $bin)
    Note "child(read ungranted sibling)             exit: $codeSib   (expect 5 => confinement genuine)"

    if(-not $inAC){
        Record 'AXIS1 traverse-bypass' $false "INCONCLUSIVE: child not in AppContainer (TokenIsAppContainer!=1)"
    } elseif($codeSib -ne 5){
        Record 'AXIS1 traverse-bypass' $false "negative control broke: ungranted sibling read gave exit=$codeSib (expected 5) -> setup invalid"
    } elseif($codeLeaf -eq 0){
        Record 'AXIS1 traverse-bypass' $true  "leaf reachable with LEAF-ONLY grant + ungranted ancestors => LowBox bypasses traverse => CLAIM 1 (needs ancestor grants / C:\ store) is FALSE. SeChangeNotify present=$hasChangeNotify enabled=$changeNotifyEnabled"
    } elseif($codeLeaf -eq 5){
        Record 'AXIS1 traverse-bypass' $false "leaf NOT reachable (exit 5) with leaf-only grant => CLAIM 1 CONFIRMED: ancestor traverse grants ARE required. SeChangeNotify present=$hasChangeNotify enabled=$changeNotifyEnabled"
    } else {
        Record 'AXIS1 traverse-bypass' $false "indeterminate leaf-read exit=$codeLeaf"
    }

    # =====================================================================================
    Section 'AXIS 3 -- read+write confine fully inside %TEMP% (all four quadrants)'
    $wdir = Join-Path $stage 'wdir'; New-Item -ItemType Directory -Path $wdir -Force | Out-Null
    & icacls $wdir /inheritance:r /grant:r "${me}:(OI)(CI)(F)" | Out-Null
    & icacls $wdir /grant "*${acSidStr}:(OI)(CI)(M)" | Out-Null                 # AC writable
    $nowrite = Join-Path $stage 'nowrite'; New-Item -ItemType Directory -Path $nowrite -Force | Out-Null
    & icacls $nowrite /inheritance:r /grant:r "${me}:(OI)(CI)(F)" | Out-Null    # user-only, AC has nothing
    $vault = Join-Path $stage 'vault'; New-Item -ItemType Directory -Path $vault -Force | Out-Null
    & icacls $vault /inheritance:r /grant:r "${me}:(OI)(CI)(F)" | Out-Null
    $vsecret = Join-Path $vault 'secret.env'; Set-Content -Path $vsecret -Value 'VAULT_SECRET=sk-x' -NoNewline

    $rGrant = [AC]::Launch($acSidPtr, "`"$child`" read  `"$allowed`"",  $bin)   # granted (reuse leaf)
    $rDeny  = [AC]::Launch($acSidPtr, "`"$child`" read  `"$vsecret`"",  $bin)   # ungranted vault secret
    $wGrant = [AC]::Launch($acSidPtr, "`"$child`" write `"$(Join-Path $wdir 'out.txt')`"", $bin)
    $wDeny  = [AC]::Launch($acSidPtr, "`"$child`" write `"$(Join-Path $nowrite 'out.txt')`"", $bin)
    Note "read granted=$rGrant (exp 0)  read vault-secret=$rDeny (exp 5)  write granted=$wGrant (exp 0)  write outside=$wDeny (exp 5)"
    $a3 = ($rGrant -eq 0 -and $rDeny -eq 5 -and $wGrant -eq 0 -and $wDeny -eq 5)
    Record 'AXIS3 read+write-confine-in-%TEMP%' $a3 "read granted=$rGrant secret=$rDeny ; write granted=$wGrant outside=$wDeny (all in %TEMP%, no C:\ store)"

    # =====================================================================================
    Section 'AXIS 4 -- deny-inside-allow: is an explicit DENY-ACE honored against a lowbox token?'
    # INFORMATIONAL (does not gate): windows.rs claims an inherited package/AAP allow satisfies the
    # lowbox check "before the file deny is reached" -> a deny-ACE denylist is defeated. We test all
    # three deny spellings AND dump the resulting ACL so a null result cannot be "the deny never landed".
    function Test-Deny($tag, $rootGrantSid, $denySid){
        $r = Join-Path $stage $tag; New-Item -ItemType Directory -Path $r -Force | Out-Null
        & icacls $r /inheritance:r /grant:r "${me}:(OI)(CI)(F)" | Out-Null
        (& icacls $r /grant "*${rootGrantSid}:(OI)(CI)(RX)") | Out-Null
        $pub = Join-Path $r 'public.txt'; Set-Content $pub 'pub' -NoNewline
        $sec = Join-Path $r 'secret.txt'; Set-Content $sec 'SECRET' -NoNewline
        $denyOut = (& icacls $sec /deny "*${denySid}:(R)" 2>&1 | Out-String); $denyRc = $LASTEXITCODE
        Note "$tag deny icacls rc=$denyRc : $($denyOut.Trim())"
        Note "$tag secret.txt ACL after deny:`n$((& icacls $sec 2>&1 | Out-String).Trim())"
        $p = [AC]::Launch($acSidPtr, "`"$child`" read `"$pub`"", $bin)
        $s = [AC]::Launch($acSidPtr, "`"$child`" read `"$sec`"", $bin)
        Note "$tag read: public=$p secret=$s (secret 5 => deny honored ; 0 => deny defeated)"
        return @($p,$s)
    }
    $d4a = Test-Deny 'da_acsid'      $acSidStr $acSidStr   # AC-SID allow, deny AC-SID
    $d4b = Test-Deny 'da_aap_denyac' $AAP      $acSidStr   # AAP allow, deny the per-run AC-SID (windows.rs's decisive case)
    $d4c = Test-Deny 'da_aap_denyaap' $AAP     $AAP        # AAP allow, deny AAP
    $denyHonored = ($d4a[1] -eq 5 -or $d4b[1] -eq 5 -or $d4c[1] -eq 5)
    Info 'AXIS4 deny-ACE-vs-lowbox' (-not $denyHonored) "deny-ACE secret reads: AC/AC=$($d4a[1]) AAP/AC=$($d4b[1]) AAP/AAP=$($d4c[1]) (all 0 CONFIRMS windows.rs: an inherited package/AAP allow defeats a per-file deny-ACE -> deny-inside-allow is NOT expressible via deny-ACEs)"

    # 4d -- THE WORKING RESOLUTION (gated): carve a secret out of a broadly-AC-SID-granted tree by
    # STRIPPING its inheritance (PROTECTED DACL) + regranting only non-AppContainer principals. The
    # lowbox check then finds no package/AAP grant -> denied. This is generous-read-minus-secrets
    # expressed as allowlist + inheritance-break, entirely in %TEMP%, no C:\ store.
    $r4d = Join-Path $stage 'carveout'; New-Item -ItemType Directory -Path $r4d -Force | Out-Null
    & icacls $r4d /inheritance:r /grant:r "${me}:(OI)(CI)(F)" | Out-Null
    & icacls $r4d /grant "*${acSidStr}:(OI)(CI)(RX)" | Out-Null          # broad AC read grant (the "allow")
    $pub4d = Join-Path $r4d 'public.txt'; Set-Content $pub4d 'pub' -NoNewline
    $sec4d = Join-Path $r4d 'secret.txt'; Set-Content $sec4d 'SECRET' -NoNewline
    & icacls $sec4d /inheritance:r /grant:r "${me}:(F)" | Out-Null       # carve-out: strip AC-SID inheritance
    Note "4d carve-out secret ACL:`n$((& icacls $sec4d 2>&1 | Out-String).Trim())"
    $p4d = [AC]::Launch($acSidPtr, "`"$child`" read `"$pub4d`"", $bin)
    $s4d = [AC]::Launch($acSidPtr, "`"$child`" read `"$sec4d`"", $bin)
    Note "4d allowlist+inheritance-break: public=$p4d (exp 0) secret=$s4d (exp 5)"
    $a4d = ($p4d -eq 0 -and $s4d -eq 5)
    Record 'AXIS4d generous-read-minus-secret via inheritance-break' $a4d "public=$p4d secret=$s4d (the WORKING resolution; deny-ACEs are not needed)"

    # =====================================================================================
    Section 'AXIS 5 -- AAP inheritance issue is real, and inheritance:r is the clean fix'
    # 5a: a dir that INHERITS an AAP grant is AC-readable with NO explicit AC/AAP ace on it.
    $paap = Join-Path $stage 'aap_parent'; New-Item -ItemType Directory -Path $paap -Force | Out-Null
    & icacls $paap /grant "*${AAP}:(OI)(CI)(RX)" | Out-Null       # parent carries AAP (like a dir under C:\)
    $sub = Join-Path $paap 'sub'; New-Item -ItemType Directory -Path $sub -Force | Out-Null   # inherits AAP
    $isec = Join-Path $sub 'inherited-secret.txt'; Set-Content $isec 'INHERITED_SECRET' -NoNewline
    $r5a = [AC]::Launch($acSidPtr, "`"$child`" read `"$isec`"", $bin)
    Note "5a read secret under INHERITED AAP: $r5a (expect 0 => the AAP-inheritance hazard is REAL)"
    # 5b: break inheritance on sub (PROTECTED DACL) + user-only -> inherited AAP gone -> AC blocked.
    & icacls $sub /inheritance:r /grant:r "${me}:(OI)(CI)(F)" | Out-Null
    $acl5 = (& icacls $sub 2>&1 | Out-String)
    Note "5b sub ACL after /inheritance:r : $acl5"
    $r5b = [AC]::Launch($acSidPtr, "`"$child`" read `"$isec`"", $bin)
    Note "5b read same secret after inheritance:r: $r5b (expect 5 => inheritance-break confines even in %TEMP%)"
    $a5 = ($r5a -eq 0 -and $r5b -eq 5)
    Record 'AXIS5 AAP-inheritance + inheritance:r fix' $a5 "inherited-AAP-read=$r5a  after-inheritance:r=$r5b"
}
finally {
    [void][AC]::DeleteAppContainerProfile($acName)
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}

Section 'SUMMARY'
foreach($s in $summary){ Write-Host "  $s" }
Write-Host ""
Write-Host "unprivileged(parent non-elevated)=$([bool](-not $isAdmin))"
if($fail.Count -gt 0){
    Write-Host "OVERALL: FAIL ($($fail.Count) gated axis/axes not as-expected) -- read the per-axis RESULT lines above"
    exit 1
}
Write-Host "OVERALL: PASS -- non-elevated AppContainer confinement works in an ordinary %TEMP% location"
Write-Host "  CLAIM 1 (needs ancestor-traverse grants / a C:\-owned store) => REFUTED (AXIS 1/2/3)"
Write-Host "  generous-read-minus-secrets => expressible via allowlist + inheritance-break (AXIS 4d/5),"
Write-Host "  NOT via deny-ACEs (AXIS 4 info CONFIRMS deny-ACEs are defeated by an inherited package/AAP allow)"
exit 0
