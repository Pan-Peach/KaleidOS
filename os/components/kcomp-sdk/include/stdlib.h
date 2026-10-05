/* Freestanding allocation only. Implementation is private to each .kcomp. */
#ifndef KCOMP_STDLIB_H
#define KCOMP_STDLIB_H
#include <stddef.h>
#ifdef __cplusplus
extern "C" {
#endif
void *malloc(size_t size);
void free(void *ptr);
void *calloc(size_t count, size_t size);
void *realloc(void *ptr, size_t size);
#ifdef __cplusplus
}
#endif
#endif
