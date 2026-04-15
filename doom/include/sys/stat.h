#ifndef _SYS_STAT_H
#define _SYS_STAT_H
#include <stddef.h>
struct stat { int st_mode; long st_size; };
int stat(const char *path, struct stat *buf);
int mkdir(const char *path, int mode);
#endif
