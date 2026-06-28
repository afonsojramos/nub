import { execFile } from "node:child_process";
// Spawn `node` DIRECTLY (execFile, not exec) so each child is a single process,
// not a `cmd.exe`/`sh` wrapper around `node`. On Windows `exec` routes through
// `cmd.exe /c`, doubling the process count to ~100 under 50-way concurrency and
// regularly exhausting a shared CI runner's headroom — children then exceed the
// timeout and the test flakes (observed 36/50 failures on windows-latest). Direct
// `execFile` halves the process count and tests exactly what this fixture is for:
// nub's per-child `node`-spawn augmentation under concurrency. The generous
// timeout absorbs a loaded runner's cold-start latency without masking a real hang.
let ok = 0;
let fail = 0;
let done = 0;
for (let i = 0; i < 50; i++) {
  execFile("node", ["-e", "process.exit(0)"], { timeout: 60000 }, (err) => {
    if (err) fail++;
    else ok++;
    done++;
    if (done === 50) console.log("concurrent:" + ok + "/50,fail:" + fail);
  });
}
