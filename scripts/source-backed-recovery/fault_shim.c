#define _GNU_SOURCE

#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <signal.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

static _Atomic int matched_calls;
static _Atomic int seen_manifest_rename;
static _Atomic int seen_generation_meta_rename;
static _Atomic int seen_pointer_rename;
static _Thread_local int inside_hook;

static void *required_symbol(const char *name) {
    void *symbol = dlsym(RTLD_NEXT, name);
    if (symbol == NULL) {
        static const char prefix[] = "ctx fault shim could not resolve symbol\n";
        syscall(SYS_write, STDERR_FILENO, prefix, sizeof(prefix) - 1);
        syscall(SYS_exit_group, 127);
    }
    return symbol;
}

static bool env_equals(const char *name, const char *expected) {
    const char *actual = getenv(name);
    return actual != NULL && strcmp(actual, expected) == 0;
}

static bool starts_with(const char *value, const char *prefix) {
    return strncmp(value, prefix, strlen(prefix)) == 0;
}

static bool ends_with(const char *value, const char *suffix) {
    size_t value_length = strlen(value);
    size_t suffix_length = strlen(suffix);
    return value_length >= suffix_length
        && strcmp(value + value_length - suffix_length, suffix) == 0;
}

static bool join_path(char *output, size_t capacity, const char *left, const char *right) {
    int length = snprintf(output, capacity, "%s/%s", left, right);
    return length > 0 && (size_t)length < capacity;
}

static bool resolve_fd_path(int fd, char *output, size_t capacity) {
    char proc_path[64];
    int length = snprintf(proc_path, sizeof(proc_path), "/proc/self/fd/%d", fd);
    if (length <= 0 || (size_t)length >= sizeof(proc_path)) {
        return false;
    }
    ssize_t read = readlink(proc_path, output, capacity - 1);
    if (read < 0 || (size_t)read >= capacity) {
        return false;
    }
    output[read] = '\0';
    return true;
}

static bool resolve_at_path(
    int directory_fd,
    const char *path,
    char *output,
    size_t capacity
) {
    if (path == NULL) {
        return false;
    }
    if (path[0] == '/') {
        int length = snprintf(output, capacity, "%s", path);
        return length > 0 && (size_t)length < capacity;
    }
    char directory[PATH_MAX];
    if (directory_fd == AT_FDCWD) {
        if (getcwd(directory, sizeof(directory)) == NULL) {
            return false;
        }
    } else if (!resolve_fd_path(directory_fd, directory, sizeof(directory))) {
        return false;
    }
    return join_path(output, capacity, directory, path);
}

static bool root_child_path(const char *path, const char *root, const char **basename) {
    size_t root_length = strlen(root);
    if (!starts_with(path, root) || path[root_length] != '/') {
        return false;
    }
    const char *relative = path + root_length + 1;
    if (strchr(relative, '/') != NULL) {
        return false;
    }
    *basename = relative;
    return true;
}

static bool generation_child_path(const char *path, const char *root, const char **basename) {
    char generations_directory[PATH_MAX];
    if (!join_path(
            generations_directory,
            sizeof(generations_directory),
            root,
            "index-generations"
        )) {
        return false;
    }
    size_t directory_length = strlen(generations_directory);
    if (!starts_with(path, generations_directory) || path[directory_length] != '/') {
        return false;
    }
    const char *relative = path + directory_length + 1;
    const char *separator = strchr(relative, '/');
    if (!starts_with(relative, "generation-") || separator == NULL) {
        return false;
    }
    const char *child = separator + 1;
    if (child[0] == '\0' || strchr(child, '/') != NULL) {
        return false;
    }
    *basename = child;
    return true;
}

static bool generation_directory_path(const char *path, const char *root) {
    char generations_directory[PATH_MAX];
    if (!join_path(
            generations_directory,
            sizeof(generations_directory),
            root,
            "index-generations"
        )) {
        return false;
    }
    size_t directory_length = strlen(generations_directory);
    if (!starts_with(path, generations_directory) || path[directory_length] != '/') {
        return false;
    }
    const char *relative = path + directory_length + 1;
    return starts_with(relative, "generation-") && strchr(relative, '/') == NULL;
}

static bool target_matches(const char *path) {
    const char *root = getenv("CTX_RECOVERY_FAULT_ROOT");
    const char *target = getenv("CTX_RECOVERY_FAULT_TARGET");
    if (root == NULL || target == NULL || path == NULL) {
        return false;
    }

    char manifest_directory[PATH_MAX];
    if (!join_path(
            manifest_directory,
            sizeof(manifest_directory),
            root,
            "ctx-generations"
        )) {
        return false;
    }
    char pointer_path[PATH_MAX];
    if (!join_path(
            pointer_path,
            sizeof(pointer_path),
            root,
            "active-generation.json"
        )) {
        return false;
    }

    if (strcmp(target, "manifest_dir") == 0) {
        return strcmp(path, manifest_directory) == 0;
    }
    if (strcmp(target, "root_dir") == 0) {
        return strcmp(path, root) == 0;
    }
    if (strcmp(target, "generation_dir") == 0) {
        return generation_directory_path(path, root);
    }
    if (strcmp(target, "generation_meta_final") == 0) {
        const char *basename = NULL;
        return generation_child_path(path, root, &basename)
            && strcmp(basename, "meta.json") == 0;
    }
    if (strcmp(target, "pointer_final") == 0) {
        return strcmp(path, pointer_path) == 0;
    }
    if (strcmp(target, "manifest_final") == 0) {
        size_t directory_length = strlen(manifest_directory);
        return starts_with(path, manifest_directory)
            && path[directory_length] == '/'
            && ends_with(path, ".json")
            && strstr(path, "/.ctx-tantivy-atomic-") == NULL;
    }
    if (strcmp(target, "manifest_temp") == 0) {
        char prefix[PATH_MAX];
        if (!join_path(
                prefix,
                sizeof(prefix),
                manifest_directory,
                ".ctx-tantivy-atomic-"
            )) {
            return false;
        }
        return starts_with(path, prefix);
    }
    if (strcmp(target, "generation_temp") == 0) {
        const char *basename = NULL;
        return generation_child_path(path, root, &basename)
            && starts_with(basename, ".ctx-tantivy-atomic-");
    }
    if (strcmp(target, "pointer_temp") == 0) {
        const char *basename = NULL;
        return root_child_path(path, root, &basename)
            && starts_with(basename, ".ctx-tantivy-atomic-");
    }
    if (strcmp(target, "index_data") == 0) {
        const char *basename = NULL;
        if (!generation_child_path(path, root, &basename)) {
            return false;
        }
        static const char *extensions[] = {
            ".del", ".fast", ".fieldnorm", ".idx", ".pos", ".store", ".term"
        };
        for (size_t index = 0; index < sizeof(extensions) / sizeof(extensions[0]); index++) {
            if (ends_with(basename, extensions[index])) {
                return true;
            }
        }
    }
    return false;
}

static bool arm_is_ready(void) {
    const char *arm = getenv("CTX_RECOVERY_FAULT_ARM_AFTER");
    if (arm == NULL || arm[0] == '\0') {
        return true;
    }
    if (strcmp(arm, "manifest_rename") == 0) {
        return atomic_load(&seen_manifest_rename) != 0;
    }
    if (strcmp(arm, "generation_meta_rename") == 0) {
        return atomic_load(&seen_generation_meta_rename) != 0;
    }
    if (strcmp(arm, "pointer_rename") == 0) {
        return atomic_load(&seen_pointer_rename) != 0;
    }
    return false;
}

static bool should_trigger(const char *operation, const char *path, const char *timing) {
    if (!env_equals("CTX_RECOVERY_FAULT_OP", operation)
        || !env_equals("CTX_RECOVERY_FAULT_TIMING", timing)
        || !arm_is_ready()
        || !target_matches(path)) {
        return false;
    }
    int occurrence = 1;
    const char *configured = getenv("CTX_RECOVERY_FAULT_OCCURRENCE");
    if (configured != NULL) {
        int parsed = atoi(configured);
        if (parsed > 0) {
            occurrence = parsed;
        }
    }
    return atomic_fetch_add(&matched_calls, 1) + 1 == occurrence;
}

static void write_marker(void) {
    const char *marker = getenv("CTX_RECOVERY_FAULT_MARKER");
    if (marker == NULL) {
        return;
    }
    int fd = (int)syscall(
        SYS_openat,
        AT_FDCWD,
        marker,
        O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC,
        0600
    );
    if (fd < 0) {
        return;
    }
    static const char reached[] = "fault-reached\n";
    syscall(SYS_write, fd, reached, sizeof(reached) - 1);
    syscall(SYS_fsync, fd);
    syscall(SYS_close, fd);
}

static int configured_errno(void) {
    const char *name = getenv("CTX_RECOVERY_FAULT_ERRNO");
    if (name != NULL && strcmp(name, "EIO") == 0) {
        return EIO;
    }
    if (name != NULL && strcmp(name, "EACCES") == 0) {
        return EACCES;
    }
    return ENOSPC;
}

static bool perform_action(void) {
    if (env_equals("CTX_RECOVERY_FAULT_ACTION", "stop")) {
        write_marker();
        // Target the calling thread so the stop is delivered at this syscall
        // boundary instead of racing publication on another Tantivy thread.
        syscall(SYS_tgkill, getpid(), syscall(SYS_gettid), SIGSTOP);
        return false;
    }
    if (env_equals("CTX_RECOVERY_FAULT_ACTION", "fail")) {
        errno = configured_errno();
        return true;
    }
    return false;
}

static void record_successful_rename(const char *target_path) {
    const char *root = getenv("CTX_RECOVERY_FAULT_ROOT");
    if (root == NULL || target_path == NULL) {
        return;
    }
    char pointer_path[PATH_MAX];
    char manifest_directory[PATH_MAX];
    if (join_path(
            pointer_path,
            sizeof(pointer_path),
            root,
            "active-generation.json"
        )
        && strcmp(target_path, pointer_path) == 0) {
        atomic_store(&seen_pointer_rename, 1);
    }
    const char *generation_basename = NULL;
    if (generation_child_path(target_path, root, &generation_basename)
        && strcmp(generation_basename, "meta.json") == 0) {
        atomic_store(&seen_generation_meta_rename, 1);
    }
    if (join_path(
            manifest_directory,
            sizeof(manifest_directory),
            root,
            "ctx-generations"
        )) {
        size_t directory_length = strlen(manifest_directory);
        if (starts_with(target_path, manifest_directory)
            && target_path[directory_length] == '/'
            && ends_with(target_path, ".json")) {
            atomic_store(&seen_manifest_rename, 1);
        }
    }
}

ssize_t write(int fd, const void *buffer, size_t count) {
    typedef ssize_t (*function_type)(int, const void *, size_t);
    static function_type real_function;
    if (real_function == NULL) {
        real_function = (function_type)required_symbol("write");
    }
    if (inside_hook) {
        return real_function(fd, buffer, count);
    }
    char path[PATH_MAX];
    bool have_path = resolve_fd_path(fd, path, sizeof(path));
    inside_hook = 1;
    if (have_path && should_trigger("write", path, "before") && perform_action()) {
        inside_hook = 0;
        return -1;
    }
    ssize_t result = real_function(fd, buffer, count);
    if (result >= 0 && have_path && should_trigger("write", path, "after")) {
        (void)perform_action();
    }
    inside_hook = 0;
    return result;
}

ssize_t pwrite(int fd, const void *buffer, size_t count, off_t offset) {
    typedef ssize_t (*function_type)(int, const void *, size_t, off_t);
    static function_type real_function;
    if (real_function == NULL) {
        real_function = (function_type)required_symbol("pwrite");
    }
    if (inside_hook) {
        return real_function(fd, buffer, count, offset);
    }
    char path[PATH_MAX];
    bool have_path = resolve_fd_path(fd, path, sizeof(path));
    inside_hook = 1;
    if (have_path && should_trigger("write", path, "before") && perform_action()) {
        inside_hook = 0;
        return -1;
    }
    ssize_t result = real_function(fd, buffer, count, offset);
    if (result >= 0 && have_path && should_trigger("write", path, "after")) {
        (void)perform_action();
    }
    inside_hook = 0;
    return result;
}

ssize_t writev(int fd, const struct iovec *iov, int iov_count) {
    typedef ssize_t (*function_type)(int, const struct iovec *, int);
    static function_type real_function;
    if (real_function == NULL) {
        real_function = (function_type)required_symbol("writev");
    }
    if (inside_hook) {
        return real_function(fd, iov, iov_count);
    }
    char path[PATH_MAX];
    bool have_path = resolve_fd_path(fd, path, sizeof(path));
    inside_hook = 1;
    if (have_path && should_trigger("write", path, "before") && perform_action()) {
        inside_hook = 0;
        return -1;
    }
    ssize_t result = real_function(fd, iov, iov_count);
    if (result >= 0 && have_path && should_trigger("write", path, "after")) {
        (void)perform_action();
    }
    inside_hook = 0;
    return result;
}

static int sync_hook(int fd, const char *symbol_name) {
    typedef int (*function_type)(int);
    static function_type real_fsync;
    static function_type real_fdatasync;
    function_type *slot = strcmp(symbol_name, "fsync") == 0
        ? &real_fsync
        : &real_fdatasync;
    if (*slot == NULL) {
        *slot = (function_type)required_symbol(symbol_name);
    }
    if (inside_hook) {
        return (*slot)(fd);
    }
    char path[PATH_MAX];
    bool have_path = resolve_fd_path(fd, path, sizeof(path));
    inside_hook = 1;
    if (have_path && should_trigger("sync", path, "before") && perform_action()) {
        inside_hook = 0;
        return -1;
    }
    int result = (*slot)(fd);
    if (result == 0 && have_path && should_trigger("sync", path, "after")) {
        (void)perform_action();
    }
    inside_hook = 0;
    return result;
}

int fsync(int fd) {
    return sync_hook(fd, "fsync");
}

int fdatasync(int fd) {
    return sync_hook(fd, "fdatasync");
}

int rename(const char *old_path, const char *new_path) {
    typedef int (*function_type)(const char *, const char *);
    static function_type real_function;
    if (real_function == NULL) {
        real_function = (function_type)required_symbol("rename");
    }
    if (inside_hook) {
        return real_function(old_path, new_path);
    }
    inside_hook = 1;
    if (should_trigger("rename", new_path, "before") && perform_action()) {
        inside_hook = 0;
        return -1;
    }
    int result = real_function(old_path, new_path);
    if (result == 0) {
        record_successful_rename(new_path);
        if (should_trigger("rename", new_path, "after")) {
            (void)perform_action();
        }
    }
    inside_hook = 0;
    return result;
}

int renameat(
    int old_directory_fd,
    const char *old_path,
    int new_directory_fd,
    const char *new_path
) {
    typedef int (*function_type)(int, const char *, int, const char *);
    static function_type real_function;
    if (real_function == NULL) {
        real_function = (function_type)required_symbol("renameat");
    }
    if (inside_hook) {
        return real_function(old_directory_fd, old_path, new_directory_fd, new_path);
    }
    char resolved_new[PATH_MAX];
    bool have_path = resolve_at_path(
        new_directory_fd,
        new_path,
        resolved_new,
        sizeof(resolved_new)
    );
    inside_hook = 1;
    if (have_path
        && should_trigger("rename", resolved_new, "before")
        && perform_action()) {
        inside_hook = 0;
        return -1;
    }
    int result = real_function(old_directory_fd, old_path, new_directory_fd, new_path);
    if (result == 0 && have_path) {
        record_successful_rename(resolved_new);
        if (should_trigger("rename", resolved_new, "after")) {
            (void)perform_action();
        }
    }
    inside_hook = 0;
    return result;
}

int renameat2(
    int old_directory_fd,
    const char *old_path,
    int new_directory_fd,
    const char *new_path,
    unsigned int flags
) {
    typedef int (*function_type)(int, const char *, int, const char *, unsigned int);
    static function_type real_function;
    if (real_function == NULL) {
        real_function = (function_type)required_symbol("renameat2");
    }
    if (inside_hook) {
        return real_function(
            old_directory_fd,
            old_path,
            new_directory_fd,
            new_path,
            flags
        );
    }
    char resolved_new[PATH_MAX];
    bool have_path = resolve_at_path(
        new_directory_fd,
        new_path,
        resolved_new,
        sizeof(resolved_new)
    );
    inside_hook = 1;
    if (have_path
        && should_trigger("rename", resolved_new, "before")
        && perform_action()) {
        inside_hook = 0;
        return -1;
    }
    int result = real_function(
        old_directory_fd,
        old_path,
        new_directory_fd,
        new_path,
        flags
    );
    if (result == 0 && have_path) {
        record_successful_rename(resolved_new);
        if (should_trigger("rename", resolved_new, "after")) {
            (void)perform_action();
        }
    }
    inside_hook = 0;
    return result;
}
