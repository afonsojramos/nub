// Minimal AppContainer child for the nub Windows AppContainer file-access probes.
// Tiny load surface (no PowerShell, no heavy CLR module probing) so an AppContainer
// child starts cleanly and the probe measures the SECURITY outcome, not a host-init crash.
//
// Usage + exit-code contract (read by the parent probe):
//   probe-child.exe whoami <dumpPath>  -> 0 ; dumps its own token (IsAppContainer, AC SID,
//                                        integrity level, package/capability groups AND every
//                                        token PRIVILEGE with its enabled state) to <dumpPath>
//                                        so the parent can PROVE the child is really in the
//                                        AppContainer and inspect SeChangeNotifyPrivilege.
//   probe-child.exe read   <path>  -> 0 read OK | 5 ACCESS_DENIED | 9 other error
//   probe-child.exe write  <path>  -> 0 write OK | 5 ACCESS_DENIED | 9 other error
//   probe-child.exe getenv <NAME>  -> 0 present (prints value) | 4 absent
//
// Anything that prints "CHILD ..." goes to stdout so the parent's captured log shows it.
using System;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Principal;
using System.Text;

static class ProbeChild {
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool OpenProcessToken(IntPtr h, uint access, out IntPtr tok);
    [DllImport("advapi32.dll", SetLastError=true)] static extern bool GetTokenInformation(IntPtr tok, int cls, IntPtr buf, int len, out int ret);
    [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool ConvertSidToStringSid(IntPtr sid, out IntPtr str);
    [DllImport("advapi32.dll", SetLastError=true, CharSet=CharSet.Unicode)] static extern bool LookupPrivilegeName(string sys, ref LUID luid, StringBuilder name, ref int cch);
    [DllImport("kernel32.dll")] static extern IntPtr LocalFree(IntPtr h);

    const uint TOKEN_QUERY = 0x0008;
    const int TokenPrivileges = 3, TokenIntegrityLevel = 25, TokenIsAppContainer = 29, TokenAppContainerSid = 31;
    const uint SE_PRIVILEGE_ENABLED = 0x2, SE_PRIVILEGE_ENABLED_BY_DEFAULT = 0x1;

    [StructLayout(LayoutKind.Sequential)] struct LUID { public uint Low; public int High; }
    [StructLayout(LayoutKind.Sequential)] struct LUID_AND_ATTRIBUTES { public LUID Luid; public uint Attributes; }

    static int GetDword(IntPtr tok, int cls) {
        IntPtr buf = Marshal.AllocHGlobal(4);
        try { int ret; return GetTokenInformation(tok, cls, buf, 4, out ret) ? Marshal.ReadInt32(buf) : -1; }
        finally { Marshal.FreeHGlobal(buf); }
    }
    static string SidStr(IntPtr sid) {
        if (sid == IntPtr.Zero) return "<null>";
        IntPtr s; if (!ConvertSidToStringSid(sid, out s)) return "<convfail " + Marshal.GetLastWin32Error() + ">";
        string r = Marshal.PtrToStringUni(s); LocalFree(s); return r;
    }
    // The first pointer-sized field of TOKEN_APPCONTAINER_INFORMATION and TOKEN_MANDATORY_LABEL.Label
    // is a PSID, so one helper reads both the AppContainer SID and the integrity-level SID.
    static string GetLeadingSid(IntPtr tok, int cls) {
        int len; GetTokenInformation(tok, cls, IntPtr.Zero, 0, out len);
        if (len <= 0) return "<none/err " + Marshal.GetLastWin32Error() + ">";
        IntPtr buf = Marshal.AllocHGlobal(len);
        try {
            if (!GetTokenInformation(tok, cls, buf, len, out len)) return "<err " + Marshal.GetLastWin32Error() + ">";
            return SidStr(Marshal.ReadIntPtr(buf));
        } finally { Marshal.FreeHGlobal(buf); }
    }
    // Enumerate TOKEN_PRIVILEGES and report each privilege name + its enabled state. This is the
    // direct evidence for the traverse-bypass question: SeChangeNotifyPrivilege (Bypass Traverse
    // Checking) present + enabled on the LowBox token means intermediate-dir ACLs are not checked.
    static void DumpPrivileges(IntPtr tok, Action<string> emit) {
        int len; GetTokenInformation(tok, TokenPrivileges, IntPtr.Zero, 0, out len);
        if (len <= 0) { emit("CHILD priv <none/err " + Marshal.GetLastWin32Error() + ">"); return; }
        IntPtr buf = Marshal.AllocHGlobal(len);
        try {
            if (!GetTokenInformation(tok, TokenPrivileges, buf, len, out len)) { emit("CHILD priv <err " + Marshal.GetLastWin32Error() + ">"); return; }
            int count = Marshal.ReadInt32(buf);
            emit("CHILD priv count=" + count);
            int recSz = Marshal.SizeOf(typeof(LUID_AND_ATTRIBUTES));
            long baseAddr = buf.ToInt64() + 4; // skip the leading PrivilegeCount DWORD
            for (int i = 0; i < count; i++) {
                var la = (LUID_AND_ATTRIBUTES)Marshal.PtrToStructure((IntPtr)(baseAddr + i * recSz), typeof(LUID_AND_ATTRIBUTES));
                LUID luid = la.Luid;
                int cch = 0; LookupPrivilegeName(null, ref luid, null, ref cch);
                var sb2 = new StringBuilder(cch + 1); int cch2 = cch + 1;
                string name = LookupPrivilegeName(null, ref luid, sb2, ref cch2) ? sb2.ToString() : ("<luid " + la.Luid.Low + ">");
                bool enabled = (la.Attributes & SE_PRIVILEGE_ENABLED) != 0;
                bool enabledByDefault = (la.Attributes & SE_PRIVILEGE_ENABLED_BY_DEFAULT) != 0;
                emit("CHILD priv " + name + " enabled=" + enabled + " enabledByDefault=" + enabledByDefault + " attr=0x" + la.Attributes.ToString("X"));
            }
        } finally { Marshal.FreeHGlobal(buf); }
    }
    // An AppContainer child's stdout is NOT reliably captured by the parent's redirected log, so the
    // token dump is TEE'd to outPath (a file in a dir the AC SID was granted write) -- that file is
    // the reliable diagnostic channel the parent reads back.
    static void DumpToken(string outPath) {
        var sb = new StringBuilder();
        Action<string> emit = line => { Console.WriteLine(line); sb.AppendLine(line); };
        try {
            emit("CHILD whoami user=" + WindowsIdentity.GetCurrent().Name);
            IntPtr tok;
            if (!OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, out tok)) {
                emit("CHILD whoami OpenProcessToken failed " + Marshal.GetLastWin32Error());
            } else {
                emit("CHILD whoami TokenIsAppContainer=" + GetDword(tok, TokenIsAppContainer));
                emit("CHILD whoami TokenAppContainerSid=" + GetLeadingSid(tok, TokenAppContainerSid));
                emit("CHILD whoami IntegrityLevelSid=" + GetLeadingSid(tok, TokenIntegrityLevel));
                foreach (IdentityReference g in WindowsIdentity.GetCurrent().Groups) {
                    string s = g.Value;
                    if (s.StartsWith("S-1-15-2") || s.StartsWith("S-1-15-3") || s.StartsWith("S-1-16"))
                        emit("CHILD whoami group=" + s);
                }
                DumpPrivileges(tok, emit);
            }
        } catch (Exception e) { emit("CHILD whoami ERR: " + e.Message); }
        if (outPath != null) {
            try { File.WriteAllText(outPath, sb.ToString()); }
            catch (Exception e) { Console.WriteLine("CHILD whoami could not write dump to " + outPath + ": " + e.Message); }
        }
    }

    static int Main(string[] a) {
        try {
            if (a.Length < 1) { Console.WriteLine("CHILD bad-args"); return 2; }
            if (a[0] == "whoami") { DumpToken(a.Length >= 2 ? a[1] : null); return 0; }
            if (a.Length < 2) { Console.WriteLine("CHILD bad-args"); return 2; }
            switch (a[0]) {
                case "read": {
                    try {
                        string s = File.ReadAllText(a[1]);
                        Console.WriteLine("CHILD read OK len=" + s.Length);
                        return 0;
                    } catch (UnauthorizedAccessException e) {
                        Console.WriteLine("CHILD read DENIED: " + e.Message); return 5;
                    } catch (Exception e) {
                        Console.WriteLine("CHILD read ERR: " + e.GetType().Name + " " + e.Message); return 9;
                    }
                }
                case "write": {
                    try {
                        File.WriteAllText(a[1], "from-appcontainer-child");
                        Console.WriteLine("CHILD write OK");
                        return 0;
                    } catch (UnauthorizedAccessException e) {
                        Console.WriteLine("CHILD write DENIED: " + e.Message); return 5;
                    } catch (Exception e) {
                        Console.WriteLine("CHILD write ERR: " + e.GetType().Name + " " + e.Message); return 9;
                    }
                }
                case "getenv": {
                    string v = Environment.GetEnvironmentVariable(a[1]);
                    if (v == null) { Console.WriteLine("CHILD env ABSENT"); return 4; }
                    Console.WriteLine("CHILD env PRESENT: " + v); return 0;
                }
                default:
                    Console.WriteLine("CHILD unknown-cmd"); return 2;
            }
        } catch (Exception e) {
            Console.WriteLine("CHILD FATAL: " + e); return 3;
        }
    }
}
