#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <stdint.h>
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <sys/mman.h>
#include <pthread.h>
#include <errno.h>

static _Thread_local int local = 17;
static void *worker(void *arg) {
    if (local != 17) return (void *)1;
    local = 29;
    *(int *)arg = 42;
    return (void *)73;
}
int main(void) {
    puts("DYNAMIC_LIBC_OK");
    int fd = open("/etc/mapping-data", O_RDONLY);
    if (fd < 0) { perror("open"); return 1; }
    char *p = mmap(NULL,4096,PROT_READ|PROT_WRITE,MAP_PRIVATE,fd,4096);
    if (p == MAP_FAILED) { perror("file mmap"); return 2; }
    if (memcmp(p,"second page",11) || p[11]) return 3;
    p[0]='X';
    char original=0;
    if (pread(fd,&original,1,4096)!=1 || original!='s') return 4;
    close(fd);
    if (p[1]!='e') return 5;
    munmap(p,4096);
    puts("PRIVATE_FILE_MMAP_OK");
    void *reservation=mmap(NULL,8192,PROT_NONE,MAP_PRIVATE|MAP_ANONYMOUS,-1,0);
    if (reservation==MAP_FAILED) return 6;
    p=mmap(reservation,4096,PROT_READ|PROT_WRITE,MAP_PRIVATE|MAP_FIXED|MAP_ANONYMOUS,-1,0);
    if (p!=reservation) return 7;
    p[0]=11;
    errno=0;
    if (mmap(p,4096,PROT_READ,MAP_PRIVATE|MAP_FIXED,-1,0)!=MAP_FAILED || errno!=EBADF || p[0]!=11) return 11;
    errno=0;
    if (write(1,(char *)reservation+4096,1)!=-1 || errno!=EFAULT) return 12;
    if (mprotect(p,4096,PROT_READ)) return 8;
    munmap(reservation,8192);
    puts("FIXED_MAPPING_OK");
    pthread_t thread;
    int shared=0;
    int rc=pthread_create(&thread,0,worker,&shared);
    if (rc) { printf("PTHREAD_CREATE_FAILED: %d (%s)\n",rc,strerror(rc)); return 9; }
    void *result=0;
    rc=pthread_join(thread,&result);
    if (rc || result!=(void *)73 || shared!=42 || local!=17) return 10;
    puts("PTHREAD_TLS_JOIN_OK");
    return 0;
}
