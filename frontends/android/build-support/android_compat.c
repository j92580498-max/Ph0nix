/* SPDX-License-Identifier: GPL-2.0-or-later */
#include "android_compat.h"
#include <errno.h>

int shm_open(const char *name, int flags, mode_t mode)
{
    (void)name; (void)flags; (void)mode;
    errno = ENOSYS;
    return -1;
}

int shm_unlink(const char *name)
{
    (void)name;
    errno = ENOSYS;
    return -1;
}
