#include <stdlib.h>

/* A counter. */
struct counter {
    int n;
};

int bump(struct counter *c) {
    c->n += 1;
    return c->n;
}
