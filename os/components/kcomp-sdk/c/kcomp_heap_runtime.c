/* Deployment adapter. State belongs to this image, never to Core's registry.
 * KernelNative uses the supplied shared-heap C ABI; private domains grow their
 * own heap through memory_acquire. Private allocation is not IRQ-safe. */
#include "kcomp.h"
#include "kcomp_kalloc.h"

typedef void *(*alloc_fn)(size_t, size_t);
typedef int32_t (*free_fn)(void *, size_t, size_t);
static alloc_fn shared_alloc;
static free_fn shared_free;
static void *private_heap;
static unsigned initialized;
static unsigned heap_lock;

int32_t kcomp_runtime_init(const struct kcomp_runtime *runtime) {
    if (!runtime || runtime->reserved != 0) return -22;
    if (initialized) return -16;
    if (runtime->domain == 0) {
        if (!runtime->heap_alloc || !runtime->heap_dealloc) return -22;
        shared_alloc = (alloc_fn)runtime->heap_alloc;
        shared_free = (free_fn)runtime->heap_dealloc;
    } else if (runtime->domain == 1 || runtime->domain == 2) {
        if (runtime->heap_alloc || runtime->heap_dealloc) return -22;
    } else {
        return -95;
    }
    initialized = 1;
    return 0;
}

static int backing(size_t min_len, size_t min_align,
                   uintptr_t *out_base, size_t *out_len) {
    struct kcore_memory_view view;
    int32_t result = kcore_memory_acquire(min_len, min_align, &view);
    if (result != 0) return result;
    if (view.kind != KCORE_MEMORY_VIEW_LOCAL_VA || view.reserved != 0 ||
        view.base == 0 || view.base > UINTPTR_MAX || view.len > SIZE_MAX ||
        view.len < min_len) return -14;
    *out_base = (uintptr_t)view.base;
    *out_len = (size_t)view.len;
    return 0;
}

static void lock_heap(void) {
    while (__atomic_exchange_n(&heap_lock, 1, __ATOMIC_ACQUIRE)) {}
}
static void unlock_heap(void) {
    __atomic_store_n(&heap_lock, 0, __ATOMIC_RELEASE);
}

void *kcomp_runtime_alloc(size_t size, size_t align) {
    if (!initialized || size == 0 || align == 0 || (align & (align - 1)))
        return (void *)0;
    if (shared_alloc) return shared_alloc(size, align);
    lock_heap();
    if (!private_heap) {
        uintptr_t base = 0;
        size_t len = 0;
        if (backing(4096, sizeof(void *), &base, &len) == 0)
            private_heap = kcomp_heap_place((void *)base, len, backing);
    }
    void *ptr = kcomp_heap_alloc(private_heap, size, align);
    unlock_heap();
    return ptr;
}

void kcomp_runtime_free(void *ptr, size_t size, size_t align) {
    if (!initialized || !ptr) return;
    if (shared_free) {
        (void)shared_free(ptr, size, align);
        return;
    }
    lock_heap();
    kcomp_heap_free(private_heap, ptr);
    unlock_heap();
}

#ifndef KCOMP_HOST_TEST
/* The C front end uses the same deployment adapter. Keep the original layout
 * in an aligned header, so shared-heap free receives exactly malloc's layout.
 * Host tests keep libc's allocator; these names never interpose on std. */
typedef union {
    max_align_t alignment;
    size_t size;
} malloc_header;

__attribute__((weak)) void *malloc(size_t size) {
    if (size == 0 || size > SIZE_MAX - sizeof(malloc_header)) return (void *)0;
    malloc_header *header = kcomp_runtime_alloc(size + sizeof(*header), _Alignof(malloc_header));
    if (!header) return (void *)0;
    header->size = size;
    return header + 1;
}

__attribute__((weak)) void free(void *ptr) {
    if (!ptr) return;
    malloc_header *header = (malloc_header *)ptr - 1;
    kcomp_runtime_free(header, header->size + sizeof(*header), _Alignof(malloc_header));
}

__attribute__((weak)) void *calloc(size_t count, size_t size) {
    if (size && count > SIZE_MAX / size) return (void *)0;
    size_t len = count * size;
    unsigned char *ptr = malloc(len);
    if (ptr) for (size_t i = 0; i < len; ++i) ptr[i] = 0;
    return ptr;
}

__attribute__((weak)) void *realloc(void *ptr, size_t size) {
    if (!ptr) return malloc(size);
    if (!size) { free(ptr); return (void *)0; }
    unsigned char *fresh = malloc(size);
    if (!fresh) return (void *)0;
    size_t old = ((malloc_header *)ptr - 1)->size;
    size_t len = old < size ? old : size;
    for (size_t i = 0; i < len; ++i) fresh[i] = ((unsigned char *)ptr)[i];
    free(ptr);
    return fresh;
}
#endif

#ifdef KCOMP_HOST_TEST
/* Host tests serialize this image's runtime. Never exported in a .kcomp. */
void kcomp_runtime_reset_for_test(void) {
    shared_alloc = (alloc_fn)0;
    shared_free = (free_fn)0;
    private_heap = (void *)0;
    initialized = 0;
    heap_lock = 0;
}
#endif
