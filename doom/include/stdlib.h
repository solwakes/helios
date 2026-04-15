#ifndef _STDLIB_H
#define _STDLIB_H
#include <stddef.h>
void *malloc(size_t size);
void *calloc(size_t nmemb, size_t size);
void *realloc(void *ptr, size_t size);
void free(void *ptr);
void exit(int status);
void abort(void);
int atoi(const char *s);
long strtol(const char *s, char **endp, int base);
unsigned long strtoul(const char *s, char **endp, int base);
int abs(int x);
void qsort(void *base, size_t nmemb, size_t size, int (*cmp)(const void *, const void *));
char *getenv(const char *name);
#define RAND_MAX 32767
int rand(void);
void srand(unsigned int seed);
#endif
double atof(const char *s);
int system(const char *cmd);
