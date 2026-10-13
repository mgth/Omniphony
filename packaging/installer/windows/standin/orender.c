/* A stand-in for orender.exe in the installer's tests (test.ps1): it answers
 * `--version` and `check-bridges` as the real one does, without an engine.
 * `check-bridges` reports a bridge when a *bridge*.dll sits in the per-user
 * engine folder, and fails without one. */
#include <stdio.h>
#include <string.h>
#include <windows.h>

int main(int argc, char **argv)
{
    if (argc > 1 && !strcmp(argv[1], "--version")) {
        puts("omniphony-renderer stand-in");
        puts("liborender ABI: 0.12");
        puts("bridge_api: 0.6.x");
        return 0;
    }
    if (argc > 1 && !strcmp(argv[1], "check-bridges")) {
        char pattern[MAX_PATH];
        WIN32_FIND_DATAA found;
        HANDLE search;
        puts("host: bridge_api 0.6.x");
        snprintf(pattern, sizeof pattern, "%s\\omniphony\\lib\\*bridge*.dll",
                 getenv("LOCALAPPDATA"));
        search = FindFirstFileA(pattern, &found);
        if (search == INVALID_HANDLE_VALUE) {
            fputs("Error: no decoder bridge found\n", stderr);
            return 1;
        }
        printf("loaded: %s (test)\n", found.cFileName);
        FindClose(search);
        return 0;
    }
    puts("orender stand-in");
    return 0;
}
