/* Disposable outage injection: add restrictions to this process and its children. */
#define _GNU_SOURCE
#include <errno.h>
#include <sched.h>
#include <seccomp.h>
#include <stdio.h>
#include <unistd.h>

int main(int argc, char **argv)
{
    const unsigned long namespaces[] = {
        CLONE_NEWUSER, CLONE_NEWNS, CLONE_NEWNET, CLONE_NEWPID,
        CLONE_NEWUTS, CLONE_NEWIPC, CLONE_NEWCGROUP
    };
    if (argc < 2)
        return 64;
    scmp_filter_ctx filter = seccomp_init(SCMP_ACT_ALLOW);
    if (!filter)
        return 70;
    int result = seccomp_rule_add(filter, SCMP_ACT_ERRNO(EPERM), SCMP_SYS(unshare), 0);
    /* libc can use its ordinary clone fallback; namespace-bearing clone is denied. */
    if (result == 0)
        result = seccomp_rule_add(filter, SCMP_ACT_ERRNO(ENOSYS), SCMP_SYS(clone3), 0);
    for (unsigned int i = 0; result == 0 && i < sizeof(namespaces) / sizeof(namespaces[0]); i++)
        result = seccomp_rule_add(filter, SCMP_ACT_ERRNO(EPERM), SCMP_SYS(clone), 1,
                                 SCMP_A0(SCMP_CMP_MASKED_EQ, namespaces[i], namespaces[i]));
    if (result == 0)
        result = seccomp_load(filter);
    seccomp_release(filter);
    if (result != 0) {
        fprintf(stderr, "cannot install namespace outage restriction: %d\n", result);
        return 70;
    }
    execv(argv[1], argv + 1);
    perror("exec namespace-restricted verifier");
    return 127;
}
