/* In front of gcc and g++: finds the real compiler, adds the flags every compile gets, and, when
 * linking, adds the interpreter and library paths a program needs to run from the store. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static int links(int argc, char **argv) {
    for (int i = 1; i < argc; i++) {
        const char *a = argv[i];
        if (!strcmp(a, "-c") || !strcmp(a, "-S") || !strcmp(a, "-E") || !strcmp(a, "-M") ||
            !strcmp(a, "-MM") || !strcmp(a, "--version") || !strcmp(a, "-v") ||
            !strncmp(a, "-print-", 7) || !strncmp(a, "-dump", 5))
            return 0;
    }
    return 1;
}

/* Appends the space-separated flags in `flags` to `args` from `n`, and gives the new count. */
static int add(char **args, int n, char *flags) {
    for (char *flag = strtok(flags, " "); flag; flag = strtok(NULL, " "))
        args[n++] = flag;
    return n;
}

int main(int argc, char **argv) {
    const char *name = strrchr(argv[0], '/');
    name = name ? name + 1 : argv[0];
    if (!strcmp(name, "cc"))
        name = "gcc";
    else if (!strcmp(name, "c++"))
        name = "g++";

    const char *bin = getenv("LAB_REAL_BIN");
    if (!bin) {
        fprintf(stderr, "lab-cc: LAB_REAL_BIN is not set\n");
        return 127;
    }
    char real[4096];
    snprintf(real, sizeof real, "%s/%s", bin, name);

    const char *always = getenv("LAB_COMPILE_FLAGS");
    const char *linking = getenv("LAB_LINK_FLAGS");
    char *compile = strdup(always ? always : "");
    char *link = strdup(linking && links(argc, argv) ? linking : "");
    char **args = calloc(argc + (strlen(compile) + strlen(link)) / 2 + 3, sizeof *args);
    int n = 0;
    args[n++] = real;
    n = add(args, n, compile);
    n = add(args, n, link);
    for (int i = 1; i < argc; i++)
        args[n++] = argv[i];
    args[n] = NULL;
    execv(real, args);
    perror(real);
    return 127;
}
