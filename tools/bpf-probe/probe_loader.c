// Minimal libbpf loader for the falsification probe.
// Usage:
//   probe_loader <obj> load-only              -> open+load, no attach (report errors verbatim)
//   probe_loader <obj> test [N]               -> load, attach all tp programs, exec /bin/true N
//                                               times, dump exec_count map, detach on exit
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/wait.h>
#include <unistd.h>
#include <bpf/bpf.h>
#include <bpf/libbpf.h>

static int libbpf_print_fn(enum libbpf_print_level level, const char *format,
			   va_list args)
{
	return vfprintf(stderr, format, args);
}

static int do_execs(int n)
{
	int i, ok = 0;

	for (i = 0; i < n; i++) {
		pid_t pid = fork();
		if (pid == 0) {
			char *argv[] = { "/bin/true", NULL };
			execve("/bin/true", argv, NULL);
			_exit(127);
		}
		if (pid > 0) {
			int st;
			waitpid(pid, &st, 0);
			if (WIFEXITED(st) && WEXITSTATUS(st) == 0)
				ok++;
		}
	}
	return ok;
}

int main(int argc, char **argv)
{
	struct bpf_object *obj = NULL;
	struct bpf_program *prog;
	struct bpf_link *links[8] = { 0 };
	int nlinks = 0, err = 0, i;
	const char *mode;

	if (argc < 3) {
		fprintf(stderr, "usage: %s <obj> {load-only|test} [N]\n", argv[0]);
		return 2;
	}
	mode = argv[2];

	libbpf_set_print(libbpf_print_fn);

	obj = bpf_object__open_file(argv[1], NULL);
	if (!obj) {
		err = -errno;
		fprintf(stderr, "OPEN-FAIL errno=%d (%s)\n", errno, strerror(errno));
		return 1;
	}
	fprintf(stderr, "OPEN-OK\n");

	err = bpf_object__load(obj);
	if (err) {
		fprintf(stderr, "LOAD-FAIL err=%d (%s)\n", err, strerror(-err));
		bpf_object__close(obj);
		return 1;
	}
	fprintf(stderr, "LOAD-OK\n");

	if (strcmp(mode, "load-only") == 0) {
		bpf_object__close(obj);
		fprintf(stderr, "RESULT: load-only OK\n");
		return 0;
	}

	bpf_object__for_each_program(prog, obj) {
		struct bpf_link *l = bpf_program__attach(prog);
		if (!l) {
			err = -errno;
			fprintf(stderr, "ATTACH-FAIL prog=%s errno=%d (%s)\n",
				bpf_program__name(prog), errno, strerror(errno));
			goto cleanup;
		}
		fprintf(stderr, "ATTACH-OK prog=%s\n", bpf_program__name(prog));
		if (nlinks < 8)
			links[nlinks++] = l;
	}

	{
		int n = (argc > 3) ? atoi(argv[3]) : 100;
		int ok = do_execs(n);
		fprintf(stderr, "EXECS-DONE ok=%d requested=%d\n", ok, n);
	}

	{
		struct bpf_map *map = bpf_object__find_map_by_name(obj, "exec_count");
		if (!map) {
			fprintf(stderr, "MAP-NOT-FOUND\n");
		} else {
			__u32 key = 0;
			__u64 total = 0;
			int ncpu = libbpf_num_possible_cpus();
			__u64 *vals = calloc(ncpu, sizeof(__u64));

			if (vals) {
				if (bpf_map_lookup_elem(bpf_map__fd(map), &key, vals) == 0) {
					for (i = 0; i < ncpu; i++)
						total += vals[i];
				} else {
					fprintf(stderr, "MAP-LOOKUP-FAIL errno=%d (%s)\n",
						errno, strerror(errno));
				}
				free(vals);
			}
			fprintf(stderr, "MAP exec_count TOTAL=%llu\n",
				(unsigned long long)total);
		}
	}

cleanup:
	for (i = 0; i < nlinks; i++)
		bpf_link__destroy(links[i]);
	bpf_object__close(obj);
	fprintf(stderr, "CLEANED-UP\n");
	return err ? 1 : 0;
}
