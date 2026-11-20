// ディレクトリの中身を1行に1つずつ出す(. と .. は出さない)
#include <dirent.h>
#include <stdio.h>
#include <string.h>

int main(int argc, char **argv) {
    const char *path = argc > 1 ? argv[1] : ".";
    DIR *d = opendir(path);
    if (!d) {
        perror(path);
        return 1;
    }
    for (struct dirent *e; (e = readdir(d));) {
        if (strcmp(e->d_name, ".") && strcmp(e->d_name, "..")) puts(e->d_name);
    }
    closedir(d);
    return 0;
}
