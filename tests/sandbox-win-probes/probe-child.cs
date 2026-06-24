// Minimal AppContainer child for the nub Windows sandbox validation probes.
// Tiny load surface (no PowerShell, no heavy CLR module probing) so an AppContainer
// child starts cleanly and the probe measures the SECURITY outcome, not a host-init crash.
//
// Usage + exit-code contract (read by the parent probe):
//   probe-child.exe read   <path>   -> 0 read OK | 5 ACCESS_DENIED | 9 other error
//   probe-child.exe write  <path>   -> 0 write OK | 5 ACCESS_DENIED | 9 other error
//   probe-child.exe connect <ip> <port>
//                                   -> 0 connect OK | 5 access-denied(WSAEACCES/10013)
//                                      | 6 timeout | 9 other error
//   probe-child.exe getenv <NAME>   -> 0 present (prints value) | 4 absent
//
// Anything that prints "CHILD ..." goes to stdout so the parent's captured log shows it.
using System;
using System.IO;
using System.Net.Sockets;

static class ProbeChild {
    static int Main(string[] a) {
        try {
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
                case "connect": {
                    if (a.Length < 3) { Console.WriteLine("CHILD connect bad-args"); return 2; }
                    int port = int.Parse(a[2]);
                    try {
                        using (var c = new TcpClient()) {
                            var iar = c.BeginConnect(a[1], port, null, null);
                            if (!iar.AsyncWaitHandle.WaitOne(8000, false)) {
                                Console.WriteLine("CHILD connect TIMEOUT"); return 6;
                            }
                            c.EndConnect(iar);
                            Console.WriteLine("CHILD connect OK");
                            return 0;
                        }
                    } catch (SocketException se) {
                        // 10013 = WSAEACCES (AppContainer egress block surfaces here)
                        Console.WriteLine("CHILD connect FAILED SocketErrorCode=" + (int)se.SocketErrorCode
                            + " (" + se.SocketErrorCode + ") msg=" + se.Message);
                        return (se.SocketErrorCode == SocketError.AccessDenied) ? 5 : 9;
                    } catch (Exception e) {
                        Console.WriteLine("CHILD connect ERR: " + e.GetType().Name + " " + e.Message);
                        // unwrap a nested SocketException
                        var se = e.InnerException as SocketException;
                        if (se != null) {
                            Console.WriteLine("  inner SocketErrorCode=" + (int)se.SocketErrorCode);
                            return (se.SocketErrorCode == SocketError.AccessDenied) ? 5 : 9;
                        }
                        return 9;
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
