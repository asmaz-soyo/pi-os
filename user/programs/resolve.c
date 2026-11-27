// resolve <名前>:musl の getaddrinfo で名前を IPv4 アドレスに引き、1行に1つずつ出す。
// musl は /etc/resolv.conf の nameserver に UDP で問い合わせる。
#include <netdb.h>
#include <stdio.h>
#include <arpa/inet.h>
#include <netinet/in.h>
#include <sys/socket.h>

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: resolve NAME\n");
        return 2;
    }
    struct addrinfo hints = {.ai_family = AF_INET, .ai_socktype = SOCK_STREAM};
    struct addrinfo *res;
    int err = getaddrinfo(argv[1], NULL, &hints, &res);
    if (err != 0) {
        fprintf(stderr, "resolve: %s: %s\n", argv[1], gai_strerror(err));
        return 1;
    }
    for (struct addrinfo *p = res; p; p = p->ai_next) {
        char buf[INET_ADDRSTRLEN];
        struct sockaddr_in *sin = (struct sockaddr_in *)p->ai_addr;
        printf("%s\n", inet_ntop(AF_INET, &sin->sin_addr, buf, sizeof buf));
    }
    freeaddrinfo(res);
    return 0;
}
