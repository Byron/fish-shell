/* Test-only driver for unchanged GNU Bash 3.2 lib/sh/strtrans.c.
 * The normal Fish and Rash builds do not compile or link this driver. */
#include <locale.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

extern char *ansicstr(char *, int, int, int *, int *);
extern char *ansic_quote(char *, int, int *);
extern int ansic_shouldquote(const char *);
extern char *ansiexpand(char *, int, int, int *);

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
    if (argc != 2 && argc != 4) return 2;
    if (!setlocale(LC_ALL, "C")) return 2;
    while ((length = getdelim(&input, &capacity, '\0', stdin)) != -1) {
        char *result;
        int size = 0, saw_c = 0, i;
        if (input[length - 1] != '\0') return 2;
        if (strcmp(argv[1], "decode") == 0 && argc == 4) {
            result = ansicstr(input, length - 1, atoi(argv[2]),
                             atoi(argv[3]) ? &saw_c : NULL, &size);
        } else if (strcmp(argv[1], "quote") == 0) {
            result = ansic_quote(input, 0, &size);
        } else if (strcmp(argv[1], "should-quote") == 0) {
            printf("0:%02x\n", ansic_shouldquote(input));
            continue;
        } else if (strcmp(argv[1], "expand") == 0) {
            result = ansiexpand(input, 0, length - 1, &size);
        } else if (strcmp(argv[1], "expand-inner") == 0) {
            int start = length > 2 ? 1 : 0;
            int end = length > 2 ? length - 2 : 0;
            result = ansiexpand(input, start, end, &size);
        } else return 2;
        if (!result) {
            puts("null");
            continue;
        }
        printf("%d:", saw_c);
        for (i = 0; i < size; i++) printf("%02x", (unsigned char)result[i]);
        putchar('\n');
        free(result);
    }
    free(input);
    return ferror(stdin) || fflush(stdout) != 0 ? 2 : 0;
}
