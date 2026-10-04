/* Minimal libc-test harness surface.
 * T_LOC2/T_LOC1/t_error and declarations are derived from libc-test's
 * src/common/test.h, Copyright (c) 2005-2013 libc-test AUTHORS, MIT.
 * See third_party/libc-test/COPYRIGHT. No libc function under test is replaced.
 */
#ifndef KALEIDOS_COMPAT_TEST_H
#define KALEIDOS_COMPAT_TEST_H

#include <stddef.h>
#include <stdint.h>

extern volatile int t_status;

#define T_LOC2(l) __FILE__ ":" #l
#define T_LOC1(l) T_LOC2(l)
#define t_error(...) t_printf(T_LOC1(__LINE__) ": " __VA_ARGS__)

int t_printf(const char *s, ...);
void t_randseed(uint64_t s);
uint64_t t_randn(uint64_t n);
void t_shuffle(uint64_t *p, size_t n);

#endif
