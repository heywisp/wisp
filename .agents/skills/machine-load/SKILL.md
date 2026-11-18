---
name: machine-load
description: How to start builds, tests, clippy, cargo install, fuzzers, docker builds and test loops on a machine that several agents share, through the heavy wrapper. Read before running any of them.
---

# Machine load

This skill spells out AGENTS.md's `heavy` rule, which wins over it.

Unthrottled parallel builds, tests and stress loops can exhaust the shared machine.
- Check resources before every heavy command by starting it through `heavy`: builds, tests, clippy, `cargo install`, fuzzers, `docker run` of a build or test, and loops that rerun a test. Example: `heavy cargo nextest run -p wisp-core`.
- `heavy` waits for a free job slot, a 1-minute load under the core count and enough free memory, then runs the command. It can wait several minutes, so run long jobs in the background. Exit code 75 means it gave up waiting; try again later rather than bypassing it. It also refuses at once, with exit 75, when the disk holding the target dir has too little free space. Waiting doesn't help then: report it, so finished target dirs can be deleted, and never bypass `heavy`.
- Memory: `heavy` runs each job in its own group, capped at a third of the machine's RAM with no swap, and watches memory while the job runs. It stops a job that passes its cap, and every running job once under 3 GB is left on the machine. A stopped job ends with exit code 137 and a `heavy: stopped the job` line that gives the reason; a job that is a single process can pass its cap and end with 137 and no line. Treat it like exit 75: try again later, never bypass. Report a job that passes its cap twice; `HEAVY_JOB_MEM_GB=<GB>` raises the cap for one call, only for a job known to need it. `heavy` can't cap what it doesn't start, such as a container, an editor or a language server.
- Disk: each target dir can take 10 to 35 GB on the shared disk. Clear your target's `debug/incremental` once it passes about 60 GB. A task's target dir is the main session's helper's to remove or keep, at the flow's Clean up step or earlier when the task is paused; delete your own scratch target in the turn you report, after checking that no process uses it.
- Wrap a whole stress loop in one call (`heavy bash -c 'for i in ...; done'`) and use the fewest reruns that answer the question.
- `docker run` builds don't read `~/.cargo/config.toml`, and their containers run outside `heavy`'s memory cap, so cap them: `heavy docker run --cpus=12 --memory=16g --memory-swap=16g -e CARGO_BUILD_JOBS=12 --user "$(id -u):$(id -g)" ...`. Add `-e HOME=/tmp` if the image needs a writable home. Pass `--user` for every container that writes to a bind-mounted host path: a root container leaves root-owned files that nobody can delete without sudo.
- If `heavy` isn't installed, run `uptime` first and wait while the 1-minute load is above the core count.
