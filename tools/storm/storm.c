/* storm.c — controlled fork+exec storm generator for daemon benchmarks.
 *
 * Each iteration forks a child that immediately execv()s a target binary.
 * argv[0] of the child is set independently of the target path so a benchmark
 * can present a ruled or unruled *name* to the auto-nice daemon under test
 * without copying or renaming binaries.
 *
 * Output (single line on stdout at exit):
 *   RESULT execs=<N> elapsed=<sec> rate=<execs/sec>
 */
#define _GNU_SOURCE
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>

static atomic_ulong g_execs;
static volatile sig_atomic_t g_stop;

static void on_alarm(int sig)
{
    (void)sig;
    g_stop = 1;
}

static void usage(const char *p)
{
    fprintf(stderr,
            "usage: %s [--rate N] [--duration S] [--workers W] [--argv0 NAME] [--target PATH]\n"
            "  --rate      execs/sec across all workers (default 2000)\n"
            "  --duration  seconds to run (default 60)\n"
            "  --workers   forking threads (default 4)\n"
            "  --argv0     argv[0] presented to the child (default gsa-storm-probe)\n"
            "  --target    binary to execv (default /bin/true)\n",
            p);
    exit(2);
}

static void add_ns(struct timespec *ts, long ns)
{
    ts->tv_nsec += ns;
    while (ts->tv_nsec >= 1000000000L) {
        ts->tv_nsec -= 1000000000L;
        ts->tv_sec++;
    }
}

struct worker_arg {
    long interval_ns;
    const char *argv0;
    const char *target;
};

static void *worker(void *p)
{
    struct worker_arg *a = p;
    struct timespec next;
    clock_gettime(CLOCK_MONOTONIC, &next);
    while (!g_stop) {
        pid_t pid = fork();
        if (pid == 0) {
            char *child_argv[] = { (char *)a->argv0, NULL };
            execv(a->target, child_argv);
            _exit(127);
        }
        if (pid > 0)
            atomic_fetch_add(&g_execs, 1);
        add_ns(&next, a->interval_ns);
        clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &next, NULL);
    }
    return NULL;
}

int main(int argc, char **argv)
{
    long rate = 2000, duration = 60, workers = 4;
    const char *argv0 = "gsa-storm-probe";
    const char *target = "/bin/true";

    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--rate") && i + 1 < argc)
            rate = atol(argv[++i]);
        else if (!strcmp(argv[i], "--duration") && i + 1 < argc)
            duration = atol(argv[++i]);
        else if (!strcmp(argv[i], "--workers") && i + 1 < argc)
            workers = atol(argv[++i]);
        else if (!strcmp(argv[i], "--argv0") && i + 1 < argc)
            argv0 = argv[++i];
        else if (!strcmp(argv[i], "--target") && i + 1 < argc)
            target = argv[++i];
        else
            usage(argv[0]);
    }
    if (rate <= 0 || duration <= 0 || workers <= 0)
        usage(argv[0]);
    if (workers > rate)
        workers = rate;
    long per_worker = rate / workers;
    if (per_worker < 1)
        per_worker = 1;

    /* Auto-reap children so the storm cannot pile up zombies. */
    signal(SIGCHLD, SIG_IGN);
    signal(SIGALRM, on_alarm);
    alarm((unsigned)duration);

    struct worker_arg wa = {
        .interval_ns = 1000000000L / per_worker,
        .argv0 = argv0,
        .target = target,
    };

    struct timespec t0, t1;
    clock_gettime(CLOCK_MONOTONIC, &t0);

    pthread_t *th = calloc((size_t)workers, sizeof(*th));
    for (long i = 0; i < workers; i++)
        pthread_create(&th[i], NULL, worker, &wa);
    for (long i = 0; i < workers; i++)
        pthread_join(th[i], NULL);

    clock_gettime(CLOCK_MONOTONIC, &t1);
    double elapsed = (double)(t1.tv_sec - t0.tv_sec) +
                     (double)(t1.tv_nsec - t0.tv_nsec) / 1e9;
    unsigned long execs = atomic_load(&g_execs);
    printf("RESULT execs=%lu elapsed=%.3f rate=%.1f\n",
           execs, elapsed, elapsed > 0 ? execs / elapsed : 0.0);
    free(th);
    return 0;
}
