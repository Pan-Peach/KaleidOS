/* Portable diagnostic backend for libc-test. Success needs no output.
 * Setting t_status before formatting preserves FAIL if diagnostics fail.
 */
#include <stdarg.h>
#include <stdio.h>
#include "test.h"

volatile int t_status = 0;

int t_printf(const char *format, ...)
{
    va_list args;
    int result;

    t_status = 1;
    va_start(args, format);
    result = vfprintf(stderr, format, args);
    va_end(args);
    fflush(stderr);
    return result;
}
