/* Component checks link without QEMU's monitor and error reporting runtime. */
#include "qemu/osdep.h"
#include "qemu/error-report.h"

void error_report(const char *fmt, ...)
{
    va_list args;
    va_start(args, fmt);
    vfprintf(stderr, fmt, args);
    va_end(args);
    fputc('\n', stderr);
}
