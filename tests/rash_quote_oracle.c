/* Test-only driver for the unchanged GNU Bash 3.2 lib/sh/shquote.c.
 * Compile explicitly with build_tools/rash/build-quote-oracle.py.
 * No C code from this driver is linked into Fish or Rash. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern char *sh_single_quote(char *);
extern char *sh_double_quote(char *);
extern char *sh_mkdoublequoted(const char *, int, int);
extern char *sh_un_double_quote(char *);
extern char *sh_backslash_quote(char *);
extern char *sh_backslash_quote_for_double_quotes(char *);
extern int sh_contains_shell_metas(char *);

void *xmalloc(size_t size) {
    void *result = malloc(size);
    if (!result) {
        perror("malloc");
        exit(2);
    }
    return result;
}

int main(int argc, char **argv) {
    char *input = NULL;
    size_t capacity = 0;
    ssize_t length;
    if (argc != 2) return 2;
    while ((length = getdelim(&input, &capacity, '\0', stdin)) != -1) {
        char *result;
        if (input[length - 1] != '\0') return 2;
        if (strcmp(argv[1], "single") == 0) result = sh_single_quote(input);
        else if (strcmp(argv[1], "double") == 0) result = sh_double_quote(input);
        /* 3.2 underallocates for all-quote inputs when flags=1. This argument
         * controls allocation only; supply room for both quotes and NUL. */
        else if (strcmp(argv[1], "make-double") == 0) result = sh_mkdoublequoted(input, length + 1, 0);
        else if (strcmp(argv[1], "make-double-escaped") == 0) result = sh_mkdoublequoted(input, length + 1, 1);
        else if (strcmp(argv[1], "un-double") == 0) result = sh_un_double_quote(input);
        else if (strcmp(argv[1], "backslash") == 0) result = sh_backslash_quote(input);
        else if (strcmp(argv[1], "prompt") == 0) result = sh_backslash_quote_for_double_quotes(input);
        else if (strcmp(argv[1], "metas") == 0) {
            if (fwrite(sh_contains_shell_metas(input) ? "1" : "0", 1, 2, stdout) != 2) return 2;
            continue;
        } else return 2;
        size_t size = strlen(result) + 1;
        if (fwrite(result, 1, size, stdout) != size) return 2;
        free(result);
    }
    free(input);
    return ferror(stdin) || fflush(stdout) != 0 ? 2 : 0;
}
