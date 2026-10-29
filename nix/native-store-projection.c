/* Fixed guest-only projection of the already admitted, read-only input store. */
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
#include <sys/statvfs.h>
#include <unistd.h>

static int failure(const char *operation)
{
    perror(operation);
    return 78;
}

int main(int argc, char **argv)
{
    const char *source = "/run/loom-input-store";
    const char *target = "/nix/store";
    if (argc < 2)
        return 64;
    struct statvfs filesystem;
    if (statvfs(source, &filesystem) < 0)
        return failure("inspect admitted store");
    if (!(filesystem.f_flag & ST_RDONLY)) {
        fprintf(stderr, "admitted input store must be read-only\n");
        return 78;
    }
    DIR *directory = opendir(source);
    if (!directory)
        return failure("open admitted store");
    int destination = open(target, O_DIRECTORY | O_CLOEXEC | O_NOFOLLOW);
    if (destination < 0)
        return failure("open private store");
    unsigned int count = 0;
    for (;;) {
        errno = 0;
        struct dirent *entry = readdir(directory);
        if (!entry) {
            if (errno)
                return failure("read admitted store");
            break;
        }
        if (!strcmp(entry->d_name, ".") || !strcmp(entry->d_name, ".."))
            continue;
        struct stat object;
        if (fstatat(dirfd(directory), entry->d_name, &object, AT_SYMLINK_NOFOLLOW) < 0)
            return failure("inspect admitted object");
        if (S_ISDIR(object.st_mode)) {
            if (mkdirat(destination, entry->d_name, 0755) < 0)
                return failure("create private object directory");
        } else if (S_ISREG(object.st_mode)) {
            int file = openat(destination, entry->d_name,
                              O_CREAT | O_EXCL | O_WRONLY | O_NOFOLLOW | O_CLOEXEC, 0444);
            if (file < 0)
                return failure("create private object file");
            if (close(file) < 0)
                return failure("close private object file");
        } else {
            fprintf(stderr, "unsupported admitted top-level object: %s\n", entry->d_name);
            return 78;
        }
        char from[PATH_MAX], to[PATH_MAX];
        int from_size = snprintf(from, sizeof(from), "%s/%s", source, entry->d_name);
        int to_size = snprintf(to, sizeof(to), "%s/%s", target, entry->d_name);
        if (from_size < 0 || to_size < 0 || (size_t)from_size >= sizeof(from)
            || (size_t)to_size >= sizeof(to)) {
            fprintf(stderr, "admitted object path exceeds the fixed projection bound\n");
            return 78;
        }
        /* Bind inherits the source's read-only mount and filesystem. */
        if (mount(from, to, NULL, MS_BIND, NULL) < 0)
            return failure("bind admitted object");
        count++;
    }
    if (closedir(directory) < 0 || close(destination) < 0)
        return failure("close input projection");
    if (!count) {
        fprintf(stderr, "admitted input projection is empty\n");
        return 78;
    }
    execv(argv[1], argv + 1);
    return failure("exec projected guest command");
}
