/* SPDX-License-Identifier: GPL-2.0-or-later */
/* Bionic lacks POSIX named shared memory. Anonymous QEMU RAM is supported. */
#ifndef FEATUREPHONE_ANDROID_COMPAT_H
#define FEATUREPHONE_ANDROID_COMPAT_H
#include <sys/types.h>
int shm_open(const char *name, int flags, mode_t mode);
int shm_unlink(const char *name);
#endif
