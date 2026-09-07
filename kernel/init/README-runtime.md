# Dynamic runtime probe

```sh
bash scripts/build-runtime-test.sh
python3 -m unittest discover -s tests -p test_kernel_runtime.py
```

The builder uses Debian's arm64 GCC to compile `runtime.c` as a PIE,
dynamically linked with pthread support. It bundles that binary as `/nk-init`,
the image's real `ld-linux-aarch64.so.1` and `libc.so.6`, and a small test file
in `kernel/ldk/build/runtime.cpio`. It requires the existing `nethos-ldk`
Docker image; it does not install packages or build a replacement libc.

nk maps ET_EXEC/ET_DYN segments, follows PT_INTERP, enters the interpreter,
and supplies the executable's AT_ENTRY/AT_PHDR plus the interpreter's AT_BASE.
The interpreter performs relocation and loads libraries through Linux file
operations and nk's private file mappings. Its current fixed placement is
at USER_MMAP_TOP, above the downward-growing mmap arena; there is no ASLR.

The probe checks:

- arrival in a real dynamically linked libc application;
- a private writable file mapping at a nonzero offset, zero-filled final
  page, an unchanged file after a private write, and access after closing fd;
- PROT_NONE reservation, MAP_FIXED replacement, mprotect, refusal of an
  invalid descriptor without destroying an existing mapping, and EFAULT
  when a syscall reads a PROT_NONE page;
- pthread_create followed by a worker changing shared state and its own TLS,
  and pthread_join verifying the worker result and the parent's independent TLS.

The first three groups pass in both nk processes. The pthread test currently
fails at creation: clone3 returns ENOSYS safely, glibc falls back to clone,
and nk rejects CLONE_THREAD. The last assertion is an explicit expected
failure, not a claim that threads work. Before clone3 was intercepted, LKL
interpreted this request as a host kernel thread and called a null function.

Private file mappings currently eagerly copy file bytes through Linux pread
into nk pages. They are enough for the tested dynamic loader, but are not a
full Linux mmap implementation: MAP_SHARED, page-cache coherence after later
file writes, writeback/msync, demand paging and SIGBUS past EOF are absent.
Beyond-EOF pages are currently zero-filled. Unsupported mapping flags are
rejected. Mappings respect nk's current user bounds and exclude its inherited
device range. PROT_NONE pages retain their backing but deny EL0 access,
including after fork. There are no general VMA records or mapped-byte quota.

Thread support needs shared address-space lifetime/accounting, Linux task-group
association, clone TLS/TID flags, clear-child-tid/futex wakeup on exit, and
complete thread register state. Passing pthread_create is a separate next
milestone; extending the ELF loader does not provide those semantics.
