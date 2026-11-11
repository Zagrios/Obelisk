// A local process fixture. Verifies the exec boundary without running a game.
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

int main(int argc, char **argv) {
    char cwd[4096];
    if (argc != 3 || strcmp(argv[1], "a b") || strcmp(argv[2], "$not-a-shell")) return 1;
    if (!getcwd(cwd, sizeof(cwd)) || strcmp(cwd, getenv("EXPECTED_CWD"))) return 2;
    if (strcmp(getenv("DYLD_INSERT_LIBRARIES"), getenv("EXPECTED_ADAPTER"))) return 3;
    if (strcmp(getenv("OBELISK_PRIVATE_STEAM_DISCOVERY"), "com.zagrios.obelisk.discovery.fixture")) return 4;
    // The environment is passed verbatim, including values that are not UTF-8.
    const char *bytes = getenv("OBELISK_FIXTURE_BYTES");
    if (!bytes || strcmp(bytes, "\xff\xfe")) return 8;
    // A game starts with the default SIGPIPE disposition, never an inherited ignore.
    struct sigaction pipe;
    if (sigaction(SIGPIPE, NULL, &pipe) || pipe.sa_handler != SIG_DFL) return 9;
    struct stat input;
    char byte;
    if (fstat(STDIN_FILENO, &input) || !S_ISCHR(input.st_mode) || read(STDIN_FILENO, &byte, 1) != 0) return 7;
    FILE *proof = fopen("game-proof.txt", "w");
    if (!proof) return 5;
    fprintf(proof, "%d\n", getpid());
    fclose(proof);
    for (int i = 0; i < 500; ++i) {
        if (!access("finish", F_OK)) return 23;
        usleep(10000);
    }
    return 6;
}
