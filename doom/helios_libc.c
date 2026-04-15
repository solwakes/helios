/*
 * Helios libc stubs for DOOM
 * Provides all C library functions that Doom needs, forwarding to Helios kernel.
 */

typedef unsigned long size_t;
typedef long ssize_t;
typedef long off_t;
typedef int mode_t;

/* va_list support */
typedef __builtin_va_list va_list;
#define va_start(v,l) __builtin_va_start(v,l)
#define va_end(v) __builtin_va_end(v)
#define va_arg(v,l) __builtin_va_arg(v,l)
#define va_copy(d,s) __builtin_va_copy(d,s)

#define NULL ((void*)0)

/* ========================================================================
 * Forward declarations to Rust kernel
 * ======================================================================== */

extern void *helios_alloc(unsigned long size);
extern void helios_dealloc(void *ptr, unsigned long size);
extern void helios_uart_putc(unsigned char c);

/* WAD data from Rust */
extern const unsigned char *helios_get_wad_data(void);
extern unsigned long helios_get_wad_size(void);

/* memcpy/memset/memmove/memcmp from compiler-builtins — do NOT redefine */

/* ========================================================================
 * Memory allocation (header approach: store size in first 16 bytes)
 * ======================================================================== */

void *malloc(size_t size) {
    if (size == 0) size = 1;
    void *raw = helios_alloc(size + 16);
    if (!raw) return NULL;
    *(unsigned long *)raw = size;
    return (char *)raw + 16;
}

void free(void *ptr) {
    if (!ptr) return;
    void *raw = (char *)ptr - 16;
    unsigned long size = *(unsigned long *)raw;
    helios_dealloc(raw, size + 16);
}

void *calloc(size_t n, size_t size) {
    size_t total = n * size;
    void *p = malloc(total);
    if (p) {
        /* use compiler builtin */
        __builtin_memset(p, 0, total);
    }
    return p;
}

void *realloc(void *ptr, size_t new_size) {
    if (!ptr) return malloc(new_size);
    if (new_size == 0) {
        free(ptr);
        return NULL;
    }
    void *raw = (char *)ptr - 16;
    unsigned long old_size = *(unsigned long *)raw;
    void *new_ptr = malloc(new_size);
    if (!new_ptr) return NULL;
    unsigned long copy_size = old_size < new_size ? old_size : new_size;
    __builtin_memcpy(new_ptr, ptr, copy_size);
    free(ptr);
    return new_ptr;
}

/* ========================================================================
 * String functions
 * ======================================================================== */

size_t strlen(const char *s) {
    size_t len = 0;
    while (s[len]) len++;
    return len;
}

char *strcpy(char *dst, const char *src) {
    char *d = dst;
    while ((*d++ = *src++));
    return dst;
}

char *strncpy(char *dst, const char *src, size_t n) {
    size_t i;
    for (i = 0; i < n && src[i]; i++)
        dst[i] = src[i];
    for (; i < n; i++)
        dst[i] = '\0';
    return dst;
}

char *strcat(char *dst, const char *src) {
    char *d = dst + strlen(dst);
    while ((*d++ = *src++));
    return dst;
}

char *strncat(char *dst, const char *src, size_t n) {
    char *d = dst + strlen(dst);
    size_t i;
    for (i = 0; i < n && src[i]; i++)
        d[i] = src[i];
    d[i] = '\0';
    return dst;
}

int strcmp(const char *a, const char *b) {
    while (*a && *a == *b) { a++; b++; }
    return (unsigned char)*a - (unsigned char)*b;
}

int strncmp(const char *a, const char *b, size_t n) {
    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i]) return (unsigned char)a[i] - (unsigned char)b[i];
        if (a[i] == '\0') return 0;
    }
    return 0;
}

static char to_lower(char c) {
    return (c >= 'A' && c <= 'Z') ? c + 32 : c;
}

int strcasecmp(const char *a, const char *b) {
    while (*a && to_lower(*a) == to_lower(*b)) { a++; b++; }
    return (unsigned char)to_lower(*a) - (unsigned char)to_lower(*b);
}

int strncasecmp(const char *a, const char *b, size_t n) {
    for (size_t i = 0; i < n; i++) {
        char la = to_lower(a[i]), lb = to_lower(b[i]);
        if (la != lb) return (unsigned char)la - (unsigned char)lb;
        if (a[i] == '\0') return 0;
    }
    return 0;
}

char *strchr(const char *s, int c) {
    while (*s) {
        if (*s == (char)c) return (char *)s;
        s++;
    }
    return (c == '\0') ? (char *)s : NULL;
}

char *strrchr(const char *s, int c) {
    const char *last = NULL;
    while (*s) {
        if (*s == (char)c) last = s;
        s++;
    }
    if (c == '\0') return (char *)s;
    return (char *)last;
}

char *strstr(const char *haystack, const char *needle) {
    if (!*needle) return (char *)haystack;
    for (; *haystack; haystack++) {
        const char *h = haystack, *n = needle;
        while (*h && *n && *h == *n) { h++; n++; }
        if (!*n) return (char *)haystack;
    }
    return NULL;
}

char *strdup(const char *s) {
    size_t len = strlen(s) + 1;
    char *d = (char *)malloc(len);
    if (d) __builtin_memcpy(d, s, len);
    return d;
}

char *strtok(char *str, const char *delim) {
    static char *last = NULL;
    if (str) last = str;
    if (!last) return NULL;
    /* skip leading delimiters */
    while (*last && strchr(delim, *last)) last++;
    if (!*last) { last = NULL; return NULL; }
    char *start = last;
    while (*last && !strchr(delim, *last)) last++;
    if (*last) { *last = '\0'; last++; }
    else last = NULL;
    return start;
}

/* ========================================================================
 * Character functions
 * ======================================================================== */

int toupper(int c) { return (c >= 'a' && c <= 'z') ? c - 32 : c; }
int tolower(int c) { return (c >= 'A' && c <= 'Z') ? c + 32 : c; }
int isdigit(int c) { return c >= '0' && c <= '9'; }
int isalpha(int c) { return (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z'); }
int isspace(int c) { return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\f' || c == '\v'; }
int isprint(int c) { return c >= 0x20 && c < 0x7f; }
int isupper(int c) { return c >= 'A' && c <= 'Z'; }
int islower(int c) { return c >= 'a' && c <= 'z'; }
int isalnum(int c) { return isalpha(c) || isdigit(c); }
int isxdigit(int c) { return isdigit(c) || (c >= 'a' && c <= 'f') || (c >= 'A' && c <= 'F'); }

/* ========================================================================
 * Conversion functions
 * ======================================================================== */

int atoi(const char *s) {
    int sign = 1, val = 0;
    while (isspace(*s)) s++;
    if (*s == '-') { sign = -1; s++; }
    else if (*s == '+') s++;
    while (isdigit(*s)) { val = val * 10 + (*s - '0'); s++; }
    return sign * val;
}

double atof(const char *s) {
    (void)s;
    return 0.0;
}

long strtol(const char *s, char **endp, int base) {
    long val = 0;
    int sign = 1;
    while (isspace(*s)) s++;
    if (*s == '-') { sign = -1; s++; }
    else if (*s == '+') s++;
    if (base == 0) {
        if (*s == '0' && (s[1] == 'x' || s[1] == 'X')) { base = 16; s += 2; }
        else if (*s == '0') { base = 8; s++; }
        else base = 10;
    } else if (base == 16 && *s == '0' && (s[1] == 'x' || s[1] == 'X')) {
        s += 2;
    }
    while (*s) {
        int d;
        if (*s >= '0' && *s <= '9') d = *s - '0';
        else if (*s >= 'a' && *s <= 'f') d = *s - 'a' + 10;
        else if (*s >= 'A' && *s <= 'F') d = *s - 'A' + 10;
        else break;
        if (d >= base) break;
        val = val * base + d;
        s++;
    }
    if (endp) *endp = (char *)s;
    return sign * val;
}

unsigned long strtoul(const char *s, char **endp, int base) {
    return (unsigned long)strtol(s, endp, base);
}

long long strtoll(const char *s, char **endp, int base) {
    return (long long)strtol(s, endp, base);
}

/* ========================================================================
 * printf family — minimal vsnprintf
 * ======================================================================== */

static void uart_puts(const char *s) {
    while (*s) helios_uart_putc((unsigned char)*s++);
}

static void out_char(char *buf, size_t *pos, size_t max, char c) {
    if (*pos < max - 1) buf[*pos] = c;
    (*pos)++;
}

static void out_string(char *buf, size_t *pos, size_t max, const char *s, int width, int left_align) {
    int len = 0;
    const char *p = s;
    while (*p++) len++;
    int pad = (width > len) ? width - len : 0;
    if (!left_align) for (int i = 0; i < pad; i++) out_char(buf, pos, max, ' ');
    for (int i = 0; i < len; i++) out_char(buf, pos, max, s[i]);
    if (left_align) for (int i = 0; i < pad; i++) out_char(buf, pos, max, ' ');
}

static void out_num(char *buf, size_t *pos, size_t max, unsigned long val,
                    int base, int is_signed, int width, int zero_pad, int left_align, int uppercase) {
    char tmp[24];
    int i = 0;
    int neg = 0;
    if (is_signed && (long)val < 0) { neg = 1; val = -(long)val; }
    if (val == 0) { tmp[i++] = '0'; }
    else {
        const char *digits = uppercase ? "0123456789ABCDEF" : "0123456789abcdef";
        while (val) { tmp[i++] = digits[val % base]; val /= base; }
    }
    int num_len = i + neg;
    int pad = (width > num_len) ? width - num_len : 0;
    if (!left_align && !zero_pad) for (int j = 0; j < pad; j++) out_char(buf, pos, max, ' ');
    if (neg) out_char(buf, pos, max, '-');
    if (!left_align && zero_pad) for (int j = 0; j < pad; j++) out_char(buf, pos, max, '0');
    while (i > 0) out_char(buf, pos, max, tmp[--i]);
    if (left_align) for (int j = 0; j < pad; j++) out_char(buf, pos, max, ' ');
}

int vsnprintf(char *buf, size_t max, const char *fmt, va_list ap) {
    size_t pos = 0;
    if (max == 0) return 0;

    while (*fmt) {
        if (*fmt != '%') {
            out_char(buf, &pos, max, *fmt++);
            continue;
        }
        fmt++; /* skip '%' */

        /* flags */
        int left_align = 0, zero_pad = 0;
        while (*fmt == '-' || *fmt == '0') {
            if (*fmt == '-') left_align = 1;
            if (*fmt == '0') zero_pad = 1;
            fmt++;
        }
        if (left_align) zero_pad = 0;

        /* width */
        int width = 0;
        while (*fmt >= '0' && *fmt <= '9') {
            width = width * 10 + (*fmt - '0');
            fmt++;
        }

        /* length modifier */
        int is_long = 0;
        if (*fmt == 'l') { is_long = 1; fmt++; }
        if (*fmt == 'l') { is_long = 2; fmt++; } /* ll */
        if (*fmt == 'h') { fmt++; if (*fmt == 'h') fmt++; } /* hh or h - ignore */
        if (*fmt == 'z') { is_long = 1; fmt++; } /* size_t */

        switch (*fmt) {
        case 'd': case 'i': {
            long val = is_long ? va_arg(ap, long) : (long)va_arg(ap, int);
            out_num(buf, &pos, max, (unsigned long)val, 10, 1, width, zero_pad, left_align, 0);
            break;
        }
        case 'u': {
            unsigned long val = is_long ? va_arg(ap, unsigned long) : (unsigned long)va_arg(ap, unsigned int);
            out_num(buf, &pos, max, val, 10, 0, width, zero_pad, left_align, 0);
            break;
        }
        case 'x': {
            unsigned long val = is_long ? va_arg(ap, unsigned long) : (unsigned long)va_arg(ap, unsigned int);
            out_num(buf, &pos, max, val, 16, 0, width, zero_pad, left_align, 0);
            break;
        }
        case 'X': {
            unsigned long val = is_long ? va_arg(ap, unsigned long) : (unsigned long)va_arg(ap, unsigned int);
            out_num(buf, &pos, max, val, 16, 0, width, zero_pad, left_align, 1);
            break;
        }
        case 'p': {
            unsigned long val = (unsigned long)va_arg(ap, void*);
            out_char(buf, &pos, max, '0');
            out_char(buf, &pos, max, 'x');
            out_num(buf, &pos, max, val, 16, 0, 0, 0, 0, 0);
            break;
        }
        case 's': {
            const char *s = va_arg(ap, const char *);
            if (!s) s = "(null)";
            out_string(buf, &pos, max, s, width, left_align);
            break;
        }
        case 'c': {
            char c = (char)va_arg(ap, int);
            out_char(buf, &pos, max, c);
            break;
        }
        case '%':
            out_char(buf, &pos, max, '%');
            break;
        default:
            out_char(buf, &pos, max, '%');
            out_char(buf, &pos, max, *fmt);
            break;
        }
        fmt++;
    }

    if (pos < max) buf[pos] = '\0';
    else buf[max - 1] = '\0';
    return (int)pos;
}

int snprintf(char *buf, size_t max, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int ret = vsnprintf(buf, max, fmt, ap);
    va_end(ap);
    return ret;
}

int sprintf(char *buf, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int ret = vsnprintf(buf, 0x7fffffff, fmt, ap);
    va_end(ap);
    return ret;
}

int printf(const char *fmt, ...) {
    char buf[512];
    va_list ap;
    va_start(ap, fmt);
    int ret = vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    uart_puts(buf);
    return ret;
}

/* FILE type - just an int index into memfs */
typedef struct { int idx; } FILE;

int vfprintf(FILE *f, const char *fmt, va_list ap) {
    char buf[512];
    int ret = vsnprintf(buf, sizeof(buf), fmt, ap);
    uart_puts(buf);
    return ret;
}

int fprintf(FILE *f, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int ret = vfprintf(f, fmt, ap);
    va_end(ap);
    return ret;
}

int vprintf(const char *fmt, va_list ap) {
    return vfprintf(NULL, fmt, ap);
}

int puts(const char *s) {
    uart_puts(s);
    helios_uart_putc('\n');
    return 0;
}

int putchar(int c) {
    helios_uart_putc((unsigned char)c);
    return c;
}

int fputs(const char *s, FILE *f) {
    uart_puts(s);
    return 0;
}

/* Minimal sscanf: supports %d and %s only */
int sscanf(const char *str, const char *fmt, ...) {
    va_list ap;
    va_start(ap, fmt);
    int count = 0;

    while (*fmt && *str) {
        if (*fmt == '%') {
            fmt++;
            if (*fmt == 'd') {
                int *ip = va_arg(ap, int *);
                int sign = 1, val = 0;
                while (isspace(*str)) str++;
                if (*str == '-') { sign = -1; str++; }
                else if (*str == '+') str++;
                if (!isdigit(*str)) break;
                while (isdigit(*str)) { val = val * 10 + (*str - '0'); str++; }
                *ip = sign * val;
                count++;
                fmt++;
            } else if (*fmt == 's') {
                char *sp = va_arg(ap, char *);
                while (isspace(*str)) str++;
                while (*str && !isspace(*str)) *sp++ = *str++;
                *sp = '\0';
                count++;
                fmt++;
            } else {
                break;
            }
        } else if (isspace(*fmt)) {
            while (isspace(*str)) str++;
            fmt++;
        } else {
            if (*fmt != *str) break;
            fmt++;
            str++;
        }
    }

    va_end(ap);
    return count;
}

/* ========================================================================
 * File I/O (memfs)
 * ======================================================================== */

#define MAX_MEMFS_FILES 16

struct memfs_file {
    const char *name;
    unsigned char *data;
    unsigned long size;
    unsigned long capacity;
    unsigned long pos;
    int is_open;
    int is_readonly;
};

static struct memfs_file memfs_files[MAX_MEMFS_FILES];

/* We use FILE* = pointer to memfs_file */

/* Fake stdin/stdout/stderr */
static struct memfs_file fake_stdout_file = { "stdout", 0, 0, 0, 0, 1, 0 };
static struct memfs_file fake_stderr_file = { "stderr", 0, 0, 0, 0, 1, 0 };
static struct memfs_file fake_stdin_file  = { "stdin",  0, 0, 0, 0, 1, 1 };

FILE *stdin  = (FILE *)&fake_stdin_file;
FILE *stdout = (FILE *)&fake_stdout_file;
FILE *stderr = (FILE *)&fake_stderr_file;

FILE *fopen(const char *path, const char *mode) {
    (void)mode;
    int slot = -1;
    for (int i = 0; i < MAX_MEMFS_FILES; i++) {
        if (!memfs_files[i].is_open) { slot = i; break; }
    }
    if (slot < 0) return NULL;

    struct memfs_file *f = &memfs_files[slot];
    f->is_open = 1;
    f->pos = 0;
    f->name = path; /* NOTE: caller must keep path alive or we strdup */

    /* Check if this is the WAD file */
    if (strstr(path, "doom1.wad") || strstr(path, "DOOM1.WAD")) {
        f->data = (unsigned char *)helios_get_wad_data();
        f->size = helios_get_wad_size();
        f->capacity = helios_get_wad_size();
        f->is_readonly = 1;
        return (FILE *)f;
    }

    /* Regular file (config, save, etc) - allocate small buffer */
    f->is_readonly = 0;
    if (mode[0] == 'r') {
        /* Read mode on non-WAD file: file not found effectively */
        f->data = NULL;
        f->size = 0;
        f->capacity = 0;
    } else {
        f->capacity = 4096;
        f->data = (unsigned char *)malloc(f->capacity);
        f->size = 0;
    }

    return (FILE *)f;
}

int fclose(FILE *fp) {
    if (!fp) return -1;
    struct memfs_file *f = (struct memfs_file *)fp;
    if (f == &fake_stdout_file || f == &fake_stderr_file || f == &fake_stdin_file) return 0;
    if (!f->is_readonly && f->data) {
        free(f->data);
    }
    f->data = NULL;
    f->size = 0;
    f->capacity = 0;
    f->pos = 0;
    f->is_open = 0;
    f->is_readonly = 0;
    return 0;
}

size_t fread(void *ptr, size_t size, size_t nmemb, FILE *fp) {
    if (!fp) return 0;
    struct memfs_file *f = (struct memfs_file *)fp;
    size_t total = size * nmemb;
    if (f->pos >= f->size) return 0;
    size_t avail = f->size - f->pos;
    if (total > avail) total = avail;
    __builtin_memcpy(ptr, f->data + f->pos, total);
    f->pos += total;
    return total / size;
}

size_t fwrite(const void *ptr, size_t size, size_t nmemb, FILE *fp) {
    if (!fp) return 0;
    struct memfs_file *f = (struct memfs_file *)fp;

    /* stdout/stderr: write to UART */
    if (f == &fake_stdout_file || f == &fake_stderr_file) {
        const unsigned char *p = (const unsigned char *)ptr;
        size_t total = size * nmemb;
        for (size_t i = 0; i < total; i++) helios_uart_putc(p[i]);
        return nmemb;
    }

    if (f->is_readonly) return 0;

    size_t total = size * nmemb;
    size_t needed = f->pos + total;
    if (needed > f->capacity) {
        size_t new_cap = f->capacity * 2;
        if (new_cap < needed) new_cap = needed;
        unsigned char *new_data = (unsigned char *)malloc(new_cap);
        if (!new_data) return 0;
        if (f->data) {
            __builtin_memcpy(new_data, f->data, f->size);
            free(f->data);
        }
        f->data = new_data;
        f->capacity = new_cap;
    }
    __builtin_memcpy(f->data + f->pos, ptr, total);
    f->pos += total;
    if (f->pos > f->size) f->size = f->pos;
    return nmemb;
}

#define SEEK_SET 0
#define SEEK_CUR 1
#define SEEK_END 2

int fseek(FILE *fp, long offset, int whence) {
    if (!fp) return -1;
    struct memfs_file *f = (struct memfs_file *)fp;
    long new_pos;
    switch (whence) {
    case SEEK_SET: new_pos = offset; break;
    case SEEK_CUR: new_pos = (long)f->pos + offset; break;
    case SEEK_END: new_pos = (long)f->size + offset; break;
    default: return -1;
    }
    if (new_pos < 0) new_pos = 0;
    f->pos = (unsigned long)new_pos;
    return 0;
}

long ftell(FILE *fp) {
    if (!fp) return -1;
    struct memfs_file *f = (struct memfs_file *)fp;
    return (long)f->pos;
}

int feof(FILE *fp) {
    if (!fp) return 1;
    struct memfs_file *f = (struct memfs_file *)fp;
    return f->pos >= f->size;
}

int fflush(FILE *fp) {
    (void)fp;
    return 0;
}

int ferror(FILE *fp) {
    (void)fp;
    return 0;
}

void clearerr(FILE *fp) {
    (void)fp;
}

int fgetc(FILE *fp) {
    unsigned char c;
    if (fread(&c, 1, 1, fp) == 1) return c;
    return -1; /* EOF */
}

int ungetc(int c, FILE *fp) {
    if (!fp) return -1;
    struct memfs_file *f = (struct memfs_file *)fp;
    if (f->pos > 0) f->pos--;
    return c;
}

char *fgets(char *buf, int n, FILE *fp) {
    if (n <= 0 || !fp) return NULL;
    int i = 0;
    while (i < n - 1) {
        int c = fgetc(fp);
        if (c < 0) break;
        buf[i++] = (char)c;
        if (c == '\n') break;
    }
    if (i == 0) return NULL;
    buf[i] = '\0';
    return buf;
}

int setvbuf(FILE *fp, char *buf, int mode, size_t size) {
    (void)fp; (void)buf; (void)mode; (void)size;
    return 0;
}

int fileno(FILE *fp) {
    (void)fp;
    return 1;
}

/* ========================================================================
 * Other stubs
 * ======================================================================== */

int errno = 0;

void exit(int code) {
    char buf[64];
    snprintf(buf, sizeof(buf), "DOOM EXIT (code %d)\n", code);
    uart_puts(buf);
    while (1) {
        __asm__ volatile("wfi");
    }
}

void abort(void) {
    uart_puts("DOOM ABORT\n");
    while (1) {
        __asm__ volatile("wfi");
    }
}

int atexit(void (*fn)(void)) {
    (void)fn;
    return 0;
}

char *getenv(const char *name) {
    (void)name;
    return NULL;
}

int system(const char *cmd) {
    (void)cmd;
    return -1;
}

int remove(const char *path) {
    (void)path;
    return 0;
}

int rename(const char *old, const char *new_name) {
    (void)old; (void)new_name;
    return 0;
}

int mkdir(const char *path, mode_t mode) {
    (void)path; (void)mode;
    return 0;
}

struct stat;
int stat(const char *path, struct stat *buf) {
    (void)path; (void)buf;
    return -1;
}

int access(const char *path, int mode) {
    (void)path; (void)mode;
    return -1;
}

/* Simple insertion sort for qsort */
void qsort(void *base, size_t nmemb, size_t size, int (*cmp)(const void *, const void *)) {
    unsigned char *b = (unsigned char *)base;
    /* Use a small stack buffer for swap */
    unsigned char tmp[256];
    unsigned char *swap_buf = tmp;
    if (size > sizeof(tmp)) {
        swap_buf = (unsigned char *)malloc(size);
        if (!swap_buf) return;
    }

    for (size_t i = 1; i < nmemb; i++) {
        size_t j = i;
        while (j > 0 && cmp(b + (j - 1) * size, b + j * size) > 0) {
            /* swap elements j-1 and j */
            __builtin_memcpy(swap_buf, b + (j - 1) * size, size);
            __builtin_memcpy(b + (j - 1) * size, b + j * size, size);
            __builtin_memcpy(b + j * size, swap_buf, size);
            j--;
        }
    }

    if (swap_buf != tmp) free(swap_buf);
}

void *bsearch(const void *key, const void *base, size_t nmemb, size_t size,
              int (*cmp)(const void *, const void *)) {
    const unsigned char *b = (const unsigned char *)base;
    size_t lo = 0, hi = nmemb;
    while (lo < hi) {
        size_t mid = lo + (hi - lo) / 2;
        int r = cmp(key, b + mid * size);
        if (r == 0) return (void *)(b + mid * size);
        if (r < 0) hi = mid;
        else lo = mid + 1;
    }
    return NULL;
}

int abs(int x) { return x < 0 ? -x : x; }

static unsigned long rand_seed = 1;
void srand(unsigned int seed) { rand_seed = seed; }
int rand(void) {
    rand_seed = rand_seed * 6364136223846793005UL + 1442695040888963407UL;
    return (int)((rand_seed >> 33) & 0x7fffffff);
}

double fabs(double x) { return x < 0 ? -x : x; }
double floor(double x) {
    long i = (long)x;
    if (x < 0 && x != (double)i) i--;
    return (double)i;
}
double ceil(double x) {
    long i = (long)x;
    if (x > 0 && x != (double)i) i++;
    return (double)i;
}
double sqrt(double x) {
    if (x <= 0) return 0;
    double guess = x / 2;
    for (int i = 0; i < 20; i++) {
        guess = (guess + x / guess) / 2;
    }
    return guess;
}
float fabsf(float x) { return x < 0 ? -x : x; }
float sqrtf(float x) { return (float)sqrt((double)x); }

/* clock_t / time_t stubs */
typedef long clock_t;
typedef long time_t;

clock_t clock(void) { return 0; }
time_t time(time_t *t) {
    if (t) *t = 0;
    return 0;
}

/* signal stub */
typedef void (*sighandler_t)(int);
sighandler_t signal(int sig, sighandler_t handler) {
    (void)sig; (void)handler;
    return (sighandler_t)0;
}

/* raise stub */
int raise(int sig) {
    (void)sig;
    return 0;
}

/* Weak memcpy/memset/memmove/memcmp in case compiler-builtins doesn't provide them */
__attribute__((weak)) void *memcpy(void *dst, const void *src, size_t n) {
    return __builtin_memcpy(dst, src, n);
}
__attribute__((weak)) void *memset(void *s, int c, size_t n) {
    return __builtin_memset(s, c, n);
}
__attribute__((weak)) void *memmove(void *dst, const void *src, size_t n) {
    return __builtin_memmove(dst, src, n);
}
__attribute__((weak)) int memcmp(const void *a, const void *b, size_t n) {
    return __builtin_memcmp(a, b, n);
}

/* open/close/read/write/lseek stubs for w_file_stdc.c and friends */
int open(const char *path, int flags, ...) {
    (void)path; (void)flags;
    return -1;
}

int close(int fd) {
    (void)fd;
    return -1;
}

ssize_t read(int fd, void *buf, size_t count) {
    (void)fd; (void)buf; (void)count;
    return -1;
}

ssize_t write(int fd, const void *buf, size_t count) {
    if (fd == 1 || fd == 2) {
        const unsigned char *p = (const unsigned char *)buf;
        for (size_t i = 0; i < count; i++) helios_uart_putc(p[i]);
        return (ssize_t)count;
    }
    return -1;
}

off_t lseek(int fd, off_t offset, int whence) {
    (void)fd; (void)offset; (void)whence;
    return -1;
}

int isatty(int fd) {
    (void)fd;
    return 0;
}

/* struct dirent stubs */
typedef struct { int fd; } DIR;
struct dirent { char d_name[256]; };

DIR *opendir(const char *name) { (void)name; return NULL; }
struct dirent *readdir(DIR *d) { (void)d; return NULL; }
int closedir(DIR *d) { (void)d; return 0; }

/* __assert_fail for assert() macro */
void __assert_fail(const char *expr, const char *file, unsigned int line, const char *func) {
    printf("ASSERT FAIL: %s at %s:%u (%s)\n", expr, file, line, func ? func : "?");
    abort();
}

/* getpwuid / uid stubs */
typedef unsigned int uid_t;
uid_t getuid(void) { return 0; }

struct passwd {
    char *pw_name;
    char *pw_dir;
};
static struct passwd fake_pw = { "doom", "/" };
struct passwd *getpwuid(uid_t uid) {
    (void)uid;
    return &fake_pw;
}

/* strerror */
char *strerror(int errnum) {
    (void)errnum;
    return "error";
}

/* perror */
void perror(const char *s) {
    if (s && *s) {
        uart_puts(s);
        uart_puts(": ");
    }
    uart_puts("error\n");
}
