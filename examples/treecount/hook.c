// slimfit#66's hook, injected (DYLD_INSERT_LIBRARIES) into every process of a
// test's tree that dyld lets it into. macOS keeps instructions retired per
// process and adds none of a child's to its parent, so:
// - each process logs its own counts when it exits (atexit), and the counts
//   of each child it reaps, read while the child is a zombie (the caller's
//   wait, first with WNOWAIT): the second covers children the hook cannot
//   enter, the system binaries (SIP);
// - every exec and spawn keeps the hook's variables in the new environment
//   (a test may clear it), and runs a system binary that runs others (the
//   shells, git, env, xargs, find, nice, nohup) as its re-signed copy in
//   $TREECOUNT_COPIES, which the hook can enter: else dyld drops the
//   variables and their children go uncounted. Other system binaries (ps,
//   sed, ...) are counted as zombies, their own children not at all; so
//   are the orphans of a killed process, which launchd reaps.
// A line per process: pid start(mach) instructions cycles cpu_ns children_cpu_ns.
// Built for arm64 and arm64e by treecount (main.rs), which sums the log.
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <mach/mach_time.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/wait.h>
#include <unistd.h>

static unsigned long long ns(unsigned long long t) {
    static mach_timebase_info_data_t tb;
    if (tb.denom == 0) mach_timebase_info(&tb);
    return (unsigned long long)((__uint128_t)t * tb.numer / tb.denom);
}

static void log_pid(pid_t pid) {
    const char *path = getenv("TREECOUNT_LOG");
    struct rusage_info_v4 ri;
    if (!path || proc_pid_rusage(pid, RUSAGE_INFO_V4, (rusage_info_t *)&ri) != 0) return;
    char line[160];
    int n = snprintf(line, sizeof line, "%d %llu %llu %llu %llu %llu\n", pid, ri.ri_proc_start_abstime,
                     ri.ri_instructions, ri.ri_cycles, ns(ri.ri_user_time + ri.ri_system_time),
                     ns(ri.ri_child_user_time + ri.ri_child_system_time));
    int fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0644);
    if (fd < 0) return;
    (void)!write(fd, line, (size_t)n); // one short write: lines do not interleave
    close(fd);
}

static void at_exit(void) { log_pid(getpid()); }
__attribute__((constructor)) static void init(void) { atexit(at_exit); }

// The caller's wait, with WNOWAIT: logs the child it is about to reap.
// -1 is that wait's failure, which the caller's own wait would have met.
static int peek(idtype_t t, id_t id, int options) {
    siginfo_t si;
    memset(&si, 0, sizeof si);
    if (waitid(t, id, &si, options | WNOWAIT) != 0) return -1;
    if (si.si_pid > 0 && (si.si_code == CLD_EXITED || si.si_code == CLD_KILLED || si.si_code == CLD_DUMPED))
        log_pid(si.si_pid);
    return 0;
}

static int as_waitid(pid_t pid, int options, idtype_t *t, id_t *id) {
    int o = WEXITED | (options & (WNOHANG | WCONTINUED)) | (options & WUNTRACED ? WSTOPPED : 0);
    if (pid == -1) *t = P_ALL, *id = 0;
    else if (pid == 0) *t = P_PGID, *id = (id_t)getpgrp();
    else if (pid < 0) *t = P_PGID, *id = (id_t)-pid;
    else *t = P_PID, *id = (id_t)pid;
    return o;
}

static pid_t my_wait4(pid_t pid, int *status, int options, struct rusage *ru) {
    idtype_t t; id_t id;
    int o = as_waitid(pid, options, &t, &id);
    return peek(t, id, o) ? -1 : wait4(pid, status, options, ru);
}
static pid_t my_waitpid(pid_t pid, int *status, int options) {
    idtype_t t; id_t id;
    int o = as_waitid(pid, options, &t, &id);
    return peek(t, id, o) ? -1 : waitpid(pid, status, options);
}
static pid_t my_wait(int *status) { return peek(P_ALL, 0, WEXITED) ? -1 : wait(status); }
static int my_waitid(idtype_t t, id_t id, siginfo_t *info, int options) {
    if (!(options & WNOWAIT) && (options & WEXITED) && peek(t, id, options)) return -1;
    return waitid(t, id, info, options);
}

// A system binary's re-signed copy in $TREECOUNT_COPIES, or `path` itself.
static const char *copy(const char *path, char *buf, size_t len) {
    const char *dir = getenv("TREECOUNT_COPIES");
    static const char *const from[] = {"/bin/sh",       "/bin/bash",     "/bin/zsh",      "/usr/bin/git",
                                       "/usr/bin/env",  "/usr/bin/xargs", "/usr/bin/find", "/usr/bin/nice",
                                       "/usr/bin/nohup", 0};
    static const char *const to[] = {"sh", "bash", "zsh", "git/bin/git", "env", "xargs", "find", "nice", "nohup", 0};
    char real[1024];
    if (!dir || !path || !realpath(path, real)) return path; // a link to one counts as one
    for (int i = 0; from[i]; i++)
        if (strcmp(real, from[i]) == 0) {
            snprintf(buf, len, "%s/%s", dir, to[i]);
            return access(buf, X_OK) == 0 ? buf : path;
        }
    return path;
}

// `envp` with the hook's variables as this process has them: on the stack,
// so it is safe in a forked child (no malloc).
#define KEEP 3
static const char *const kept[KEEP] = {"DYLD_INSERT_LIBRARIES", "TREECOUNT_LOG", "TREECOUNT_COPIES"};
static size_t count(char *const *v) { size_t n = 0; while (v && v[n]) n++; return n; }
static void with_hook(char *const *envp, char **out, char (*bufs)[1024]) {
    size_t n = 0;
    for (size_t i = 0; envp && envp[i]; i++) {
        int ours = 0;
        for (int k = 0; k < KEEP; k++) {
            size_t l = strlen(kept[k]);
            if (strncmp(envp[i], kept[k], l) == 0 && envp[i][l] == '=') ours = 1;
        }
        if (!ours) out[n++] = envp[i];
    }
    for (int k = 0; k < KEEP; k++) {
        const char *v = getenv(kept[k]);
        if (v && snprintf(bufs[k], 1024, "%s=%s", kept[k], v) < 1024) out[n++] = bufs[k];
    }
    out[n] = 0;
}

static int my_execve(const char *path, char *const argv[], char *const envp[]) {
    char sh[1024], bufs[KEEP][1024];
    char *env[count(envp) + KEEP + 1];
    with_hook(envp, env, bufs);
    return execve(copy(path, sh, sizeof sh), argv, env);
}

static int my_posix_spawn(pid_t *pid, const char *path, const posix_spawn_file_actions_t *fa,
                          const posix_spawnattr_t *attr, char *const argv[], char *const envp[]) {
    char sh[1024], bufs[KEEP][1024];
    char *env[count(envp) + KEEP + 1];
    with_hook(envp, env, bufs);
    return posix_spawn(pid, copy(path, sh, sizeof sh), fa, attr, argv, env);
}

// posix_spawnp, with the file found on PATH here, so a system binary found
// there runs as its copy.
static int my_posix_spawnp(pid_t *pid, const char *file, const posix_spawn_file_actions_t *fa,
                           const posix_spawnattr_t *attr, char *const argv[], char *const envp[]) {
    char sh[1024], bufs[KEEP][1024], found[1024];
    char *env[count(envp) + KEEP + 1];
    with_hook(envp, env, bufs);
    const char *p = getenv("PATH");
    if (file && !strchr(file, '/') && p) {
        while (*p) {
            const char *e = strchr(p, ':');
            size_t l = e ? (size_t)(e - p) : strlen(p);
            if (snprintf(found, sizeof found, "%.*s/%s", (int)l, l ? p : ".", file) < (int)sizeof found &&
                access(found, X_OK) == 0) {
                const char *s = copy(found, sh, sizeof sh);
                if (s != found) return posix_spawn(pid, s, fa, attr, argv, env);
                break;
            }
            if (!e) break;
            p = e + 1;
        }
    }
    return posix_spawnp(pid, file, fa, attr, argv, env);
}

#define INTERPOSE(n) \
    __attribute__((used, section("__DATA,__interpose"))) static struct { const void *new, *old; } \
        interpose_##n = {(const void *)my_##n, (const void *)n};
INTERPOSE(wait4)
INTERPOSE(waitpid)
INTERPOSE(wait)
INTERPOSE(waitid)
INTERPOSE(execve)
INTERPOSE(posix_spawn)
INTERPOSE(posix_spawnp)
