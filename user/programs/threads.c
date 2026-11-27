// threads:musl の pthread を使う(スレッド、ミューテックス、条件変数、join、スレッドごとの変数)。
// どれも、争いがあるときだけカーネルの futex を使う。
#include <pthread.h>
#include <stdio.h>

#define ADDERS 4
#define ROUNDS 20000
#define ITEMS 200

static pthread_mutex_t lock = PTHREAD_MUTEX_INITIALIZER;
static long counter;
static __thread int mine; // スレッドごとに別の変数(TLS)

static void *adder(void *arg) {
    int id = (int)(long)arg;
    mine = id * 100;
    for (int i = 0; i < ROUNDS; i++) {
        pthread_mutex_lock(&lock);
        counter++;
        pthread_mutex_unlock(&lock);
    }
    // ほかのスレッドが mine を書いても、自分のものは変わらない
    return (void *)(long)(mine == id * 100 ? id + 1 : -1);
}

// 作る側と使う側:16 個の箱を条件変数で譲り合う
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static int box[16], head, tail, count;
static long sum;

static void *producer(void *arg) {
    (void)arg;
    for (int i = 1; i <= ITEMS; i++) {
        pthread_mutex_lock(&lock);
        while (count == 16)
            pthread_cond_wait(&changed, &lock);
        box[tail] = i;
        tail = (tail + 1) % 16;
        count++;
        pthread_cond_broadcast(&changed);
        pthread_mutex_unlock(&lock);
    }
    return 0;
}

static void *consumer(void *arg) {
    (void)arg;
    for (int i = 0; i < ITEMS; i++) {
        pthread_mutex_lock(&lock);
        while (count == 0)
            pthread_cond_wait(&changed, &lock);
        sum += box[head];
        head = (head + 1) % 16;
        count--;
        pthread_cond_broadcast(&changed);
        pthread_mutex_unlock(&lock);
    }
    return 0;
}

int main(void) {
    mine = -7;
    pthread_t t[ADDERS];
    for (long i = 0; i < ADDERS; i++)
        if (pthread_create(&t[i], NULL, adder, (void *)i) != 0) {
            perror("threads: pthread_create");
            return 1;
        }
    int joined = 0;
    for (long i = 0; i < ADDERS; i++) {
        void *ret;
        pthread_join(t[i], &ret);
        joined += (long)ret == i + 1;
    }
    printf("mutex: counter = %ld (want %d)\n", counter, ADDERS * ROUNDS);
    printf("join: %d of %d returned their value, tls kept = %d\n", joined, ADDERS, mine == -7);

    pthread_t p, c;
    pthread_create(&c, NULL, consumer, NULL);
    pthread_create(&p, NULL, producer, NULL);
    pthread_join(p, NULL);
    pthread_join(c, NULL);
    printf("cond: sum = %ld (want %d)\n", sum, ITEMS * (ITEMS + 1) / 2);

    int ok = counter == ADDERS * ROUNDS && joined == ADDERS && mine == -7 &&
             sum == ITEMS * (ITEMS + 1) / 2;
    return ok ? 0 : 1;
}
